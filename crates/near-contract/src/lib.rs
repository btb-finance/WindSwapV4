//! Wind Swap v4 NEAR contract, v0 (testnet demo).
//!
//! Thin wrapper around `windswap-engine`. Every pool operation is delegated to the engine;
//! this crate only handles NEAR concerns: caller mapping, internal token balances, token
//! deposits and withdrawals, storage and panics. An engine `Err` becomes `env::panic_str`,
//! which makes NEAR roll back the whole call.
//!
//! Tokens:
//! - A token id is the NEP-141 token contract's `AccountId` as a string (e.g. `usdc.fakes.testnet`).
//! - Deposits: the owner whitelists tokens with `add_token`. A user sends tokens with
//!   `ft_transfer_call` to this contract, which lands in `ft_on_transfer` and credits the
//!   internal balance. Non-whitelisted tokens are refunded in full.
//! - Withdrawals: `withdraw` debits the internal balance, then calls `ft_transfer`. If the
//!   transfer fails, `on_withdraw` credits the balance back.
//!
//! v0 limitations (intentional, for the testnet demo):
//! - `msg` in `ft_on_transfer` is ignored. An empty message is a plain deposit. It will carry
//!   swap instructions later.
//! - `create_pool` only checks that token ids parse as account ids. It does not require the
//!   tokens to be whitelisted.
//! - `test_credit` is an owner-only faucet, compiled only with the `testnet-faucet` feature
//!   (on by default). Build mainnet with `--no-default-features`.
//! - Each pool is one Borsh blob in `pools`. This is fine for v0 but should move to
//!   per-tick / per-position storage later, since the blob grows with every tick.
//! - Fees start from `FeeConfig::default()`. The owner changes them with `enable_tick_spacing`,
//!   `set_custom_swap_fee`, `set_custom_unstaked_fee` and `set_fee_whitelist`.
//! - No gauges: `unstaked_fee` is always computed with `gauge_alive = false`. So a custom
//!   unstaked fee is stored but does not change any swap output yet.
//! - Multi-hop routes (`swap_exact_input`, `swap_exact_output`, quotes) run on working copies
//!   of the touched pools. Only a successful swap writes them back.
//! - Chain Signatures: `request_evm_payout` has the MPC network sign an EIP-1559 transfer and
//!   returns the signed raw transaction. See `evm.rs` and docs/chain-signatures.md.

mod evm;

use std::collections::BTreeMap;

use near_sdk::json_types::U128;
use near_sdk::store::{IterableMap, IterableSet, LookupMap};
use near_sdk::{
    assert_one_yocto, env, ext_contract, near, AccountId, BorshStorageKey, Gas, NearToken,
    PanicOnDefault, Promise, PromiseError, PromiseOrValue,
};
use windswap_engine::error::Result as EngineResult;
use windswap_engine::fees::FeeConfig;
use windswap_engine::pool::cl_pool::ClPool;
use windswap_engine::router::{Hop, Router};
use windswap_engine::{I256, U256};

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Pools,
    PoolTokens,
    Balances,
    // Appended last so the prefixes of the existing collections do not change.
    AllowedTokens,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner: AccountId,
    fee_config: FeeConfig,
    /// One Borsh blob per pool (v0). See the module docs.
    pools: IterableMap<u64, ClPool>,
    /// Token pair `(token0, token1)` for each pool id.
    pool_tokens: LookupMap<u64, (String, String)>,
    next_pool_id: u64,
    /// Internal balances keyed by `(account, token id)`. Missing key means zero.
    balances: LookupMap<(AccountId, String), u128>,
    /// Tokens accepted by `ft_on_transfer`. Managed by the owner.
    allowed_tokens: IterableSet<AccountId>,
    /// The Chain Signatures MPC contract that `request_evm_payout` calls. Owner-settable.
    mpc_contract: AccountId,
}

/// State layout of the deployed testnet contract (v1 and v2 share it): everything up to and
/// including `allowed_tokens`. Only used by `migrate`. Field order and types must match the
/// deployed state exactly.
#[near(serializers = [borsh])]
struct ContractV1 {
    owner: AccountId,
    fee_config: FeeConfig,
    pools: IterableMap<u64, ClPool>,
    pool_tokens: LookupMap<u64, (String, String)>,
    next_pool_id: u64,
    balances: LookupMap<(AccountId, String), u128>,
    allowed_tokens: IterableSet<AccountId>,
}

/// The default MPC contract on NEAR testnet. `set_mpc_contract` changes it.
const DEFAULT_MPC_CONTRACT: &str = "v1.signer-prod.testnet";

/// Gas for the `sign` call. The probe measured about 8.6 Tgas burnt over the whole receipt tree
/// for one signature, so this leaves room on the MPC side.
const SIGN_CALL_GAS_TGAS: u64 = 50;
/// Gas for `on_evm_signed`. It only parses the response and builds the transaction.
const CALLBACK_GAS_TGAS: u64 = 20;
/// `request_evm_payout` needs at least this much prepaid gas, so the sign call and the
/// callback both get their share.
const PAYOUT_MIN_PREPAID_GAS_TGAS: u64 = 100;

#[near(serializers = [json])]
pub struct Slot0View {
    pub sqrt_price_x96: String,
    pub tick: i32,
    pub observation_index: u16,
    pub observation_cardinality: u16,
    pub observation_cardinality_next: u16,
}

#[near(serializers = [json])]
pub struct PoolView {
    pub pool_id: u64,
    pub token0: String,
    pub token1: String,
    pub initialized: bool,
    pub tick_spacing: i32,
    pub slot0: Slot0View,
    pub liquidity: U128,
    pub staked_liquidity: U128,
    pub fee_growth_global0_x128: String,
    pub fee_growth_global1_x128: String,
}

#[near(serializers = [json])]
pub struct PositionView {
    pub liquidity: U128,
    pub fee_growth_inside0_last_x128: String,
    pub fee_growth_inside1_last_x128: String,
    pub tokens_owed0: U128,
    pub tokens_owed1: U128,
}

/// One hop of a multi-hop route as passed by the caller. `token_in` is the token paid into
/// this hop. The output token is the other token of the pool.
#[derive(Clone)]
#[near(serializers = [json])]
pub struct HopInput {
    pub pool_id: u64,
    pub token_in: AccountId,
}

/// A validated route: engine hops plus the tokens at both ends.
struct Route {
    hops: Vec<Hop>,
    token_in: String,
    token_out: String,
}

/// The trader used by the views. It is the all-zero engine account, so views quote the fees
/// a non-whitelisted account pays. Whitelisted accounts pay 0 on a swap, so a swap by them can
/// return more than the matching view.
const QUOTE_TRADER: [u8; 32] = [0u8; 32];

/// The NEP-141 interface of a token contract, used for withdrawals.
#[allow(dead_code)]
#[ext_contract(ext_ft)]
trait FungibleTokenCore {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
}

/// The Chain Signatures MPC contract. `sign` yields until the MPC nodes answer.
#[allow(dead_code)]
#[ext_contract(ext_mpc)]
trait ChainSignatures {
    fn sign(&mut self, request: SignRequest) -> MpcSignature;
}

/// This contract's own callbacks.
#[allow(dead_code)]
#[ext_contract(ext_self)]
trait SelfCallbacks {
    fn on_withdraw(&mut self, account: AccountId, token: AccountId, amount: U128);
    fn on_evm_signed(&mut self, tx: EvmTx);
}

/// The `request` argument of the MPC `sign` call. Only secp256k1 (`domain_id` 0) is used here.
#[near(serializers = [json])]
pub struct SignRequest {
    pub path: String,
    pub payload_v2: Payload,
    pub domain_id: u64,
}

/// A 32-byte digest, hex without a prefix. `Ecdsa` is the only variant used here.
#[near(serializers = [json])]
pub enum Payload {
    Ecdsa(String),
    Eddsa(String),
}

/// The MPC response: `big_r` is a compressed point, `s` a scalar, both as hex.
#[near(serializers = [json])]
pub struct MpcSignature {
    pub big_r: AffinePoint,
    pub s: ScalarValue,
    pub recovery_id: u8,
    pub scheme: String,
}

#[near(serializers = [json])]
pub struct AffinePoint {
    pub affine_point: String,
}

#[near(serializers = [json])]
pub struct ScalarValue {
    pub scalar: String,
}

/// An EIP-1559 transfer as passed to `request_evm_payout`. It is also the argument of the
/// callback, so the callback can build the signed transaction from the same fields.
#[derive(Clone)]
#[near(serializers = [json])]
pub struct EvmTx {
    pub chain_id: u64,
    pub nonce: u64,
    /// The recipient, a 0x-prefixed 20-byte address.
    pub to: String,
    pub value_wei: U128,
    pub max_fee_per_gas: U128,
    pub max_priority_fee_per_gas: U128,
    pub gas_limit: u64,
}

impl EvmTx {
    fn transfer(&self) -> Result<evm::Eip1559Transfer, String> {
        let transfer = evm::Eip1559Transfer {
            chain_id: self.chain_id,
            nonce: self.nonce,
            max_priority_fee_per_gas: self.max_priority_fee_per_gas.0,
            max_fee_per_gas: self.max_fee_per_gas.0,
            gas_limit: self.gas_limit,
            to: evm::parse_address(&self.to)?,
            value: self.value_wei.0,
        };
        transfer.validate()?;
        Ok(transfer)
    }
}

/// Maps a NEAR account to the engine's 32-byte account id.
fn engine_account(account: &AccountId) -> [u8; 32] {
    env::sha256_array(account.as_bytes())
}

/// Current block time in seconds, as the engine expects it.
fn now() -> u32 {
    u32::try_from(env::block_timestamp() / 1_000_000_000)
        .unwrap_or_else(|_| env::panic_str("block timestamp does not fit in u32"))
}

/// Unwraps an engine result. On `Err` the call panics and NEAR reverts the state.
fn ok<T>(result: EngineResult<T>) -> T {
    result.unwrap_or_else(|e| env::panic_str(&format!("{e}")))
}

/// Checked `U256 -> u128`. Panics instead of truncating.
fn to_u128(x: U256) -> u128 {
    u128::try_from(x).unwrap_or_else(|_| env::panic_str("amount does not fit in u128"))
}

fn parse_u256(s: &str) -> U256 {
    s.parse::<U256>()
        .unwrap_or_else(|_| env::panic_str(&format!("invalid U256: {s}")))
}

fn parse_i256(s: &str) -> I256 {
    s.parse::<I256>()
        .unwrap_or_else(|_| env::panic_str(&format!("invalid I256: {s}")))
}

fn default_mpc_contract() -> AccountId {
    DEFAULT_MPC_CONTRACT
        .parse()
        .unwrap_or_else(|_| env::panic_str("invalid default MPC contract id"))
}

/// A token id must parse as a NEAR account id, since it is the token contract's account.
fn validate_token_id(token: &str) {
    if token.parse::<AccountId>().is_err() {
        env::panic_str(&format!("invalid token id: {token}"));
    }
}

impl Contract {
    fn assert_owner(&self) {
        if env::predecessor_account_id() != self.owner {
            env::panic_str("only owner");
        }
    }

    fn pool_ref(&self, pool_id: u64) -> &ClPool {
        self.pools
            .get(&pool_id)
            .unwrap_or_else(|| env::panic_str(&format!("pool {pool_id} does not exist")))
    }

    fn pool_mut(&mut self, pool_id: u64) -> &mut ClPool {
        self.pools
            .get_mut(&pool_id)
            .unwrap_or_else(|| env::panic_str(&format!("pool {pool_id} does not exist")))
    }

    fn tokens_of(&self, pool_id: u64) -> (String, String) {
        self.pool_tokens
            .get(&pool_id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(&format!("pool {pool_id} does not exist")))
    }

    fn balance_of(&self, account: &AccountId, token: &str) -> u128 {
        self.balances
            .get(&(account.clone(), token.to_string()))
            .copied()
            .unwrap_or(0)
    }

    fn credit(&mut self, account: &AccountId, token: &str, amount: u128) {
        if amount == 0 {
            return;
        }
        let key = (account.clone(), token.to_string());
        let current = self.balances.get(&key).copied().unwrap_or(0);
        let next = current
            .checked_add(amount)
            .unwrap_or_else(|| env::panic_str("balance overflow"));
        self.balances.insert(key, next);
    }

    fn debit(&mut self, account: &AccountId, token: &str, amount: u128) {
        if amount == 0 {
            return;
        }
        let key = (account.clone(), token.to_string());
        let current = self.balances.get(&key).copied().unwrap_or(0);
        let next = current.checked_sub(amount).unwrap_or_else(|| {
            env::panic_str(&format!(
                "insufficient {token} balance: have {current}, need {amount}"
            ))
        });
        if next == 0 {
            self.balances.remove(&key);
        } else {
            self.balances.insert(key, next);
        }
    }

    /// Applies a signed pool delta for `account`. Positive means the account owes the pool
    /// (debit), negative means the pool owes the account (credit).
    fn settle(&mut self, account: &AccountId, token: &str, delta: I256) {
        if delta.is_positive() {
            self.debit(account, token, to_u128(delta.into_raw()));
        } else if delta.is_negative() {
            self.credit(account, token, to_u128(delta.unsigned_abs()));
        }
    }

    /// Checks a path and turns it into engine hops. Panics on an empty path, a hop whose
    /// input token is not the previous hop's output, a token that is not in its pool, or an
    /// unknown pool.
    fn resolve_path(&self, path: &[HopInput]) -> Route {
        if path.is_empty() {
            env::panic_str("empty path");
        }
        let token_in = path[0].token_in.to_string();
        let mut current = token_in.clone();
        let mut hops = Vec::with_capacity(path.len());
        for (i, input) in path.iter().enumerate() {
            if input.token_in.as_str() != current.as_str() {
                env::panic_str(&format!("discontinuous path at hop {i}"));
            }
            let (token0, token1) = self.tokens_of(input.pool_id);
            let (zero_for_one, token_out) = if current == token0 {
                (true, token1)
            } else if current == token1 {
                (false, token0)
            } else {
                env::panic_str(&format!(
                    "token not in pool: {current} is not in pool {}",
                    input.pool_id
                ));
            };
            hops.push(Hop {
                pool_id: input.pool_id,
                zero_for_one,
            });
            current = token_out;
        }
        Route {
            hops,
            token_in,
            token_out: current,
        }
    }

    /// Working copies of the pools a route touches. The route runs on these copies, and only a
    /// successful swap writes them back.
    fn working_pools(&self, hops: &[Hop]) -> BTreeMap<u64, ClPool> {
        hops.iter()
            .map(|hop| (hop.pool_id, self.pool_ref(hop.pool_id).clone()))
            .collect()
    }

    /// Engine exact-input route on `pools`. Returns the engine error without panicking, so a
    /// caller can show that a failed route leaves the stored pools unchanged.
    fn route_exact_input(
        &self,
        route: &Route,
        pools: &mut BTreeMap<u64, ClPool>,
        trader: [u8; 32],
        amount_in: u128,
        min_out: u128,
    ) -> EngineResult<U256> {
        Router {
            store: pools,
            fees: &self.fee_config,
            trader,
            time: now(),
        }
        .exact_input(&route.hops, U256::from(amount_in), U256::from(min_out))
    }

    /// Engine exact-output route on `pools`. See `route_exact_input`.
    fn route_exact_output(
        &self,
        route: &Route,
        pools: &mut BTreeMap<u64, ClPool>,
        trader: [u8; 32],
        amount_out: u128,
        max_in: u128,
    ) -> EngineResult<U256> {
        Router {
            store: pools,
            fees: &self.fee_config,
            trader,
            time: now(),
        }
        .exact_output(&route.hops, U256::from(amount_out), U256::from(max_in))
    }

    /// Writes the working copies of a successful route back to the pool storage.
    fn write_back(&mut self, pools: BTreeMap<u64, ClPool>) {
        for (id, pool) in pools {
            self.pools.insert(id, pool);
        }
    }
}

#[near]
impl Contract {
    #[init]
    pub fn new(owner: AccountId) -> Self {
        Self {
            owner,
            fee_config: FeeConfig::default(),
            pools: IterableMap::new(StorageKey::Pools),
            pool_tokens: LookupMap::new(StorageKey::PoolTokens),
            next_pool_id: 0,
            balances: LookupMap::new(StorageKey::Balances),
            allowed_tokens: IterableSet::new(StorageKey::AllowedTokens),
            mpc_contract: default_mpc_contract(),
        }
    }

    /// Upgrade from the deployed testnet state (the layout of `ContractV1`) to the current
    /// one. Run once, as the init call of the redeploy:
    /// `near contract deploy <acct> use-file <wasm> with-init-call migrate json-args '{}'`.
    /// Existing state is kept, and `mpc_contract` starts at the testnet default. Only the
    /// contract account can call it (`#[private]`).
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        let old: ContractV1 =
            env::state_read().unwrap_or_else(|| env::panic_str("no contract state to migrate"));
        Self {
            owner: old.owner,
            fee_config: old.fee_config,
            pools: old.pools,
            pool_tokens: old.pool_tokens,
            next_pool_id: old.next_pool_id,
            balances: old.balances,
            allowed_tokens: old.allowed_tokens,
            mpc_contract: default_mpc_contract(),
        }
    }

    /// Owner only. Sets the Chain Signatures MPC contract used by `request_evm_payout`.
    pub fn set_mpc_contract(&mut self, mpc_contract: AccountId) {
        self.assert_owner();
        self.mpc_contract = mpc_contract;
    }

    /// The Chain Signatures MPC contract used by `request_evm_payout`.
    pub fn get_mpc_contract(&self) -> AccountId {
        self.mpc_contract.clone()
    }

    /// Owner only. Asks the MPC network to sign an EIP-1559 transfer from this contract's
    /// derived EVM address (path `path`, secp256k1, domain 0), and returns the signed raw
    /// transaction `0x02 || rlp(...)` as a 0x-hex string.
    ///
    /// The caller must attach exactly 1 yoctoNEAR, as for `withdraw`. The `sign` call attaches
    /// 1 yoctoNEAR from the contract's own balance, which the MPC contract requires. The caller
    /// must attach at least 100 Tgas, of which 50 go to `sign` and 20 to the callback.
    ///
    /// The signing key is derived from `(path, predecessor = this contract)`. The EVM address
    /// is `keccak256(pubkey)[12..]` of `derived_public_key` on the MPC contract. Nothing in this
    /// contract checks that the address holds funds or that `nonce` is the next one.
    // The eight arguments are the request fields of the spec, so they stay separate.
    #[allow(clippy::too_many_arguments)]
    #[payable]
    pub fn request_evm_payout(
        &mut self,
        chain_id: u64,
        nonce: u64,
        to: String,
        value_wei: U128,
        max_fee_per_gas: U128,
        max_priority_fee_per_gas: U128,
        gas_limit: u64,
        path: String,
    ) -> Promise {
        self.assert_owner();
        assert_one_yocto();
        if env::prepaid_gas() < Gas::from_tgas(PAYOUT_MIN_PREPAID_GAS_TGAS) {
            env::panic_str(&format!(
                "attach at least {PAYOUT_MIN_PREPAID_GAS_TGAS} Tgas of prepaid gas"
            ));
        }
        if path.is_empty() {
            env::panic_str("path must not be empty");
        }
        let tx = EvmTx {
            chain_id,
            nonce,
            to,
            value_wei,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            gas_limit,
        };
        let transfer = tx.transfer().unwrap_or_else(|e| env::panic_str(&e));
        let digest = transfer.signing_hash();

        let request = SignRequest {
            path,
            payload_v2: Payload::Ecdsa(evm::hex_plain(&digest)),
            domain_id: 0,
        };
        ext_mpc::ext(self.mpc_contract.clone())
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(Gas::from_tgas(SIGN_CALL_GAS_TGAS))
            .sign(request)
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(CALLBACK_GAS_TGAS))
                    .on_evm_signed(tx),
            )
    }

    /// Callback for `request_evm_payout`. Builds the signed raw transaction from the MPC
    /// response and returns it as 0x-hex. Panics if the MPC call failed or the response is
    /// not a valid secp256k1 signature.
    #[private]
    pub fn on_evm_signed(
        &self,
        tx: EvmTx,
        #[callback_result] sig: Result<MpcSignature, PromiseError>,
    ) -> String {
        let sig = sig.unwrap_or_else(|e| env::panic_str(&format!("MPC sign failed: {e:?}")));
        if sig.scheme != "Secp256k1" {
            env::panic_str(&format!("unexpected MPC scheme: {}", sig.scheme));
        }
        let transfer = tx.transfer().unwrap_or_else(|e| env::panic_str(&e));
        let big_r = evm::hex_decode(&sig.big_r.affine_point)
            .unwrap_or_else(|e| env::panic_str(&format!("big_r: {e}")));
        let r = evm::r_from_affine_point(&big_r).unwrap_or_else(|e| env::panic_str(&e));
        let s = evm::hex_decode(&sig.s.scalar)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .unwrap_or_else(|| env::panic_str("s must be 32 bytes of hex"));
        let raw = evm::assemble_signed(&transfer, &r, &s, sig.recovery_id)
            .unwrap_or_else(|e| env::panic_str(&e));
        evm::hex_0x(&raw)
    }

    /// Owner only. Creates and initializes a pool for `(token0, token1)`. The tick spacing
    /// must be enabled in the fee config. Token ids must parse as account ids.
    pub fn create_pool(
        &mut self,
        token0: String,
        token1: String,
        tick_spacing: i32,
        sqrt_price_x96: String,
    ) -> u64 {
        self.assert_owner();
        if token0 == token1 {
            env::panic_str("token0 and token1 must differ");
        }
        validate_token_id(&token0);
        validate_token_id(&token1);
        if !self
            .fee_config
            .tick_spacing_to_fee
            .contains_key(&tick_spacing)
        {
            env::panic_str(&format!("tick spacing {tick_spacing} is not enabled"));
        }
        let sqrt_price = parse_u256(&sqrt_price_x96);

        let mut pool = ClPool::default();
        ok(pool.initialize(tick_spacing, sqrt_price, now()));

        let pool_id = self.next_pool_id;
        self.next_pool_id = pool_id
            .checked_add(1)
            .unwrap_or_else(|| env::panic_str("pool id overflow"));
        self.pools.insert(pool_id, pool);
        self.pool_tokens.insert(pool_id, (token0, token1));
        pool_id
    }

    /// Owner only. Adds `token` (a NEP-141 contract account) to the deposit whitelist.
    pub fn add_token(&mut self, token: AccountId) {
        self.assert_owner();
        if !self.allowed_tokens.insert(token) {
            env::panic_str("token already allowed");
        }
    }

    /// Owner only. Removes `token` from the deposit whitelist. Existing balances of the token
    /// can still be withdrawn. New deposits of it are refunded.
    pub fn remove_token(&mut self, token: AccountId) {
        self.assert_owner();
        if !self.allowed_tokens.remove(&token) {
            env::panic_str("token not allowed");
        }
    }

    /// Tokens currently accepted for deposit.
    pub fn get_tokens(&self) -> Vec<AccountId> {
        self.allowed_tokens.iter().cloned().collect()
    }

    /// NEP-141 receiver hook, called by a token contract after `ft_transfer_call` to this
    /// account. The predecessor is the token contract and `sender_id` is the account that
    /// sent the tokens.
    ///
    /// For a whitelisted token, credits `sender_id` with `amount` and returns 0 (nothing
    /// unused). For any other token, returns `amount` so the token contract refunds it.
    ///
    /// `msg` is ignored for now. An empty message is a plain deposit. It will carry swap
    /// instructions in a later version.
    #[allow(unused_variables)]
    pub fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        let token = env::predecessor_account_id();
        if !self.allowed_tokens.contains(&token) {
            return PromiseOrValue::Value(amount);
        }
        self.credit(&sender_id, token.as_str(), amount.0);
        PromiseOrValue::Value(U128(0))
    }

    /// Withdraws `amount` of `token` from the caller's internal balance to the caller's
    /// account on the token contract. Requires exactly 1 yoctoNEAR attached.
    ///
    /// The balance is debited before the transfer. `on_withdraw` credits it back if the
    /// transfer fails. Withdrawals do not check the whitelist, so a delisted token's balance
    /// can still be withdrawn.
    #[payable]
    pub fn withdraw(&mut self, token: AccountId, amount: U128) -> Promise {
        assert_one_yocto();
        if amount.0 == 0 {
            env::panic_str("amount must be positive");
        }
        let account = env::predecessor_account_id();
        self.debit(&account, token.as_str(), amount.0);
        ext_ft::ext(token.clone())
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(Gas::from_tgas(5))
            .ft_transfer(account.clone(), amount, None)
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(10))
                    .on_withdraw(account, token, amount),
            )
    }

    /// Callback for `withdraw`. Re-credits the balance if the `ft_transfer` failed.
    #[private]
    pub fn on_withdraw(&mut self, account: AccountId, token: AccountId, amount: U128) {
        // `ft_transfer` returns nothing, so the only error here is a failed promise.
        if env::promise_result_checked(0, usize::MAX).is_err() {
            self.credit(&account, token.as_str(), amount.0);
        }
    }

    /// Testnet-only faucet: credits an internal balance. Owner only. Compiled only with the
    /// `testnet-faucet` feature (on by default). Mainnet builds must use `--no-default-features`.
    #[cfg(feature = "testnet-faucet")]
    pub fn test_credit(&mut self, account_id: AccountId, token: String, amount: U128) {
        self.assert_owner();
        self.credit(&account_id, &token, amount.0);
    }

    /// Adds liquidity for the caller. Debits the caller's internal balances for the token
    /// amounts the position requires. Returns `[amount0, amount1]`.
    pub fn mint_position(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity: U128,
    ) -> [String; 2] {
        let caller = env::predecessor_account_id();
        let (token0, token1) = self.tokens_of(pool_id);
        let (amount0, amount1) = {
            let pool = self.pool_mut(pool_id);
            ok(pool.mint(
                engine_account(&caller),
                tick_lower,
                tick_upper,
                liquidity.0,
                now(),
            ))
        };
        self.debit(&caller, &token0, to_u128(amount0));
        self.debit(&caller, &token1, to_u128(amount1));
        [amount0.to_string(), amount1.to_string()]
    }

    /// Removes liquidity from the caller's position. The withdrawn amounts become owed to the
    /// position and are paid out by `collect`. Returns `[amount0, amount1]`.
    pub fn burn_position(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity: U128,
    ) -> [String; 2] {
        let caller = env::predecessor_account_id();
        self.tokens_of(pool_id);
        let (amount0, amount1) = {
            let pool = self.pool_mut(pool_id);
            ok(pool.burn(
                engine_account(&caller),
                tick_lower,
                tick_upper,
                liquidity.0,
                now(),
            ))
        };
        [amount0.to_string(), amount1.to_string()]
    }

    /// Collects everything owed to the caller's position and credits the caller's internal
    /// balances. Returns `[amount0, amount1]`.
    pub fn collect(&mut self, pool_id: u64, tick_lower: i32, tick_upper: i32) -> [U128; 2] {
        let caller = env::predecessor_account_id();
        let (token0, token1) = self.tokens_of(pool_id);
        let (amount0, amount1) = {
            let pool = self.pool_mut(pool_id);
            ok(pool.collect(
                engine_account(&caller),
                tick_lower,
                tick_upper,
                u128::MAX,
                u128::MAX,
            ))
        };
        self.credit(&caller, &token0, amount0);
        self.credit(&caller, &token1, amount1);
        [U128(amount0), U128(amount1)]
    }

    /// Swaps for the caller. `amount_specified > 0` is exact input, `< 0` is exact output.
    /// Debits the input token and credits the output token. Returns the signed pool deltas
    /// `[amount0, amount1]` (positive = paid by the caller, negative = paid to the caller).
    pub fn swap(
        &mut self,
        pool_id: u64,
        zero_for_one: bool,
        amount_specified: String,
        sqrt_price_limit_x96: String,
    ) -> [String; 2] {
        let caller = env::predecessor_account_id();
        let account = engine_account(&caller);
        let amount = parse_i256(&amount_specified);
        let limit = parse_u256(&sqrt_price_limit_x96);
        let (token0, token1) = self.tokens_of(pool_id);

        let tick_spacing = self.pool_ref(pool_id).tick_spacing;
        let fee = self.fee_config.swap_fee(pool_id, tick_spacing, &account);
        // No gauges in v0, so the unstaked fee is computed as if the gauge is not alive.
        let unstaked_fee = self.fee_config.unstaked_fee(pool_id, false);

        let result = {
            let pool = self.pool_mut(pool_id);
            ok(pool.swap(zero_for_one, amount, limit, fee, unstaked_fee, now()))
        };
        self.settle(&caller, &token0, result.amount0);
        self.settle(&caller, &token1, result.amount1);
        [result.amount0.to_string(), result.amount1.to_string()]
    }

    /// Pool state (no secrets, all numbers as strings where they exceed JS integer range).
    pub fn get_pool(&self, pool_id: u64) -> PoolView {
        let (token0, token1) = self.tokens_of(pool_id);
        let pool = self.pool_ref(pool_id);
        PoolView {
            pool_id,
            token0,
            token1,
            initialized: pool.initialized,
            tick_spacing: pool.tick_spacing,
            slot0: Slot0View {
                sqrt_price_x96: pool.slot0.sqrt_price_x96.to_string(),
                tick: pool.slot0.tick,
                observation_index: pool.slot0.observation_index,
                observation_cardinality: pool.slot0.observation_cardinality,
                observation_cardinality_next: pool.slot0.observation_cardinality_next,
            },
            liquidity: U128(pool.liquidity),
            staked_liquidity: U128(pool.staked_liquidity),
            fee_growth_global0_x128: pool.fee_growth_global0_x128.to_string(),
            fee_growth_global1_x128: pool.fee_growth_global1_x128.to_string(),
        }
    }

    /// Position of `account_id` in `[tick_lower, tick_upper)`.
    pub fn get_position(
        &self,
        pool_id: u64,
        account_id: AccountId,
        tick_lower: i32,
        tick_upper: i32,
    ) -> PositionView {
        let position =
            self.pool_ref(pool_id)
                .position(&engine_account(&account_id), tick_lower, tick_upper);
        PositionView {
            liquidity: U128(position.liquidity),
            fee_growth_inside0_last_x128: position.fee_growth_inside0_last_x128.to_string(),
            fee_growth_inside1_last_x128: position.fee_growth_inside1_last_x128.to_string(),
            tokens_owed0: U128(position.tokens_owed0),
            tokens_owed1: U128(position.tokens_owed1),
        }
    }

    /// Multi-hop exact-input swap for the caller. Spends exactly `amount_in` of the first hop's
    /// input token and credits the caller with the output of the last hop. Panics if that
    /// output is below `min_amount_out`, or if the caller's balance is too small. Returns the
    /// output amount.
    pub fn swap_exact_input(
        &mut self,
        path: Vec<HopInput>,
        amount_in: U128,
        min_amount_out: U128,
    ) -> U128 {
        let caller = env::predecessor_account_id();
        let route = self.resolve_path(&path);
        let mut pools = self.working_pools(&route.hops);
        let out = ok(self.route_exact_input(
            &route,
            &mut pools,
            engine_account(&caller),
            amount_in.0,
            min_amount_out.0,
        ));
        let out = to_u128(out);
        self.debit(&caller, &route.token_in, amount_in.0);
        self.credit(&caller, &route.token_out, out);
        self.write_back(pools);
        U128(out)
    }

    /// Multi-hop exact-output swap for the caller. Credits exactly `amount_out` of the last
    /// hop's output token and debits the caller for the input the route needs. Panics if that
    /// input is above `max_amount_in`, or if the caller's balance is too small. Returns the
    /// input amount actually debited.
    pub fn swap_exact_output(
        &mut self,
        path: Vec<HopInput>,
        amount_out: U128,
        max_amount_in: U128,
    ) -> U128 {
        let caller = env::predecessor_account_id();
        let route = self.resolve_path(&path);
        let mut pools = self.working_pools(&route.hops);
        let paid = ok(self.route_exact_output(
            &route,
            &mut pools,
            engine_account(&caller),
            amount_out.0,
            max_amount_in.0,
        ));
        let paid = to_u128(paid);
        self.debit(&caller, &route.token_in, paid);
        self.credit(&caller, &route.token_out, amount_out.0);
        self.write_back(pools);
        U128(paid)
    }

    /// Output of `swap_exact_input` for `path` and `amount_in`, at the current block time.
    /// Uses the fees of `QUOTE_TRADER` (see that constant). Changes no state.
    pub fn quote_exact_input(&self, path: Vec<HopInput>, amount_in: U128) -> U128 {
        let route = self.resolve_path(&path);
        let mut pools = self.working_pools(&route.hops);
        let out = ok(self.route_exact_input(&route, &mut pools, QUOTE_TRADER, amount_in.0, 0));
        U128(to_u128(out))
    }

    /// Input that `swap_exact_output` would debit for `path` and `amount_out`, at the current
    /// block time. Uses the fees of `QUOTE_TRADER`. Changes no state.
    pub fn quote_exact_output(&self, path: Vec<HopInput>, amount_out: U128) -> U128 {
        let route = self.resolve_path(&path);
        let mut pools = self.working_pools(&route.hops);
        let paid =
            ok(self.route_exact_output(&route, &mut pools, QUOTE_TRADER, amount_out.0, u128::MAX));
        U128(to_u128(paid))
    }

    /// Owner only. Enables a tick spacing with its swap fee, in hundredths of a bip. Pools
    /// can then be created with that spacing.
    pub fn enable_tick_spacing(&mut self, tick_spacing: i32, fee: u32) {
        self.assert_owner();
        ok(self.fee_config.enable_tick_spacing(tick_spacing, fee));
    }

    /// Owner only. Sets the swap fee of one pool, in hundredths of a bip. `0` clears it. The
    /// value `420` means a fee of 0%.
    pub fn set_custom_swap_fee(&mut self, pool_id: u64, fee: u32) {
        self.assert_owner();
        self.pool_ref(pool_id);
        ok(self.fee_config.set_custom_swap_fee(pool_id, fee));
    }

    /// Owner only. Sets the unstaked fee of one pool, in hundredths of a bip. `0` clears it.
    /// The value `420` means a fee of 0%. No gauges exist yet, so this does not affect swaps.
    pub fn set_custom_unstaked_fee(&mut self, pool_id: u64, fee: u32) {
        self.assert_owner();
        self.pool_ref(pool_id);
        ok(self.fee_config.set_custom_unstaked_fee(pool_id, fee));
    }

    /// Owner only. Adds or removes `account_id` from the swap fee whitelist. A whitelisted
    /// account pays a swap fee of 0 on every pool.
    pub fn set_fee_whitelist(&mut self, account_id: AccountId, whitelisted: bool) {
        self.assert_owner();
        self.fee_config
            .set_whitelist(engine_account(&account_id), whitelisted);
    }

    /// Swap fee that `account_id` pays on `pool_id`, in hundredths of a bip.
    pub fn get_fee(&self, pool_id: u64, account_id: AccountId) -> u32 {
        let tick_spacing = self.pool_ref(pool_id).tick_spacing;
        self.fee_config
            .swap_fee(pool_id, tick_spacing, &engine_account(&account_id))
    }

    /// Internal balance of `account_id` for `token`.
    pub fn get_balance(&self, account_id: AccountId, token: String) -> U128 {
        U128(self.balance_of(&account_id, &token))
    }
}

pub mod accounts;
#[cfg(test)]
mod accounts_tests;
#[cfg(test)]
mod tests;
