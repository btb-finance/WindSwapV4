//! Wallet linking: one Wind Swap account, reachable from many wallets on many chains.
//!
//! This module is pure logic. It has no storage and no public contract methods; the contract
//! wires [`LinkRegistry`] into its state later.
//!
//! # Wallet identifiers
//!
//! [`WalletId`] has a canonical string form (also used for JSON via serde):
//! - `near:<account>`: a NEAR account id, e.g. `near:alice.testnet`.
//! - `evm:0x<40 lowercase hex>`: a 20-byte EVM address. Parsing accepts any hex case; output is lowercase.
//! - `ed25519:<base58>`: a 32-byte Ed25519 public key (the NEAR and Solana key format).
//!
//! # Link message
//!
//! The user signs this exact UTF-8 text with the wallet (EIP-191 `personal_sign` for EVM,
//! raw bytes for Ed25519):
//!
//! ```text
//! Wind Swap v4: link wallet {wallet} to account {account} on {chain_id} nonce {nonce} expires {expires}
//! ```
//!
//! - `{wallet}`: the canonical wallet string (see above).
//! - `{account}`: the NEAR account id the wallet is linked to.
//! - `{chain_id}`: the network the link is registered on, e.g. `testnet` or `mainnet`. The contract
//!   passes it in from its own network so a signature cannot be replayed on another network.
//!   It must be non-empty and contain no whitespace.
//! - `{nonce}`: decimal `u64`, unique per wallet.
//! - `{expires}`: decimal unix time in seconds. The link is valid while `now <= expires`.
//!
//! # Engine ids
//!
//! The engine identifies an account by a 32-byte id:
//! - `Near(a)`: `sha256(a)`, exactly what `engine_account` in `lib.rs` already computes, so
//!   existing NEAR positions keep working.
//! - `Evm` / `Ed25519`: `sha256(canonical string)`, used for wallets that are not linked to any
//!   NEAR account yet.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::{env, AccountId};

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
    chain_id: &str,
    nonce: u64,
    expires: u64,
) -> String {
    format!(
        "{LINK_MESSAGE_PREFIX}{wallet} to account {account} on {chain_id} nonce {nonce} expires {expires}"
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

/// Deterministic 32-byte engine id for a wallet. See the module docs.
pub fn engine_id_for(wallet: &WalletId) -> [u8; 32] {
    match wallet {
        WalletId::Near(account) => env::sha256_array(account.as_bytes()),
        other => env::sha256_array(other.to_string().as_bytes()),
    }
}

/// A request to link `wallet` to `account`. The wallet must sign `message()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRequest {
    pub wallet: WalletId,
    pub account: AccountId,
    pub chain_id: String,
    pub nonce: u64,
    pub expires: u64,
}

impl LinkRequest {
    /// The exact text the wallet must sign.
    pub fn message(&self) -> String {
        link_message(
            &self.wallet,
            &self.account,
            &self.chain_id,
            self.nonce,
            self.expires,
        )
    }
}

/// A wallet's signature over a link message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletSignature<'a> {
    /// 65-byte EIP-191 signature `r || s || v`.
    Evm(&'a [u8]),
    /// 64-byte Ed25519 signature.
    Ed25519(&'a [u8; 64]),
}

/// Why a link or unlink was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    /// NEAR accounts are themselves and cannot be linked.
    NearWalletNotLinkable,
    /// `chain_id` is empty or contains whitespace.
    InvalidChainId,
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
            Self::InvalidChainId => "chain id must be non-empty and contain no whitespace",
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

/// In-memory wallet-to-account links. Storage is added by the contract later.
///
/// Rules:
/// - A wallet belongs to at most one account. Re-linking it to the same account is a no-op.
/// - Linking needs a valid wallet signature over the request message, an unexpired request,
///   and an unused `(wallet, nonce)`. A consumed nonce stays consumed after unlinking, so a
///   signed request cannot be replayed.
/// - Account consent is not checked here. The contract must check that the caller (the NEAR
///   predecessor) is `account` before calling [`LinkRegistry::link`] and [`LinkRegistry::unlink`].
/// - NEAR wallets are never stored. They resolve to `engine_account(account)` directly.
/// - Unlinked wallets resolve to their own [`engine_id_for`] value.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LinkRegistry {
    /// Linked wallet -> engine id of its account.
    wallet_to_account: BTreeMap<WalletId, [u8; 32]>,
    /// Engine id of an account -> its linked wallets.
    account_to_wallets: BTreeMap<[u8; 32], BTreeSet<WalletId>>,
    /// Consumed `(wallet, nonce)` pairs. Never pruned.
    used_nonces: BTreeSet<(WalletId, u64)>,
}

impl LinkRegistry {
    pub fn new() -> Self {
        Self::default()
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
        if !valid_chain_id(&req.chain_id) {
            return Err(LinkError::InvalidChainId);
        }
        if now > req.expires {
            return Err(LinkError::Expired);
        }
        let nonce_key = (req.wallet.clone(), req.nonce);
        if self.used_nonces.contains(&nonce_key) {
            return Err(LinkError::NonceUsed);
        }

        let msg = req.message();
        let signed = match (&req.wallet, sig) {
            (WalletId::Evm(address), WalletSignature::Evm(s)) => verify_evm_link(&msg, s, *address),
            (WalletId::Ed25519(pk), WalletSignature::Ed25519(s)) => {
                verify_ed25519_link(&msg, s, pk)
            }
            _ => false,
        };
        if !signed {
            return Err(LinkError::BadSignature);
        }

        let account = engine_account_of(&req.account);
        match self.wallet_to_account.get(&req.wallet) {
            Some(existing) if *existing == account => return Ok(()),
            Some(_) => return Err(LinkError::AlreadyLinked),
            None => {}
        }

        self.wallet_to_account.insert(req.wallet.clone(), account);
        self.account_to_wallets
            .entry(account)
            .or_default()
            .insert(req.wallet.clone());
        self.used_nonces.insert(nonce_key);
        Ok(())
    }

    /// Unlinks `wallet` from `account`. The nonce stays consumed.
    pub fn unlink(&mut self, account: &AccountId, wallet: &WalletId) -> Result<(), LinkError> {
        let account = engine_account_of(account);
        if self.wallet_to_account.get(wallet) != Some(&account) {
            return Err(LinkError::NotLinked);
        }
        self.wallet_to_account.remove(wallet);
        if let Some(wallets) = self.account_to_wallets.get_mut(&account) {
            wallets.remove(wallet);
            if wallets.is_empty() {
                self.account_to_wallets.remove(&account);
            }
        }
        Ok(())
    }

    /// Engine id of the account that `wallet` acts for.
    pub fn resolve(&self, wallet: &WalletId) -> [u8; 32] {
        self.wallet_to_account
            .get(wallet)
            .copied()
            .unwrap_or_else(|| engine_id_for(wallet))
    }

    /// Wallets linked to `account`, in canonical order.
    pub fn wallets_of(&self, account: &AccountId) -> Vec<WalletId> {
        self.account_to_wallets
            .get(&engine_account_of(account))
            .map(|wallets| wallets.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Same value as `engine_account` in `lib.rs`: `sha256(account_id bytes)`.
fn engine_account_of(account: &AccountId) -> [u8; 32] {
    env::sha256_array(account.as_bytes())
}

fn valid_chain_id(chain_id: &str) -> bool {
    !chain_id.is_empty() && !chain_id.chars().any(char::is_whitespace)
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

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.is_ascii() || s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
