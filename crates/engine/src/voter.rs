//! Port of the windv2 `Voter.sol` vote and emission logic.
//!
//! Ported: `vote`, `reset`, `poke` (`_reset`, `_poke`, `_vote`), `createGauge`, `killGauge`, `reviveGauge`,
//! `whitelistToken`, `whitelistNFT`, `setMaxVotingNum`, `initialize`, `setEmergencyCouncil`,
//! `notifyRewardAmount`, `updateFor` (all three forms), `distribute` (all three forms), `claimBribes` and
//! `claimFees`, with the `onlyNewEpoch` modifier and the vote-window rules.
//!
//! Not ported:
//! - `depositManaged`, `withdrawManaged` and the managed-NFT check in `vote` (`deactivated`). Every token is a
//!   normal veNFT, so `deactivated` is always false. The managed paths need the managed-lock state of the
//!   VotingEscrow port, which is out of scope here.
//! - `setGovernor`, `setEpochGovernor`, `epochStart` and the other pure time helpers (they are in
//!   `rewards.rs`). `claimRewards` (gauge `getReward`) is not ported: the gauge is outside this module.
//! - Events, the ERC2771 forwarder and `nonReentrant`. A call into this module cannot re-enter.
//! - Token balances. Solidity pulls WIND in `notifyRewardAmount` and sends it in `_updateFor` (dead gauges),
//!   `killGauge` and `_distribute`. Those are the caller's job: `notify_reward_amount` takes the amount
//!   already pulled, and the functions that send WIND to the minter return the amount sent.
//! - `IGauge.left()` and `IGauge.notifyRewardAmount` are external. `distribute_*` take a closure that gives the
//!   gauge's `left()` and whether its `notifyRewardAmount` succeeds (`Err` in Solidity is caught by the `try`).
//! - `IMinter.updatePeriod()` is called by `distribute*` in Solidity. It only moves the minter's period state, so
//!   it is omitted.
//!
//! Identifiers. `PoolId` and `GaugeId` are `u64`. A pool is the `address` of the pool, and a gauge is the
//! `address` of the gauge. The gauge id is `1, 2, 3, ...` in creation order (`createGauge` returns it), and `0`
//! stands for `address(0)`. The pool id is chosen by the caller. The differential generator uses `1..=6`. Reward
//! token ids are `RewardToken` strings, with `""` for the zero address, as in `rewards.rs`.
//!
//! Atomicity: as with the other ports, an `Err` means the Solidity call reverted, so the caller must discard the
//! call. The differential test clones the voter and the escrow before each call.
//!
//! Inputs replace `block.timestamp` (`now`), `block.number` (`block`) and `msg.sender` (`sender`, which must
//! be the veNFT owner where Solidity uses `isApprovedOrOwner`; the escrow port has no approvals).

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{MathError, Result};
use crate::fees::AccountId;
use crate::rewards::{epoch_start, epoch_vote_end, epoch_vote_start, Reward, RewardToken};
use crate::ve::{VotingEscrow, ZERO_ACCOUNT};
use crate::U256;

/// `Voter.DURATION` (`7 days`): the minimum claimable amount that is distributed to a gauge.
pub const DURATION: u64 = 7 * 24 * 60 * 60;
/// `Voter.MIN_MAXVOTINGNUM`.
const MIN_MAXVOTINGNUM: u64 = 10;
/// The `maxVotingNum` set by the constructor.
const DEFAULT_MAX_VOTING_NUM: u64 = 30;
/// The `1e18` scale of `notifyRewardAmount` and `_updateFor`.
const ONE_E18: u128 = 1_000_000_000_000_000_000;

/// Pool identifier (the pool address in Solidity). Chosen by the caller.
pub type PoolId = u64;
/// Gauge identifier, assigned in creation order from `1`. `0` is `address(0)`.
pub type GaugeId = u64;
/// veNFT token id.
pub type TokenId = u64;

/// The fees and bribe voting rewards of one gauge (`gaugeToFees` / `gaugeToBribe`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GaugeRewards {
    /// `FeesVotingReward`.
    pub fees: Reward,
    /// `BribeVotingReward`.
    pub bribe: Reward,
}

/// State of the `Voter` contract. Field names mirror the Solidity state variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voter {
    /// The address of this contract, passed to the escrow as `voter` (`msg.sender` of `voting`).
    pub address: AccountId,
    /// `minter`.
    pub minter: AccountId,
    /// `governor`.
    pub governor: AccountId,
    /// `epochGovernor`. Not used by any function ported here.
    pub epoch_governor: AccountId,
    /// `emergencyCouncil`.
    pub emergency_council: AccountId,
    /// `totalWeight`.
    pub total_weight: U256,
    /// `maxVotingNum`.
    pub max_voting_num: U256,
    /// `pools`: pools with a gauge, in creation order.
    pub pools: Vec<PoolId>,
    /// `gauges[pool]`. A missing entry is `address(0)`.
    pub gauges: BTreeMap<PoolId, GaugeId>,
    /// `poolForGauge[gauge]`.
    pub pool_for_gauge: BTreeMap<GaugeId, PoolId>,
    /// `gaugeToFees` and `gaugeToBribe`, held as the reward contracts themselves.
    pub gauge_rewards: BTreeMap<GaugeId, GaugeRewards>,
    /// `isGauge[gauge]`.
    pub is_gauge: BTreeMap<GaugeId, bool>,
    /// `isAlive[gauge]`.
    pub is_alive: BTreeMap<GaugeId, bool>,
    /// `weights[pool]`.
    pub weights: BTreeMap<PoolId, U256>,
    /// `votes[tokenId][pool]`. A missing entry is zero.
    pub votes: BTreeMap<(TokenId, PoolId), U256>,
    /// `poolVote[tokenId]`: the pools voted for, in order.
    pub pool_vote: BTreeMap<TokenId, Vec<PoolId>>,
    /// `usedWeights[tokenId]`.
    pub used_weights: BTreeMap<TokenId, U256>,
    /// `lastVoted[tokenId]`.
    pub last_voted: BTreeMap<TokenId, u64>,
    /// `isWhitelistedToken`: the set of whitelisted tokens.
    pub is_whitelisted_token: BTreeSet<RewardToken>,
    /// `isWhitelistedNFT`: the set of whitelisted token ids.
    pub is_whitelisted_nft: BTreeSet<TokenId>,
    /// `index`: the global accumulated distribution per unit of weight, scaled by `1e18`.
    pub index: U256,
    /// `supplyIndex[gauge]`.
    pub supply_index: BTreeMap<GaugeId, U256>,
    /// `claimable[gauge]`.
    pub claimable: BTreeMap<GaugeId, U256>,
    /// The number of gauges created so far. The next gauge id is `gauge_count + 1`.
    pub gauge_count: u64,
}

fn one_e18() -> U256 {
    U256::from(ONE_E18)
}

impl Voter {
    /// Port of the `Voter` constructor. `owner` is the governor, minter, epoch governor and emergency council
    /// until `initialize` and `set_emergency_council` change them. `address` is this contract's own address.
    pub fn new(address: AccountId, owner: AccountId) -> Self {
        Self {
            address,
            minter: owner,
            governor: owner,
            epoch_governor: owner,
            emergency_council: owner,
            total_weight: U256::ZERO,
            max_voting_num: U256::from(DEFAULT_MAX_VOTING_NUM),
            pools: Vec::new(),
            gauges: BTreeMap::new(),
            pool_for_gauge: BTreeMap::new(),
            gauge_rewards: BTreeMap::new(),
            is_gauge: BTreeMap::new(),
            is_alive: BTreeMap::new(),
            weights: BTreeMap::new(),
            votes: BTreeMap::new(),
            pool_vote: BTreeMap::new(),
            used_weights: BTreeMap::new(),
            last_voted: BTreeMap::new(),
            is_whitelisted_token: BTreeSet::new(),
            is_whitelisted_nft: BTreeSet::new(),
            index: U256::ZERO,
            supply_index: BTreeMap::new(),
            claimable: BTreeMap::new(),
            gauge_count: 0,
        }
    }

    // ---- STORAGE HELPERS ----

    /// `gauges[pool]`, zero when the pool has no gauge.
    pub fn gauge_of(&self, pool: PoolId) -> GaugeId {
        self.gauges.get(&pool).copied().unwrap_or(0)
    }

    /// `weights[pool]`.
    pub fn weight_of(&self, pool: PoolId) -> U256 {
        self.weights.get(&pool).copied().unwrap_or(U256::ZERO)
    }

    /// `votes[tokenId][pool]`.
    pub fn votes_of(&self, token: TokenId, pool: PoolId) -> U256 {
        self.votes
            .get(&(token, pool))
            .copied()
            .unwrap_or(U256::ZERO)
    }

    /// `usedWeights[tokenId]`.
    pub fn used_weight_of(&self, token: TokenId) -> U256 {
        self.used_weights.get(&token).copied().unwrap_or(U256::ZERO)
    }

    /// `lastVoted[tokenId]`.
    pub fn last_voted_of(&self, token: TokenId) -> u64 {
        self.last_voted.get(&token).copied().unwrap_or(0)
    }

    /// `isAlive[gauge]`.
    pub fn alive(&self, gauge: GaugeId) -> bool {
        self.is_alive.get(&gauge).copied().unwrap_or(false)
    }

    /// `claimable[gauge]`.
    pub fn claimable_of(&self, gauge: GaugeId) -> U256 {
        self.claimable.get(&gauge).copied().unwrap_or(U256::ZERO)
    }

    /// `supplyIndex[gauge]`.
    pub fn supply_index_of(&self, gauge: GaugeId) -> U256 {
        self.supply_index.get(&gauge).copied().unwrap_or(U256::ZERO)
    }

    /// `isWhitelistedToken(token)`.
    pub fn whitelisted_token(&self, token: &str) -> bool {
        self.is_whitelisted_token.contains(token)
    }

    /// `isWhitelistedNFT(tokenId)`.
    pub fn whitelisted_nft(&self, token: TokenId) -> bool {
        self.is_whitelisted_nft.contains(&token)
    }

    /// `pools.length`.
    pub fn length(&self) -> usize {
        self.pools.len()
    }

    /// The reward contract of `gauge`, `fees` when `is_fees`, otherwise `bribe`.
    pub fn reward(&self, gauge: GaugeId, is_fees: bool) -> Option<&Reward> {
        self.gauge_rewards
            .get(&gauge)
            .map(|r| if is_fees { &r.fees } else { &r.bribe })
    }

    // ---- ADMIN ----

    /// `initialize(_tokens, _minter)`: only the current minter may call it. Whitelists the tokens and sets the
    /// minter.
    pub fn initialize(
        &mut self,
        sender: AccountId,
        tokens: &[RewardToken],
        minter: AccountId,
    ) -> Result<()> {
        if sender != self.minter {
            return Err(MathError::NotMinter);
        }
        for token in tokens {
            self.is_whitelisted_token.insert(token.clone());
        }
        self.minter = minter;
        Ok(())
    }

    /// `setEmergencyCouncil(_council)`.
    pub fn set_emergency_council(&mut self, sender: AccountId, council: AccountId) -> Result<()> {
        if sender != self.emergency_council {
            return Err(MathError::NotEmergencyCouncil);
        }
        if council == ZERO_ACCOUNT {
            return Err(MathError::ZeroAddress);
        }
        self.emergency_council = council;
        Ok(())
    }

    /// `setMaxVotingNum(_maxVotingNum)`.
    pub fn set_max_voting_num(&mut self, sender: AccountId, value: U256) -> Result<()> {
        if sender != self.governor {
            return Err(MathError::NotGovernor);
        }
        if value < U256::from(MIN_MAXVOTINGNUM) {
            return Err(MathError::MaximumVotingNumberTooLow);
        }
        if value == self.max_voting_num {
            return Err(MathError::SameValue);
        }
        self.max_voting_num = value;
        Ok(())
    }

    /// `whitelistToken(_token, _bool)`.
    pub fn whitelist_token(&mut self, sender: AccountId, token: &str, flag: bool) -> Result<()> {
        if sender != self.governor {
            return Err(MathError::NotGovernor);
        }
        if flag {
            self.is_whitelisted_token.insert(token.to_owned());
        } else {
            self.is_whitelisted_token.remove(token);
        }
        Ok(())
    }

    /// `whitelistNFT(_tokenId, _bool)`.
    pub fn whitelist_nft(&mut self, sender: AccountId, token: TokenId, flag: bool) -> Result<()> {
        if sender != self.governor {
            return Err(MathError::NotGovernor);
        }
        if flag {
            self.is_whitelisted_nft.insert(token);
        } else {
            self.is_whitelisted_nft.remove(&token);
        }
        Ok(())
    }

    // ---- GAUGES ----

    /// `createGauge(_poolFactory, _pool)`. The caller resolves the factory checks and the pool's facts:
    /// `pool_factory_approved` is `isPoolFactoryApproved(_poolFactory)`, and `pool_tokens` is `Some((token0,
    /// token1))` when `IPoolFactory(_poolFactory).isPool(_pool)` is true and `None` otherwise. The new gauge id is
    /// returned. Its fees and bribe rewards are created with the pool's tokens (`""` when not a pool).
    pub fn create_gauge(
        &mut self,
        sender: AccountId,
        pool_factory_approved: bool,
        pool: PoolId,
        pool_tokens: Option<(RewardToken, RewardToken)>,
    ) -> Result<GaugeId> {
        if !pool_factory_approved {
            return Err(MathError::FactoryPathNotApproved);
        }
        if self.gauge_of(pool) != 0 {
            return Err(MathError::GaugeExists);
        }
        let is_pool = pool_tokens.is_some();
        let (token0, token1) = pool_tokens.unwrap_or_default();
        if sender != self.governor {
            if !is_pool {
                return Err(MathError::NotAPool);
            }
            if !self.whitelisted_token(&token0) || !self.whitelisted_token(&token1) {
                return Err(MathError::NotWhitelistedToken);
            }
        }
        let rewards = [token0, token1];
        self.gauge_count += 1;
        let gauge = self.gauge_count;
        self.gauge_rewards.insert(
            gauge,
            GaugeRewards {
                fees: Reward::new(&rewards),
                bribe: Reward::new(&rewards),
            },
        );
        self.is_gauge.insert(gauge, true);
        self.is_alive.insert(gauge, true);
        self.gauges.insert(pool, gauge);
        self.pool_for_gauge.insert(gauge, pool);
        // `_updateFor` on a gauge with no weight only sets its supply index.
        self.update_for(gauge)?;
        self.pools.push(pool);
        Ok(gauge)
    }

    /// `killGauge(_gauge)`: emergency council only. The gauge's unpaid `claimable` goes back to the minter. The
    /// returned amount is what is sent to the minter.
    pub fn kill_gauge(&mut self, sender: AccountId, gauge: GaugeId) -> Result<U256> {
        if sender != self.emergency_council {
            return Err(MathError::NotEmergencyCouncil);
        }
        if !self.alive(gauge) {
            return Err(MathError::GaugeAlreadyKilled);
        }
        let mut to_minter = U256::ZERO;
        let claimable = self.claimable_of(gauge);
        if !claimable.is_zero() {
            to_minter = claimable;
            self.claimable.remove(&gauge);
        }
        self.is_alive.insert(gauge, false);
        Ok(to_minter)
    }

    /// `reviveGauge(_gauge)`: emergency council only.
    pub fn revive_gauge(&mut self, sender: AccountId, gauge: GaugeId) -> Result<()> {
        if sender != self.emergency_council {
            return Err(MathError::NotEmergencyCouncil);
        }
        if self.alive(gauge) {
            return Err(MathError::GaugeAlreadyRevived);
        }
        self.is_alive.insert(gauge, true);
        Ok(())
    }

    // ---- EMISSIONS ----

    /// `notifyRewardAmount(_amount)`: only the minter may call it. `amount` is the WIND the minter has already
    /// transferred in. The index grows by `amount * 1e18 / max(totalWeight, 1)`.
    pub fn notify_reward_amount(&mut self, sender: AccountId, amount: U256) -> Result<()> {
        if sender != self.minter {
            return Err(MathError::NotMinter);
        }
        let scaled = amount.checked_mul(one_e18()).ok_or(MathError::Overflow)?;
        let ratio = scaled / self.total_weight.max(U256::from(1));
        if !ratio.is_zero() {
            self.index = self.index.checked_add(ratio).ok_or(MathError::Overflow)?;
        }
        Ok(())
    }

    /// `_updateFor(gauge)`. Accrues the gauge's share of the index. A living gauge gets `claimable`. A dead gauge
    /// sends its share back to the minter, and the returned amount is that share.
    pub fn update_for(&mut self, gauge: GaugeId) -> Result<U256> {
        let pool = self.pool_for_gauge.get(&gauge).copied().unwrap_or(0);
        let supplied = self.weight_of(pool);
        if supplied.is_zero() {
            // New gauges and gauges with no weight are set to the current global state.
            self.supply_index.insert(gauge, self.index);
            return Ok(U256::ZERO);
        }
        let supply_index = self.supply_index_of(gauge);
        let index = self.index;
        self.supply_index.insert(gauge, index);
        let delta = index
            .checked_sub(supply_index)
            .ok_or(MathError::Underflow)?;
        if delta.is_zero() {
            return Ok(U256::ZERO);
        }
        let share = supplied.checked_mul(delta).ok_or(MathError::Overflow)? / one_e18();
        if self.alive(gauge) {
            let claimable = self
                .claimable_of(gauge)
                .checked_add(share)
                .ok_or(MathError::Overflow)?;
            self.claimable.insert(gauge, claimable);
            Ok(U256::ZERO)
        } else {
            Ok(share)
        }
    }

    /// `updateFor(address[])`. Returns the total sent to the minter.
    pub fn update_for_gauges(&mut self, gauges: &[GaugeId]) -> Result<U256> {
        let mut to_minter = U256::ZERO;
        for gauge in gauges {
            to_minter = to_minter
                .checked_add(self.update_for(*gauge)?)
                .ok_or(MathError::Overflow)?;
        }
        Ok(to_minter)
    }

    /// `updateFor(start, end)`: over `pools[start..end]`. Returns the total sent to the minter.
    pub fn update_for_range(&mut self, start: u64, end: u64) -> Result<U256> {
        let mut to_minter = U256::ZERO;
        for x in start..end {
            let pool = *self
                .pools
                .get(usize::try_from(x).map_err(|_| MathError::IndexOutOfBounds)?)
                .ok_or(MathError::IndexOutOfBounds)?;
            to_minter = to_minter
                .checked_add(self.update_for(self.gauge_of(pool))?)
                .ok_or(MathError::Overflow)?;
        }
        Ok(to_minter)
    }

    /// `_distribute(gauge)`. `left` is the gauge's `left()`. `notify_ok` is whether its `notifyRewardAmount`
    /// succeeds. When it succeeds, `claimable` is paid to the gauge and cleared. When it fails, `claimable` is
    /// restored, as in the `catch` branch. Returns the total sent to the minter by `_updateFor`.
    pub fn distribute_gauge(
        &mut self,
        gauge: GaugeId,
        left: U256,
        notify_ok: bool,
    ) -> Result<U256> {
        let to_minter = self.update_for(gauge)?;
        let claimable = self.claimable_of(gauge);
        if claimable > left && claimable > U256::from(DURATION) {
            // The `claimable` cleared here is what the gauge receives. The caller does the transfer.
            if notify_ok {
                self.claimable.remove(&gauge);
            } else {
                self.claimable.insert(gauge, claimable);
            }
        }
        Ok(to_minter)
    }

    /// `distribute(_start, _finish)`: distributes `pools[start..finish]`. `gauge_state(gauge)` returns the
    /// `(left, notify_ok)` of the gauge's mock. Returns the total sent to the minter.
    pub fn distribute_range(
        &mut self,
        start: u64,
        finish: u64,
        gauge_state: impl Fn(GaugeId) -> (U256, bool),
    ) -> Result<U256> {
        let mut to_minter = U256::ZERO;
        for x in start..finish {
            let pool = *self
                .pools
                .get(usize::try_from(x).map_err(|_| MathError::IndexOutOfBounds)?)
                .ok_or(MathError::IndexOutOfBounds)?;
            let gauge = self.gauge_of(pool);
            let (left, ok) = gauge_state(gauge);
            to_minter = to_minter
                .checked_add(self.distribute_gauge(gauge, left, ok)?)
                .ok_or(MathError::Overflow)?;
        }
        Ok(to_minter)
    }

    /// `distribute(address[])` and `distributeForGauge` (a single gauge). Returns the total sent to the minter.
    pub fn distribute_gauges(
        &mut self,
        gauges: &[GaugeId],
        gauge_state: impl Fn(GaugeId) -> (U256, bool),
    ) -> Result<U256> {
        let mut to_minter = U256::ZERO;
        for gauge in gauges {
            let (left, ok) = gauge_state(*gauge);
            to_minter = to_minter
                .checked_add(self.distribute_gauge(*gauge, left, ok)?)
                .ok_or(MathError::Overflow)?;
        }
        Ok(to_minter)
    }

    // ---- VOTING ----

    /// `onlyNewEpoch(_tokenId)`: the token has not voted or deposited in this epoch, and the time is past the
    /// first hour of the epoch.
    fn check_new_epoch(&self, token: TokenId, now: u64) -> Result<()> {
        if epoch_start(now) <= self.last_voted_of(token) {
            return Err(MathError::AlreadyVotedOrDeposited);
        }
        if now <= epoch_vote_start(now)? {
            return Err(MathError::DistributeWindow);
        }
        Ok(())
    }

    /// `_reset(tokenId)`: removes every vote of the token, and clears the voting flag in the escrow.
    fn reset_inner(&mut self, ve: &mut VotingEscrow, token: TokenId, now: u64) -> Result<()> {
        let list = self.pool_vote.get(&token).cloned().unwrap_or_default();
        let mut total = U256::ZERO;
        for pool in list {
            let votes = self.votes_of(token, pool);
            if votes.is_zero() {
                continue;
            }
            let gauge = self.gauge_of(pool);
            // The return value is the minter transfer of a dead gauge. Solidity sends it here and ignores it.
            self.update_for(gauge)?;
            let weight = self
                .weight_of(pool)
                .checked_sub(votes)
                .ok_or(MathError::Underflow)?;
            self.weights.insert(pool, weight);
            self.votes.remove(&(token, pool));
            let rewards = self
                .gauge_rewards
                .get_mut(&gauge)
                .ok_or(MathError::InvalidInput)?;
            rewards.fees.withdraw(votes, token, now, true)?;
            rewards.bribe.withdraw(votes, token, now, true)?;
            total = total.checked_add(votes).ok_or(MathError::Overflow)?;
        }
        ve.voting(self.address, token, false)?;
        self.total_weight = self
            .total_weight
            .checked_sub(total)
            .ok_or(MathError::Underflow)?;
        self.used_weights.insert(token, U256::ZERO);
        self.pool_vote.remove(&token);
        Ok(())
    }

    /// `_vote(tokenId, weight, pools, weights)`: resets the token, then votes `weight` split by `weights`.
    fn vote_inner(
        &mut self,
        ve: &mut VotingEscrow,
        token: TokenId,
        weight: U256,
        now: u64,
        pools: &[PoolId],
        wts: &[U256],
    ) -> Result<()> {
        self.reset_inner(ve, token, now)?;
        let mut total_vote_weight = U256::ZERO;
        for w in wts {
            total_vote_weight = total_vote_weight
                .checked_add(*w)
                .ok_or(MathError::Overflow)?;
        }
        let mut total_added = U256::ZERO;
        let mut used = U256::ZERO;
        for (i, pool) in pools.iter().enumerate() {
            let pool = *pool;
            let gauge = self.gauge_of(pool);
            if gauge == 0 {
                return Err(MathError::GaugeDoesNotExist);
            }
            if !self.alive(gauge) {
                return Err(MathError::GaugeNotAlive);
            }
            if self.is_gauge.get(&gauge).copied().unwrap_or(false) {
                let product = wts[i].checked_mul(weight).ok_or(MathError::Overflow)?;
                if total_vote_weight.is_zero() {
                    return Err(MathError::DivisionByZero);
                }
                let pool_weight = product / total_vote_weight;
                if !self.votes_of(token, pool).is_zero() {
                    return Err(MathError::NonZeroVotes);
                }
                if pool_weight.is_zero() {
                    return Err(MathError::ZeroBalance);
                }
                self.update_for(gauge)?;
                self.pool_vote.entry(token).or_default().push(pool);
                let weight_after = self
                    .weight_of(pool)
                    .checked_add(pool_weight)
                    .ok_or(MathError::Overflow)?;
                self.weights.insert(pool, weight_after);
                let votes_after = self
                    .votes_of(token, pool)
                    .checked_add(pool_weight)
                    .ok_or(MathError::Overflow)?;
                self.votes.insert((token, pool), votes_after);
                let rewards = self
                    .gauge_rewards
                    .get_mut(&gauge)
                    .ok_or(MathError::InvalidInput)?;
                rewards.fees.deposit(pool_weight, token, now, true)?;
                rewards.bribe.deposit(pool_weight, token, now, true)?;
                used = used.checked_add(pool_weight).ok_or(MathError::Overflow)?;
                total_added = total_added
                    .checked_add(pool_weight)
                    .ok_or(MathError::Overflow)?;
            }
        }
        if !used.is_zero() {
            ve.voting(self.address, token, true)?;
        }
        self.total_weight = self
            .total_weight
            .checked_add(total_added)
            .ok_or(MathError::Overflow)?;
        self.used_weights.insert(token, used);
        Ok(())
    }

    // Eight parameters mirror the Solidity signature plus its environment (`block`) and the escrow.
    #[allow(clippy::too_many_arguments)]
    /// `vote(_tokenId, _poolVote, _weights)`. `sender` must be the veNFT owner. `wts` must have the same length as
    /// `pools`. `block` is `block.number`, read by `balanceOfNFT`. Returns nothing: the votes are the state.
    pub fn vote(
        &mut self,
        ve: &mut VotingEscrow,
        sender: AccountId,
        token: TokenId,
        pools: &[PoolId],
        wts: &[U256],
        now: u64,
        block: u64,
    ) -> Result<()> {
        self.check_new_epoch(token, now)?;
        if ve.owner(token) != sender {
            return Err(MathError::NotApprovedOrOwner);
        }
        if pools.len() != wts.len() {
            return Err(MathError::UnequalLengths);
        }
        if U256::from(pools.len()) > self.max_voting_num {
            return Err(MathError::TooManyPools);
        }
        if now > epoch_vote_end(now)? && !self.whitelisted_nft(token) {
            return Err(MathError::NotWhitelistedNft);
        }
        self.last_voted.insert(token, now);
        let weight = ve.balance_of_nft(now, block, token)?;
        self.vote_inner(ve, token, weight, now, pools, wts)
    }

    /// `reset(_tokenId)`. `sender` must be the veNFT owner.
    pub fn reset(
        &mut self,
        ve: &mut VotingEscrow,
        sender: AccountId,
        token: TokenId,
        now: u64,
    ) -> Result<()> {
        self.check_new_epoch(token, now)?;
        if ve.owner(token) != sender {
            return Err(MathError::NotApprovedOrOwner);
        }
        self.reset_inner(ve, token, now)
    }

    /// `poke(_tokenId)`. Re-votes the token's current pools with its current voting power. Anyone may call it.
    /// It is blocked in the first hour of the epoch, and it does not set `lastVoted`.
    pub fn poke(
        &mut self,
        ve: &mut VotingEscrow,
        token: TokenId,
        now: u64,
        block: u64,
    ) -> Result<()> {
        if now <= epoch_vote_start(now)? {
            return Err(MathError::DistributeWindow);
        }
        let weight = ve.balance_of_nft(now, block, token)?;
        let pools = self.pool_vote.get(&token).cloned().unwrap_or_default();
        let wts: Vec<U256> = pools.iter().map(|p| self.votes_of(token, *p)).collect();
        self.vote_inner(ve, token, weight, now, &pools, &wts)
    }

    // ---- CLAIMS ----

    /// `claimBribes(_bribes, _tokens, _tokenId)` (`is_fees == false`) and `claimFees(...)` (`is_fees == true`).
    /// `sender` must be the veNFT owner. Each entry is `(gauge, reward tokens)`. The reward contract of that
    /// gauge pays the owner. Returns the total paid per token.
    pub fn claim(
        &mut self,
        ve: &VotingEscrow,
        sender: AccountId,
        token: TokenId,
        now: u64,
        is_fees: bool,
        entries: &[(GaugeId, Vec<RewardToken>)],
    ) -> Result<BTreeMap<RewardToken, U256>> {
        if ve.owner(token) != sender {
            return Err(MathError::NotApprovedOrOwner);
        }
        let mut paid: BTreeMap<RewardToken, U256> = BTreeMap::new();
        for (gauge, tokens) in entries {
            let rewards = self
                .gauge_rewards
                .get_mut(gauge)
                .ok_or(MathError::InvalidInput)?;
            // The voter is the caller of the reward, so `sender_is_voter` is true.
            let reward = if is_fees {
                &mut rewards.fees
            } else {
                &mut rewards.bribe
            };
            for (name, amount) in reward.get_reward(token, tokens, now, false, true)? {
                let total = paid.get(&name).copied().unwrap_or(U256::ZERO);
                paid.insert(name, total.checked_add(amount).ok_or(MathError::Overflow)?);
            }
        }
        Ok(paid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(n: u64) -> AccountId {
        let mut a = [0u8; 32];
        a[24..].copy_from_slice(&n.to_be_bytes());
        a
    }

    #[test]
    fn notify_then_update_pays_gauge_claimable() {
        let mut ve = VotingEscrow::new(acct(777), 1_780_000_000, 1);
        let mut v = Voter::new(acct(1), acct(2));
        v.initialize(acct(2), &["t0".to_owned(), "t1".to_owned()], acct(3))
            .unwrap();
        let g = v
            .create_gauge(acct(2), true, 1, Some(("t0".to_owned(), "t1".to_owned())))
            .unwrap();
        assert_eq!(g, 1);
        let t = 1_780_000_000u64;
        ve.voter = v.address;
        let lock = ve
            .create_lock(
                acct(0x1001),
                t,
                1,
                U256::from(10u64).pow(U256::from(18u64)),
                2 * 7 * 86_400,
            )
            .unwrap()
            .0;
        let next = epoch_start(t) + 7 * 86_400 + 2 * 3_600;
        v.notify_reward_amount(acct(3), U256::from(1_000_000u64))
            .unwrap();
        assert_eq!(v.claimable_of(g), U256::ZERO);
        v.vote(
            &mut ve,
            acct(0x1001),
            lock,
            &[1],
            &[U256::from(5u64)],
            next,
            2,
        )
        .unwrap();
        assert_eq!(v.used_weight_of(lock), v.votes_of(lock, 1));
        assert_eq!(v.gauge_count, 1);
    }

    #[test]
    fn errors_leave_callers_to_discard() {
        let mut v = Voter::new(acct(1), acct(2));
        assert_eq!(
            v.notify_reward_amount(acct(9), U256::from(1u64)),
            Err(MathError::NotMinter)
        );
        assert_eq!(
            v.set_max_voting_num(acct(2), U256::from(5u64)),
            Err(MathError::MaximumVotingNumberTooLow)
        );
        assert_eq!(
            v.kill_gauge(acct(9), 1),
            Err(MathError::NotEmergencyCouncil)
        );
    }
}
