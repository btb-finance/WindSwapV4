//! Wallet linking and signed wallet actions: one Wind Swap account, reachable from many wallets
//! on many chains.
//!
//! [`LinkRegistry`] keeps its links in contract storage; the rest is pure logic. The contract
//! exposes the public methods in `lib.rs`.
//!
//! # Owners
//!
//! Balances and positions belong to an [`Owner`]: the engine's 32-byte account id.
//! - A NEAR account `a` owns `sha256(a)` (the same id positions already used).
//! - A wallet that is not linked to any NEAR account owns `sha256(canonical wallet string)`.
//! - A linked wallet acts for the owner of the NEAR account it is linked to.
//!
//! # Wallet identifiers
//!
//! [`WalletId`] has a canonical string form (also used for JSON via serde):
//! - `near:<account>`: a NEAR account id, e.g. `near:alice.testnet`.
//! - `evm:0x<40 lowercase hex>`: a 20-byte EVM address. Parsing accepts any hex case; output is lowercase.
//! - `ed25519:<base58>`: a 32-byte Ed25519 public key (the NEAR and Solana key format).
//!
//! # Signed messages
//!
//! Every signed message names the contract account (`env::current_account_id()`), so a signature
//! made for one deployment cannot be replayed on another. The wallet signs the exact UTF-8 text
//! (EIP-191 `personal_sign` for EVM, raw bytes for Ed25519).
//!
//! Link message (signed once per wallet, see `LinkRegistry::link`):
//!
//! ```text
//! Wind Swap v4: link wallet {wallet} to account {account} on {contract} nonce {nonce} expires {expires}
//! ```
//!
//! Action message (signed per action, see `execute` in `lib.rs`):
//!
//! ```text
//! Wind Swap v4 action on {contract} by {wallet} nonce {nonce} expires {expires}: {action}
//! ```
//!
//! - `{wallet}`: the canonical wallet string.
//! - `{account}`: the NEAR account id the wallet is linked to.
//! - `{contract}`: the contract account id, e.g. `windswapv4-btb.testnet`.
//! - `{nonce}`: decimal `u64`, unique per wallet and per message kind.
//! - `{expires}`: decimal unix time in seconds. Valid while `now <= expires`.
//! - `{action}`: `serde_json::to_string` of the action enum (externally tagged, fields in
//!   declaration order, no whitespace). See `SignedAction` in `lib.rs`.

use std::fmt;
use std::str::FromStr;

use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::store::{LookupMap, LookupSet};
use near_sdk::{env, AccountId};

/// The engine id of an account, a wallet, or any other owner of balances and positions.
pub type Owner = [u8; 32];

/// Prefix of every link message. Also documents the format at a glance.
pub const LINK_MESSAGE_PREFIX: &str = "Wind Swap v4: link wallet ";

/// Half of the secp256k1 group order `n`. Signatures with `s > n/2` are malleable and rejected.
const SECP256K1_HALF_ORDER: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

/// A wallet on some chain that can be linked to a Wind Swap account.
#[derive(
    BorshSerialize,
    BorshDeserialize,
    Serialize,
    Deserialize,
    Clone,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde", try_from = "String", into = "String")]
pub enum WalletId {
    /// A NEAR account. It is always the account itself and cannot be linked to another one.
    Near(AccountId),
    /// A 20-byte EVM address.
    Evm([u8; 20]),
    /// A 32-byte Ed25519 public key.
    Ed25519([u8; 32]),
}

/// Error from parsing a canonical wallet string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalletIdError {
    /// Missing or unknown `near:` / `evm:` / `ed25519:` prefix.
    UnknownPrefix,
    /// `near:` value is not a valid NEAR account id.
    InvalidAccount,
    /// `evm:` value is not `0x` followed by 40 hex digits.
    InvalidEvmAddress,
    /// `ed25519:` value is not base58 for exactly 32 bytes.
    InvalidEd25519Key,
}

impl fmt::Display for WalletIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::UnknownPrefix => "wallet id needs a near:, evm: or ed25519: prefix",
            Self::InvalidAccount => "invalid NEAR account id in wallet id",
            Self::InvalidEvmAddress => "evm wallet id must be 0x followed by 40 hex digits",
            Self::InvalidEd25519Key => "ed25519 wallet id must be base58 for 32 bytes",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for WalletIdError {}

impl fmt::Display for WalletId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Near(account) => write!(f, "near:{account}"),
            Self::Evm(address) => write!(f, "evm:0x{}", hex_lower(address)),
            Self::Ed25519(key) => write!(f, "ed25519:{}", bs58::encode(key).into_string()),
        }
    }
}

impl FromStr for WalletId {
    type Err = WalletIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix("near:") {
            let account = AccountId::from_str(rest).map_err(|_| WalletIdError::InvalidAccount)?;
            return Ok(Self::Near(account));
        }
        if let Some(rest) = s.strip_prefix("evm:") {
            let hex = rest
                .strip_prefix("0x")
                .ok_or(WalletIdError::InvalidEvmAddress)?;
            let bytes = hex_decode(hex).ok_or(WalletIdError::InvalidEvmAddress)?;
            let address: [u8; 20] = bytes
                .try_into()
                .map_err(|_| WalletIdError::InvalidEvmAddress)?;
            return Ok(Self::Evm(address));
        }
        if let Some(rest) = s.strip_prefix("ed25519:") {
            let bytes = bs58::decode(rest)
                .into_vec()
                .map_err(|_| WalletIdError::InvalidEd25519Key)?;
            let key: [u8; 32] = bytes
                .try_into()
                .map_err(|_| WalletIdError::InvalidEd25519Key)?;
            return Ok(Self::Ed25519(key));
        }
        Err(WalletIdError::UnknownPrefix)
    }
}

impl TryFrom<String> for WalletId {
    type Error = WalletIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<WalletId> for String {
    fn from(wallet: WalletId) -> Self {
        wallet.to_string()
    }
}

/// Builds the exact link message the wallet must sign. See the module docs for the format.
pub fn link_message(
    wallet: &WalletId,
    account: &AccountId,
    contract: &AccountId,
    nonce: u64,
    expires: u64,
) -> String {
    format!(
        "{LINK_MESSAGE_PREFIX}{wallet} to account {account} on {contract} nonce {nonce} expires {expires}"
    )
}

/// Builds the exact action message the wallet must sign. `action_json` is the canonical JSON of
/// the action (`serde_json::to_string` of `SignedAction`). See the module docs for the format.
pub fn action_message(
    contract: &AccountId,
    wallet: &WalletId,
    nonce: u64,
    expires: u64,
    action_json: &str,
) -> String {
    format!(
        "Wind Swap v4 action on {contract} by {wallet} nonce {nonce} expires {expires}: {action_json}"
    )
}

/// Verifies an EIP-191 `personal_sign` signature over `msg` (65 bytes `r || s || v`).
///
/// `v` may be 27/28 or 0/1. High-s signatures are rejected. Returns true only when the
/// recovered address equals `expected`.
pub fn verify_evm_link(msg: &str, sig: &[u8], expected: [u8; 20]) -> bool {
    recover_evm_address(msg, sig) == Some(expected)
}

/// Verifies an Ed25519 signature over the raw bytes of `msg` (as Solana `signMessage` produces).
pub fn verify_ed25519_link(msg: &str, sig: &[u8; 64], pk: &[u8; 32]) -> bool {
    env::ed25519_verify(sig, msg.as_bytes(), pk)
}

/// Verifies a wallet's signature over `msg`. A signature of the wrong type for the wallet fails.
pub fn verify_wallet_signature(msg: &str, wallet: &WalletId, sig: WalletSignature<'_>) -> bool {
    match (wallet, sig) {
        (WalletId::Evm(address), WalletSignature::Evm(s)) => verify_evm_link(msg, s, *address),
        (WalletId::Ed25519(pk), WalletSignature::Ed25519(s)) => verify_ed25519_link(msg, s, pk),
        _ => false,
    }
}

/// Deterministic engine id for a wallet. See the module docs.
pub fn engine_id_for(wallet: &WalletId) -> Owner {
    match wallet {
        WalletId::Near(account) => owner_of_account(account),
        other => env::sha256_array(other.to_string().as_bytes()),
    }
}

/// The owner of a NEAR account: `sha256(account id)`, the same value positions always used.
pub fn owner_of_account(account: &AccountId) -> Owner {
    env::sha256_array(account.as_bytes())
}

/// Lower-case hex of an owner id (64 characters, no prefix).
pub fn owner_hex(owner: &Owner) -> String {
    hex_lower(owner)
}

/// Parses an owner id written as 64 hex digits (an optional `0x` prefix is allowed).
pub fn parse_owner_hex(s: &str) -> Option<Owner> {
    let bytes = hex_decode(s.strip_prefix("0x").unwrap_or(s))?;
    bytes.try_into().ok()
}

/// A request to link `wallet` to `account`. The wallet must sign `message()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRequest {
    pub wallet: WalletId,
    pub account: AccountId,
    /// The contract account the wallet signs for. Normally `env::current_account_id()`.
    pub contract: AccountId,
    pub nonce: u64,
    pub expires: u64,
}

impl LinkRequest {
    /// The exact text the wallet must sign.
    pub fn message(&self) -> String {
        link_message(
            &self.wallet,
            &self.account,
            &self.contract,
            self.nonce,
            self.expires,
        )
    }
}

/// A wallet's signature over a message, borrowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletSignature<'a> {
    /// 65-byte EIP-191 signature `r || s || v`.
    Evm(&'a [u8]),
    /// 64-byte Ed25519 signature.
    Ed25519(&'a [u8; 64]),
}

/// A decoded wallet signature, owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureBytes {
    Evm([u8; 65]),
    Ed25519([u8; 64]),
}

impl SignatureBytes {
    pub fn as_wallet_signature(&self) -> WalletSignature<'_> {
        match self {
            Self::Evm(s) => WalletSignature::Evm(s),
            Self::Ed25519(s) => WalletSignature::Ed25519(s),
        }
    }
}

/// Decodes a signature string for `wallet`.
///
/// - EVM: 65 bytes as hex, with or without `0x`.
/// - Ed25519: 64 bytes, as 128 hex digits (with or without `0x`) or as base58.
pub fn parse_signature(wallet: &WalletId, s: &str) -> Result<SignatureBytes, String> {
    match wallet {
        WalletId::Near(_) => Err("NEAR accounts call methods directly and do not sign".into()),
        WalletId::Evm(_) => {
            let bytes =
                hex_decode(s.strip_prefix("0x").unwrap_or(s)).ok_or("EVM signature must be hex")?;
            let sig: [u8; 65] = bytes
                .try_into()
                .map_err(|_| "EVM signature must be 65 bytes".to_string())?;
            Ok(SignatureBytes::Evm(sig))
        }
        WalletId::Ed25519(_) => {
            let hex_digits = s.strip_prefix("0x").unwrap_or(s);
            let bytes = if hex_digits.len() == 128 {
                hex_decode(hex_digits).ok_or("Ed25519 signature must be hex or base58")?
            } else {
                bs58::decode(s)
                    .into_vec()
                    .map_err(|_| "Ed25519 signature must be hex or base58".to_string())?
            };
            let sig: [u8; 64] = bytes
                .try_into()
                .map_err(|_| "Ed25519 signature must be 64 bytes".to_string())?;
            Ok(SignatureBytes::Ed25519(sig))
        }
    }
}

/// Why a link or unlink was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    /// NEAR accounts are themselves and cannot be linked.
    NearWalletNotLinkable,
    /// `now` is after the request's `expires`.
    Expired,
    /// This `(wallet, nonce)` was already used.
    NonceUsed,
    /// The signature does not verify for this wallet, or its type does not match the wallet.
    BadSignature,
    /// The wallet is already linked to a different account.
    AlreadyLinked,
    /// The wallet is not linked to the given account.
    NotLinked,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::NearWalletNotLinkable => {
                "NEAR wallets are the account itself and cannot be linked"
            }
            Self::Expired => "link request has expired",
            Self::NonceUsed => "link nonce was already used for this wallet",
            Self::BadSignature => "wallet signature does not verify",
            Self::AlreadyLinked => "wallet is already linked to another account",
            Self::NotLinked => "wallet is not linked to this account",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for LinkError {}

/// Wallet-to-account links, kept in contract storage.
///
/// Rules:
/// - A wallet belongs to at most one account. Re-linking it to the same account is a no-op.
/// - Linking needs a valid wallet signature over the request message, an unexpired request,
///   and an unused `(wallet, nonce)`. A consumed nonce stays consumed after unlinking, so a
///   signed request cannot be replayed.
/// - Account consent is not checked here. The contract must check that the caller (the NEAR
///   predecessor) is `account` before calling [`LinkRegistry::link`] and [`LinkRegistry::unlink`].
/// - NEAR wallets are never stored. They resolve to [`owner_of_account`] directly.
/// - Unlinked wallets resolve to their own [`engine_id_for`] value.
#[derive(BorshSerialize, BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
pub struct LinkRegistry {
    /// Linked wallet -> owner of its account.
    wallet_to_owner: LookupMap<WalletId, Owner>,
    /// Owner of an account -> its linked wallets.
    owner_to_wallets: LookupMap<Owner, Vec<WalletId>>,
    /// Consumed `(wallet, nonce)` pairs. Never pruned.
    used_nonces: LookupSet<(WalletId, u64)>,
}

impl LinkRegistry {
    /// A registry whose collections use the storage prefixes `{prefix}:wallet`, `{prefix}:owner`
    /// and `{prefix}:nonce`. `prefix` must be unique in the contract.
    pub fn new(prefix: &str) -> Self {
        let key = |suffix: &str| [prefix.as_bytes(), suffix.as_bytes()].concat();
        Self {
            wallet_to_owner: LookupMap::new(key(":wallet")),
            owner_to_wallets: LookupMap::new(key(":owner")),
            used_nonces: LookupSet::new(key(":nonce")),
        }
    }

    /// Links the request's wallet to its account after checking the wallet's signature.
    ///
    /// `now` is the current unix time in seconds.
    pub fn link(
        &mut self,
        req: &LinkRequest,
        sig: WalletSignature<'_>,
        now: u64,
    ) -> Result<(), LinkError> {
        if matches!(req.wallet, WalletId::Near(_)) {
            return Err(LinkError::NearWalletNotLinkable);
        }
        if now > req.expires {
            return Err(LinkError::Expired);
        }
        let nonce_key = (req.wallet.clone(), req.nonce);
        if self.used_nonces.contains(&nonce_key) {
            return Err(LinkError::NonceUsed);
        }
        if !verify_wallet_signature(&req.message(), &req.wallet, sig) {
            return Err(LinkError::BadSignature);
        }

        let owner = owner_of_account(&req.account);
        match self.wallet_to_owner.get(&req.wallet) {
            Some(existing) if *existing == owner => return Ok(()),
            Some(_) => return Err(LinkError::AlreadyLinked),
            None => {}
        }

        self.wallet_to_owner.insert(req.wallet.clone(), owner);
        let mut wallets = self
            .owner_to_wallets
            .get(&owner)
            .cloned()
            .unwrap_or_default();
        wallets.push(req.wallet.clone());
        self.owner_to_wallets.insert(owner, wallets);
        self.used_nonces.insert(nonce_key);
        Ok(())
    }

    /// Unlinks `wallet` from `account`. The nonce stays consumed.
    pub fn unlink(&mut self, account: &AccountId, wallet: &WalletId) -> Result<(), LinkError> {
        let owner = owner_of_account(account);
        if self.wallet_to_owner.get(wallet) != Some(&owner) {
            return Err(LinkError::NotLinked);
        }
        self.wallet_to_owner.remove(wallet);
        let mut wallets = self
            .owner_to_wallets
            .get(&owner)
            .cloned()
            .unwrap_or_default();
        wallets.retain(|w| w != wallet);
        if wallets.is_empty() {
            self.owner_to_wallets.remove(&owner);
        } else {
            self.owner_to_wallets.insert(owner, wallets);
        }
        Ok(())
    }

    /// Owner that `wallet` acts for.
    pub fn resolve(&self, wallet: &WalletId) -> Owner {
        self.wallet_to_owner
            .get(wallet)
            .copied()
            .unwrap_or_else(|| engine_id_for(wallet))
    }

    /// Wallets linked to `account`, in canonical order.
    pub fn wallets_of(&self, account: &AccountId) -> Vec<WalletId> {
        let mut wallets = self
            .owner_to_wallets
            .get(&owner_of_account(account))
            .cloned()
            .unwrap_or_default();
        wallets.sort();
        wallets
    }
}

/// EIP-191 `personal_sign` digest: `keccak256("\x19Ethereum Signed Message:\n" + len + msg)`.
fn eip191_hash(msg: &str) -> [u8; 32] {
    let mut data = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
    data.extend_from_slice(msg.as_bytes());
    env::keccak256_array(&data)
}

fn recover_evm_address(msg: &str, sig: &[u8]) -> Option<[u8; 20]> {
    let sig: &[u8; 65] = sig.try_into().ok()?;
    let v = match sig[64] {
        27 | 0 => 0,
        28 | 1 => 1,
        _ => return None,
    };
    let s = &sig[32..64];
    if s > SECP256K1_HALF_ORDER.as_slice() {
        return None;
    }
    let pubkey = env::ecrecover(&eip191_hash(msg), &sig[..64], v, true)?;
    let hash = env::keccak256_array(pubkey);
    hash[12..].try_into().ok()
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Strict hex decoding: even length, ASCII hex digits only.
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.is_ascii() || s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
