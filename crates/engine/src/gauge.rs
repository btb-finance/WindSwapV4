//! Port of `CLGauge.sol`: the gauge that stakes concentrated-liquidity position NFTs into a
//! [`ClPool`] and pays the pool's reward token to the stakers, plus the fee claim that routes
//! the pool's gauge fees to the fees voting reward.
//!
//! Differences from the Solidity contract (same conventions as `cl_pool.rs`):
//! - No token transfers. Amounts the contract transfers are returned instead: NFT collects
//!   (`deposit`, `withdraw`), reward payouts (`withdraw`, `get_reward*`) and the fee tokens sent to
//!   the fees voting reward (`notify_reward_amount`). The gauge's reward-token balance is kept as
//!   a ledger ([`ClGauge::reward_balance`]) because `notifyRewardAmount` checks it (`RRH`).
//! - `msg.sender` is the explicit `caller` argument. The `require`s on it (`NV`, `NA`, ownership)
//!   are checked here. `onlyGauge` on the pool is checked against [`ClPool::gauge`].
//! - Time is passed in as `now` (`block.timestamp`), as u32 like the pool's `lastUpdated`.
//! - The voter's `isAlive(gauge)` answer is state on the gauge ([`ClGauge::set_alive`]).
//! - `nonReentrant` is not modeled: calls are single-threaded and each one is atomic from
//!   the caller's view (see the atomicity note below).
//! - The NFT manager is [`PositionManager`]: a minimal model of the positions, owners and
//!   approvals of NonfungiblePositionManager, enough for the gauge's calls.
//!
//! Atomicity: like a Solidity revert, an `Err` means the whole call must be discarded. The gauge,
//! the NFT manager and the pool may be partially updated when an error is returned. Callers
//! that need revert semantics clone the three first (the differential test does this).

use std::collections::{BTreeMap, BTreeSet};

use crate::error::MathError;
use crate::math::full_math::mul_div;
use crate::math::Q128;
use crate::pool::cl_pool::{AccountId, ClPool};
use crate::U256;

/// `ProtocolTimeLibrary.WEEK`
pub const WEEK: u32 = 7 * 24 * 60 * 60;
/// `CLGauge.MIN_STAKE_DURATION` (anti-JIT, 10 minutes).
pub const MIN_STAKE_DURATION: u32 = 10 * 60;
/// `CLGauge.sol` fee threshold: fees above one week are claimed to the fees voting reward.
const FEE_CLAIM_THRESHOLD: U256 = U256::from_limbs([WEEK as u64, 0, 0, 0]);

/// Failures of the gauge. `Math` wraps the pool and math errors; the other variants name the
/// Solidity `require` (or the ERC20/ERC721 revert) that fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GaugeError {
    /// A pool or math error (overflow, pool not initialized, invalid tick range, ...).
    Math(MathError),
    /// `"NA"` in `earned`, `getReward(tokenId)` and `withdraw`: the token is not staked by the account.
    NotStaked,
    /// `"NA"` in `deposit`: the caller does not own the NFT (also: the token does not exist).
    NotOwner,
    /// `"NV"`: the caller is not the voter.
    NotVoter,
    /// `"NA"` in `notifyRewardWithoutClaim`: the caller is not the notify admin.
    NotNotifyAdmin,
    /// `"ZR"`: a zero amount was notified.
    ZeroAmount,
    /// `"GK"`: the voter reports the gauge as not alive.
    NotAlive,
    /// `"PM"`: the NFT's tokens or tick spacing differ from the gauge's pool.
    PoolMismatch,
    /// `"NG"`: the pool's `onlyGauge` check fails (the gauge is not the pool's gauge).
    NotPoolGauge,
    /// `"ZRR"`: the notified reward rate is zero.
    ZeroRewardRate,
    /// `"RRH"`: the reward rate exceeds the gauge's reward balance over the period.
    RewardRateTooHigh,
    /// The NFT does not exist (`ownerOf` / `positions` revert on an unknown token).
    InvalidToken,
    /// The NFT manager's `collect` requires the caller to be owner or approved.
    NotApproved,
    /// `safeTransferFrom`: `from` is not the owner, or the caller is not authorized.
    TransferNotAllowed,
    /// The gauge holds fewer tokens than it pays out (the ERC20 `BAL` revert): a reward payout, or
    /// the fee tokens sent to the fees voting reward.
    InsufficientBalance,
}

impl From<MathError> for GaugeError {
    fn from(e: MathError) -> Self {
        GaugeError::Math(e)
    }
}

pub type Result<T> = core::result::Result<T, GaugeError>;

/// `ProtocolTimeLibrary.epochStart`
fn epoch_start(ts: U256) -> U256 {
    let week = U256::from(WEEK);
    ts - ts % week
}

/// `ProtocolTimeLibrary.epochNext`
fn epoch_next(ts: U256) -> U256 {
    epoch_start(ts) + U256::from(WEEK)
}

/// `SafeCast.toInt128` on a liquidity amount: `require(y < 2 ** 127)`.
fn to_i128(liquidity: u128) -> Result<i128> {
    i128::try_from(liquidity).map_err(|_| GaugeError::Math(MathError::Overflow))
}

/// One NFT position as the gauge reads it through `positions(tokenId)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NftPosition {
    pub token0: AccountId,
    pub token1: AccountId,
    pub tick_spacing: i32,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub liquidity: u128,
}

/// Minimal model of the NonfungiblePositionManager (`nft` of the pool and the gauge).
///
/// - Positions: minted with `mint`, which puts the liquidity into the pool under this manager's
///   account (`account`), as the real manager does. Ids start at 1.
/// - Owners and approvals: `transfer_from` (safeTransferFrom) requires the caller to be the owner
///   or approved for all by the owner (`set_approval_for_all`). Receiver hooks are not modeled.
/// - `collect` mirrors `NonfungiblePositionManager.collect`: the caller must be owner or approved;
///   when the position has liquidity it calls `burn(0)` on the pool position that holds it (the
///   gauge's position if the gauge owns the NFT, the manager's otherwise), then pays out the
///   requested owed amounts from that same pool position. The real manager pays per-NFT
///   `tokensOwed`; this model pays the pool position's owed amounts (see the Solidity mock's note).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionManager {
    /// The manager's account: owner of the pool positions it mints (`ClPool::nft`).
    pub account: AccountId,
    next_id: U256,
    positions: BTreeMap<U256, NftPosition>,
    owners: BTreeMap<U256, AccountId>,
    approvals: BTreeSet<(AccountId, AccountId)>,
}

impl PositionManager {
    pub fn new(account: AccountId) -> Self {
        PositionManager {
            account,
            next_id: U256::from(1),
            positions: BTreeMap::new(),
            owners: BTreeMap::new(),
            approvals: BTreeSet::new(),
        }
    }

    /// `mintPosition`: `pool.mint(this, lo, up, liquidity)`, then records the NFT for `owner`.
    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        &mut self,
        pool: &mut ClPool,
        owner: AccountId,
        token0: AccountId,
        token1: AccountId,
        tick_spacing: i32,
        tick_lower: i32,
        tick_upper: i32,
        liquidity: u128,
        time: u32,
    ) -> Result<(U256, U256, U256)> {
        let (a0, a1) = pool.mint(self.account, tick_lower, tick_upper, liquidity, time)?;
        let id = self.next_id;
        self.next_id += U256::from(1);
        self.owners.insert(id, owner);
        self.positions.insert(
            id,
            NftPosition {
                token0,
                token1,
                tick_spacing,
                tick_lower,
                tick_upper,
                liquidity,
            },
        );
        Ok((id, a0, a1))
    }

    /// `setApprovalForAll(operator, approved)` called by `owner`.
    pub fn set_approval_for_all(&mut self, owner: AccountId, operator: AccountId, approved: bool) {
        if approved {
            self.approvals.insert((owner, operator));
        } else {
            self.approvals.remove(&(owner, operator));
        }
    }

    /// `ownerOf(tokenId)`; reverts on an unknown token.
    pub fn owner_of(&self, token_id: U256) -> Result<AccountId> {
        self.owners
            .get(&token_id)
            .copied()
            .ok_or(GaugeError::InvalidToken)
    }

    /// The owner, or `None` for an unknown token (a view that does not revert).
    pub fn owner_or_zero(&self, token_id: U256) -> Option<AccountId> {
        self.owners.get(&token_id).copied()
    }

    /// `positions(tokenId)`; reverts on an unknown token.
    pub fn position(&self, token_id: U256) -> Result<&NftPosition> {
        self.positions
            .get(&token_id)
            .ok_or(GaugeError::InvalidToken)
    }

    /// Owner or approved for all by the owner.
    pub fn is_authorized(&self, spender: AccountId, token_id: U256) -> Result<bool> {
        let owner = self.owner_of(token_id)?;
        Ok(owner == spender || self.approvals.contains(&(owner, spender)))
    }

    /// `safeTransferFrom(from, to, tokenId)` called by `caller`.
    pub fn transfer_from(
        &mut self,
        caller: AccountId,
        from: AccountId,
        to: AccountId,
        token_id: U256,
    ) -> Result<()> {
        if self.owner_of(token_id)? != from || !self.is_authorized(caller, token_id)? {
            return Err(GaugeError::TransferNotAllowed);
        }
        self.owners.insert(token_id, to);
        Ok(())
    }

    /// `collect(CollectParams{tokenId, recipient, amount0Max, amount1Max})` called by `caller`.
    /// Returns the amounts paid to the recipient (the recipient itself is not modeled).
    pub fn collect(
        &mut self,
        pool: &mut ClPool,
        caller: AccountId,
        token_id: U256,
        amount0_max: u128,
        amount1_max: u128,
        time: u32,
    ) -> Result<(u128, u128)> {
        if !self.is_authorized(caller, token_id)? {
            return Err(GaugeError::NotApproved);
        }
        let owner = self.owner_of(token_id)?;
        let position = self.position(token_id)?.clone();
        let pool_owner = if pool.gauge == Some(owner) {
            owner
        } else {
            self.account
        };
        if position.liquidity > 0 {
            pool.burn(
                pool_owner,
                position.tick_lower,
                position.tick_upper,
                0,
                time,
            )?;
        }
        Ok(pool.collect(
            pool_owner,
            position.tick_lower,
            position.tick_upper,
            amount0_max,
            amount1_max,
        )?)
    }
}

/// Constructor inputs of a gauge (the arguments of `CLGauge.initialize` that the gauge uses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GaugeConfig {
    /// The gauge's own account: the pool's registered gauge.
    pub account: AccountId,
    /// The voter: `notifyRewardAmount` and `getReward(account)` are called by it.
    pub voter: AccountId,
    /// The gauge factory's `notifyAdmin()`: `notifyRewardWithoutClaim` is called by it.
    pub notify_admin: AccountId,
    pub token0: AccountId,
    pub token1: AccountId,
    pub tick_spacing: i32,
    /// `isPool`: whether the gauge's pool is a real pool (fees are claimed only then).
    pub is_pool: bool,
}

/// Per-token reward bookkeeping (`rewards`, `rewardGrowthInside`, `lastUpdateTime`, `depositTimestamp`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenRewards {
    pub rewards: U256,
    pub reward_growth_inside: U256,
    pub last_update_time: U256,
    pub deposit_timestamp: U256,
}

/// The fee tokens the gauge sent to the fees voting reward in one `notifyRewardAmount` call.
pub type FeesToVotingReward = (U256, U256);

/// The gauge state. See the module docs for the convention on transfers and `msg.sender`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClGauge {
    pub account: AccountId,
    pub voter: AccountId,
    pub notify_admin: AccountId,
    pub token0: AccountId,
    pub token1: AccountId,
    pub tick_spacing: i32,
    pub is_pool: bool,
    /// The voter's `isAlive(gauge)` answer.
    pub alive: bool,
    pub reward_rate: U256,
    pub period_finish: U256,
    pub reward_rate_by_epoch: BTreeMap<U256, U256>,
    /// The gauge's reward-token balance (what `balanceOf(this)` returns on chain).
    pub reward_balance: U256,
    pub fees0: U256,
    pub fees1: U256,
    /// The gauge's balances of `token0` and `token1` (fee tokens it has received from the pool and
    /// not yet sent to the fees voting reward).
    pub fee_balance0: U256,
    pub fee_balance1: U256,
    pub tokens: BTreeMap<U256, TokenRewards>,
    /// `_stakes[depositor]`: staked token ids in EnumerableSet order.
    pub stakes: BTreeMap<AccountId, Vec<U256>>,
}

impl ClGauge {
    pub fn new(config: GaugeConfig) -> Self {
        ClGauge {
            account: config.account,
            voter: config.voter,
            notify_admin: config.notify_admin,
            token0: config.token0,
            token1: config.token1,
            tick_spacing: config.tick_spacing,
            is_pool: config.is_pool,
            alive: true,
            reward_rate: U256::ZERO,
            period_finish: U256::ZERO,
            reward_rate_by_epoch: BTreeMap::new(),
            reward_balance: U256::ZERO,
            fees0: U256::ZERO,
            fees1: U256::ZERO,
            fee_balance0: U256::ZERO,
            fee_balance1: U256::ZERO,
            tokens: BTreeMap::new(),
            stakes: BTreeMap::new(),
        }
    }

    /// Sets the voter's `isAlive` answer for this gauge.
    pub fn set_alive(&mut self, alive: bool) {
        self.alive = alive;
    }

    /// A reward-token transfer straight to the gauge (not through `notifyRewardAmount`).
    pub fn donate(&mut self, amount: U256) {
        self.reward_balance = self.reward_balance.wrapping_add(amount);
    }

    /// `rewards`, `rewardGrowthInside`, `lastUpdateTime`, `depositTimestamp` of a token.
    pub fn token(&self, token_id: U256) -> TokenRewards {
        self.tokens.get(&token_id).cloned().unwrap_or_default()
    }

    /// `stakedValues(depositor)`: staked token ids in EnumerableSet order.
    pub fn staked_values(&self, account: AccountId) -> Vec<U256> {
        self.stakes.get(&account).cloned().unwrap_or_default()
    }

    pub fn staked_contains(&self, account: AccountId, token_id: U256) -> bool {
        self.stakes
            .get(&account)
            .is_some_and(|ids| ids.contains(&token_id))
    }

    /// `left()`: the reward still to stream in the current period.
    pub fn left(&self, now: u32) -> U256 {
        let ts = U256::from(now);
        if ts >= self.period_finish {
            return U256::ZERO;
        }
        (self.period_finish - ts).wrapping_mul(self.reward_rate)
    }

    /// `rewardRateByEpoch(epochStart)` for the epoch that contains `now`.
    pub fn reward_rate_for_epoch(&self, now: u32) -> U256 {
        self.reward_rate_by_epoch
            .get(&epoch_start(U256::from(now)))
            .copied()
            .unwrap_or_default()
    }

    /// `earned(account, tokenId)`: `require(staked)`, then `_earned`.
    pub fn earned(
        &self,
        pool: &ClPool,
        nft: &PositionManager,
        account: AccountId,
        token_id: U256,
        now: u32,
    ) -> Result<U256> {
        if !self.staked_contains(account, token_id) {
            return Err(GaugeError::NotStaked);
        }
        self.earned_inner(pool, nft, token_id, now)
    }

    /// `_earned(tokenId)`: the reward accrued since the token's last snapshot, including the reward
    /// the pool has not yet folded into its growth (uses the pool's current rate and reserve).
    fn earned_inner(
        &self,
        pool: &ClPool,
        nft: &PositionManager,
        token_id: U256,
        now: u32,
    ) -> Result<U256> {
        let time_delta = U256::from(now).wrapping_sub(U256::from(pool.last_updated));
        let mut global = pool.reward_growth_global_x128;
        if !time_delta.is_zero() && !pool.reward_reserve.is_zero() && pool.staked_liquidity > 0 {
            let mut reward = self.reward_rate.wrapping_mul(time_delta);
            if reward > pool.reward_reserve {
                reward = pool.reward_reserve;
            }
            global = global.wrapping_add(mul_div(reward, Q128, U256::from(pool.staked_liquidity))?);
        }
        let position = nft.position(token_id)?;
        let inside_initial = self.token(token_id).reward_growth_inside;
        let inside =
            pool.get_reward_growth_inside(position.tick_lower, position.tick_upper, global)?;
        Ok(mul_div(
            inside.wrapping_sub(inside_initial),
            U256::from(position.liquidity),
            Q128,
        )?)
    }

    /// `_updateRewards(tokenId, tickLower, tickUpper)`.
    fn update_rewards(
        &mut self,
        pool: &mut ClPool,
        nft: &PositionManager,
        token_id: U256,
        now: u32,
    ) -> Result<()> {
        if self.token(token_id).last_update_time == U256::from(now) {
            return Ok(());
        }
        self.require_pool_gauge(pool)?;
        pool.update_rewards_growth_global(now)?;
        let (lower, upper) = {
            let p = nft.position(token_id)?;
            (p.tick_lower, p.tick_upper)
        };
        let earned = self.earned_inner(pool, nft, token_id, now)?;
        let inside = pool.get_reward_growth_inside(lower, upper, U256::ZERO)?;
        let entry = self.tokens.entry(token_id).or_default();
        entry.last_update_time = U256::from(now);
        entry.rewards = entry.rewards.wrapping_add(earned);
        entry.reward_growth_inside = inside;
        Ok(())
    }

    /// `_getReward(tickLower, tickUpper, tokenId, owner)`: returns the amount paid to the owner.
    /// A reward claimed within [`MIN_STAKE_DURATION`] of the deposit is forfeited (stays in the gauge).
    fn get_reward_for(
        &mut self,
        pool: &mut ClPool,
        nft: &PositionManager,
        token_id: U256,
        now: u32,
    ) -> Result<U256> {
        self.update_rewards(pool, nft, token_id, now)?;
        let entry = self.tokens.entry(token_id).or_default();
        let reward = entry.rewards;
        if reward.is_zero() {
            return Ok(U256::ZERO);
        }
        entry.rewards = U256::ZERO;
        if U256::from(now) < entry.deposit_timestamp + U256::from(MIN_STAKE_DURATION) {
            return Ok(U256::ZERO);
        }
        self.reward_balance = self
            .reward_balance
            .checked_sub(reward)
            .ok_or(GaugeError::InsufficientBalance)?;
        Ok(reward)
    }

    /// `getReward(account)`, called by the voter: pays every token the account has staked.
    /// Returns the total paid.
    pub fn get_reward(
        &mut self,
        pool: &mut ClPool,
        nft: &PositionManager,
        caller: AccountId,
        account: AccountId,
        now: u32,
    ) -> Result<U256> {
        if caller != self.voter {
            return Err(GaugeError::NotVoter);
        }
        let mut total = U256::ZERO;
        for token_id in self.staked_values(account) {
            total = total.wrapping_add(self.get_reward_for(pool, nft, token_id, now)?);
        }
        Ok(total)
    }

    /// `getReward(tokenId)`, called by the staker. Returns the amount paid.
    pub fn get_reward_token(
        &mut self,
        pool: &mut ClPool,
        nft: &PositionManager,
        caller: AccountId,
        token_id: U256,
        now: u32,
    ) -> Result<U256> {
        if !self.staked_contains(caller, token_id) {
            return Err(GaugeError::NotStaked);
        }
        self.get_reward_for(pool, nft, token_id, now)
    }

    /// `deposit(tokenId)`: stakes an NFT owned by `caller`. Returns the amounts the NFT manager
    /// collected to the caller while the position was synced.
    pub fn deposit(
        &mut self,
        pool: &mut ClPool,
        nft: &mut PositionManager,
        caller: AccountId,
        token_id: U256,
        now: u32,
    ) -> Result<(u128, u128)> {
        if nft.owner_of(token_id)? != caller {
            return Err(GaugeError::NotOwner);
        }
        if !self.alive {
            return Err(GaugeError::NotAlive);
        }
        let position = nft.position(token_id)?.clone();
        if position.token0 != self.token0
            || position.token1 != self.token1
            || position.tick_spacing != self.tick_spacing
        {
            return Err(GaugeError::PoolMismatch);
        }
        let collected = nft.collect(pool, self.account, token_id, u128::MAX, u128::MAX, now)?;
        nft.transfer_from(self.account, caller, self.account, token_id)?;
        let ids = self.stakes.entry(caller).or_default();
        if !ids.contains(&token_id) {
            ids.push(token_id);
        }
        let liquidity = nft.position(token_id)?.liquidity;
        self.require_pool_gauge(pool)?;
        pool.stake(
            to_i128(liquidity)?,
            position.tick_lower,
            position.tick_upper,
            true,
            now,
        )?;
        let growth =
            pool.get_reward_growth_inside(position.tick_lower, position.tick_upper, U256::ZERO)?;
        let entry = self.tokens.entry(token_id).or_default();
        entry.reward_growth_inside = growth;
        entry.last_update_time = U256::from(now);
        entry.deposit_timestamp = U256::from(now);
        Ok(collected)
    }

    /// `withdraw(tokenId)`: unstakes an NFT the caller has staked. Returns the collected amounts
    /// and the reward paid.
    pub fn withdraw(
        &mut self,
        pool: &mut ClPool,
        nft: &mut PositionManager,
        caller: AccountId,
        token_id: U256,
        now: u32,
    ) -> Result<(u128, u128, U256)> {
        if !self.staked_contains(caller, token_id) {
            return Err(GaugeError::NotStaked);
        }
        let collected = nft.collect(pool, self.account, token_id, u128::MAX, u128::MAX, now)?;
        let (lower, upper, liquidity) = {
            let p = nft.position(token_id)?;
            (p.tick_lower, p.tick_upper, p.liquidity)
        };
        let paid = self.get_reward_for(pool, nft, token_id, now)?;
        if liquidity != 0 {
            self.require_pool_gauge(pool)?;
            pool.stake(-to_i128(liquidity)?, lower, upper, true, now)?;
        }
        if let Some(ids) = self.stakes.get_mut(&caller) {
            if let Some(i) = ids.iter().position(|id| *id == token_id) {
                ids.swap_remove(i);
            }
        }
        nft.transfer_from(self.account, self.account, caller, token_id)?;
        Ok((collected.0, collected.1, paid))
    }

    /// `_claimFees()`: collects the pool's gauge fees (for real pools only) and returns the amounts
    /// the gauge sends to the fees voting reward. Fees at or below one week stay in the gauge.
    ///
    /// `CLPool.collectFees` transfers `amount - 1` and returns `amount`, except that a slot holding
    /// only the 1-wei sentinel returns 1 without a transfer. The gauge books the returned amount
    /// either way, so it can book wei it never received; the pull of a booked total above the
    /// gauge's balance then reverts (`BAL`). The fee balances below track what was really received.
    fn claim_fees(&mut self, pool: &mut ClPool) -> Result<FeesToVotingReward> {
        if !self.is_pool {
            return Ok((U256::ZERO, U256::ZERO));
        }
        self.require_pool_gauge(pool)?;
        let (before0, before1) = (pool.gauge_fees0, pool.gauge_fees1);
        let (claimed0, claimed1) = pool.collect_fees();
        if claimed0 == 0 && claimed1 == 0 {
            return Ok((U256::ZERO, U256::ZERO));
        }
        if before0 > 1 {
            self.fee_balance0 = self.fee_balance0.wrapping_add(U256::from(claimed0));
        }
        if before1 > 1 {
            self.fee_balance1 = self.fee_balance1.wrapping_add(U256::from(claimed1));
        }
        let fees0 = self.fees0.wrapping_add(U256::from(claimed0));
        let fees1 = self.fees1.wrapping_add(U256::from(claimed1));
        let mut to_reward0 = U256::ZERO;
        let mut to_reward1 = U256::ZERO;
        if fees0 > FEE_CLAIM_THRESHOLD {
            self.fees0 = U256::ZERO;
            self.fee_balance0 = self
                .fee_balance0
                .checked_sub(fees0)
                .ok_or(GaugeError::InsufficientBalance)?;
            to_reward0 = fees0;
        } else {
            self.fees0 = fees0;
        }
        if fees1 > FEE_CLAIM_THRESHOLD {
            self.fees1 = U256::ZERO;
            self.fee_balance1 = self
                .fee_balance1
                .checked_sub(fees1)
                .ok_or(GaugeError::InsufficientBalance)?;
            to_reward1 = fees1;
        } else {
            self.fees1 = fees1;
        }
        Ok((to_reward0, to_reward1))
    }

    /// `notifyRewardAmount(amount)`, called by the voter. Claims the fees first, then streams
    /// `amount` (plus the pool's rollover) over the rest of the epoch. Returns the fee tokens sent
    /// to the fees voting reward.
    pub fn notify_reward_amount(
        &mut self,
        pool: &mut ClPool,
        caller: AccountId,
        amount: U256,
        now: u32,
    ) -> Result<FeesToVotingReward> {
        if caller != self.voter {
            return Err(GaugeError::NotVoter);
        }
        if amount.is_zero() {
            return Err(GaugeError::ZeroAmount);
        }
        let fees = self.claim_fees(pool)?;
        self.notify_inner(pool, amount, now)?;
        Ok(fees)
    }

    /// `notifyRewardWithoutClaim(amount)`, called by the notify admin. No fee claim.
    pub fn notify_reward_without_claim(
        &mut self,
        pool: &mut ClPool,
        caller: AccountId,
        amount: U256,
        now: u32,
    ) -> Result<()> {
        if caller != self.notify_admin {
            return Err(GaugeError::NotNotifyAdmin);
        }
        if amount.is_zero() {
            return Err(GaugeError::ZeroAmount);
        }
        self.notify_inner(pool, amount, now)
    }

    /// `_notifyRewardAmount(sender, amount)`: rollover, rate for the rest of the epoch, sync to the pool.
    fn notify_inner(&mut self, pool: &mut ClPool, amount: U256, now: u32) -> Result<()> {
        let ts = U256::from(now);
        let time_until_next = epoch_next(ts) - ts;
        self.require_pool_gauge(pool)?;
        pool.update_rewards_growth_global(now)?;
        let next_period_finish = ts + time_until_next;
        self.reward_balance = self.reward_balance.wrapping_add(amount);
        let amount = amount.wrapping_add(pool.rollover);
        if ts >= self.period_finish {
            self.reward_rate = amount / time_until_next;
            pool.sync_reward(self.reward_rate, amount, next_period_finish);
        } else {
            let leftover = time_until_next.wrapping_mul(self.reward_rate);
            self.reward_rate = amount.wrapping_add(leftover) / time_until_next;
            pool.sync_reward(
                self.reward_rate,
                amount.wrapping_add(leftover),
                next_period_finish,
            );
        }
        self.reward_rate_by_epoch
            .insert(epoch_start(ts), self.reward_rate);
        if self.reward_rate.is_zero() {
            return Err(GaugeError::ZeroRewardRate);
        }
        if self.reward_rate > self.reward_balance / time_until_next {
            return Err(GaugeError::RewardRateTooHigh);
        }
        self.period_finish = next_period_finish;
        Ok(())
    }

    /// The pool's `onlyGauge` check for the calls this gauge makes.
    fn require_pool_gauge(&self, pool: &ClPool) -> Result<()> {
        if pool.gauge == Some(self.account) {
            Ok(())
        } else {
            Err(GaugeError::NotPoolGauge)
        }
    }
}
