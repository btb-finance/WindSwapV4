//! Owner-addressed views, the owner indexes behind them, and their backfill.
//!
//! NEAR maps cannot be iterated by owner, so the contract keeps four indexes, one list per owner:
//! - `positions_by_owner`: the `(pool, tick_lower, tick_upper)` ranges where the owner has a
//!   position (liquidity or owed fees) or a stake. `sync_position_index` keeps it in step with the
//!   engine after every mint, burn, collect, stake and unstake, so it is a set of live ranges.
//! - `stakes_by_owner`: the keys of the owner's staked positions (`stake_index_add/remove`).
//! - `withdrawals_by_owner`: the ids of the owner's EVM withdrawals, appended and never removed.
//! - `balance_tokens_by_owner`: the tokens the owner holds a nonzero balance of, kept by `credit`
//!   and `debit`.
//!
//! The indexes were added after the v6 deployment, so they start empty at `migrate`. The owner
//! fills them with the `backfill_*` calls (see docs/testnet.md). Each backfill is idempotent.
//!
//! Owners are passed as 64 hex digits (see `resolve_wallet`). Lists are paged with `from_index`
//! and `limit`; a `limit` above `MAX_PAGE` is capped.

use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::json_types::U128;
use near_sdk::store::LookupMap;
use near_sdk::{env, near};
use windswap_engine::U256;

use crate::accounts::{parse_owner_hex, Owner};
use crate::tokenomics::{GaugeView, StakeKey};
use crate::{Contract, ContractExt, PositionView, WithdrawalView};

/// A position range of one pool: `(pool id, tick_lower, tick_upper)`.
pub type RangeKey = (u64, i32, i32);

/// The most entries one paged view returns.
const MAX_PAGE: u64 = 100;

/// Owner-facing view of one position range. A staked range reports the NFT's liquidity and no
/// owed fees, since its fees sit in the NFT manager until unstake.
#[near(serializers = [json])]
pub struct OwnerPositionView {
    pub pool_id: u64,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub liquidity: U128,
    pub tokens_owed0: U128,
    pub tokens_owed1: U128,
    pub staked: bool,
}

/// A staked position: its range and the NFT that holds it.
#[near(serializers = [json])]
pub struct StakeView {
    pub pool_id: u64,
    pub tick_lower: i32,
    pub tick_upper: i32,
    /// The NFT id of the staked position in the NFT manager.
    pub token_id: String,
    pub liquidity: U128,
}

fn owner_arg(owner: &str) -> Owner {
    parse_owner_hex(owner).unwrap_or_else(|| env::panic_str("owner must be 64 hex digits"))
}

/// Page of `list`: at most `limit` items from `from_index`.
fn page<T: Clone>(list: Option<&Vec<T>>, from_index: u64, limit: u64) -> Vec<T> {
    let from = usize::try_from(from_index).unwrap_or(usize::MAX);
    let limit = usize::try_from(limit.min(MAX_PAGE)).unwrap_or(usize::MAX);
    list.map(|items| items.iter().skip(from).take(limit).cloned().collect())
        .unwrap_or_default()
}

/// Adds `item` to the list of `owner`, unless it is already there.
fn list_add<T>(map: &mut LookupMap<Owner, Vec<T>>, owner: Owner, item: T)
where
    T: PartialEq + Clone + BorshSerialize + BorshDeserialize,
{
    let mut list = map.get(&owner).cloned().unwrap_or_default();
    if !list.contains(&item) {
        list.push(item);
        map.insert(owner, list);
    }
}

/// Removes `item` from the list of `owner`. An empty list is removed from the map.
fn list_remove<T>(map: &mut LookupMap<Owner, Vec<T>>, owner: Owner, item: &T)
where
    T: PartialEq + Clone + BorshSerialize + BorshDeserialize,
{
    let Some(mut list) = map.get(&owner).cloned() else {
        return;
    };
    let before = list.len();
    list.retain(|x| x != item);
    if list.len() == before {
        return;
    }
    if list.is_empty() {
        map.remove(&owner);
    } else {
        map.insert(owner, list);
    }
}

impl Contract {
    /// Makes `positions_by_owner` match the engine state of one range: present while the owner
    /// has liquidity, owed fees or a stake there. Reads the pool, so it can run after any change.
    pub(crate) fn sync_position_index(
        &mut self,
        owner: Owner,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    ) {
        let Some(pool) = self.pools.get(&pool_id) else {
            return;
        };
        let position = pool.position(&owner, tick_lower, tick_upper);
        let staked = self
            .stakes
            .contains_key(&(owner, pool_id, tick_lower, tick_upper));
        let key: RangeKey = (pool_id, tick_lower, tick_upper);
        if staked
            || position.liquidity > 0
            || position.tokens_owed0 > 0
            || position.tokens_owed1 > 0
        {
            list_add(&mut self.positions_by_owner, owner, key);
        } else {
            list_remove(&mut self.positions_by_owner, owner, &key);
        }
    }

    /// Records a new stake in the owner's stake index and syncs its range.
    pub(crate) fn stake_index_add(&mut self, owner: Owner, pool_id: u64, lo: i32, hi: i32) {
        list_add(&mut self.stakes_by_owner, owner, (pool_id, lo, hi));
        self.sync_position_index(owner, pool_id, lo, hi);
    }

    /// Removes a stake from the owner's stake index and syncs its range.
    pub(crate) fn stake_index_remove(&mut self, owner: Owner, pool_id: u64, lo: i32, hi: i32) {
        list_remove(&mut self.stakes_by_owner, owner, &(pool_id, lo, hi));
        self.sync_position_index(owner, pool_id, lo, hi);
    }

    /// Records that `owner` now holds a nonzero balance of `token`.
    pub(crate) fn index_token_added(&mut self, owner: &Owner, token: &str) {
        list_add(&mut self.balance_tokens_by_owner, *owner, token.to_string());
    }

    /// Records that `owner`'s balance of `token` went to zero.
    pub(crate) fn index_token_removed(&mut self, owner: &Owner, token: &str) {
        list_remove(
            &mut self.balance_tokens_by_owner,
            *owner,
            &token.to_string(),
        );
    }

    /// Records a withdrawal of `owner`.
    pub(crate) fn index_withdrawal(&mut self, owner: Owner, id: u64) {
        list_add(&mut self.withdrawals_by_owner, owner, id);
    }

    /// The view of one indexed range of `owner`. Staked ranges read the NFT's liquidity.
    fn owner_position_view(
        &self,
        owner: Owner,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    ) -> OwnerPositionView {
        if let Some(token_id) = self.stakes.get(&(owner, pool_id, tick_lower, tick_upper)) {
            let liquidity = self.staked_liquidity(*token_id);
            return OwnerPositionView {
                pool_id,
                tick_lower,
                tick_upper,
                liquidity: U128(liquidity),
                tokens_owed0: U128(0),
                tokens_owed1: U128(0),
                staked: true,
            };
        }
        let position = self
            .pool_ref(pool_id)
            .position(&owner, tick_lower, tick_upper);
        OwnerPositionView {
            pool_id,
            tick_lower,
            tick_upper,
            liquidity: U128(position.liquidity),
            tokens_owed0: U128(position.tokens_owed0),
            tokens_owed1: U128(position.tokens_owed1),
            staked: false,
        }
    }

    /// Liquidity of an NFT of the NFT manager. Zero if the NFT does not exist.
    fn staked_liquidity(&self, token_id: U256) -> u128 {
        self.nft_manager
            .get()
            .as_ref()
            .and_then(|nft| nft.position(token_id).ok())
            .map(|p| p.liquidity)
            .unwrap_or(0)
    }

    /// Creates a lock. With `permanent` it is made permanent in the same step, as `create_lock`
    /// followed by `lock_permanent`.
    pub(crate) fn lock_create_with(
        &mut self,
        owner: Owner,
        amount: u128,
        duration: u64,
        permanent: bool,
    ) -> u64 {
        let token = self.lock_create(owner, amount, duration);
        if permanent {
            self.lock_set_permanent(owner, token, true);
        }
        token
    }
}

#[near]
impl Contract {
    /// Position of `owner` (64 hex digits) in `[tick_lower, tick_upper)` of `pool_id`. Same
    /// values as `get_position`, addressed by owner instead of account.
    pub fn get_position_of_owner(
        &self,
        owner: String,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    ) -> PositionView {
        let position = self
            .pool_ref(pool_id)
            .position(&owner_arg(&owner), tick_lower, tick_upper);
        PositionView {
            liquidity: U128(position.liquidity),
            fee_growth_inside0_last_x128: position.fee_growth_inside0_last_x128.to_string(),
            fee_growth_inside1_last_x128: position.fee_growth_inside1_last_x128.to_string(),
            tokens_owed0: U128(position.tokens_owed0),
            tokens_owed1: U128(position.tokens_owed1),
        }
    }

    /// Up to `limit` positions of `owner`, from index `from_index` of its list. A position is
    /// listed while it has liquidity, owed fees or a stake.
    pub fn get_positions_of_owner(
        &self,
        owner: String,
        from_index: u64,
        limit: u64,
    ) -> Vec<OwnerPositionView> {
        let owner = owner_arg(&owner);
        page(self.positions_by_owner.get(&owner), from_index, limit)
            .into_iter()
            .map(|(pool_id, lo, hi)| self.owner_position_view(owner, pool_id, lo, hi))
            .collect()
    }

    /// Up to `limit` staked positions of `owner`, from index `from_index`.
    pub fn get_stakes_of_owner(
        &self,
        owner: String,
        from_index: u64,
        limit: u64,
    ) -> Vec<StakeView> {
        let owner = owner_arg(&owner);
        page(self.stakes_by_owner.get(&owner), from_index, limit)
            .into_iter()
            .filter_map(|(pool_id, lo, hi)| {
                let key: StakeKey = (owner, pool_id, lo, hi);
                let token_id = *self.stakes.get(&key)?;
                Some(StakeView {
                    pool_id,
                    tick_lower: lo,
                    tick_upper: hi,
                    token_id: token_id.to_string(),
                    liquidity: U128(self.staked_liquidity(token_id)),
                })
            })
            .collect()
    }

    /// Up to `limit` EVM withdrawals of `owner`, from index `from_index`, in creation order.
    pub fn get_withdrawals_of_owner(
        &self,
        owner: String,
        from_index: u64,
        limit: u64,
    ) -> Vec<WithdrawalView> {
        let owner = owner_arg(&owner);
        page(self.withdrawals_by_owner.get(&owner), from_index, limit)
            .into_iter()
            .filter_map(|id| self.withdrawals.get(&id).map(|w| (id, w)))
            .map(|(id, w)| crate::withdrawal_view(id, w))
            .collect()
    }

    /// Every token `owner` holds a nonzero internal balance of, with the amount. Not paged: the
    /// list has one entry per token the owner holds.
    pub fn get_balances_of_owner(&self, owner: String) -> Vec<(String, U128)> {
        let owner = owner_arg(&owner);
        self.balance_tokens_by_owner
            .get(&owner)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|token| {
                let amount = self.balance_of(&owner, &token);
                (token, U128(amount))
            })
            .filter(|(_, amount)| amount.0 > 0)
            .collect()
    }

    /// Tokens the owner accepts as bribes (`whitelist_bribe_token`).
    pub fn get_bribe_tokens(&self) -> Vec<String> {
        self.voter
            .get()
            .as_ref()
            .map(|voter| {
                voter
                    .is_whitelisted_token
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The view of every gauge, in the order of the voter's pools.
    pub fn get_gauges(&self) -> Vec<GaugeView> {
        let pools = self
            .voter
            .get()
            .as_ref()
            .map(|voter| voter.pools.clone())
            .unwrap_or_default();
        pools
            .into_iter()
            .filter_map(|pool| self.get_gauge(pool))
            .collect()
    }

    /// Owner only. Sets the position index of each listed `(owner, pool, tick_lower, tick_upper)`
    /// from the engine state, so it is safe to pass ranges that are empty or unknown. Returns the
    /// number of entries processed. Run it for every range that existed before the index.
    pub fn backfill_position_index(&mut self, entries: Vec<(String, u64, i32, i32)>) -> u64 {
        self.assert_owner();
        let count = entries.len();
        for (owner, pool_id, lo, hi) in entries {
            self.sync_position_index(owner_arg(&owner), pool_id, lo, hi);
        }
        u64::try_from(count).unwrap_or(u64::MAX)
    }

    /// Owner only. Indexes each listed `(owner, pool, tick_lower, tick_upper)` that is staked.
    /// Entries that are not staked are skipped. Returns the number indexed.
    pub fn backfill_stake_index(&mut self, entries: Vec<(String, u64, i32, i32)>) -> u64 {
        self.assert_owner();
        let mut indexed = 0;
        for (owner, pool_id, lo, hi) in entries {
            let owner = owner_arg(&owner);
            if self.stakes.contains_key(&(owner, pool_id, lo, hi)) {
                self.stake_index_add(owner, pool_id, lo, hi);
                indexed += 1;
            }
        }
        indexed
    }

    /// Owner only. Indexes each listed `(owner, token)` whose balance is nonzero. Returns the
    /// number indexed.
    pub fn backfill_token_index(&mut self, entries: Vec<(String, String)>) -> u64 {
        self.assert_owner();
        let mut indexed = 0;
        for (owner, token) in entries {
            let owner = owner_arg(&owner);
            if self.balance_of(&owner, &token) > 0 {
                self.index_token_added(&owner, &token);
                indexed += 1;
            }
        }
        indexed
    }

    /// Owner only. Indexes each withdrawal id under its owner. Ids that do not exist are skipped.
    /// Returns the number indexed.
    pub fn backfill_withdrawal_index(&mut self, ids: Vec<u64>) -> u64 {
        self.assert_owner();
        let mut indexed = 0;
        for id in ids {
            if let Some(owner) = self.withdrawals.get(&id).map(|w| w.owner) {
                self.index_withdrawal(owner, id);
                indexed += 1;
            }
        }
        indexed
    }
}
