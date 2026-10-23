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
//! - Fees are the static `FeeConfig::default()`. There is no admin method to change them yet.
//! - No gauges: `unstaked_fee` is always computed with `gauge_alive = false`.
//! - No Chain Signatures: the engine's `[u8; 32]` account id is `sha256(account_id)`.

use near_sdk::json_types::U128;
use near_sdk::store::{IterableMap, IterableSet, LookupMap};
use near_sdk::{
    assert_one_yocto, env, ext_contract, near, AccountId, BorshStorageKey, Gas, NearToken,
    PanicOnDefault, Promise, PromiseOrValue,
};
use windswap_engine::error::Result as EngineResult;
use windswap_engine::fees::FeeConfig;
use windswap_engine::pool::cl_pool::ClPool;
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
}

/// State layout of the deployed testnet contract, before `allowed_tokens` was added.
/// Only used by `migrate`. Field order and types must match the old deployment exactly.
#[near(serializers = [borsh])]
struct ContractV0 {
    owner: AccountId,
    fee_config: FeeConfig,
    pools: IterableMap<u64, ClPool>,
    pool_tokens: LookupMap<u64, (String, String)>,
    next_pool_id: u64,
    balances: LookupMap<(AccountId, String), u128>,
}

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

/// The NEP-141 interface of a token contract, used for withdrawals.
#[allow(dead_code)]
#[ext_contract(ext_ft)]
trait FungibleTokenCore {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
}

/// This contract's own callbacks.
#[allow(dead_code)]
#[ext_contract(ext_self)]
trait SelfCallbacks {
    fn on_withdraw(&mut self, account: AccountId, token: AccountId, amount: U128);
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
        }
    }

    /// Upgrade path from the v0 testnet state (no `allowed_tokens`). Run once right after
    /// deploying this code over the old contract. The new `allowed_tokens` set starts empty,
    /// so the owner must `add_token` again before deposits work. Only the contract account
    /// can call it (`#[private]`).
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        let old: ContractV0 =
            env::state_read().unwrap_or_else(|| env::panic_str("no contract state to migrate"));
        Self {
            owner: old.owner,
            fee_config: old.fee_config,
            pools: old.pools,
            pool_tokens: old.pool_tokens,
            next_pool_id: old.next_pool_id,
            balances: old.balances,
            allowed_tokens: IterableSet::new(StorageKey::AllowedTokens),
        }
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

    /// Internal balance of `account_id` for `token`.
    pub fn get_balance(&self, account_id: AccountId, token: String) -> U128 {
        U128(self.balance_of(&account_id, &token))
    }
}

#[cfg(test)]
mod tests;
