//! Contract-level tests for wallet linking, owner-keyed deposits and `execute`.

use k256::ecdsa::SigningKey;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId};

use super::*;
use crate::accounts::{action_message, owner_hex, owner_of_account, LinkRequest, WalletId};
use crate::accounts_tests::{evm_sign, evm_wallet, hex, sk};

const OWNER: &str = "owner.testnet";
const SELF: &str = "windswapv4-btb.testnet";
const ALICE: &str = "alice.testnet";
const BOB: &str = "bob.testnet";
const USDC: &str = "usdc.fakes.testnet";
const WNEAR: &str = "wnear.fakes.testnet";
const TICK_SPACING: i32 = 3;
const Q96_STR: &str = "79228162514264337593543950336";
const MIN_LIMIT: &str = "4295128740";
const FUNDS: u128 = 1_000_000_000_000_000_000_000_000; // 1e24
const LIQUIDITY: u128 = 1_000_000_000_000_000_000; // 1e18
const TO_ADDRESS: &str = "0x2222222222222222222222222222222222222222";
/// The internal id of the registered native Base Sepolia ETH asset.
const ETH: &str = "base-sepolia:eth";
const BASE_SEPOLIA: u64 = 84532;
const RELAYER: &str = "relayer.testnet";
const DEPOSIT_TX: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
/// Times in unix seconds used by the tests. The block time is `NOW`.
const NOW: u64 = 1_000;
const EXPIRES: u64 = 2_000;

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}

/// Sets the caller, block time and deposit. The contract account is `SELF`.
fn ctx(predecessor: &str, block_secs: u64, deposit: u128) {
    let mut builder = VMContextBuilder::new();
    builder
        .current_account_id(acct(SELF))
        .predecessor_account_id(acct(predecessor))
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .block_timestamp(block_secs * 1_000_000_000);
    testing_env!(
        builder.build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        vec![]
    );
}

fn setup() -> Contract {
    ctx(OWNER, NOW, 0);
    Contract::new(acct(OWNER))
}

fn balance(c: &Contract, who: &str, token: &str) -> u128 {
    c.get_balance(acct(who), token.to_string()).0
}

fn owner_balance(c: &Contract, owner: &str, token: &str) -> u128 {
    c.get_balance_of_owner(owner.to_string(), token.to_string())
        .0
}

fn owner_id_of(name: &str) -> String {
    owner_hex(&owner_of_account(&acct(name)))
}

/// Whitelists `token` if needed, then delivers `amount` from the token contract to `receiver`.
fn deliver(c: &mut Contract, token: &str, sender: &str, amount: u128, msg: String) {
    ctx(OWNER, NOW, 0);
    if !c.get_tokens().contains(&acct(token)) {
        c.add_token(acct(token));
    }
    ctx(token, NOW, 0);
    match c.ft_on_transfer(acct(sender), U128(amount), msg) {
        PromiseOrValue::Value(U128(0)) => {}
        _ => panic!("whitelisted deposit must not be refunded"),
    }
}

/// A NEAR user's own deposit.
fn fund(c: &mut Contract, who: &str, token: &str, amount: u128) {
    deliver(c, token, who, amount, String::new());
}

/// A deposit for a wallet: `msg = {"owner":"<wallet>"}`, paid by `sender`.
fn deposit_for(c: &mut Contract, sender: &str, wallet: &str, token: &str, amount: u128) {
    deliver(
        c,
        token,
        sender,
        amount,
        format!("{{\"owner\":\"{wallet}\"}}"),
    );
}

/// The wallet's signature over the link message for `account` on `contract`.
fn evm_link_sig(
    key: &SigningKey,
    account: &str,
    contract: &str,
    nonce: u64,
    expires: u64,
) -> String {
    let req = LinkRequest {
        wallet: evm_wallet(key),
        account: acct(account),
        contract: acct(contract),
        nonce,
        expires,
    };
    format!("0x{}", hex(&evm_sign(key, &req.message())))
}

/// The wallet's signature over an action message, for `contract`.
fn evm_action_sig(
    key: &SigningKey,
    contract: &str,
    action: &SignedAction,
    nonce: u64,
    expires: u64,
) -> String {
    let json = near_sdk::serde_json::to_string(action).unwrap();
    let msg = action_message(&acct(contract), &evm_wallet(key), nonce, expires, &json);
    format!("0x{}", hex(&evm_sign(key, &msg)))
}

/// Registers native Base Sepolia ETH, a relayer, and the chain's signing settings.
fn register_eth(c: &mut Contract) {
    ctx(OWNER, NOW, 0);
    c.register_evm_asset(
        BASE_SEPOLIA,
        "native".into(),
        ETH.into(),
        18,
        U128(FUNDS),
        U128(FUNDS),
    );
    c.add_relayer(acct(RELAYER));
}

/// The relayer reports a native ETH deposit of `amount` from the EVM `wallet` (`evm:0x…`).
fn report_eth_deposit(c: &mut Contract, wallet: &str, amount: u128) {
    ctx(RELAYER, NOW, 0);
    let from = wallet.strip_prefix("evm:").unwrap();
    let credited = c.report_evm_deposit(
        BASE_SEPOLIA,
        DEPOSIT_TX.into(),
        0,
        from.into(),
        "native".into(),
        U128(amount),
    );
    assert!(credited);
}

fn key_wallet(key: &SigningKey) -> String {
    evm_wallet(key).to_string()
}

/// An EVM wallet's relayed swap call: `BOB` relays, `SELF` is the contract.
fn swap_action(pool_id: u64, amount: &str) -> SignedAction {
    SignedAction::Swap {
        pool_id,
        zero_for_one: true,
        amount_specified: amount.into(),
        sqrt_price_limit_x96: MIN_LIMIT.into(),
    }
}

/// Creates a USDC/WNEAR pool, funds ALICE and mints liquidity for her. Returns the pool id.
fn pool_with_alice_liquidity(c: &mut Contract) -> u64 {
    fund(c, ALICE, USDC, FUNDS);
    fund(c, ALICE, WNEAR, FUNDS);
    ctx(OWNER, NOW, 0);
    let pool = c.create_pool(USDC.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    ctx(ALICE, NOW + 10, 0);
    c.mint_position(pool, -600, 600, U128(LIQUIDITY));
    pool
}

// ---------- linking ----------

#[test]
fn link_evm_wallet_to_caller_account() {
    let mut c = setup();
    let key = sk(40);
    let wallet = key_wallet(&key);
    ctx(ALICE, NOW, 0);
    c.link_wallet(
        wallet.clone(),
        1,
        EXPIRES,
        evm_link_sig(&key, ALICE, SELF, 1, EXPIRES),
    );

    assert_eq!(c.get_wallets(acct(ALICE)), vec![wallet.clone()]);
    assert_eq!(c.resolve_wallet(wallet), owner_id_of(ALICE));
}

#[test]
fn link_ed25519_wallet_accepts_base58_and_hex_signatures() {
    use ed25519_dalek::{Signer, SigningKey as EdKey};

    let key = EdKey::from_bytes(&[41u8; 32]);
    let pk = key.verifying_key().to_bytes();
    let wallet = WalletId::Ed25519(pk).to_string();
    let mut c = setup();

    let req = |nonce| LinkRequest {
        wallet: WalletId::Ed25519(pk),
        account: acct(ALICE),
        contract: acct(SELF),
        nonce,
        expires: EXPIRES,
    };
    let sig1 = key.sign(req(1).message().as_bytes()).to_bytes();
    ctx(ALICE, NOW, 0);
    c.link_wallet(wallet.clone(), 1, EXPIRES, bs58::encode(sig1).into_string());
    assert_eq!(c.resolve_wallet(wallet.clone()), owner_id_of(ALICE));

    let sig2 = key.sign(req(2).message().as_bytes()).to_bytes();
    c.link_wallet(wallet.clone(), 2, EXPIRES, hex(&sig2));
    assert_eq!(c.get_wallets(acct(ALICE)), vec![wallet]);
}

#[test]
#[should_panic(expected = "wallet signature does not verify")]
fn link_rejects_signature_for_another_contract() {
    let mut c = setup();
    let key = sk(42);
    ctx(ALICE, NOW, 0);
    c.link_wallet(
        key_wallet(&key),
        1,
        EXPIRES,
        evm_link_sig(&key, ALICE, "other.testnet", 1, EXPIRES),
    );
}

#[test]
#[should_panic(expected = "wallet signature does not verify")]
fn link_rejects_signature_for_another_account() {
    let mut c = setup();
    let key = sk(42);
    ctx(BOB, NOW, 0);
    // Signed for ALICE, submitted by BOB: the caller is the account, so this must fail.
    c.link_wallet(
        key_wallet(&key),
        1,
        EXPIRES,
        evm_link_sig(&key, ALICE, SELF, 1, EXPIRES),
    );
}

#[test]
#[should_panic(expected = "link nonce was already used")]
fn link_rejects_nonce_replay_after_unlink() {
    let mut c = setup();
    let key = sk(43);
    let wallet = key_wallet(&key);
    let sig = evm_link_sig(&key, ALICE, SELF, 7, EXPIRES);
    ctx(ALICE, NOW, 0);
    c.link_wallet(wallet.clone(), 7, EXPIRES, sig.clone());
    c.unlink_wallet(wallet.clone());
    c.link_wallet(wallet, 7, EXPIRES, sig);
}

#[test]
#[should_panic(expected = "link request has expired")]
fn link_rejects_expired_request() {
    let mut c = setup();
    let key = sk(44);
    ctx(ALICE, NOW + 1_001, 0);
    c.link_wallet(
        key_wallet(&key),
        1,
        EXPIRES,
        evm_link_sig(&key, ALICE, SELF, 1, EXPIRES),
    );
}

#[test]
#[should_panic(expected = "wallet is not linked to this account")]
fn unlink_by_account_that_does_not_own_the_wallet_panics() {
    let mut c = setup();
    let key = sk(45);
    let wallet = key_wallet(&key);
    ctx(ALICE, NOW, 0);
    c.link_wallet(
        wallet.clone(),
        1,
        EXPIRES,
        evm_link_sig(&key, ALICE, SELF, 1, EXPIRES),
    );
    ctx(BOB, NOW, 0);
    c.unlink_wallet(wallet);
}

#[test]
fn unlink_removes_wallet_from_account() {
    let mut c = setup();
    let key = sk(46);
    let wallet = key_wallet(&key);
    ctx(ALICE, NOW, 0);
    c.link_wallet(
        wallet.clone(),
        1,
        EXPIRES,
        evm_link_sig(&key, ALICE, SELF, 1, EXPIRES),
    );
    c.unlink_wallet(wallet.clone());
    assert!(c.get_wallets(acct(ALICE)).is_empty());
    assert_eq!(
        c.resolve_wallet(wallet.clone()),
        owner_hex(&engine_id_for_str(&wallet))
    );
}

fn engine_id_for_str(wallet: &str) -> Owner {
    crate::accounts::engine_id_for(&wallet.parse().unwrap())
}

// ---------- deposits for wallets ----------

#[test]
fn deposit_with_owner_msg_credits_the_wallet_not_the_sender() {
    let mut c = setup();
    let key = sk(47);
    let wallet = key_wallet(&key);
    deposit_for(&mut c, BOB, &wallet, USDC, 500);

    let owner = c.resolve_wallet(wallet);
    assert_eq!(owner_balance(&c, &owner, USDC), 500);
    assert_eq!(balance(&c, BOB, USDC), 0, "the sender is not credited");
}

#[test]
fn deposit_with_near_owner_msg_credits_that_account() {
    let mut c = setup();
    deliver(
        &mut c,
        USDC,
        BOB,
        77,
        format!("{{\"owner\":\"near:{ALICE}\"}}"),
    );
    assert_eq!(balance(&c, ALICE, USDC), 77);
    assert_eq!(balance(&c, BOB, USDC), 0);
}

#[test]
#[should_panic(expected = "wallet id needs a near:, evm: or ed25519: prefix")]
fn deposit_with_invalid_owner_panics_so_it_is_refunded() {
    let mut c = setup();
    deliver(&mut c, USDC, BOB, 1, "{\"owner\":\"alice.testnet\"}".into());
}

#[test]
fn deposit_with_other_msg_still_credits_sender() {
    let mut c = setup();
    deliver(&mut c, USDC, BOB, 9, "{\"hello\":true}".into());
    assert_eq!(balance(&c, BOB, USDC), 9);
}

// ---------- execute ----------

#[test]
fn wallet_only_evm_user_swaps_with_its_own_balance() {
    let mut c = setup();
    let pool = pool_with_alice_liquidity(&mut c);
    let key = sk(50);
    let wallet = key_wallet(&key);
    let owner = c.resolve_wallet(wallet.clone());
    deposit_for(&mut c, BOB, &wallet, USDC, 1_000_000_000);

    let alice_before = (balance(&c, ALICE, USDC), balance(&c, ALICE, WNEAR));
    let action = swap_action(pool, "100000000");
    let sig = evm_action_sig(&key, SELF, &action, 1, EXPIRES);

    // BOB relays the signed action and pays the gas.
    ctx(BOB, NOW + 20, 0);
    let result = c.execute(wallet, action, 1, EXPIRES, sig).value();
    match result {
        ActionResult::Swap([amount0, _]) => assert_eq!(amount0, "100000000"),
        _ => panic!("expected a swap result"),
    }

    assert_eq!(owner_balance(&c, &owner, USDC), 900_000_000);
    assert!(
        owner_balance(&c, &owner, WNEAR) > 0,
        "output credited to the wallet owner"
    );
    assert_eq!(balance(&c, BOB, USDC), 0, "relayer is not charged");
    assert_eq!(
        (balance(&c, ALICE, USDC), balance(&c, ALICE, WNEAR)),
        alice_before,
        "ALICE's balances are untouched"
    );
}

#[test]
fn linked_wallet_acts_for_the_account_balances() {
    let mut c = setup();
    let pool = pool_with_alice_liquidity(&mut c);
    let key = sk(51);
    let wallet = key_wallet(&key);

    ctx(ALICE, NOW + 15, 0);
    c.link_wallet(
        wallet.clone(),
        1,
        EXPIRES,
        evm_link_sig(&key, ALICE, SELF, 1, EXPIRES),
    );
    // A deposit for the wallet lands on ALICE's account.
    let alice_usdc = balance(&c, ALICE, USDC);
    deposit_for(&mut c, BOB, &wallet, USDC, 1_000_000_000);
    assert_eq!(balance(&c, ALICE, USDC), alice_usdc + 1_000_000_000);

    // A signed mint by the wallet creates a position for ALICE.
    let mint = SignedAction::MintPosition {
        pool_id: pool,
        tick_lower: -300,
        tick_upper: 300,
        liquidity: U128(LIQUIDITY / 10),
    };
    let sig = evm_action_sig(&key, SELF, &mint, 2, EXPIRES);
    ctx(BOB, NOW + 20, 0);
    c.execute(wallet.clone(), mint, 2, EXPIRES, sig).detach();
    assert_eq!(
        c.get_position(pool, acct(ALICE), -300, 300).liquidity.0,
        LIQUIDITY / 10
    );
    assert_eq!(c.resolve_wallet(wallet), owner_id_of(ALICE));
}

#[test]
fn execute_withdraw_evm_debits_and_records_pending_withdrawal() {
    let mut c = setup();
    let key = sk(52);
    let wallet = key_wallet(&key);
    register_eth(&mut c);
    report_eth_deposit(&mut c, &wallet, 100);
    let owner = c.resolve_wallet(wallet.clone());

    let action = SignedAction::WithdrawEvm {
        token: ETH.into(),
        amount: U128(40),
        to_chain_id: BASE_SEPOLIA,
        to_address: TO_ADDRESS.into(),
    };
    let sig = evm_action_sig(&key, SELF, &action, 1, EXPIRES);
    ctx(BOB, NOW + 20, 0);
    match c.execute(wallet, action, 1, EXPIRES, sig).value() {
        ActionResult::WithdrawEvm(id) => assert_eq!(id, 0),
        _ => panic!("expected a withdrawal id"),
    }

    assert_eq!(owner_balance(&c, &owner, ETH), 60);
    let record = c.get_withdrawal(0).expect("recorded");
    assert_eq!(record.owner_hex, owner);
    assert_eq!(record.token_id, ETH);
    assert_eq!(record.amount.0, 40);
    assert_eq!(record.to_chain_id, BASE_SEPOLIA);
    assert_eq!(record.to_address, TO_ADDRESS);
    assert_eq!(record.status, "Pending");
}

#[test]
#[should_panic(expected = "is on chain 84532, not 1")]
fn execute_withdraw_evm_rejects_wrong_chain() {
    let mut c = setup();
    let key = sk(57);
    let wallet = key_wallet(&key);
    register_eth(&mut c);
    report_eth_deposit(&mut c, &wallet, 100);
    let action = SignedAction::WithdrawEvm {
        token: ETH.into(),
        amount: U128(40),
        to_chain_id: 1,
        to_address: TO_ADDRESS.into(),
    };
    let sig = evm_action_sig(&key, SELF, &action, 1, EXPIRES);
    ctx(BOB, NOW + 20, 0);
    c.execute(wallet, action, 1, EXPIRES, sig).detach();
}

#[test]
#[should_panic(expected = "wallet signature does not verify")]
fn execute_rejects_action_changed_after_signing() {
    let mut c = setup();
    let key = sk(53);
    let wallet = key_wallet(&key);
    deposit_for(&mut c, BOB, &wallet, USDC, 1_000);
    let signed = swap_action(0, "100");
    let sig = evm_action_sig(&key, SELF, &signed, 1, EXPIRES);
    ctx(BOB, NOW + 20, 0);
    c.execute(wallet, swap_action(0, "999"), 1, EXPIRES, sig)
        .detach();
}

#[test]
#[should_panic(expected = "wallet signature does not verify")]
fn execute_rejects_signature_for_another_contract() {
    let mut c = setup();
    let key = sk(54);
    let wallet = key_wallet(&key);
    let action = swap_action(0, "100");
    let sig = evm_action_sig(&key, "other.testnet", &action, 1, EXPIRES);
    ctx(BOB, NOW + 20, 0);
    c.execute(wallet, action, 1, EXPIRES, sig).detach();
}

#[test]
#[should_panic(expected = "action nonce was already used")]
fn execute_rejects_nonce_replay() {
    let mut c = setup();
    let key = sk(55);
    let wallet = key_wallet(&key);
    register_eth(&mut c);
    report_eth_deposit(&mut c, &wallet, 1_000);
    let action = SignedAction::WithdrawEvm {
        token: ETH.into(),
        amount: U128(1),
        to_chain_id: BASE_SEPOLIA,
        to_address: TO_ADDRESS.into(),
    };
    let sig = evm_action_sig(&key, SELF, &action, 1, EXPIRES);
    ctx(BOB, NOW + 20, 0);
    c.execute(wallet.clone(), action.clone(), 1, EXPIRES, sig.clone())
        .detach();
    c.execute(wallet, action, 1, EXPIRES, sig).detach();
}

#[test]
#[should_panic(expected = "action has expired")]
fn execute_rejects_expired_action() {
    let mut c = setup();
    let key = sk(56);
    let wallet = key_wallet(&key);
    let action = swap_action(0, "100");
    let sig = evm_action_sig(&key, SELF, &action, 1, EXPIRES);
    ctx(BOB, EXPIRES + 1, 0);
    c.execute(wallet, action, 1, EXPIRES, sig).detach();
}

#[test]
#[should_panic(expected = "NEAR accounts call methods directly")]
fn execute_rejects_near_wallet() {
    let mut c = setup();
    ctx(BOB, NOW, 0);
    c.execute(
        format!("near:{ALICE}"),
        swap_action(0, "100"),
        1,
        EXPIRES,
        format!("0x{}", hex(&[0u8; 65])),
    )
    .detach();
}
