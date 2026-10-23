//! Permissionless listing (v8). Anyone can use the contract without the owner registering anything:
//!
//! - **EVM tokens.** A deposit of an ERC-20 that is not registered on an enabled chain registers it
//!   on the spot (`report_evm_deposit` with `decimals` and an optional `symbol`). Its token id is
//!   `evm:{chain_id}:{address}`, derived from the address, so a look-alike token can never take the id
//!   of the real one. Auto-registered assets get the owner's default caps (`set_default_caps`) and
//!   `verified: false`. The owner marks major tokens verified with `set_evm_asset_verified`.
//! - **Pools.** `create_pool` is callable by anyone when both tokens are known (a whitelisted NEP-141
//!   or a registered EVM asset), the tick spacing is enabled, and no pool exists yet for the pair and
//!   tick spacing. A non-owner attaches at least `POOL_STORAGE_DEPOSIT`, which stays in the contract,
//!   and the excess is refunded. The owner needs no deposit.
//! - **NEP-141 tokens.** `register_token` whitelists a token after its `ft_metadata` call succeeds,
//!   with a storage deposit of `REGISTER_TOKEN_DEPOSIT`. Failure refunds the deposit.
//!
//! Trust note: a whitelisted token contract (or an EVM token that emits Transfer logs) can credit
//! itself by calling the deposit hooks. Permissionless listing is therefore only as safe as the
//! token contract. See docs/testnet.md, "Permissionless listing".
//!
//! The state is the v8 layout (`main`: one-step flows and intents) plus one field, `permissionless`.
//! `migrate_from_v8` reads the v8 layout
//! and backfills the pool index from the existing pools.

use super::*;
use near_sdk::json_types::U128;
use near_sdk::store::LookupMap;
use near_sdk::{Promise, PromiseError};

/// Deposit a non-owner attaches to `create_pool`. It stays in the contract to pay for the pool's
/// storage. Anything above it is refunded.
pub const POOL_STORAGE_DEPOSIT: u128 = 100_000_000_000_000_000_000_000; // 0.1 NEAR
/// Deposit a caller attaches to `register_token`. Kept on success, refunded on failure.
pub const REGISTER_TOKEN_DEPOSIT: u128 = 10_000_000_000_000_000_000_000; // 0.01 NEAR
/// Largest `decimals` a token may report.
pub const MAX_DECIMALS: u8 = 36;
/// Longest symbol kept. The symbol is informational only.
const MAX_SYMBOL_LEN: usize = 16;
/// The most pools one `get_pools` call returns.
const MAX_POOL_PAGE: u64 = 100;

/// Key of the pool index: the sorted token pair and the tick spacing.
pub type PoolKey = (String, String, i32);

/// The light record of a pool that `get_pools` and `get_pool_summary` read. It holds no ticks or
/// positions, so reading it costs the same however large the pool grows. It is refreshed on every
/// write to the pool (see `sync_pool`). The swap counters add up the swaps from v8 on.
#[derive(Clone)]
#[near(serializers = [borsh, json])]
pub struct PoolSummary {
    pub pool_id: u64,
    pub token0: String,
    pub token1: String,
    pub tick_spacing: i32,
    /// The swap fee of a trader that is not whitelisted, in hundredths of a bip.
    pub fee: u32,
    pub sqrt_price_x96: String,
    pub tick: i32,
    pub liquidity: U128,
    pub staked_liquidity: U128,
    pub fee_growth_global0_x128: String,
    pub fee_growth_global1_x128: String,
    /// True once the pool has a gauge.
    pub has_gauge: bool,
    /// Unix seconds of the pool's creation. For a pool that existed before v8, the migration time.
    pub created_at: u64,
    pub swap_count: u64,
    /// Gross amount of token0 moved by swaps, in and out.
    pub volume0: U128,
    /// Gross amount of token1 moved by swaps, in and out.
    pub volume1: U128,
}

/// Caps given to an auto-registered EVM asset.
#[derive(Clone, Copy)]
#[near(serializers = [borsh])]
pub struct DefaultCaps {
    pub per_deposit_cap: u128,
    pub daily_withdraw_cap: u128,
}

/// Listing metadata of an EVM asset. An asset with no entry counts as verified: it was registered
/// by the owner, which is the vouching.
#[derive(Clone)]
#[near(serializers = [borsh])]
pub struct EvmMeta {
    pub verified: bool,
    pub symbol: String,
}

/// State added in v8. Each map has its own string prefix, so it cannot collide with `StorageKey`.
#[near(serializers = [borsh])]
pub struct Permissionless {
    /// Pool id per `(sorted pair, tick spacing)`. One pool per key.
    pub(crate) pool_by_key: LookupMap<PoolKey, u64>,
    /// One summary per pool, read by the pool list views. See `PoolSummary`.
    pool_summaries: LookupMap<u64, PoolSummary>,
    /// Listing metadata per EVM asset key (`"{chain_id}:{asset}"`).
    evm_meta: LookupMap<String, EvmMeta>,
    /// Caps for auto-registered EVM assets. `None` until the owner sets them, and then no
    /// unknown ERC-20 can be registered.
    default_caps: Option<DefaultCaps>,
}

impl Permissionless {
    pub fn new() -> Self {
        Self {
            pool_by_key: LookupMap::new(b"perm_pool_by_key".to_vec()),
            pool_summaries: LookupMap::new(b"perm_pool_summaries".to_vec()),
            evm_meta: LookupMap::new(b"perm_evm_meta".to_vec()),
            default_caps: None,
        }
    }
}

/// Metadata of a NEP-141 token. Only `decimals` is read; other fields are ignored.
#[derive(Clone)]
#[near(serializers = [json])]
pub struct FtMetadata {
    pub decimals: u8,
}

/// The `ft_metadata` view of a NEP-141 token.
#[allow(dead_code)]
#[ext_contract(ext_fungible_meta)]
trait FungibleMetadataView {
    fn ft_metadata(&self) -> FtMetadata;
}

/// This contract's `register_token` callback.
#[allow(dead_code)]
#[ext_contract(ext_register)]
trait RegisterCallbacks {
    fn on_register_token(&mut self, token: AccountId, payer: AccountId, attached: U128);
}

/// JSON view of the default caps.
#[near(serializers = [json])]
pub struct DefaultCapsView {
    pub per_deposit_cap: U128,
    pub daily_withdraw_cap: U128,
}

/// The token id of an auto-registered EVM asset. It is derived from the address, so two tokens
/// cannot share it.
pub fn auto_token_id(chain_id: u64, asset: &str) -> String {
    format!("evm:{chain_id}:{asset}")
}

/// Keeps ASCII letters, digits, `-`, `_` and `.`, and at most `MAX_SYMBOL_LEN` of them.
pub fn sanitize_symbol(raw: Option<String>) -> String {
    raw.unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(MAX_SYMBOL_LEN)
        .collect()
}

/// The trader a summary's fee is quoted for: an account that is not whitelisted.
const PUBLIC_TRADER: [u8; 32] = [0u8; 32];

/// The sorted key of a token pair.
pub fn pool_key(token0: &str, token1: &str, tick_spacing: i32) -> PoolKey {
    if token0 <= token1 {
        (token0.to_string(), token1.to_string(), tick_spacing)
    } else {
        (token1.to_string(), token0.to_string(), tick_spacing)
    }
}

/// The state layout of the v8 contract: `Contract` before the `permissionless` field, so the layout
/// on `main` (one-step flows and intents). Only `migrate_from_v8` reads it. Field order and types
/// must match the deployed state exactly.
#[near(serializers = [borsh])]
pub(crate) struct ContractV8 {
    owner: AccountId,
    fee_config: FeeConfig,
    /// One Borsh blob per pool (v0). See the module docs.
    pools: IterableMap<u64, ClPool>,
    /// Token pair `(token0, token1)` for each pool id.
    pool_tokens: LookupMap<u64, (String, String)>,
    next_pool_id: u64,
    /// Balances of the deployed layout (v3), keyed by `(NEAR account, token id)`. Not used for
    /// new credits. `migrate_balances` moves each listed entry to `balances`.
    legacy_balances: LookupMap<(AccountId, String), u128>,
    /// Internal balances keyed by `(owner, token id)`. Missing key means zero.
    balances: LookupMap<(Owner, String), u128>,
    /// Tokens accepted by `ft_on_transfer`. Managed by the owner.
    allowed_tokens: IterableSet<AccountId>,
    /// The Chain Signatures MPC contract that `request_evm_payout` calls. Owner-settable.
    mpc_contract: AccountId,
    /// Wallet <-> owner links, and consumed link nonces.
    links: LinkRegistry,
    /// Consumed `(wallet, nonce)` pairs of signed actions. Never pruned.
    action_nonces: LookupSet<(WalletId, u64)>,
    /// EVM withdrawals, keyed by id. Ids are sequential and never removed.
    withdrawals: LookupMap<u64, Withdrawal>,
    next_withdrawal_id: u64,
    /// Accounts that may call `report_evm_deposit`, `process_withdrawal` and
    /// `mark_withdrawal_broadcast`.
    relayers: IterableSet<AccountId>,
    /// Registered EVM assets, keyed by `"{chain_id}:{asset}"`.
    evm_assets: IterableMap<String, EvmAsset>,
    /// Internal token id -> key of its EVM asset in `evm_assets`.
    evm_asset_by_token: LookupMap<String, String>,
    /// EVM chain settings (signing path, next nonce, fee cap), keyed by chain id.
    evm_chains: LookupMap<u64, EvmChain>,
    /// `(chain_id, tx_hash, log_index)` of every deposit credited before `evm_deposits_by_asset`
    /// existed. Read only: a report is a duplicate if its legacy key or its asset key is here.
    evm_deposits: LookupSet<(u64, [u8; 32], u32)>,
    /// `(chain_id, tx_hash, log_index, asset)` of every deposit credited since. The asset is in the
    /// key because a native transfer and an ERC-20 `Transfer` can both be log 0 of one tx.
    evm_deposits_by_asset: LookupSet<(u64, [u8; 32], u32, String)>,
    /// Owner ids that have ever been credited or had a position minted. A wallet whose own owner
    /// id is in this set cannot be linked. Set in `credit` and `mint_for`, and by
    /// `flag_wallet_state` for data from before v5.
    has_state: LookupSet<Owner>,
    // Appended in v6. The ve(3,3) state is one Borsh blob per engine object (see `tokenomics`).
    /// The WIND NEP-141 token, set once by the owner. Minted only by this contract.
    wind_token: Option<AccountId>,
    ve: LazyOption<VotingEscrow>,
    voter: LazyOption<Voter>,
    minter: LazyOption<Minter>,
    distributor: LazyOption<RewardsDistributor>,
    /// One gauge per pool that has one, keyed by pool id.
    gauges: LookupMap<u64, ClGauge>,
    /// Staked positions: `(owner, pool, tick_lower, tick_upper)` to the NFT id.
    stakes: LookupMap<StakeKey, U256>,
    /// The NFT manager that holds every staked position (the pools' `nft` account).
    nft_manager: LazyOption<PositionManager>,
    /// Set once the genesis WIND supply is minted, and while that mint is in flight.
    wind_genesis_done: bool,
    wind_genesis_pending: bool,
    /// Total WIND minted by `distribute`. The WIND supply the minter sees.
    wind_minted: u128,
    /// The epoch whose mint is in flight, if any. See `tokenomics`.
    pending_epoch: Option<PendingEpoch>,
    // Appended in v7 (solvency accounting). See `accounting` for what each total covers.
    /// Per token: sum of the internal balances that are counted (see `accounting`).
    liabilities: LookupMap<String, u128>,
    /// `(owner, token)` keys whose balance is already in `liabilities`. Used only if `needs_seed`.
    accounted: LookupSet<(Owner, String)>,
    /// True on a migrated contract: balances are counted on first touch, and the owner seeds
    /// the rest. False on a fresh deployment, which counts every change from the start.
    needs_seed: bool,
    /// Per `(pool, token)`: tokens held for the pool, its gauge and its NFT manager. Signed, so
    /// a missing seed shows up as a negative reserve instead of a panic in a swap.
    pool_reserves: LookupMap<(u64, String), i128>,
    /// Per token: the sum of `pool_reserves` over the pools.
    reserve_totals: LookupMap<String, i128>,
    /// `(pool, token)` pairs whose reserve was set by `seed_pool_reserves`.
    reserves_seeded: LookupSet<(u64, String)>,
    /// Per token: EVM withdrawals that are debited but not yet broadcast.
    pending_withdrawals: LookupMap<String, u128>,
    /// Per token: faucet credit (`test_credit`) that no deposit backs.
    unbacked: LookupMap<String, u128>,
    // Appended after v6. Owner indexes for the owner-addressed views (see `views`). They start
    // empty at `migrate` and are filled by the `backfill_*` calls.
    /// Position ranges per owner that have liquidity, owed fees or a stake.
    positions_by_owner: LookupMap<Owner, Vec<RangeKey>>,
    /// Staked position ranges per owner.
    stakes_by_owner: LookupMap<Owner, Vec<RangeKey>>,
    /// Withdrawal ids per owner, in creation order.
    withdrawals_by_owner: LookupMap<Owner, Vec<u64>>,
    /// Tokens with a nonzero internal balance, per owner.
    balance_tokens_by_owner: LookupMap<Owner, Vec<String>>,
    // Appended in v8 (one-step flows and intents, see `intents`).
    /// Intents by id.
    intents: LookupMap<u64, intents::StoredIntent>,
    next_intent_id: u64,
    /// Every intent id per owner, in submission order.
    intents_by_owner: LookupMap<Owner, Vec<u64>>,
    /// Open intent ids per owner, oldest first. At most `intents::MAX_OPEN_INTENTS`.
    open_intents: LookupMap<Owner, Vec<u64>>,
}

/// Reads the v8 state and returns the permissionless contract. The pool index is backfilled from the existing
/// pools, lowest id first, so the first pool of a pair and tick spacing keeps the key. Later duplicates
/// stay in place and are not indexed.
/// The v8 state of a permissionless contract: every field but `permissionless`. Test fixtures use it to
/// write the layout on `main` before `migrate_from_v8` runs.
#[cfg(test)]
pub(crate) fn into_v8(c: Contract) -> ContractV8 {
    let Contract {
        owner,
        fee_config,
        pools,
        pool_tokens,
        next_pool_id,
        legacy_balances,
        balances,
        allowed_tokens,
        mpc_contract,
        links,
        action_nonces,
        withdrawals,
        next_withdrawal_id,
        relayers,
        evm_assets,
        evm_asset_by_token,
        evm_chains,
        evm_deposits,
        evm_deposits_by_asset,
        has_state,
        wind_token,
        ve,
        voter,
        minter,
        distributor,
        gauges,
        stakes,
        nft_manager,
        wind_genesis_done,
        wind_genesis_pending,
        wind_minted,
        pending_epoch,
        liabilities,
        accounted,
        needs_seed,
        pool_reserves,
        reserve_totals,
        reserves_seeded,
        pending_withdrawals,
        unbacked,
        positions_by_owner,
        stakes_by_owner,
        withdrawals_by_owner,
        balance_tokens_by_owner,
        intents,
        next_intent_id,
        intents_by_owner,
        open_intents,
        permissionless: _,
    } = c;
    ContractV8 {
        owner,
        fee_config,
        pools,
        pool_tokens,
        next_pool_id,
        legacy_balances,
        balances,
        allowed_tokens,
        mpc_contract,
        links,
        action_nonces,
        withdrawals,
        next_withdrawal_id,
        relayers,
        evm_assets,
        evm_asset_by_token,
        evm_chains,
        evm_deposits,
        evm_deposits_by_asset,
        has_state,
        wind_token,
        ve,
        voter,
        minter,
        distributor,
        gauges,
        stakes,
        nft_manager,
        wind_genesis_done,
        wind_genesis_pending,
        wind_minted,
        pending_epoch,
        liabilities,
        accounted,
        needs_seed,
        pool_reserves,
        reserve_totals,
        reserves_seeded,
        pending_withdrawals,
        unbacked,
        positions_by_owner,
        stakes_by_owner,
        withdrawals_by_owner,
        balance_tokens_by_owner,
        intents,
        next_intent_id,
        intents_by_owner,
        open_intents,
    }
}

pub(crate) fn migrate_from_v8() -> Contract {
    let old: ContractV8 =
        env::state_read().unwrap_or_else(|| env::panic_str("no contract state to migrate"));
    let mut contract = Contract {
        owner: old.owner,
        fee_config: old.fee_config,
        pools: old.pools,
        pool_tokens: old.pool_tokens,
        next_pool_id: old.next_pool_id,
        legacy_balances: old.legacy_balances,
        balances: old.balances,
        allowed_tokens: old.allowed_tokens,
        mpc_contract: old.mpc_contract,
        links: old.links,
        action_nonces: old.action_nonces,
        withdrawals: old.withdrawals,
        next_withdrawal_id: old.next_withdrawal_id,
        relayers: old.relayers,
        evm_assets: old.evm_assets,
        evm_asset_by_token: old.evm_asset_by_token,
        evm_chains: old.evm_chains,
        evm_deposits: old.evm_deposits,
        evm_deposits_by_asset: old.evm_deposits_by_asset,
        has_state: old.has_state,
        wind_token: old.wind_token,
        ve: old.ve,
        voter: old.voter,
        minter: old.minter,
        distributor: old.distributor,
        gauges: old.gauges,
        stakes: old.stakes,
        nft_manager: old.nft_manager,
        wind_genesis_done: old.wind_genesis_done,
        wind_genesis_pending: old.wind_genesis_pending,
        wind_minted: old.wind_minted,
        pending_epoch: old.pending_epoch,
        liabilities: old.liabilities,
        accounted: old.accounted,
        needs_seed: old.needs_seed,
        pool_reserves: old.pool_reserves,
        reserve_totals: old.reserve_totals,
        reserves_seeded: old.reserves_seeded,
        pending_withdrawals: old.pending_withdrawals,
        unbacked: old.unbacked,
        positions_by_owner: old.positions_by_owner,
        stakes_by_owner: old.stakes_by_owner,
        withdrawals_by_owner: old.withdrawals_by_owner,
        balance_tokens_by_owner: old.balance_tokens_by_owner,
        intents: old.intents,
        next_intent_id: old.next_intent_id,
        intents_by_owner: old.intents_by_owner,
        open_intents: old.open_intents,
        permissionless: Permissionless::new(),
    };
    let mut ids: Vec<u64> = contract.pools.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let (token0, token1) = contract.tokens_of(id);
        let tick_spacing = contract.pool_ref(id).tick_spacing;
        let key = pool_key(&token0, &token1, tick_spacing);
        if !contract.permissionless.pool_by_key.contains_key(&key) {
            contract.permissionless.pool_by_key.insert(key, id);
        }
        contract.touch_pool(id);
    }
    contract
}

impl Contract {
    /// The registered EVM asset a deposit is credited to. An unregistered ERC-20 on an enabled chain
    /// is registered now, when the report carries its `decimals`. `None` means the report is refused
    /// with a log and nothing changes. A native asset or a disabled chain panics, as before.
    pub(crate) fn asset_for_report(
        &mut self,
        chain_id: u64,
        asset: &str,
        tx_hash: &[u8; 32],
        log_index: u32,
        decimals: Option<u8>,
        symbol: Option<String>,
    ) -> Option<EvmAsset> {
        let key = evm_asset_key(chain_id, asset);
        if let Some(info) = self.evm_assets.get(&key) {
            return Some(info.clone());
        }
        if asset == "native" || !self.evm_chains.contains_key(&chain_id) {
            env::panic_str(&format!(
                "asset {asset} on chain {chain_id} is not registered"
            ));
        }
        // A deposit credited before the asset was part of the key is a duplicate, not a new asset.
        if self.evm_deposits.contains(&(chain_id, *tx_hash, log_index)) {
            return None;
        }
        let refuse = |reason: &str| {
            env::log_str(&format!(
                "deposit refused: asset {asset} on chain {chain_id}: {reason}"
            ));
            None
        };
        let Some(caps) = self.permissionless.default_caps else {
            return refuse("no default caps are set");
        };
        let Some(decimals) = decimals else {
            return refuse("decimals are required to register an unknown token");
        };
        if decimals > MAX_DECIMALS {
            return refuse("decimals above 36");
        }
        let token_id = auto_token_id(chain_id, asset);
        if self.evm_asset_by_token.contains_key(token_id.as_str()) {
            return refuse("token id is already used by another asset");
        }
        let info = EvmAsset {
            chain_id,
            asset: asset.to_string(),
            token_id: token_id.clone(),
            decimals,
            per_deposit_cap: caps.per_deposit_cap,
            daily_withdraw_cap: caps.daily_withdraw_cap,
            window_day: 0,
            window_withdrawn: 0,
        };
        self.evm_assets.insert(key.clone(), info.clone());
        self.evm_asset_by_token
            .insert(token_id.clone(), key.clone());
        self.permissionless.evm_meta.insert(
            key,
            EvmMeta {
                verified: false,
                symbol: sanitize_symbol(symbol),
            },
        );
        env::log_str(&format!(
            "auto-registered {token_id} ({asset} on chain {chain_id}, {decimals} decimals)"
        ));
        Some(info)
    }

    /// Listing metadata of an EVM asset key. Assets with no entry are verified (owner-registered).
    fn evm_meta_of(&self, key: &str) -> EvmMeta {
        self.permissionless
            .evm_meta
            .get(key)
            .cloned()
            .unwrap_or(EvmMeta {
                verified: true,
                symbol: String::new(),
            })
    }

    /// The JSON view of an EVM asset, with its listing metadata.
    pub(crate) fn evm_asset_view_of(&self, a: &EvmAsset) -> EvmAssetView {
        let meta = self.evm_meta_of(&evm_asset_key(a.chain_id, &a.asset));
        evm_asset_view(a, meta.verified, meta.symbol)
    }

    /// Refreshes the summary of `pool_id` from its stored state, and adds `swaps`, `volume0` and
    /// `volume1` to its swap counters. Call it after the pool is written, so the summary matches the
    /// stored pool. Every write path of a pool calls it: `write_back`, `swap_for`, `mint_for`,
    /// `burn_for`, `collect_for`, the stake and unstake paths, `gauge_create`, the gauge notify,
    /// `create_pool`, and the fee setters.
    pub(crate) fn sync_pool(&mut self, pool_id: u64, swaps: u64, volume0: u128, volume1: u128) {
        let (token0, token1) = self.tokens_of(pool_id);
        let pool = self.pool_ref(pool_id);
        let tick_spacing = pool.tick_spacing;
        let fee = self
            .fee_config
            .swap_fee(pool_id, tick_spacing, &PUBLIC_TRADER);
        let pool = self.pool_ref(pool_id);
        let previous = self.permissionless.pool_summaries.get(&pool_id).cloned();
        let (created_at, swap_count, total0, total1) = match &previous {
            Some(p) => (p.created_at, p.swap_count, p.volume0.0, p.volume1.0),
            None => (now_secs(), 0, 0, 0),
        };
        let summary = PoolSummary {
            pool_id,
            token0,
            token1,
            tick_spacing,
            fee,
            sqrt_price_x96: pool.slot0.sqrt_price_x96.to_string(),
            tick: pool.slot0.tick,
            liquidity: U128(pool.liquidity),
            staked_liquidity: U128(pool.staked_liquidity),
            fee_growth_global0_x128: pool.fee_growth_global0_x128.to_string(),
            fee_growth_global1_x128: pool.fee_growth_global1_x128.to_string(),
            has_gauge: pool.gauge.is_some(),
            created_at,
            swap_count: swap_count.saturating_add(swaps),
            volume0: U128(total0.saturating_add(volume0)),
            volume1: U128(total1.saturating_add(volume1)),
        };
        self.permissionless.pool_summaries.insert(pool_id, summary);
    }

    /// Refreshes the summary of `pool_id` without counting a swap.
    pub(crate) fn touch_pool(&mut self, pool_id: u64) {
        self.sync_pool(pool_id, 0, 0, 0);
    }

    /// Counts the hops of a route that just ran. Call it after the route's pools are written.
    pub(crate) fn count_swaps(&mut self, flows: &[HopFlow]) {
        for flow in flows {
            let paid = to_u128(flow.paid);
            let received = to_u128(flow.received);
            // A hop that pays token0 in moves token0 and then token1, and the other way round.
            let (volume0, volume1) = if flow.zero_for_one {
                (paid, received)
            } else {
                (received, paid)
            };
            self.sync_pool(flow.pool_id, 1, volume0, volume1);
        }
    }

    /// A token a pool may use: a whitelisted NEP-141 or a registered EVM asset.
    fn is_known_token(&self, token: &str) -> bool {
        let whitelisted = token
            .parse::<AccountId>()
            .is_ok_and(|a| self.allowed_tokens.contains(&a));
        whitelisted || self.evm_asset_by_token.contains_key(token)
    }
}

#[near]
impl Contract {
    /// Creates and initializes a pool for `(token0, token1)`. The tick spacing must be enabled. There
    /// is one pool per token pair and tick spacing, whatever the order of the tokens.
    ///
    /// Anyone can call it when both tokens are known: a whitelisted NEP-141 or a registered EVM
    /// asset. A non-owner must attach at least `POOL_STORAGE_DEPOSIT`. That deposit stays in the
    /// contract, and the excess is refunded. The owner can create pools for any account-id tokens
    /// without a deposit.
    #[payable]
    pub fn create_pool(
        &mut self,
        token0: String,
        token1: String,
        tick_spacing: i32,
        sqrt_price_x96: String,
    ) -> u64 {
        let caller = env::predecessor_account_id();
        let attached = env::attached_deposit().as_yoctonear();
        let is_owner = caller == self.owner;
        if token0 == token1 {
            env::panic_str("token0 and token1 must differ");
        }
        let kept = if is_owner {
            self.validate_pool_token(&token0);
            self.validate_pool_token(&token1);
            0
        } else {
            if attached < POOL_STORAGE_DEPOSIT {
                env::panic_str(&format!(
                    "attach at least {POOL_STORAGE_DEPOSIT} yoctoNEAR for the pool storage"
                ));
            }
            if !self.is_known_token(&token0) || !self.is_known_token(&token1) {
                env::panic_str("both tokens must be whitelisted or registered EVM assets");
            }
            POOL_STORAGE_DEPOSIT
        };
        if !self
            .fee_config
            .tick_spacing_to_fee
            .contains_key(&tick_spacing)
        {
            env::panic_str(&format!("tick spacing {tick_spacing} is not enabled"));
        }
        let key = pool_key(&token0, &token1, tick_spacing);
        if let Some(existing) = self.permissionless.pool_by_key.get(&key) {
            env::panic_str(&format!(
                "pool {existing} already exists for this pair and tick spacing"
            ));
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
        self.permissionless.pool_by_key.insert(key, pool_id);
        self.touch_pool(pool_id);

        let refund = attached - kept;
        if refund > 0 {
            let _ = Promise::new(caller).transfer(NearToken::from_yoctonear(refund));
        }
        pool_id
    }

    /// The pool id of a token pair and tick spacing, in either token order.
    pub fn get_pool_id(&self, token0: String, token1: String, tick_spacing: i32) -> Option<u64> {
        self.permissionless
            .pool_by_key
            .get(&pool_key(&token0, &token1, tick_spacing))
            .copied()
    }

    /// The summary of a pool (see `PoolSummary`). Reads no tick or position data.
    pub fn get_pool_summary(&self, pool_id: u64) -> Option<PoolSummary> {
        self.permissionless.pool_summaries.get(&pool_id).cloned()
    }

    /// Pool summaries with ids from `from_index` on, at most `limit` (capped at 100). Reads only the
    /// summaries, never the pool state, so the cost does not grow with the pools' ticks. Pools are
    /// never removed, so the ids are contiguous.
    pub fn get_pools(&self, from_index: u64, limit: u64) -> Vec<PoolSummary> {
        let end = self
            .next_pool_id
            .min(from_index.saturating_add(limit.min(MAX_POOL_PAGE)));
        (from_index..end)
            .map(|id| {
                self.permissionless
                    .pool_summaries
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| env::panic_str(&format!("no summary for pool {id}")))
            })
            .collect()
    }

    /// Anyone can list a NEP-141 token: `ft_metadata` must succeed and report `decimals <= 36`. The
    /// caller attaches at least `REGISTER_TOKEN_DEPOSIT`, which is kept on success and refunded on
    /// failure, with the excess refunded in both cases. Returns a promise. The token is whitelisted
    /// in the callback `on_register_token`.
    #[payable]
    pub fn register_token(&mut self, token: AccountId) -> Promise {
        let attached = env::attached_deposit().as_yoctonear();
        if attached < REGISTER_TOKEN_DEPOSIT {
            env::panic_str(&format!(
                "attach at least {REGISTER_TOKEN_DEPOSIT} yoctoNEAR to register a token"
            ));
        }
        if self.allowed_tokens.contains(&token) {
            env::panic_str("token already allowed");
        }
        ext_fungible_meta::ext(token.clone())
            .with_static_gas(Gas::from_tgas(10))
            .ft_metadata()
            .then(
                ext_register::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(20))
                    .on_register_token(token, env::predecessor_account_id(), U128(attached)),
            )
    }

    /// Callback for `register_token`. Whitelists the token if the metadata is valid. Otherwise it
    /// refunds the whole deposit.
    #[private]
    pub fn on_register_token(
        &mut self,
        token: AccountId,
        payer: AccountId,
        attached: U128,
        #[callback_result] meta: Result<FtMetadata, PromiseError>,
    ) -> bool {
        let listed = match meta {
            Ok(m) if m.decimals <= MAX_DECIMALS => {
                if self.allowed_tokens.contains(&token) {
                    false
                } else {
                    self.allowed_tokens.insert(token.clone());
                    true
                }
            }
            Ok(m) => {
                env::log_str(&format!(
                    "{token} not listed: decimals {} above {MAX_DECIMALS}",
                    m.decimals
                ));
                false
            }
            Err(e) => {
                env::log_str(&format!("{token} not listed: ft_metadata failed: {e:?}"));
                false
            }
        };
        let refund = if listed {
            attached.0 - REGISTER_TOKEN_DEPOSIT
        } else {
            attached.0
        };
        if refund > 0 {
            let _ = Promise::new(payer).transfer(NearToken::from_yoctonear(refund));
        }
        listed
    }

    /// Owner only. Sets the caps of auto-registered EVM assets. `per_deposit_cap` must be positive.
    /// Existing assets keep their caps. Use `register_evm_asset` to change one.
    pub fn set_default_caps(&mut self, per_deposit_cap: U128, daily_withdraw_cap: U128) {
        self.assert_owner();
        if per_deposit_cap.0 == 0 {
            env::panic_str("per_deposit_cap must be positive");
        }
        self.permissionless.default_caps = Some(DefaultCaps {
            per_deposit_cap: per_deposit_cap.0,
            daily_withdraw_cap: daily_withdraw_cap.0,
        });
    }

    /// The caps of auto-registered EVM assets, or null if not set.
    pub fn get_default_caps(&self) -> Option<DefaultCapsView> {
        self.permissionless.default_caps.map(|c| DefaultCapsView {
            per_deposit_cap: U128(c.per_deposit_cap),
            daily_withdraw_cap: U128(c.daily_withdraw_cap),
        })
    }

    /// Owner only. Sets whether a registered EVM asset is verified. The flag is informational: the
    /// contract does not change behaviour for it. `verified: true` marks a major token.
    pub fn set_evm_asset_verified(&mut self, chain_id: u64, asset: String, verified: bool) {
        self.assert_owner();
        let asset = normalize_asset(&asset).unwrap_or_else(|e| env::panic_str(&e));
        let key = evm_asset_key(chain_id, &asset);
        if !self.evm_assets.contains_key(&key) {
            env::panic_str(&format!(
                "asset {asset} on chain {chain_id} is not registered"
            ));
        }
        let symbol = self.evm_meta_of(&key).symbol;
        self.permissionless
            .evm_meta
            .insert(key, EvmMeta { verified, symbol });
    }
}
