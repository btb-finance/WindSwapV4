//! Port of `CLPool.sol`: the Wind Swap concentrated-liquidity pool.
//!
//! Differences from the Solidity contract, all outside the pool math:
//! - No token transfers or callbacks. `mint`/`swap` return the amounts the caller must
//!   collect from the user, and `burn`/`collect`/`swap` return amounts owed to the user.
//!   The surrounding engine moves balances.
//! - No `msg.sender` checks (`onlyGauge`, `onlyNftManager`). The caller authorizes.
//! - Time and fees are passed in: `time` replaces `block.timestamp`, `fee` replaces
//!   `factory.getSwapFee(pool)` and `unstaked_fee` replaces `factory.getUnstakedFee(pool)`.
//! - The WindMEV `afterSwap` hook and `flash` are not ported.
//!
//! Atomicity: like a Solidity revert, an `Err` means the whole call must be discarded. The
//! pool may be partially updated when an error is returned. On NEAR a failed call is rolled
//! back by the runtime; elsewhere use [`ClPool::atomic`].

use alloy_primitives::{I256, U256};

use super::oracle::{self, Observations};
use super::position::{PositionInfo, PositionKey, Positions};
use super::tick::{self, Ticks};
use super::tick_bitmap::{self, TickBitmap};
use crate::error::{MathError, Result};
use crate::math::full_math::{mul_div, mul_div_rounding_up};
use crate::math::{liquidity_math, sqrt_price_math, swap_math, tick_math, Q128};

/// Account identifier (32 bytes, chain-agnostic).
pub type AccountId = [u8; 32];

/// `CLPool.Slot0` without the `unlocked` flag (see [`ClPool::initialized`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "borsh",
    derive(borsh::BorshSerialize, borsh::BorshDeserialize)
)]
pub struct Slot0 {
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub observation_index: u16,
    pub observation_cardinality: u16,
    pub observation_cardinality_next: u16,
}

/// Result of a swap: signed deltas from the pool's point of view, as in `CLPool.swap`.
/// Positive = owed to the pool by the trader, negative = owed to the trader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapResult {
    pub amount0: I256,
    pub amount1: I256,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "borsh",
    derive(borsh::BorshSerialize, borsh::BorshDeserialize)
)]
pub struct ClPool {
    /// Set by `initialize`; replaces `slot0.unlocked` as the "pool is usable" flag.
    pub initialized: bool,
    pub tick_spacing: i32,
    pub max_liquidity_per_tick: u128,
    pub slot0: Slot0,
    pub fee_growth_global0_x128: U256,
    pub fee_growth_global1_x128: U256,
    pub reward_growth_global_x128: U256,
    /// `gaugeFees.token0` / `gaugeFees.token1`
    pub gauge_fees0: u128,
    pub gauge_fees1: u128,
    pub reward_rate: U256,
    pub reward_reserve: U256,
    pub period_finish: U256,
    pub rollover: U256,
    pub staked_liquidity: u128,
    pub last_updated: u32,
    pub liquidity: u128,
    pub gauge: Option<AccountId>,
    pub nft: Option<AccountId>,
    pub ticks: Ticks,
    pub tick_bitmap: TickBitmap,
    pub positions: Positions,
    pub observations: Observations,
}

/// Solidity `uint128(x)`: keeps the low 128 bits.
fn low_u128(x: U256) -> u128 {
    let limbs = x.as_limbs();
    limbs[0] as u128 | ((limbs[1] as u128) << 64)
}

/// `toInt256`
fn to_i256(x: U256) -> Result<I256> {
    I256::try_from(x).map_err(|_| MathError::Overflow)
}

/// `int256(x).toInt128()` for a `uint128` amount.
fn amount_to_i128(amount: u128) -> Result<i128> {
    i128::try_from(amount).map_err(|_| MathError::Overflow)
}

/// `LowGasSafeMath.add(int256, int256)`
fn checked_add_i256(x: I256, y: I256) -> Result<I256> {
    x.checked_add(y).ok_or(MathError::Overflow)
}

/// `LowGasSafeMath.sub(int256, int256)`
fn checked_sub_i256(x: I256, y: I256) -> Result<I256> {
    x.checked_sub(y).ok_or(MathError::Overflow)
}

fn check_ticks(tick_lower: i32, tick_upper: i32) -> Result<()> {
    if tick_lower >= tick_upper
        || tick_lower < tick_math::MIN_TICK
        || tick_upper > tick_math::MAX_TICK
    {
        return Err(MathError::InvalidTickRange);
    }
    Ok(())
}

impl ClPool {
    /// Runs `f` on a copy of the pool and keeps the result only if it succeeds.
    pub fn atomic<T>(&mut self, f: impl FnOnce(&mut ClPool) -> Result<T>) -> Result<T> {
        let mut next = self.clone();
        let out = f(&mut next)?;
        *self = next;
        Ok(out)
    }

    fn require_initialized(&self) -> Result<()> {
        if self.initialized {
            Ok(())
        } else {
            Err(MathError::PoolNotInitialized)
        }
    }

    /// `CLPool.initialize`
    pub fn initialize(&mut self, tick_spacing: i32, sqrt_price_x96: U256, time: u32) -> Result<()> {
        if self.initialized {
            return Err(MathError::PoolAlreadyInitialized);
        }
        self.tick_spacing = tick_spacing;
        self.max_liquidity_per_tick = tick::tick_spacing_to_max_liquidity_per_tick(tick_spacing)?;
        let tick = tick_math::get_tick_at_sqrt_ratio(sqrt_price_x96)?;
        let (cardinality, cardinality_next) = oracle::initialize(&mut self.observations, time);
        self.slot0 = Slot0 {
            sqrt_price_x96,
            tick,
            observation_index: 0,
            observation_cardinality: cardinality,
            observation_cardinality_next: cardinality_next,
        };
        self.initialized = true;
        Ok(())
    }

    /// `CLPool.setGaugeAndPositionManager`
    pub fn set_gauge_and_position_manager(
        &mut self,
        gauge: AccountId,
        nft: AccountId,
    ) -> Result<()> {
        self.require_initialized()?;
        if self.gauge.is_some() {
            return Err(MathError::GaugeAlreadySet);
        }
        self.gauge = Some(gauge);
        self.nft = Some(nft);
        Ok(())
    }

    /// `CLPool.increaseObservationCardinalityNext`
    pub fn increase_observation_cardinality_next(&mut self, next: u16) -> Result<()> {
        self.require_initialized()?;
        self.slot0.observation_cardinality_next = oracle::grow(
            &mut self.observations,
            self.slot0.observation_cardinality_next,
            next,
        )?;
        Ok(())
    }

    /// `CLPool.observe`
    pub fn observe(&self, time: u32, seconds_agos: &[u32]) -> Result<(Vec<i64>, Vec<U256>)> {
        oracle::observe(
            &self.observations,
            time,
            seconds_agos,
            self.slot0.tick,
            self.slot0.observation_index,
            self.liquidity,
            self.slot0.observation_cardinality,
        )
    }

    /// Position by owner and range (`positions.get(owner, tickLower, tickUpper)`).
    pub fn position(&self, owner: &AccountId, tick_lower: i32, tick_upper: i32) -> PositionInfo {
        self.positions
            .get(&position_key(owner, tick_lower, tick_upper))
            .cloned()
            .unwrap_or_default()
    }

    /// `CLPool.mint`: adds `amount` liquidity for `owner`. Returns the token amounts the
    /// caller must collect from the owner.
    pub fn mint(
        &mut self,
        owner: AccountId,
        tick_lower: i32,
        tick_upper: i32,
        amount: u128,
        time: u32,
    ) -> Result<(U256, U256)> {
        self.require_initialized()?;
        if amount == 0 {
            return Err(MathError::ZeroAmount);
        }
        let delta = amount_to_i128(amount)?;
        let (amount0, amount1) =
            self.modify_position(owner, tick_lower, tick_upper, delta, time)?;
        Ok((amount0.into_raw(), amount1.into_raw()))
    }

    /// `CLPool.burn`: removes `amount` liquidity from `owner`'s position. The withdrawn token
    /// amounts are added to the position's `tokens_owed` (collect them with [`Self::collect`]).
    pub fn burn(
        &mut self,
        owner: AccountId,
        tick_lower: i32,
        tick_upper: i32,
        amount: u128,
        time: u32,
    ) -> Result<(U256, U256)> {
        self.require_initialized()?;
        let delta = -amount_to_i128(amount)?;
        let (amount0_int, amount1_int) =
            self.modify_position(owner, tick_lower, tick_upper, delta, time)?;
        let amount0 = amount0_int.wrapping_neg().into_raw();
        let amount1 = amount1_int.wrapping_neg().into_raw();
        if !amount0.is_zero() || !amount1.is_zero() {
            let position = self
                .positions
                .entry(position_key(&owner, tick_lower, tick_upper))
                .or_default();
            // `uint128(amount)` truncates; the addition wraps as in Solidity 0.7
            position.tokens_owed0 = position.tokens_owed0.wrapping_add(low_u128(amount0));
            position.tokens_owed1 = position.tokens_owed1.wrapping_add(low_u128(amount1));
        }
        Ok((amount0, amount1))
    }

    /// `CLPool.collect`: withdraws up to the requested owed amounts. Returns the amounts to
    /// pay out to the owner.
    pub fn collect(
        &mut self,
        owner: AccountId,
        tick_lower: i32,
        tick_upper: i32,
        amount0_requested: u128,
        amount1_requested: u128,
    ) -> Result<(u128, u128)> {
        self.require_initialized()?;
        let key = position_key(&owner, tick_lower, tick_upper);
        let mut position = self.positions.get(&key).cloned().unwrap_or_default();
        let amount0 = amount0_requested.min(position.tokens_owed0);
        let amount1 = amount1_requested.min(position.tokens_owed1);
        position.tokens_owed0 -= amount0;
        position.tokens_owed1 -= amount1;
        if amount0 > 0 || amount1 > 0 {
            self.positions.insert(key, position);
        }
        Ok((amount0, amount1))
    }

    fn modify_position(
        &mut self,
        owner: AccountId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        time: u32,
    ) -> Result<(I256, I256)> {
        check_ticks(tick_lower, tick_upper)?;
        let slot0 = self.slot0.clone();

        self.update_position(
            owner,
            tick_lower,
            tick_upper,
            liquidity_delta,
            slot0.tick,
            time,
        )?;

        let mut amount0 = I256::ZERO;
        let mut amount1 = I256::ZERO;
        if liquidity_delta != 0 {
            let sqrt_lower = tick_math::get_sqrt_ratio_at_tick(tick_lower)?;
            let sqrt_upper = tick_math::get_sqrt_ratio_at_tick(tick_upper)?;
            if slot0.tick < tick_lower {
                amount0 = sqrt_price_math::get_amount0_delta_signed(
                    sqrt_lower,
                    sqrt_upper,
                    liquidity_delta,
                )?;
            } else if slot0.tick < tick_upper {
                let liquidity_before = self.liquidity;
                let (index, cardinality) = oracle::write(
                    &mut self.observations,
                    slot0.observation_index,
                    time,
                    slot0.tick,
                    liquidity_before,
                    slot0.observation_cardinality,
                    slot0.observation_cardinality_next,
                )?;
                self.slot0.observation_index = index;
                self.slot0.observation_cardinality = cardinality;

                amount0 = sqrt_price_math::get_amount0_delta_signed(
                    slot0.sqrt_price_x96,
                    sqrt_upper,
                    liquidity_delta,
                )?;
                amount1 = sqrt_price_math::get_amount1_delta_signed(
                    sqrt_lower,
                    slot0.sqrt_price_x96,
                    liquidity_delta,
                )?;
                self.liquidity = liquidity_math::add_delta(liquidity_before, liquidity_delta)?;
            } else {
                amount1 = sqrt_price_math::get_amount1_delta_signed(
                    sqrt_lower,
                    sqrt_upper,
                    liquidity_delta,
                )?;
            }
        }
        Ok((amount0, amount1))
    }

    fn update_position(
        &mut self,
        owner: AccountId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        tick: i32,
        time: u32,
    ) -> Result<()> {
        let fee_growth_global0 = self.fee_growth_global0_x128;
        let fee_growth_global1 = self.fee_growth_global1_x128;

        let mut flipped_lower = false;
        let mut flipped_upper = false;
        if liquidity_delta != 0 {
            let (tick_cumulative, seconds_per_liquidity) = oracle::observe_single(
                &self.observations,
                time,
                0,
                self.slot0.tick,
                self.slot0.observation_index,
                self.liquidity,
                self.slot0.observation_cardinality,
            )?;
            flipped_lower = tick::update(
                &mut self.ticks,
                tick_lower,
                tick,
                liquidity_delta,
                fee_growth_global0,
                fee_growth_global1,
                seconds_per_liquidity,
                tick_cumulative,
                time,
                false,
                self.max_liquidity_per_tick,
            )?;
            flipped_upper = tick::update(
                &mut self.ticks,
                tick_upper,
                tick,
                liquidity_delta,
                fee_growth_global0,
                fee_growth_global1,
                seconds_per_liquidity,
                tick_cumulative,
                time,
                true,
                self.max_liquidity_per_tick,
            )?;
            if flipped_lower {
                tick_bitmap::flip_tick(&mut self.tick_bitmap, tick_lower, self.tick_spacing)?;
            }
            if flipped_upper {
                tick_bitmap::flip_tick(&mut self.tick_bitmap, tick_upper, self.tick_spacing)?;
            }
        }

        let (inside0, inside1) = tick::get_fee_growth_inside(
            &self.ticks,
            tick_lower,
            tick_upper,
            tick,
            fee_growth_global0,
            fee_growth_global1,
        );

        let staked = self.gauge == Some(owner) && owner != [0u8; 32];
        let key = position_key(&owner, tick_lower, tick_upper);
        let mut position = self.positions.get(&key).cloned().unwrap_or_default();
        position.update(liquidity_delta, inside0, inside1, staked)?;
        self.positions.insert(key, position);

        if liquidity_delta < 0 {
            if flipped_lower {
                tick::clear(&mut self.ticks, tick_lower);
            }
            if flipped_upper {
                tick::clear(&mut self.ticks, tick_upper);
            }
        }
        Ok(())
    }

    /// `CLPool.stake` (called by the gauge). Moves `staked_liquidity_delta` of the NFT
    /// manager's position to the gauge when `position_update` is set.
    pub fn stake(
        &mut self,
        staked_liquidity_delta: i128,
        tick_lower: i32,
        tick_upper: i32,
        position_update: bool,
        time: u32,
    ) -> Result<()> {
        self.require_initialized()?;
        let tick = self.slot0.tick;
        if tick >= tick_lower && tick < tick_upper {
            self.update_rewards_growth_global(time)?;
            self.staked_liquidity =
                liquidity_math::add_delta(self.staked_liquidity, staked_liquidity_delta)?;
        }

        if position_update {
            let nft = self.nft.unwrap_or_default();
            let gauge = self.gauge.unwrap_or_default();
            let (inside0, inside1) = tick::get_fee_growth_inside(
                &self.ticks,
                tick_lower,
                tick_upper,
                tick,
                self.fee_growth_global0_x128,
                self.fee_growth_global1_x128,
            );
            let nft_key = position_key(&nft, tick_lower, tick_upper);
            let mut nft_position = self.positions.get(&nft_key).cloned().unwrap_or_default();
            nft_position.update(
                staked_liquidity_delta.wrapping_neg(),
                inside0,
                inside1,
                false,
            )?;
            self.positions.insert(nft_key, nft_position);

            let gauge_key = position_key(&gauge, tick_lower, tick_upper);
            let mut gauge_position = self.positions.get(&gauge_key).cloned().unwrap_or_default();
            gauge_position.update(staked_liquidity_delta, inside0, inside1, true)?;
            self.positions.insert(gauge_key, gauge_position);
        }

        if self.ticks.get(&tick_lower).is_some_and(|t| t.initialized) {
            tick::update_stake(&mut self.ticks, tick_lower, staked_liquidity_delta, false)?;
        }
        if self.ticks.get(&tick_upper).is_some_and(|t| t.initialized) {
            tick::update_stake(&mut self.ticks, tick_upper, staked_liquidity_delta, true)?;
        }
        Ok(())
    }

    /// `CLPool.updateRewardsGrowthGlobal` / `_updateRewardsGrowthGlobal`
    pub fn update_rewards_growth_global(&mut self, time: u32) -> Result<()> {
        // `uint32 - uint256` is uint256 arithmetic in Solidity, so this wraps mod 2^256
        let time_delta = U256::from(time).wrapping_sub(U256::from(self.last_updated));
        if !time_delta.is_zero() {
            if !self.reward_reserve.is_zero() {
                let mut reward = self.reward_rate.wrapping_mul(time_delta);
                if reward > self.reward_reserve {
                    reward = self.reward_reserve;
                }
                self.reward_reserve -= reward;
                if self.staked_liquidity > 0 {
                    let growth = mul_div(reward, Q128, U256::from(self.staked_liquidity))?;
                    self.reward_growth_global_x128 =
                        self.reward_growth_global_x128.wrapping_add(growth);
                } else {
                    self.rollover = self.rollover.wrapping_add(reward);
                }
            }
            self.last_updated = time;
        }
        Ok(())
    }

    /// `CLPool.syncReward` (called by the gauge)
    pub fn sync_reward(&mut self, reward_rate: U256, reward_reserve: U256, period_finish: U256) {
        self.reward_rate = reward_rate;
        self.reward_reserve = reward_reserve;
        self.period_finish = period_finish;
        self.rollover = U256::ZERO;
    }

    /// `CLPool.getRewardGrowthInside`
    pub fn get_reward_growth_inside(
        &self,
        tick_lower: i32,
        tick_upper: i32,
        reward_growth_global_x128: U256,
    ) -> Result<U256> {
        check_ticks(tick_lower, tick_upper)?;
        let global = if reward_growth_global_x128.is_zero() {
            self.reward_growth_global_x128
        } else {
            reward_growth_global_x128
        };
        Ok(tick::get_reward_growth_inside(
            &self.ticks,
            tick_lower,
            tick_upper,
            self.slot0.tick,
            global,
        ))
    }

    /// `CLPool.collectFees` (called by the gauge). Leaves 1 wei in each slot, as on-chain.
    pub fn collect_fees(&mut self) -> (u128, u128) {
        let mut amount0 = self.gauge_fees0;
        let mut amount1 = self.gauge_fees1;
        if amount0 > 1 {
            self.gauge_fees0 = 1;
            amount0 -= 1;
        }
        if amount1 > 1 {
            self.gauge_fees1 = 1;
            amount1 -= 1;
        }
        (amount0, amount1)
    }

    /// `applyUnstakedFees`
    fn apply_unstaked_fees(
        unstaked_fee_amount: U256,
        staked_fee_amount: U256,
        unstaked_fee: u32,
    ) -> Result<(U256, U256)> {
        let staked_fee = mul_div_rounding_up(
            unstaked_fee_amount,
            U256::from(unstaked_fee),
            U256::from(1_000_000u32),
        )?;
        Ok((
            unstaked_fee_amount.wrapping_sub(staked_fee),
            staked_fee_amount.wrapping_add(staked_fee),
        ))
    }

    /// `calculateFees`: returns `(fee_growth_global_x128 delta, staked_fee_amount)`.
    pub fn calculate_fees(
        fee_amount: U256,
        liquidity: u128,
        staked_liquidity: u128,
        unstaked_fee: u32,
    ) -> Result<(U256, U256)> {
        if liquidity == staked_liquidity {
            Ok((U256::ZERO, fee_amount))
        } else if staked_liquidity == 0 {
            let (unstaked, staked) =
                Self::apply_unstaked_fees(fee_amount, U256::ZERO, unstaked_fee)?;
            Ok((mul_div(unstaked, Q128, U256::from(liquidity))?, staked))
        } else {
            // splitFees
            let staked_fee_amount = mul_div_rounding_up(
                fee_amount,
                U256::from(staked_liquidity),
                U256::from(liquidity),
            )?;
            let (unstaked, staked) = Self::apply_unstaked_fees(
                fee_amount.wrapping_sub(staked_fee_amount),
                staked_fee_amount,
                unstaked_fee,
            )?;
            let unstaked_liquidity = liquidity.wrapping_sub(staked_liquidity);
            Ok((
                mul_div(unstaked, Q128, U256::from(unstaked_liquidity))?,
                staked,
            ))
        }
    }

    /// `CLPool.swap`. `amount_specified > 0` is exact input, `< 0` is exact output.
    /// `fee` and `unstaked_fee` are in hundredths of a bip (see [`crate::fees::FeeConfig`]).
    pub fn swap(
        &mut self,
        zero_for_one: bool,
        amount_specified: I256,
        sqrt_price_limit_x96: U256,
        fee: u32,
        unstaked_fee: u32,
        time: u32,
    ) -> Result<SwapResult> {
        if amount_specified.is_zero() {
            return Err(MathError::ZeroAmount);
        }
        self.require_initialized()?;
        let slot0_start = self.slot0.clone();
        let valid_limit = if zero_for_one {
            sqrt_price_limit_x96 < slot0_start.sqrt_price_x96
                && sqrt_price_limit_x96 > tick_math::MIN_SQRT_RATIO
        } else {
            sqrt_price_limit_x96 > slot0_start.sqrt_price_x96
                && sqrt_price_limit_x96 < tick_math::MAX_SQRT_RATIO
        };
        if !valid_limit {
            return Err(MathError::InvalidPriceLimit);
        }

        let liquidity_start = self.liquidity;
        let staked_liquidity_start = self.staked_liquidity;
        let mut computed_latest_observation = false;
        let mut cache_tick_cumulative: i64 = 0;
        let mut cache_seconds_per_liquidity = U256::ZERO;

        let exact_input = amount_specified.is_positive();
        let mut amount_specified_remaining = amount_specified;
        let mut amount_calculated = I256::ZERO;
        let mut sqrt_price_x96 = slot0_start.sqrt_price_x96;
        let mut state_tick = slot0_start.tick;
        let mut has_updated_fees = false;
        let mut fee_growth_global_x128 = if zero_for_one {
            self.fee_growth_global0_x128
        } else {
            self.fee_growth_global1_x128
        };
        let mut gauge_fee: u128 = 0;
        let mut liquidity = liquidity_start;
        let mut staked_liquidity = staked_liquidity_start;

        while !amount_specified_remaining.is_zero() && sqrt_price_x96 != sqrt_price_limit_x96 {
            let sqrt_price_start_x96 = sqrt_price_x96;
            let (mut tick_next, initialized) = tick_bitmap::next_initialized_tick_within_one_word(
                &self.tick_bitmap,
                state_tick,
                self.tick_spacing,
                zero_for_one,
            );
            tick_next = tick_next.clamp(tick_math::MIN_TICK, tick_math::MAX_TICK);
            let sqrt_price_next_x96 = tick_math::get_sqrt_ratio_at_tick(tick_next)?;

            let target = if zero_for_one {
                if sqrt_price_next_x96 < sqrt_price_limit_x96 {
                    sqrt_price_limit_x96
                } else {
                    sqrt_price_next_x96
                }
            } else if sqrt_price_next_x96 > sqrt_price_limit_x96 {
                sqrt_price_limit_x96
            } else {
                sqrt_price_next_x96
            };
            let step = swap_math::compute_swap_step(
                sqrt_price_x96,
                target,
                liquidity,
                amount_specified_remaining,
                fee,
            )?;
            sqrt_price_x96 = step.sqrt_ratio_next_x96;

            if exact_input {
                // `-=` on int256 is unchecked in Solidity 0.7
                let spent = to_i256(step.amount_in.wrapping_add(step.fee_amount))?;
                amount_specified_remaining = amount_specified_remaining.wrapping_sub(spent);
                amount_calculated = checked_sub_i256(amount_calculated, to_i256(step.amount_out)?)?;
            } else {
                amount_specified_remaining =
                    amount_specified_remaining.wrapping_add(to_i256(step.amount_out)?);
                let paid = to_i256(step.amount_in.wrapping_add(step.fee_amount))?;
                amount_calculated = checked_add_i256(amount_calculated, paid)?;
            }

            if liquidity > 0 {
                let (growth, staked_fee_amount) = Self::calculate_fees(
                    step.fee_amount,
                    liquidity,
                    staked_liquidity,
                    unstaked_fee,
                )?;
                fee_growth_global_x128 = fee_growth_global_x128.wrapping_add(growth);
                gauge_fee = gauge_fee.wrapping_add(low_u128(staked_fee_amount));
            }

            if sqrt_price_x96 == sqrt_price_next_x96 {
                if initialized {
                    if !computed_latest_observation {
                        let (tc, spl) = oracle::observe_single(
                            &self.observations,
                            time,
                            0,
                            slot0_start.tick,
                            slot0_start.observation_index,
                            liquidity_start,
                            slot0_start.observation_cardinality,
                        )?;
                        cache_tick_cumulative = tc;
                        cache_seconds_per_liquidity = spl;
                        computed_latest_observation = true;
                    }
                    if !has_updated_fees {
                        self.update_rewards_growth_global(time)?;
                        has_updated_fees = true;
                    }
                    let (global0, global1) = if zero_for_one {
                        (fee_growth_global_x128, self.fee_growth_global1_x128)
                    } else {
                        (self.fee_growth_global0_x128, fee_growth_global_x128)
                    };
                    let mut nets = tick::cross(
                        &mut self.ticks,
                        tick_next,
                        global0,
                        global1,
                        cache_seconds_per_liquidity,
                        cache_tick_cumulative,
                        time,
                        self.reward_growth_global_x128,
                    );
                    if zero_for_one {
                        nets.liquidity_net = nets.liquidity_net.wrapping_neg();
                        nets.staked_liquidity_net = nets.staked_liquidity_net.wrapping_neg();
                    }
                    liquidity = liquidity_math::add_delta(liquidity, nets.liquidity_net)?;
                    staked_liquidity =
                        liquidity_math::add_delta(staked_liquidity, nets.staked_liquidity_net)?;
                }
                state_tick = if zero_for_one {
                    tick_next - 1
                } else {
                    tick_next
                };
            } else if sqrt_price_x96 != sqrt_price_start_x96 {
                state_tick = tick_math::get_tick_at_sqrt_ratio(sqrt_price_x96)?;
            }
        }

        if state_tick != slot0_start.tick {
            let (index, cardinality) = oracle::write(
                &mut self.observations,
                slot0_start.observation_index,
                time,
                slot0_start.tick,
                liquidity_start,
                slot0_start.observation_cardinality,
                slot0_start.observation_cardinality_next,
            )?;
            self.slot0.sqrt_price_x96 = sqrt_price_x96;
            self.slot0.tick = state_tick;
            self.slot0.observation_index = index;
            self.slot0.observation_cardinality = cardinality;
        } else {
            self.slot0.sqrt_price_x96 = sqrt_price_x96;
        }

        self.liquidity = liquidity;
        self.staked_liquidity = staked_liquidity;

        if zero_for_one {
            self.fee_growth_global0_x128 = fee_growth_global_x128;
            self.gauge_fees0 = self.gauge_fees0.wrapping_add(gauge_fee);
        } else {
            self.fee_growth_global1_x128 = fee_growth_global_x128;
            self.gauge_fees1 = self.gauge_fees1.wrapping_add(gauge_fee);
        }

        // `amountSpecified - amountSpecifiedRemaining` is unchecked
        let specified_used = amount_specified.wrapping_sub(amount_specified_remaining);
        let (amount0, amount1) = if zero_for_one == exact_input {
            (specified_used, amount_calculated)
        } else {
            (amount_calculated, specified_used)
        };
        Ok(SwapResult { amount0, amount1 })
    }
}

/// Position key for an owner and range.
pub fn position_key(owner: &AccountId, tick_lower: i32, tick_upper: i32) -> PositionKey {
    PositionKey {
        owner: *owner,
        tick_lower,
        tick_upper,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Q96;

    const ALICE: AccountId = [1u8; 32];
    const BOB: AccountId = [2u8; 32];

    fn pool() -> ClPool {
        let mut p = ClPool::default();
        p.initialize(60, Q96, 1_000).unwrap();
        p
    }

    #[test]
    fn initialize_once() {
        let mut p = pool();
        assert_eq!(p.slot0.tick, 0);
        assert_eq!(
            p.initialize(60, Q96, 1),
            Err(MathError::PoolAlreadyInitialized)
        );
    }

    #[test]
    fn mint_in_range_needs_both_tokens() {
        let mut p = pool();
        let (a0, a1) = p.mint(ALICE, -600, 600, 1_000_000_000_000, 1_000).unwrap();
        assert!(a0 > U256::ZERO && a1 > U256::ZERO);
        assert_eq!(p.liquidity, 1_000_000_000_000);
        assert!(p.ticks[&-600].initialized && p.ticks[&600].initialized);
    }

    #[test]
    fn mint_out_of_range_needs_one_token() {
        let mut p = pool();
        let (a0, a1) = p.mint(ALICE, 600, 1200, 1_000_000, 1_000).unwrap();
        assert!(a0 > U256::ZERO);
        assert_eq!(a1, U256::ZERO);
        assert_eq!(p.liquidity, 0);
    }

    #[test]
    fn invalid_ticks_and_zero_amount() {
        let mut p = pool();
        assert_eq!(
            p.mint(ALICE, 600, -600, 1, 1_000),
            Err(MathError::InvalidTickRange)
        );
        assert_eq!(
            p.mint(ALICE, -600, 600, 0, 1_000),
            Err(MathError::ZeroAmount)
        );
    }

    #[test]
    fn swap_moves_price_and_pays_fees() {
        let mut p = pool();
        p.mint(ALICE, -600, 600, 10u128.pow(18), 1_000).unwrap();
        let before = p.slot0.sqrt_price_x96;
        let r = p
            .swap(
                true,
                I256::try_from(10u64.pow(15)).unwrap(),
                tick_math::MIN_SQRT_RATIO + U256::from(1),
                3000,
                0,
                1_010,
            )
            .unwrap();
        assert_eq!(r.amount0, I256::try_from(10u64.pow(15)).unwrap());
        assert!(r.amount1.is_negative());
        assert!(p.slot0.sqrt_price_x96 < before);
        assert!(p.fee_growth_global0_x128 > U256::ZERO);

        // burn everything and collect: Alice gets back her liquidity plus fees
        let (b0, _b1) = p.burn(ALICE, -600, 600, 10u128.pow(18), 1_020).unwrap();
        let pos = p.position(&ALICE, -600, 600);
        assert!(U256::from(pos.tokens_owed0) > b0);
        let (c0, c1) = p.collect(ALICE, -600, 600, u128::MAX, u128::MAX).unwrap();
        assert_eq!((c0, c1), (pos.tokens_owed0, pos.tokens_owed1));
        assert_eq!(p.liquidity, 0);
    }

    #[test]
    fn swap_crosses_ticks() {
        let mut p = pool();
        p.mint(ALICE, -120, 120, 10u128.pow(18), 1_000).unwrap();
        p.mint(BOB, -1200, 1200, 10u128.pow(18), 1_000).unwrap();
        let r = p
            .swap(
                true,
                I256::try_from(3 * 10u64.pow(16)).unwrap(),
                tick_math::MIN_SQRT_RATIO + U256::from(1),
                500,
                0,
                1_005,
            )
            .unwrap();
        assert!(r.amount1.is_negative());
        assert!(p.slot0.tick < -120 && p.slot0.tick >= -1200);
        assert_eq!(p.liquidity, 10u128.pow(18));
    }

    #[test]
    fn atomic_discards_on_error() {
        let mut p = pool();
        let before = p.clone();
        let r = p.atomic(|p| {
            p.mint(ALICE, -600, 600, 1_000, 1_000)?;
            p.mint(ALICE, 600, -600, 1, 1_000)
        });
        assert!(r.is_err());
        assert_eq!(p, before);
    }

    #[test]
    fn staked_liquidity_routes_fees_to_gauge() {
        let mut p = pool();
        let gauge = [9u8; 32];
        let nft = [8u8; 32];
        p.set_gauge_and_position_manager(gauge, nft).unwrap();
        p.mint(nft, -600, 600, 10u128.pow(18), 1_000).unwrap();
        p.stake(10i128.pow(18), -600, 600, true, 1_000).unwrap();
        assert_eq!(p.staked_liquidity, 10u128.pow(18));
        p.swap(
            true,
            I256::try_from(10u64.pow(15)).unwrap(),
            tick_math::MIN_SQRT_RATIO + U256::from(1),
            3000,
            100_000,
            1_010,
        )
        .unwrap();
        // fully staked: all fees go to the gauge, none to fee growth
        assert!(p.gauge_fees0 > 0);
        assert_eq!(p.fee_growth_global0_x128, U256::ZERO);
    }
}
