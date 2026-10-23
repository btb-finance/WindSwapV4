use k256::ecdsa::SigningKey;
use near_sdk::{env, AccountId};

use crate::accounts::{
    engine_id_for, link_message, verify_ed25519_link, verify_evm_link, LinkError, LinkRegistry,
    LinkRequest, WalletId, WalletSignature,
};

const NOW: u64 = 1_900_000_000;
const EXPIRES: u64 = NOW + 600;

/// Hardhat / Anvil default account #0. Its address is well known, so it checks address derivation.
const HARDHAT_KEY: [u8; 32] = [
    0xac, 0x09, 0x74, 0xbe, 0xc3, 0x9a, 0x17, 0xe3, 0x6b, 0xa4, 0xa6, 0xb4, 0xd2, 0x38, 0xff, 0x94,
    0x4b, 0xac, 0xb4, 0x78, 0xcb, 0xed, 0x5e, 0xfc, 0xae, 0x78, 0x4d, 0x7b, 0xf4, 0xf2, 0xff, 0x80,
];
const HARDHAT_ADDRESS: [u8; 20] = [
    0xf3, 0x9f, 0xd6, 0xe5, 0x1a, 0xad, 0x88, 0xf6, 0xf4, 0xce, 0x6a, 0xb8, 0x82, 0x72, 0x79, 0xcf,
    0xff, 0xb9, 0x22, 0x66,
];

/// Curve order n, big-endian.
const SECP256K1_ORDER: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];

fn account(s: &str) -> AccountId {
    s.parse().unwrap()
}

fn sk(seed: u8) -> SigningKey {
    let mut bytes = [0u8; 32];
    bytes[31] = seed;
    bytes[0] = 0x11;
    SigningKey::from_slice(&bytes).unwrap()
}

fn evm_address(sk: &SigningKey) -> [u8; 20] {
    let point = sk.verifying_key().to_encoded_point(false);
    let hash = env::keccak256_array(&point.as_bytes()[1..]);
    hash[12..].try_into().unwrap()
}

/// EIP-191 `personal_sign`, written out independently of the crate's own helper.
fn evm_sign(sk: &SigningKey, msg: &str) -> [u8; 65] {
    let mut data = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
    data.extend_from_slice(msg.as_bytes());
    let digest = env::keccak256_array(&data);
    let (sig, recid) = sk.sign_prehash_recoverable(&digest).unwrap();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = recid.to_byte();
    out
}

/// Same signature with `s` replaced by `n - s` and the recovery id flipped: valid but malleable.
fn high_s(sig: &[u8; 65]) -> [u8; 65] {
    let mut out = *sig;
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let diff = SECP256K1_ORDER[i] as i16 - sig[32 + i] as i16 - borrow;
        if diff < 0 {
            out[32 + i] = (diff + 256) as u8;
            borrow = 1;
        } else {
            out[32 + i] = diff as u8;
            borrow = 0;
        }
    }
    assert_eq!(borrow, 0);
    out[64] ^= 1;
    out
}

fn evm_wallet(sk: &SigningKey) -> WalletId {
    WalletId::Evm(evm_address(sk))
}

fn evm_request(sk: &SigningKey, acct: &str, nonce: u64, expires: u64) -> LinkRequest {
    LinkRequest {
        wallet: evm_wallet(sk),
        account: account(acct),
        chain_id: "testnet".to_string(),
        nonce,
        expires,
    }
}

// ---------- canonical wallet ids ----------

#[test]
fn wallet_id_canonical_round_trip() {
    let ids = [
        WalletId::Near(account("alice.testnet")),
        WalletId::Evm(HARDHAT_ADDRESS),
        WalletId::Ed25519([7u8; 32]),
    ];
    for id in ids {
        let s = id.to_string();
        assert_eq!(s.parse::<WalletId>().unwrap(), id, "round trip of {s}");
    }
    assert_eq!(
        WalletId::Evm(HARDHAT_ADDRESS).to_string(),
        "evm:0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
    );
    assert!(WalletId::Near(account("alice.testnet"))
        .to_string()
        .starts_with("near:alice.testnet"));
    assert!(WalletId::Ed25519([7u8; 32])
        .to_string()
        .starts_with("ed25519:"));
}

#[test]
fn wallet_id_evm_parse_accepts_mixed_case_and_outputs_lowercase() {
    let parsed: WalletId = "evm:0xF39Fd6e51aad88F6F4ce6aB8827279cfffB92266"
        .parse()
        .unwrap();
    assert_eq!(parsed, WalletId::Evm(HARDHAT_ADDRESS));
    assert_eq!(
        parsed.to_string(),
        "evm:0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
    );
}

#[test]
fn wallet_id_rejects_malformed_strings() {
    for bad in [
        "",
        "alice.testnet",
        "NEAR:alice.testnet",
        "near:Alice.testnet",
        "near:",
        "evm:f39fd6e51aad88f6f4ce6ab8827279cfffb92266",
        "evm:0x1234",
        "evm:0xzz9fd6e51aad88f6f4ce6ab8827279cfffb92266",
        "ed25519:0OIl",
        "ed25519:",
        "ed25519:2NEpo7TZRRrLZSi2U",
    ] {
        assert!(bad.parse::<WalletId>().is_err(), "should reject {bad:?}");
    }
}

#[test]
fn wallet_id_serde_json_uses_canonical_string() {
    let id = WalletId::Evm(HARDHAT_ADDRESS);
    let json = near_sdk::serde_json::to_string(&id).unwrap();
    assert_eq!(json, format!("\"{id}\""));
    let back: WalletId = near_sdk::serde_json::from_str(&json).unwrap();
    assert_eq!(back, id);
}

#[test]
fn engine_id_for_near_matches_engine_account() {
    let acct = account("alice.testnet");
    let expected = env::sha256_array(acct.as_bytes());
    assert_eq!(engine_id_for(&WalletId::Near(acct)), expected);
}

#[test]
fn engine_id_for_other_wallets_is_sha256_of_canonical_string() {
    let id = WalletId::Evm(HARDHAT_ADDRESS);
    assert_eq!(
        engine_id_for(&id),
        env::sha256_array(id.to_string().as_bytes())
    );
    assert_ne!(
        engine_id_for(&id),
        engine_id_for(&WalletId::Ed25519([7u8; 32]))
    );
}

#[test]
fn link_message_has_exact_format() {
    let msg = link_message(
        &WalletId::Evm(HARDHAT_ADDRESS),
        &account("alice.testnet"),
        "testnet",
        3,
        1_900_000_600,
    );
    assert_eq!(
        msg,
        "Wind Swap v4: link wallet evm:0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266 to account alice.testnet on testnet nonce 3 expires 1900000600"
    );
}

// ---------- EVM verification ----------

#[test]
fn evm_signature_verifies_for_its_address() {
    let key = sk(1);
    let msg = link_message(
        &evm_wallet(&key),
        &account("alice.testnet"),
        "testnet",
        1,
        EXPIRES,
    );
    let sig = evm_sign(&key, &msg);
    assert!(verify_evm_link(&msg, &sig, evm_address(&key)));
}

#[test]
fn evm_signature_rejects_wrong_address() {
    let key = sk(1);
    let other = sk(2);
    let msg = "hello";
    let sig = evm_sign(&key, msg);
    assert!(!verify_evm_link(msg, &sig, evm_address(&other)));
}

#[test]
fn evm_signature_rejects_tampered_message() {
    let key = sk(1);
    let sig = evm_sign(&key, "hello");
    assert!(!verify_evm_link("hellp", &sig, evm_address(&key)));
}

#[test]
fn evm_v_accepts_0_1_and_27_28() {
    let key = sk(3);
    let addr = evm_address(&key);
    let msg = "v variants";
    let sig = evm_sign(&key, msg);
    let recid = sig[64];
    assert!(recid <= 1);

    let mut raw = sig;
    raw[64] = recid;
    let mut offset = sig;
    offset[64] = recid + 27;
    assert!(verify_evm_link(msg, &raw, addr), "v in {{0,1}} must verify");
    assert!(
        verify_evm_link(msg, &offset, addr),
        "v in {{27,28}} must verify"
    );
}

#[test]
fn evm_v_outside_allowed_values_is_rejected() {
    let key = sk(3);
    let addr = evm_address(&key);
    let msg = "v variants";
    let sig = evm_sign(&key, msg);
    for v in [2u8, 26, 29, 255] {
        let mut bad = sig;
        bad[64] = v;
        assert!(!verify_evm_link(msg, &bad, addr), "v={v} must be rejected");
    }
}

#[test]
fn evm_high_s_is_rejected() {
    let key = sk(5);
    let addr = evm_address(&key);
    let msg = "malleable";
    let sig = evm_sign(&key, msg);
    assert!(
        verify_evm_link(msg, &sig, addr),
        "low-s original must verify"
    );
    let malleated = high_s(&sig);
    assert!(!verify_evm_link(msg, &malleated, addr));
}

#[test]
fn evm_rejects_wrong_length_signature() {
    let key = sk(6);
    let sig = evm_sign(&key, "len");
    assert!(!verify_evm_link("len", &sig[..64], evm_address(&key)));
    assert!(!verify_evm_link("len", &[], evm_address(&key)));
}

#[test]
fn evm_known_vector_hardhat_account_0() {
    assert_eq!(
        evm_address(&SigningKey::from_slice(&HARDHAT_KEY).unwrap()),
        HARDHAT_ADDRESS
    );

    let key = SigningKey::from_slice(&HARDHAT_KEY).unwrap();
    let msg = link_message(
        &WalletId::Evm(HARDHAT_ADDRESS),
        &account("alice.testnet"),
        "testnet",
        1,
        1_900_000_600,
    );
    let sig = evm_sign(&key, &msg);
    // Our signer emits v in {0,1}; viem emits 27/28. Same r||s, same signer.
    assert_eq!(hex(&sig[..64]), &KNOWN_VECTOR_SIG_HEX[..128]);
    assert_eq!(sig[64], 1);
    // The viem output, hardcoded byte for byte: the verifier must accept it.
    let fixed = hex_to_65(KNOWN_VECTOR_SIG_HEX);
    assert_eq!(fixed[64], 28);
    assert!(verify_evm_link(&msg, &fixed, HARDHAT_ADDRESS));
}

/// personal_sign of `link_message(evm wallet of HARDHAT_KEY, alice.testnet, testnet, 1, 1_900_000_600)`
/// by the Hardhat account #0 key, as produced by viem's `signMessage` (v = 27/28).
const KNOWN_VECTOR_SIG_HEX: &str = "5a1954491046e4d6aea1c976ae53e841e77da501750275f8067972cd105e04da23aad65d1175d2a60a9e569dfd61fdb3276b3a7d9591ccab73555835978e7b341c";

fn hex_to_65(s: &str) -> [u8; 65] {
    let mut out = [0u8; 65];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------- Ed25519 verification ----------

#[test]
fn ed25519_sign_verify_and_tamper() {
    use ed25519_dalek::{Signer, SigningKey as EdKey};

    let key = EdKey::from_bytes(&[9u8; 32]);
    let pk = key.verifying_key().to_bytes();
    let msg = link_message(
        &WalletId::Ed25519(pk),
        &account("alice.testnet"),
        "testnet",
        1,
        EXPIRES,
    );
    let sig = key.sign(msg.as_bytes()).to_bytes();
    assert!(verify_ed25519_link(&msg, &sig, &pk));

    assert!(!verify_ed25519_link(&format!("{msg}!"), &sig, &pk));

    let mut bad_sig = sig;
    bad_sig[0] ^= 1;
    assert!(!verify_ed25519_link(&msg, &bad_sig, &pk));

    let other = EdKey::from_bytes(&[10u8; 32]).verifying_key().to_bytes();
    assert!(!verify_ed25519_link(&msg, &sig, &other));
}

// ---------- registry rules ----------

#[test]
fn registry_links_evm_wallet_and_resolves_to_account() {
    let key = sk(20);
    let req = evm_request(&key, "alice.testnet", 1, EXPIRES);
    let sig = evm_sign(&key, &req.message());
    let mut reg = LinkRegistry::new();
    reg.link(&req, WalletSignature::Evm(&sig), NOW).unwrap();

    let alice_id = env::sha256_array(b"alice.testnet");
    assert_eq!(reg.resolve(&evm_wallet(&key)), alice_id);
    assert_eq!(
        reg.wallets_of(&account("alice.testnet")),
        vec![evm_wallet(&key)]
    );
}

#[test]
fn registry_unlinked_wallet_resolves_to_its_own_id() {
    let reg = LinkRegistry::new();
    let w = WalletId::Ed25519([3u8; 32]);
    assert_eq!(reg.resolve(&w), engine_id_for(&w));
}

#[test]
fn registry_near_wallet_resolves_to_engine_account_and_cannot_be_linked() {
    let near = WalletId::Near(account("alice.testnet"));
    let reg = LinkRegistry::new();
    assert_eq!(reg.resolve(&near), env::sha256_array(b"alice.testnet"));

    let mut reg = LinkRegistry::new();
    let req = LinkRequest {
        wallet: near,
        account: account("bob.testnet"),
        chain_id: "testnet".into(),
        nonce: 1,
        expires: EXPIRES,
    };
    let sig = [0u8; 65];
    assert_eq!(
        reg.link(&req, WalletSignature::Evm(&sig), NOW),
        Err(LinkError::NearWalletNotLinkable)
    );
}

#[test]
fn registry_rejects_double_link_to_another_account() {
    let key = sk(21);
    let mut reg = LinkRegistry::new();

    let req_a = evm_request(&key, "alice.testnet", 1, EXPIRES);
    let sig_a = evm_sign(&key, &req_a.message());
    reg.link(&req_a, WalletSignature::Evm(&sig_a), NOW).unwrap();

    let req_b = evm_request(&key, "bob.testnet", 2, EXPIRES);
    let sig_b = evm_sign(&key, &req_b.message());
    assert_eq!(
        reg.link(&req_b, WalletSignature::Evm(&sig_b), NOW),
        Err(LinkError::AlreadyLinked)
    );
    assert_eq!(
        reg.resolve(&evm_wallet(&key)),
        env::sha256_array(b"alice.testnet")
    );
}

#[test]
fn registry_relink_to_same_account_is_noop() {
    let key = sk(22);
    let mut reg = LinkRegistry::new();
    let req = evm_request(&key, "alice.testnet", 1, EXPIRES);
    let sig = evm_sign(&key, &req.message());
    reg.link(&req, WalletSignature::Evm(&sig), NOW).unwrap();
    // A fresh request (new nonce) to the same account is an idempotent no-op.
    let again = evm_request(&key, "alice.testnet", 2, EXPIRES);
    let sig_again = evm_sign(&key, &again.message());
    reg.link(&again, WalletSignature::Evm(&sig_again), NOW)
        .unwrap();
    assert_eq!(reg.wallets_of(&account("alice.testnet")).len(), 1);
}

#[test]
fn registry_rejects_expired_request() {
    let key = sk(23);
    let mut reg = LinkRegistry::new();
    let req = evm_request(&key, "alice.testnet", 1, NOW - 1);
    let sig = evm_sign(&key, &req.message());
    assert_eq!(
        reg.link(&req, WalletSignature::Evm(&sig), NOW),
        Err(LinkError::Expired)
    );
    // Still valid exactly at the expiry second.
    let req_edge = evm_request(&key, "alice.testnet", 2, NOW);
    let sig_edge = evm_sign(&key, &req_edge.message());
    reg.link(&req_edge, WalletSignature::Evm(&sig_edge), NOW)
        .unwrap();
}

#[test]
fn registry_rejects_nonce_replay_even_after_unlink() {
    let key = sk(24);
    let mut reg = LinkRegistry::new();
    let req = evm_request(&key, "alice.testnet", 7, EXPIRES);
    let sig = evm_sign(&key, &req.message());
    reg.link(&req, WalletSignature::Evm(&sig), NOW).unwrap();
    reg.unlink(&account("alice.testnet"), &evm_wallet(&key))
        .unwrap();

    assert_eq!(
        reg.link(&req, WalletSignature::Evm(&sig), NOW),
        Err(LinkError::NonceUsed)
    );
}

#[test]
fn registry_rejects_signature_for_different_request() {
    let key = sk(25);
    let mut reg = LinkRegistry::new();
    let signed_for = evm_request(&key, "alice.testnet", 1, EXPIRES);
    let sig = evm_sign(&key, &signed_for.message());
    let claimed = evm_request(&key, "bob.testnet", 1, EXPIRES);
    assert_eq!(
        reg.link(&claimed, WalletSignature::Evm(&sig), NOW),
        Err(LinkError::BadSignature)
    );
}

#[test]
fn registry_rejects_wallet_signature_of_wrong_key() {
    let key = sk(26);
    let imposter = sk(27);
    let mut reg = LinkRegistry::new();
    let req = evm_request(&key, "alice.testnet", 1, EXPIRES);
    let sig = evm_sign(&imposter, &req.message());
    assert_eq!(
        reg.link(&req, WalletSignature::Evm(&sig), NOW),
        Err(LinkError::BadSignature)
    );
}

#[test]
fn registry_rejects_mismatched_signature_type() {
    let key = sk(28);
    let mut reg = LinkRegistry::new();
    let req = evm_request(&key, "alice.testnet", 1, EXPIRES);
    let ed_sig = [0u8; 64];
    assert_eq!(
        reg.link(&req, WalletSignature::Ed25519(&ed_sig), NOW),
        Err(LinkError::BadSignature)
    );
}

#[test]
fn registry_rejects_invalid_chain_id() {
    let key = sk(29);
    let mut reg = LinkRegistry::new();
    let mut req = evm_request(&key, "alice.testnet", 1, EXPIRES);
    req.chain_id = "test net".into();
    let sig = evm_sign(&key, &req.message());
    assert_eq!(
        reg.link(&req, WalletSignature::Evm(&sig), NOW),
        Err(LinkError::InvalidChainId)
    );
}

#[test]
fn registry_ed25519_wallet_links_and_unlinks() {
    use ed25519_dalek::{Signer, SigningKey as EdKey};

    let key = EdKey::from_bytes(&[12u8; 32]);
    let pk = key.verifying_key().to_bytes();
    let req = LinkRequest {
        wallet: WalletId::Ed25519(pk),
        account: account("alice.testnet"),
        chain_id: "testnet".into(),
        nonce: 1,
        expires: EXPIRES,
    };
    let sig = key.sign(req.message().as_bytes()).to_bytes();
    let mut reg = LinkRegistry::new();
    reg.link(&req, WalletSignature::Ed25519(&sig), NOW).unwrap();
    assert_eq!(
        reg.resolve(&WalletId::Ed25519(pk)),
        env::sha256_array(b"alice.testnet")
    );
    reg.unlink(&account("alice.testnet"), &WalletId::Ed25519(pk))
        .unwrap();
    assert_eq!(
        reg.resolve(&WalletId::Ed25519(pk)),
        engine_id_for(&WalletId::Ed25519(pk))
    );
}

#[test]
fn registry_unlink_works_and_is_scoped_to_account() {
    let k1 = sk(30);
    let k2 = sk(31);
    let mut reg = LinkRegistry::new();

    for (key, nonce) in [(&k1, 1u64), (&k2, 2u64)] {
        let req = evm_request(key, "alice.testnet", nonce, EXPIRES);
        let sig = evm_sign(key, &req.message());
        reg.link(&req, WalletSignature::Evm(&sig), NOW).unwrap();
    }
    assert_eq!(reg.wallets_of(&account("alice.testnet")).len(), 2);

    // Wrong account cannot unlink.
    assert_eq!(
        reg.unlink(&account("bob.testnet"), &evm_wallet(&k1)),
        Err(LinkError::NotLinked)
    );

    reg.unlink(&account("alice.testnet"), &evm_wallet(&k1))
        .unwrap();
    assert_eq!(
        reg.wallets_of(&account("alice.testnet")),
        vec![evm_wallet(&k2)]
    );
    assert_eq!(
        reg.resolve(&evm_wallet(&k1)),
        engine_id_for(&evm_wallet(&k1))
    );

    // Unlinking twice fails.
    assert_eq!(
        reg.unlink(&account("alice.testnet"), &evm_wallet(&k1)),
        Err(LinkError::NotLinked)
    );

    reg.unlink(&account("alice.testnet"), &evm_wallet(&k2))
        .unwrap();
    assert!(reg.wallets_of(&account("alice.testnet")).is_empty());
}
