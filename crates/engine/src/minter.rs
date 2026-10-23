//! Port of `Minter.sol` (windv2): the weekly emission schedule, tail emissions, rebase growth,
//! team emissions and the epoch governor nudge.
//!
//! Differences from the Solidity contract, all outside the emission math:
//! - No token balances, transfers, mints or external calls. `update_period` takes the values the
//!   Solidity reads (`wind.totalSupply()`, `wind.balanceOf(minter)`, `ve.totalSupplyAt(activePeriod - 1)`)
//!   and returns the amounts the Solidity transfers or mints. The caller moves balances.
//! - `block.timestamp` is passed in as `now`.
//! - Authorization is the caller's job: `setTeam`/`acceptTeam`, `NotTeam` in `setTeamRate`,
//!   and `NotEpochGovernor` in `nudge` (the governor's `result()` is passed in as `ProposalState`).
//! - `initialize` (airdrop mints and veNFT locks) is not ported. Its mints change wind's total supply,
//!   which the caller passes in as an input, but it does not touch the emission state.
//! - Events (`Mint`, `Nudge`, ...) are not emitted. `RewardsDistributor.checkpointToken()` and
//!   `Voter.notifyRewardAmount()` are represented by the returned amounts.
//!
//! Atomicity: as with a Solidity revert, an `Err` leaves the state unchanged. Every state field is
//! written only after all arithmetic has succeeded.

use std::collections::BTreeSet;

use crate::error::{MathError, Result};
use crate::U256;

const E18: u128 = 1_000_000_000_000_000_000;

/// `Minter.WEEK` (`1 weeks`).
pub const WEEK: u64 = 7 * 24 * 60 * 60;
/// `Minter.WEEKLY_DECAY`: weekly emission factor after the growth phase, in bps.
pub const WEEKLY_DECAY: u64 = 9_900;
/// `Minter.WEEKLY_GROWTH`: weekly emission factor during the first 14 epochs, in bps.
pub const WEEKLY_GROWTH: u64 = 10_300;
/// `Minter.MAXIMUM_TAIL_RATE`: cap for `tailEmissionRate` after a nudge.
pub const MAXIMUM_TAIL_RATE: u64 = 100;
/// `Minter.MINIMUM_TAIL_RATE`: floor for `tailEmissionRate` after a nudge.
pub const MINIMUM_TAIL_RATE: u64 = 1;
/// `Minter.MAX_BPS`.
pub const MAX_BPS: u64 = 10_000;
/// `Minter.NUDGE`: step size of one nudge, in bps.
pub const NUDGE: u64 = 1;
/// `Minter.TAIL_START` (`8_969_150 * 1e18`): tail emissions begin once `weekly` drops below this.
pub const TAIL_START: u128 = 8_969_150 * E18;
/// `Minter.MAXIMUM_TEAM_RATE`.
pub const MAXIMUM_TEAM_RATE: u64 = 500;
/// `Minter.teamRate` initial value (team emissions start at 5%).
pub const INITIAL_TEAM_RATE: u64 = 500;
/// `Minter.weekly` initial value (`10_000_000 * 1e18`).
pub const INITIAL_WEEKLY: u128 = 10_000_000 * E18;
/// `Minter.tailEmissionRate` initial value, in bps.
pub const INITIAL_TAIL_EMISSION_RATE: u64 = 67;

/// `IEpochGovernor.ProposalState`, in the same order as the Solidity enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProposalState {
    Pending,
    Active,
    Canceled,
    Defeated,
    Succeeded,
    Queued,
    Expired,
    Executed,
}

/// Amounts produced by one `updatePeriod` that the Solidity moves or mints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "borsh",
    derive(borsh::BorshSerialize, borsh::BorshDeserialize)
)]
pub struct EpochEmission {
    /// `_emission`: approved to and pulled by the voter through `voter.notifyRewardAmount`.
    pub emission: U256,
    /// `_growth`: transferred to the rewards distributor (rebase).
    pub growth: U256,
    /// `_teamEmissions`: transferred to `team`.
    pub team: U256,
    /// Amount minted to the minter before the transfers: `_required - balanceOf` when the minter
    /// balance is short, otherwise `0`.
    pub mint_required: U256,
}

/// State of `Minter.sol`. Fields mirror the public Solidity state variables.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "borsh",
    derive(borsh::BorshSerialize, borsh::BorshDeserialize)
)]
pub struct Minter {
    /// `Minter.weekly`: current weekly emission. Frozen once the tail has started.
    pub weekly: U256,
    /// `Minter.tailEmissionRate`: tail emission, in bps of wind total supply per epoch.
    pub tail_emission_rate: U256,
    /// `Minter.teamRate`: team share, in bps.
    pub team_rate: U256,
    /// `Minter.activePeriod`: start of the current epoch, in seconds.
    pub active_period: u64,
    /// `Minter.epochCount`: number of completed `updatePeriod` epochs.
    pub epoch_count: u64,
    /// `Minter.proposals`: periods (keyed by `active_period`) that were already nudged.
    pub proposals: BTreeSet<u64>,
}

impl Default for Minter {
    /// The constructor with `now = 0`. Use [`Minter::new`] with the deployment time.
    fn default() -> Self {
        Self::new(0)
    }
}

fn u(v: u64) -> U256 {
    U256::from(v)
}

fn tail_start() -> U256 {
    U256::from(TAIL_START)
}

fn add(a: U256, b: U256) -> Result<U256> {
    a.checked_add(b).ok_or(MathError::Overflow)
}

fn sub(a: U256, b: U256) -> Result<U256> {
    a.checked_sub(b).ok_or(MathError::Underflow)
}

fn mul(a: U256, b: U256) -> Result<U256> {
    a.checked_mul(b).ok_or(MathError::Overflow)
}

fn div(a: U256, b: U256) -> Result<U256> {
    if b.is_zero() {
        Err(MathError::DivisionByZero)
    } else {
        Ok(a / b)
    }
}

impl Minter {
    /// Port of the `Minter` constructor. `now` replaces `block.timestamp`:
    /// `activePeriod = (block.timestamp / WEEK) * WEEK`, so the first `update_period` after
    /// one full week emits. `weekly = 10_000_000e18`, `tailEmissionRate = 67`, `teamRate = 500`.
    pub fn new(now: u64) -> Self {
        Self {
            weekly: U256::from(INITIAL_WEEKLY),
            tail_emission_rate: u(INITIAL_TAIL_EMISSION_RATE),
            team_rate: u(INITIAL_TEAM_RATE),
            active_period: now / WEEK * WEEK,
            epoch_count: 0,
            proposals: BTreeSet::new(),
        }
    }

    /// Port of `Minter.updatePeriod()`.
    ///
    /// Returns `Ok(None)` when `now < activePeriod + WEEK` (the Solidity no-op branch). Otherwise
    /// advances the epoch and returns the amounts the Solidity mints or transfers.
    ///
    /// Inputs replace the external reads:
    /// - `wind_total_supply`: `wind.totalSupply()`.
    /// - `wind_balance_of_minter`: `wind.balanceOf(address(this))` before the call.
    /// - `ve_total_supply`: `ve.totalSupplyAt(newActivePeriod - 1)`. `calculateGrowth` reads this
    ///   after `activePeriod` is set to the new period, so the caller must use the new period.
    ///
    /// Solidity order preserved:
    /// 1. `epochCount++`, `activePeriod = (now / WEEK) * WEEK`.
    /// 2. Tail (`weekly < TAIL_START`): `emission = totalSupply * tailEmissionRate / MAX_BPS`,
    ///    `weekly` unchanged. Otherwise `emission = weekly` and `weekly` grows by
    ///    `WEEKLY_GROWTH` while `epochCount < 15`, then decays by `WEEKLY_DECAY`.
    /// 3. `growth = calculateGrowth(emission)`.
    /// 4. `team = teamRate * (growth + weekly) / (MAX_BPS - teamRate)`. `weekly` here is the
    ///    **updated** value in the non-tail branch (the local `_weekly` is reassigned before this),
    ///    and the unchanged value in the tail branch.
    /// 5. `required = growth + emission + team`. Mint `required - balance` when the balance is short.
    pub fn update_period(
        &mut self,
        now: u64,
        wind_total_supply: U256,
        wind_balance_of_minter: U256,
        ve_total_supply: U256,
    ) -> Result<Option<EpochEmission>> {
        let next_period = self
            .active_period
            .checked_add(WEEK)
            .ok_or(MathError::Overflow)?;
        if now < next_period {
            return Ok(None);
        }

        let epoch_count = self.epoch_count.checked_add(1).ok_or(MathError::Overflow)?;
        let period = now / WEEK * WEEK;
        let mut weekly = self.weekly;
        let tail = weekly < tail_start();

        let emission = if tail {
            div(mul(wind_total_supply, self.tail_emission_rate)?, u(MAX_BPS))?
        } else {
            let emission = weekly;
            let factor = if epoch_count < 15 {
                WEEKLY_GROWTH
            } else {
                WEEKLY_DECAY
            };
            weekly = div(mul(weekly, u(factor))?, u(MAX_BPS))?;
            emission
        };

        let growth = self.calculate_growth(emission, ve_total_supply, wind_total_supply)?;

        let rate = self.team_rate;
        let team = div(mul(rate, add(growth, weekly)?)?, sub(u(MAX_BPS), rate)?)?;

        let required = add(add(growth, emission)?, team)?;
        let mint_required = if wind_balance_of_minter < required {
            sub(required, wind_balance_of_minter)?
        } else {
            U256::ZERO
        };

        self.epoch_count = epoch_count;
        self.active_period = period;
        self.weekly = weekly;
        Ok(Some(EpochEmission {
            emission,
            growth,
            team,
            mint_required,
        }))
    }

    /// Port of `Minter.calculateGrowth(uint256 _minted)`:
    ///
    /// `((_minted * (W - V)) / W) * (W - V) / W / 2`, where `W = wind.totalSupply()` and
    /// `V = ve.totalSupplyAt(activePeriod - 1)`. `V` is passed in as `ve_total_supply`; see
    /// [`Minter::update_period`]. Reverts (`Err(Underflow)`) when `V > W` and
    /// `Err(DivisionByZero)` when `W == 0`.
    pub fn calculate_growth(
        &self,
        minted: U256,
        ve_total_supply: U256,
        wind_total_supply: U256,
    ) -> Result<U256> {
        let w = wind_total_supply;
        let diff = sub(w, ve_total_supply)?;
        let a = div(mul(minted, diff)?, w)?;
        let b = div(mul(a, diff)?, w)?;
        Ok(b / u(2))
    }

    /// Port of `Minter.nudge()` after the caller has checked `msg.sender == voter.epochGovernor()`.
    ///
    /// Solidity order preserved: `weekly >= TAIL_START` reverts `TailEmissionsInactive`, then
    /// `proposals[activePeriod]` reverts `AlreadyNudged`. Otherwise, unless the state is `Expired`,
    /// `Succeeded` raises `tailEmissionRate` by `NUDGE` (capped at `MAXIMUM_TAIL_RATE`) and any
    /// other state lowers it by `NUDGE` (floored at `MINIMUM_TAIL_RATE`). The period is marked
    /// nudged in every non-reverting case, including `Expired`.
    pub fn nudge(&mut self, proposal_state: ProposalState) -> Result<()> {
        if self.weekly >= tail_start() {
            return Err(MathError::TailEmissionsInactive);
        }
        if self.proposals.contains(&self.active_period) {
            return Err(MathError::AlreadyNudged);
        }
        if proposal_state != ProposalState::Expired {
            let old = self.tail_emission_rate;
            self.tail_emission_rate = if proposal_state == ProposalState::Succeeded {
                let up = add(old, u(NUDGE))?;
                if up > u(MAXIMUM_TAIL_RATE) {
                    u(MAXIMUM_TAIL_RATE)
                } else {
                    up
                }
            } else {
                let down = sub(old, u(NUDGE))?;
                if down < u(MINIMUM_TAIL_RATE) {
                    u(MINIMUM_TAIL_RATE)
                } else {
                    down
                }
            };
        }
        self.proposals.insert(self.active_period);
        Ok(())
    }

    /// Port of `Minter.setTeamRate(uint256 _rate)` after the caller has checked `msg.sender == team`.
    /// Reverts `RateTooHigh` when `rate > MAXIMUM_TEAM_RATE`.
    pub fn set_team_rate(&mut self, rate: U256) -> Result<()> {
        if rate > u(MAXIMUM_TEAM_RATE) {
            return Err(MathError::RateTooHigh);
        }
        self.team_rate = rate;
        Ok(())
    }
}
