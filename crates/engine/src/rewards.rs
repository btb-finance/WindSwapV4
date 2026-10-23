//! Port of the windv2 voting rewards: `Reward.sol` (base accounting), `VotingReward.sol`,
//! `FeesVotingReward.sol`, `BribeVotingReward.sol` and the epoch helpers of `ProtocolTimeLibrary.sol`.
//!
//! Differences from the Solidity contracts, all outside the reward accounting:
//! - No token transfers. `notify_*` takes the amount the caller has already pulled
//!   (`safeTransferFrom`), and `get_reward` returns the amounts the caller must pay to the veNFT
//!   owner (`safeTransfer`). A failed transfer is the caller's revert to handle.
//! - `block.timestamp` is passed in as `now`.
//! - Authorization is the caller's job. Each external check becomes a boolean parameter:
//!   `deposit`/`withdraw`: `msg.sender == voter`; `get_reward`: `ve.isApprovedOrOwner(sender, tokenId)`
//!   and `sender == voter`; `notify_fees_reward`: `voter.gaugeToFees(sender) == this`;
//!   `notify_bribe_reward`: `voter.isWhitelistedToken(token)`.
//! - Reward tokens are `RewardToken = String` (the Solidity token address). The zero address is `""`
//!   and is skipped in the constructor, as `VotingReward` does. veNFT token ids are `u64`.
//! - `nonReentrant` and the ERC2771 forwarder are not modeled: a call into this module cannot re-enter.
//! - No events. `numCheckpoints` and `supplyNumCheckpoints` are the lengths of the checkpoint vectors.
//! - Atomicity: as with a Solidity revert, an `Err` leaves the state unchanged.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{MathError, Result};
use crate::U256;

/// Reward token identifier: the Solidity token address (`""` is the zero address).
pub type RewardToken = String;

/// `ProtocolTimeLibrary.WEEK` (`1 weeks`).
pub const WEEK: u64 = 7 * 24 * 60 * 60;
/// `Reward.DURATION` (`7 days`), the length of one reward epoch.
pub const DURATION: u64 = WEEK;
/// One hour, as used by the voting window helpers.
const HOUR: u64 = 60 * 60;

/// `ProtocolTimeLibrary.epochStart`: start of the week containing `timestamp`.
pub fn epoch_start(timestamp: u64) -> u64 {
    timestamp - timestamp % WEEK
}

/// `ProtocolTimeLibrary.epochNext`: start of the next week (end of the current one).
pub fn epoch_next(timestamp: u64) -> Result<u64> {
    epoch_start(timestamp)
        .checked_add(WEEK)
        .ok_or(MathError::Overflow)
}

/// `ProtocolTimeLibrary.epochVoteStart`: `epochStart + 1 hours`.
pub fn epoch_vote_start(timestamp: u64) -> Result<u64> {
    epoch_start(timestamp)
        .checked_add(HOUR)
        .ok_or(MathError::Overflow)
}

/// `ProtocolTimeLibrary.epochVoteEnd`: `epochStart + WEEK - 1 hours`.
pub fn epoch_vote_end(timestamp: u64) -> Result<u64> {
    epoch_start(timestamp)
        .checked_add(WEEK - HOUR)
        .ok_or(MathError::Overflow)
}

/// `Reward.Checkpoint`: a token's balance from `timestamp` on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Checkpoint {
    pub timestamp: u64,
    pub balance_of: U256,
}

/// `Reward.SupplyCheckpoint`: the total supply from `timestamp` on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SupplyCheckpoint {
    pub timestamp: u64,
    pub supply: U256,
}

/// State of `Reward.sol`. Fields mirror the public Solidity state variables.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reward {
    /// `totalSupply`: sum of all deposits.
    pub total_supply: U256,
    /// `balanceOf[tokenId]`. Missing entries are zero.
    pub balance_of: BTreeMap<u64, U256>,
    /// `tokenRewardsPerEpoch[token][epochStart]`. Missing entries are zero.
    pub token_rewards_per_epoch: BTreeMap<(RewardToken, u64), U256>,
    /// `lastEarn[token][tokenId]`. Missing entries are zero.
    pub last_earn: BTreeMap<(RewardToken, u64), u64>,
    /// `rewards`: reward token list in insertion order. Duplicates are kept, as in the constructor.
    pub rewards: Vec<RewardToken>,
    /// `isReward`.
    pub is_reward: BTreeSet<RewardToken>,
    /// `checkpoints[tokenId]`. `numCheckpoints[tokenId]` is the length.
    pub checkpoints: BTreeMap<u64, Vec<Checkpoint>>,
    /// `supplyCheckpoints`. `supplyNumCheckpoints` is the length.
    pub supply_checkpoints: Vec<SupplyCheckpoint>,
}

/// `getPriorBalanceIndex` / `getPriorSupplyIndex`: index of the last checkpoint at or before
/// `timestamp` among `n` checkpoints sorted by timestamp, `0` when none is at or before it.
pub(crate) fn prior_index(n: usize, ts_at: impl Fn(usize) -> u64, timestamp: u64) -> usize {
    if n == 0 {
        return 0;
    }
    // Most recent checkpoint first.
    if ts_at(n - 1) <= timestamp {
        return n - 1;
    }
    // Before the first checkpoint: implicit zero balance, index 0.
    if ts_at(0) > timestamp {
        return 0;
    }
    let (mut lower, mut upper) = (0usize, n - 1);
    while upper > lower {
        // ceil midpoint, as in the Solidity
        let center = upper - (upper - lower) / 2;
        let cp_ts = ts_at(center);
        if cp_ts == timestamp {
            return center;
        } else if cp_ts < timestamp {
            lower = center;
        } else {
            upper = center - 1;
        }
    }
    lower
}

impl Reward {
    /// Constructor of `VotingReward`: the initial reward list. The zero address (`""`) is skipped.
    pub fn new(initial_rewards: &[RewardToken]) -> Self {
        let mut r = Self::default();
        for token in initial_rewards.iter().filter(|t| !t.is_empty()) {
            r.is_reward.insert(token.clone());
            r.rewards.push(token.clone());
        }
        r
    }

    /// `balanceOf(tokenId)`.
    pub fn balance_of(&self, token_id: u64) -> U256 {
        self.balance_of
            .get(&token_id)
            .copied()
            .unwrap_or(U256::ZERO)
    }

    /// `numCheckpoints(tokenId)`.
    pub fn num_checkpoints(&self, token_id: u64) -> usize {
        self.checkpoints.get(&token_id).map_or(0, Vec::len)
    }

    /// `supplyNumCheckpoints()`.
    pub fn supply_num_checkpoints(&self) -> usize {
        self.supply_checkpoints.len()
    }

    /// `rewardsListLength()`.
    pub fn rewards_list_length(&self) -> usize {
        self.rewards.len()
    }

    /// `isReward(token)`.
    pub fn is_reward(&self, token: &str) -> bool {
        self.is_reward.contains(token)
    }

    /// `lastEarn[token][tokenId]`.
    pub fn last_earn(&self, token: &str, token_id: u64) -> u64 {
        self.last_earn
            .get(&(token.to_owned(), token_id))
            .copied()
            .unwrap_or(0)
    }

    /// `tokenRewardsPerEpoch[token][epoch]`. `epoch` must already be an epoch start.
    pub fn token_rewards_per_epoch(&self, token: &str, epoch: u64) -> U256 {
        self.token_rewards_per_epoch
            .get(&(token.to_owned(), epoch))
            .copied()
            .unwrap_or(U256::ZERO)
    }

    /// `getPriorBalanceIndex(tokenId, timestamp)`.
    pub fn get_prior_balance_index(&self, token_id: u64, timestamp: u64) -> usize {
        let cps: &[Checkpoint] = self.checkpoints.get(&token_id).map_or(&[], Vec::as_slice);
        prior_index(cps.len(), |i| cps[i].timestamp, timestamp)
    }

    /// `getPriorSupplyIndex(timestamp)`.
    pub fn get_prior_supply_index(&self, timestamp: u64) -> usize {
        prior_index(
            self.supply_checkpoints.len(),
            |i| self.supply_checkpoints[i].timestamp,
            timestamp,
        )
    }

    fn balance_checkpoint(&self, token_id: u64, index: usize) -> Checkpoint {
        self.checkpoints
            .get(&token_id)
            .and_then(|cps| cps.get(index))
            .copied()
            .unwrap_or_default()
    }

    fn supply_checkpoint(&self, index: usize) -> SupplyCheckpoint {
        self.supply_checkpoints
            .get(index)
            .copied()
            .unwrap_or_default()
    }

    /// `_writeCheckpoint`: overwrite the last checkpoint if it is in the same epoch, else append.
    fn write_checkpoint(&mut self, token_id: u64, balance: U256, now: u64) {
        let cps = self.checkpoints.entry(token_id).or_default();
        let cp = Checkpoint {
            timestamp: now,
            balance_of: balance,
        };
        let same_epoch = cps
            .last()
            .is_some_and(|last| epoch_start(last.timestamp) == epoch_start(now));
        if same_epoch {
            if let Some(last) = cps.last_mut() {
                *last = cp;
            }
        } else {
            cps.push(cp);
        }
    }

    /// `_writeSupplyCheckpoint`: same rule as `write_checkpoint`, for the total supply.
    fn write_supply_checkpoint(&mut self, supply: U256, now: u64) {
        let cp = SupplyCheckpoint {
            timestamp: now,
            supply,
        };
        let same_epoch = self
            .supply_checkpoints
            .last()
            .is_some_and(|last| epoch_start(last.timestamp) == epoch_start(now));
        if same_epoch {
            if let Some(last) = self.supply_checkpoints.last_mut() {
                *last = cp;
            }
        } else {
            self.supply_checkpoints.push(cp);
        }
    }

    /// `earned(token, tokenId)` at time `now`.
    pub fn earned(&self, token: &str, token_id: u64, now: u64) -> Result<U256> {
        self.earned_from(token, token_id, now, self.last_earn(token, token_id))
    }

    /// `earned` with `lastEarn[token][tokenId]` supplied by the caller. `get_reward` needs this:
    /// within one `getReward` call, a token listed twice sees the `lastEarn` written by its first entry.
    fn earned_from(&self, token: &str, token_id: u64, now: u64, last_earn: u64) -> Result<U256> {
        if self.num_checkpoints(token_id) == 0 {
            return Ok(U256::ZERO);
        }

        let mut reward = U256::ZERO;
        // Start from the epoch of the last claim.
        let mut curr = epoch_start(last_earn);
        let index = self.get_prior_balance_index(token_id, curr);
        // Accounts for a last claim before the first checkpoint.
        curr = curr.max(epoch_start(
            self.balance_checkpoint(token_id, index).timestamp,
        ));

        let num_epochs = epoch_start(now)
            .checked_sub(curr)
            .ok_or(MathError::Underflow)?
            / DURATION;
        for _ in 0..num_epochs {
            let end = curr.checked_add(DURATION - 1).ok_or(MathError::Overflow)?;
            let balance = self
                .balance_checkpoint(token_id, self.get_prior_balance_index(token_id, end))
                .balance_of;
            let supply = self
                .supply_checkpoint(self.get_prior_supply_index(end))
                .supply
                .max(U256::from(1));
            let per_epoch = self.token_rewards_per_epoch(token, curr);
            let product = balance.checked_mul(per_epoch).ok_or(MathError::Overflow)?;
            reward = reward
                .checked_add(product / supply)
                .ok_or(MathError::Overflow)?;
            curr = curr.checked_add(DURATION).ok_or(MathError::Overflow)?;
        }
        Ok(reward)
    }

    /// `_deposit(amount, tokenId)`, called by the voter.
    pub fn deposit(
        &mut self,
        amount: U256,
        token_id: u64,
        now: u64,
        sender_is_authorized: bool,
    ) -> Result<()> {
        if !sender_is_authorized {
            return Err(MathError::NotAuthorized);
        }
        let total = self
            .total_supply
            .checked_add(amount)
            .ok_or(MathError::Overflow)?;
        let balance = self
            .balance_of(token_id)
            .checked_add(amount)
            .ok_or(MathError::Overflow)?;
        self.total_supply = total;
        self.balance_of.insert(token_id, balance);
        self.write_checkpoint(token_id, balance, now);
        self.write_supply_checkpoint(total, now);
        Ok(())
    }

    /// `_withdraw(amount, tokenId)`, called by the voter.
    pub fn withdraw(
        &mut self,
        amount: U256,
        token_id: u64,
        now: u64,
        sender_is_authorized: bool,
    ) -> Result<()> {
        if !sender_is_authorized {
            return Err(MathError::NotAuthorized);
        }
        let total = self
            .total_supply
            .checked_sub(amount)
            .ok_or(MathError::Underflow)?;
        let balance = self
            .balance_of(token_id)
            .checked_sub(amount)
            .ok_or(MathError::Underflow)?;
        self.total_supply = total;
        self.balance_of.insert(token_id, balance);
        self.write_checkpoint(token_id, balance, now);
        self.write_supply_checkpoint(total, now);
        Ok(())
    }

    /// `VotingReward.getReward(tokenId, tokens)`. Returns `(token, amount)` for every entry of
    /// `tokens`, in order, including zero amounts (the Solidity emits `ClaimRewards` for each).
    /// The caller pays the amounts to the veNFT owner.
    pub fn get_reward(
        &mut self,
        token_id: u64,
        tokens: &[RewardToken],
        now: u64,
        sender_is_approved_or_owner: bool,
        sender_is_voter: bool,
    ) -> Result<Vec<(RewardToken, U256)>> {
        if !sender_is_approved_or_owner && !sender_is_voter {
            return Err(MathError::NotAuthorized);
        }
        self.claim(token_id, tokens, now)
    }

    /// `_getReward(recipient, tokenId, tokens)` without the caller check. The managed reward contracts
    /// call it after their own authorization, and `get_reward` calls it after the voting check.
    pub(crate) fn claim(
        &mut self,
        token_id: u64,
        tokens: &[RewardToken],
        now: u64,
    ) -> Result<Vec<(RewardToken, U256)>> {
        // `_getReward` processes the tokens in order, and each one writes `lastEarn` before the next
        // is read. The writes are collected and applied at the end, so an `Err` changes nothing.
        let mut claims = Vec::with_capacity(tokens.len());
        let mut written: BTreeMap<RewardToken, u64> = BTreeMap::new();
        for token in tokens {
            let last = written
                .get(token)
                .copied()
                .unwrap_or_else(|| self.last_earn(token, token_id));
            let reward = self.earned_from(token, token_id, now, last)?;
            written.insert(token.clone(), now);
            claims.push((token.clone(), reward));
        }
        for (token, when) in written {
            self.last_earn.insert((token, token_id), when);
        }
        Ok(claims)
    }

    /// `FeesVotingReward.notifyRewardAmount(token, amount)`. Only the gauge mapped to this contract
    /// may notify, and only for a token that is already a reward.
    pub fn notify_fees_reward(
        &mut self,
        token: &str,
        amount: U256,
        now: u64,
        sender_is_gauge: bool,
    ) -> Result<()> {
        if !sender_is_gauge {
            return Err(MathError::NotGauge);
        }
        if !self.is_reward(token) {
            return Err(MathError::InvalidReward);
        }
        self.add_epoch_reward(token, amount, now)
    }

    /// `BribeVotingReward.notifyRewardAmount(token, amount)`. A token that is not yet a reward is
    /// added to the list, if the voter whitelists it.
    pub fn notify_bribe_reward(
        &mut self,
        token: &str,
        amount: U256,
        now: u64,
        token_whitelisted: bool,
    ) -> Result<()> {
        if self.is_reward(token) {
            return self.add_epoch_reward(token, amount, now);
        }
        if !token_whitelisted {
            return Err(MathError::NotWhitelisted);
        }
        // Solidity adds the token before `_notifyRewardAmount`, which can still revert (zero amount),
        // so the amount is validated first and the reward list is written only after that.
        let total = self.epoch_total_after(token, amount, now)?;
        self.is_reward.insert(token.to_owned());
        self.rewards.push(token.to_owned());
        self.token_rewards_per_epoch
            .insert((token.to_owned(), epoch_start(now)), total);
        Ok(())
    }

    /// `_notifyRewardAmount`: adds `amount` to `tokenRewardsPerEpoch[token][epochStart(now)]`.
    /// The caller has already pulled the tokens.
    pub(crate) fn add_epoch_reward(&mut self, token: &str, amount: U256, now: u64) -> Result<()> {
        let total = self.epoch_total_after(token, amount, now)?;
        self.token_rewards_per_epoch
            .insert((token.to_owned(), epoch_start(now)), total);
        Ok(())
    }

    fn epoch_total_after(&self, token: &str, amount: U256, now: u64) -> Result<U256> {
        if amount.is_zero() {
            return Err(MathError::ZeroAmount);
        }
        self.token_rewards_per_epoch(token, epoch_start(now))
            .checked_add(amount)
            .ok_or(MathError::Overflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_helpers_match_protocol_time_library() {
        let t = 1_780_000_000u64;
        let start = t - t % WEEK;
        assert_eq!(epoch_start(t), start);
        assert_eq!(epoch_start(start), start);
        assert_eq!(epoch_start(start + WEEK - 1), start);
        assert_eq!(epoch_next(t).unwrap(), start + WEEK);
        assert_eq!(epoch_vote_start(t).unwrap(), start + HOUR);
        assert_eq!(epoch_vote_end(t).unwrap(), start + WEEK - HOUR);
    }

    #[test]
    fn withdraw_error_leaves_state_unchanged() {
        let mut r = Reward::new(&["t0".to_owned()]);
        r.deposit(U256::from(10), 1, 100, true).unwrap();
        let before = r.clone();
        assert_eq!(
            r.withdraw(U256::from(11), 1, 200, true),
            Err(MathError::Underflow)
        );
        assert_eq!(r, before);
    }

    #[test]
    fn duplicate_token_in_get_reward_pays_once() {
        let mut r = Reward::new(&["t0".to_owned()]);
        let t = 1_780_000_000u64;
        // Sole holder deposits in epoch E, the token is notified in epoch E+1 and claimed in E+2.
        r.deposit(U256::from(100), 1, t, true).unwrap();
        let next = epoch_next(t).unwrap();
        r.notify_fees_reward("t0", U256::from(70), next, true)
            .unwrap();
        let tokens = ["t0".to_owned(), "t0".to_owned()];
        let claims = r
            .get_reward(1, &tokens, next + WEEK + 1, true, false)
            .unwrap();
        assert_eq!(claims[0].1, U256::from(70));
        assert_eq!(claims[1].1, U256::ZERO);
        assert_eq!(r.last_earn("t0", 1), next + WEEK + 1);
    }
}
