//! Port of windv2 `RewardsDistributor.sol` (rebase distribution to veNFT holders).
//!
//! Ported: `checkpointToken` (`_checkpointToken`, the 20-week loop), `claimable` (`_claimable`, the 50-week
//! loop), `claim`, `claimMany` and `setMinter`, together with the state they touch: `startTime`,
//! `timeCursorOf`, `lastTokenTime`, `tokensPerWeek`, `tokenLastBalance` and `minter`.
//!
//! Not ported:
//! - `NotManagedOrNormalNFT`. Every token in [`VotingEscrow`] is `NORMAL`, so the check cannot fire.
//! - The WIND token and its `balanceOf` / `safeTransfer` / `safeApprove`. `checkpoint_token` takes the
//!   distributor's current balance as input. A payout is returned as a [`Payout`] and the caller moves the
//!   balances. `Deposited` means `VotingEscrow.depositFor` pulled the amount from the distributor, and it
//!   has already run on the escrow passed in.
//! - The `Minter` contract. `active_period` is passed in where `IMinter(minter).activePeriod()` would be read.
//!
//! Inputs replace `block.timestamp` (`now`), `block.number` (`block`) and `msg.sender` (`caller`).
//!
//! Atomicity: as with the other modules, an `Err` means the call must be discarded. The Solidity call
//! reverts and the state is unchanged. A caller that needs rollback clones the state before the call.

use std::collections::BTreeMap;

use crate::error::{MathError, Result};
use crate::fees::AccountId;
use crate::ve::{VotingEscrow, WEEK, ZERO_ACCOUNT};
use crate::U256;

/// The loop cap of `_checkpointToken`: at most 20 weeks of tokens are distributed per checkpoint.
pub const CHECKPOINT_WEEK_CAP: usize = 20;
/// The loop cap of `_claimable`: at most 50 weeks are summed per claim.
pub const CLAIM_WEEK_CAP: usize = 50;

/// What a successful claim did with the WIND it computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payout {
    /// The computed amount was zero, so nothing moved.
    None,
    /// `VotingEscrow.depositFor(token, amount)`: the WIND is pulled from the distributor into the lock.
    Deposited { token: u64, amount: U256 },
    /// `IERC20.safeTransfer(owner, amount)`: the lock has ended and is not permanent.
    Transferred { to: AccountId, amount: U256 },
}

impl Payout {
    /// The WIND amount moved out of the distributor (zero for `None`).
    pub fn amount(&self) -> U256 {
        match self {
            Payout::None => U256::ZERO,
            Payout::Deposited { amount, .. } | Payout::Transferred { amount, .. } => *amount,
        }
    }
}

/// State of the `RewardsDistributor`. Field names mirror the Solidity state variables.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "borsh",
    derive(borsh::BorshSerialize, borsh::BorshDeserialize)
)]
pub struct RewardsDistributor {
    /// `minter`: the only caller of `checkpointToken` and `setMinter`, and the source of `activePeriod`.
    pub minter: AccountId,
    /// `startTime`: the week the distributor was created.
    pub start_time: u64,
    /// `timeCursorOf`. A missing entry is zero.
    pub time_cursor_of: BTreeMap<u64, u64>,
    /// `lastTokenTime`: the last time `checkpointToken` ran.
    pub last_token_time: u64,
    /// `tokensPerWeek`, keyed by the week's start timestamp. A missing entry is zero.
    pub tokens_per_week: BTreeMap<u64, U256>,
    /// `tokenLastBalance`: the distributor's WIND balance at the last checkpoint, less claimed amounts.
    pub token_last_balance: U256,
}

fn week_floor(t: u64) -> u64 {
    (t / WEEK) * WEEK
}

/// `uint256` checked add (Solidity panics with `0x11`).
fn add_u(a: U256, b: U256) -> Result<U256> {
    a.checked_add(b).ok_or(MathError::Overflow)
}

/// `uint256` checked sub (Solidity panics with `0x11`).
fn sub_u(a: U256, b: U256) -> Result<U256> {
    a.checked_sub(b).ok_or(MathError::Underflow)
}

/// `uint256` checked mul (Solidity panics with `0x11`).
fn mul_u(a: U256, b: U256) -> Result<U256> {
    a.checked_mul(b).ok_or(MathError::Overflow)
}

/// `uint256` checked div (Solidity panics with `0x12`).
fn div_u(a: U256, b: U256) -> Result<U256> {
    a.checked_div(b).ok_or(MathError::DivisionByZero)
}

impl RewardsDistributor {
    /// Port of the constructor. `startTime` and `lastTokenTime` are the week containing `now`. The
    /// `token` and approval of the escrow are not modelled.
    pub fn new(now: u64, minter: AccountId) -> Self {
        let start = week_floor(now);
        Self {
            minter,
            start_time: start,
            time_cursor_of: BTreeMap::new(),
            last_token_time: start,
            tokens_per_week: BTreeMap::new(),
            token_last_balance: U256::ZERO,
        }
    }

    /// `timeCursorOf[token]`.
    pub fn time_cursor(&self, token: u64) -> u64 {
        self.time_cursor_of.get(&token).copied().unwrap_or(0)
    }

    /// `tokensPerWeek[week]`.
    pub fn tokens_in_week(&self, week: u64) -> U256 {
        self.tokens_per_week
            .get(&week)
            .copied()
            .unwrap_or(U256::ZERO)
    }

    fn add_tokens_in_week(&mut self, week: u64, amount: U256) -> Result<()> {
        let slot = self.tokens_per_week.entry(week).or_default();
        *slot = add_u(*slot, amount)?;
        Ok(())
    }

    /// Port of `setMinter(_minter)`: only the current minter may change it (`NotMinter`).
    pub fn set_minter(&mut self, caller: AccountId, minter: AccountId) -> Result<()> {
        if caller != self.minter {
            return Err(MathError::NotMinter);
        }
        self.minter = minter;
        Ok(())
    }

    /// Port of `checkpointToken()` and `_checkpointToken()`. `caller` must be the minter (`NotMinter`).
    /// `token_balance` is the distributor's WIND balance now (`balanceOf(this)`). The new WIND since the
    /// last checkpoint is spread over the weeks from `lastTokenTime` to `now`, in proportion to time.
    /// At most 20 weeks are filled (the rest is dropped, as in Solidity). Reads `now`.
    pub fn checkpoint_token(
        &mut self,
        caller: AccountId,
        now: u64,
        token_balance: U256,
    ) -> Result<()> {
        if caller != self.minter {
            return Err(MathError::NotMinter);
        }
        let to_distribute = sub_u(token_balance, self.token_last_balance)?;
        self.token_last_balance = token_balance;

        let mut t = self.last_token_time;
        let since_last = now.checked_sub(t).ok_or(MathError::Underflow)?;
        self.last_token_time = now;
        let mut this_week = week_floor(t);
        let timestamp = now;
        let since = U256::from(since_last);

        for _ in 0..CHECKPOINT_WEEK_CAP {
            let next_week = this_week.checked_add(WEEK).ok_or(MathError::Overflow)?;
            if timestamp < next_week {
                let amount = if since_last == 0 && timestamp == t {
                    to_distribute
                } else {
                    let dt = U256::from(timestamp.checked_sub(t).ok_or(MathError::Underflow)?);
                    div_u(mul_u(to_distribute, dt)?, since)?
                };
                self.add_tokens_in_week(this_week, amount)?;
                break;
            } else {
                let amount = if since_last == 0 && next_week == t {
                    to_distribute
                } else {
                    let dt = U256::from(next_week.checked_sub(t).ok_or(MathError::Underflow)?);
                    div_u(mul_u(to_distribute, dt)?, since)?
                };
                self.add_tokens_in_week(this_week, amount)?;
            }
            t = next_week;
            this_week = next_week;
        }
        Ok(())
    }

    /// Port of `_claimable(tokenId, lastTokenTime)`. Returns `(toDistribute, weekCursorStart,
    /// weekCursor)`. The reads of `startTime`, `timeCursorOf`, `tokensPerWeek` and the escrow are
    /// reproduced. The `weekCursor` returned is the clamped / advanced cursor, and `claim` stores it.
    fn claimable_inner(
        &self,
        token: u64,
        last_token_time: u64,
        ve: &VotingEscrow,
    ) -> Result<(U256, u64, u64)> {
        let start_time = self.start_time;
        let mut week_cursor = self.time_cursor(token);
        let mut week_cursor_start = week_cursor;

        // Token does not exist (or has never had a point): nothing to claim.
        let max_user_epoch = ve.user_point_epoch.get(&token).copied().unwrap_or(0);
        if max_user_epoch == 0 {
            return Ok((U256::ZERO, week_cursor_start, week_cursor));
        }

        // Never claimed: start from the week of the first user point.
        if week_cursor == 0 {
            let first = ve
                .user_point_history
                .get(&(token, 1))
                .copied()
                .unwrap_or_default();
            week_cursor = week_floor(first.ts);
            week_cursor_start = week_cursor;
        }
        if week_cursor >= last_token_time {
            return Ok((U256::ZERO, week_cursor_start, week_cursor));
        }
        if week_cursor < start_time {
            week_cursor = start_time;
        }

        let mut to_distribute = U256::ZERO;
        for _ in 0..CLAIM_WEEK_CAP {
            if week_cursor >= last_token_time {
                break;
            }
            let end_of_week = week_cursor
                .checked_add(WEEK)
                .and_then(|w| w.checked_sub(1))
                .ok_or(MathError::Overflow)?;
            let balance = ve.balance_of_nft_at(token, end_of_week)?;
            let mut supply = ve.total_supply_at(end_of_week)?;
            if supply.is_zero() {
                supply = U256::from(1u8);
            }
            let tokens = self.tokens_in_week(week_cursor);
            to_distribute = add_u(to_distribute, div_u(mul_u(balance, tokens)?, supply)?)?;
            week_cursor = week_cursor.checked_add(WEEK).ok_or(MathError::Overflow)?;
        }
        Ok((to_distribute, week_cursor_start, week_cursor))
    }

    /// Port of `claimable(tokenId)`: the amount `claim` would pay now, against the last checkpoint week.
    pub fn claimable(&self, token: u64, ve: &VotingEscrow) -> Result<U256> {
        let last = week_floor(self.last_token_time);
        Ok(self.claimable_inner(token, last, ve)?.0)
    }

    /// Pays `amount` for `token`: `depositFor` when the lock is active or permanent, else a transfer to
    /// the owner. A transfer to the zero owner reverts (`TransferToZeroAddress`).
    fn pay(
        &mut self,
        now: u64,
        block: u64,
        token: u64,
        amount: U256,
        ve: &mut VotingEscrow,
    ) -> Result<Payout> {
        let locked = ve.locked_of(token);
        if now >= locked.end && !locked.is_permanent {
            let to = ve.owner(token);
            if to == ZERO_ACCOUNT {
                return Err(MathError::TransferToZeroAddress);
            }
            Ok(Payout::Transferred { to, amount })
        } else {
            // msg.sender of VotingEscrow.depositFor is the distributor itself
            let distributor = ve.distributor;
            ve.deposit_for(distributor, now, block, token, amount)?;
            Ok(Payout::Deposited { token, amount })
        }
    }

    /// Port of `claim(tokenId)`. Reverts `UpdatePeriod` when `active_period` (the minter's
    /// `activePeriod()`) is behind the current week. Reads `now` and `block`.
    pub fn claim(
        &mut self,
        now: u64,
        block: u64,
        active_period: u64,
        token: u64,
        ve: &mut VotingEscrow,
    ) -> Result<Payout> {
        if active_period < week_floor(now) {
            return Err(MathError::UpdatePeriod);
        }
        let last = week_floor(self.last_token_time);
        let (amount, _start, cursor) = self.claimable_inner(token, last, ve)?;
        self.time_cursor_of.insert(token, cursor);
        if amount.is_zero() {
            return Ok(Payout::None);
        }
        let payout = self.pay(now, block, token, amount, ve)?;
        self.token_last_balance = sub_u(self.token_last_balance, amount)?;
        Ok(payout)
    }

    /// Port of `claimMany(tokenIds)`. Stops at the first zero id (as `break`). Returns one [`Payout`] per
    /// processed id, in order. `tokenLastBalance` is reduced once by the total. Solidity returns `true`.
    pub fn claim_many(
        &mut self,
        now: u64,
        block: u64,
        active_period: u64,
        tokens: &[u64],
        ve: &mut VotingEscrow,
    ) -> Result<Vec<Payout>> {
        if active_period < week_floor(now) {
            return Err(MathError::UpdatePeriod);
        }
        let last = week_floor(self.last_token_time);
        let mut total = U256::ZERO;
        let mut payouts = Vec::new();
        for &token in tokens {
            if token == 0 {
                break;
            }
            let (amount, _start, cursor) = self.claimable_inner(token, last, ve)?;
            self.time_cursor_of.insert(token, cursor);
            if amount.is_zero() {
                payouts.push(Payout::None);
                continue;
            }
            payouts.push(self.pay(now, block, token, amount, ve)?);
            total = add_u(total, amount)?;
        }
        if !total.is_zero() {
            self.token_last_balance = sub_u(self.token_last_balance, total)?;
        }
        Ok(payouts)
    }
}
