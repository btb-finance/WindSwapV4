//! Port of `Tick.sol` (Wind Swap fork of the Uniswap v3 tick library).
//!
//! Solidity 0.7.6 arithmetic is unchecked, so every subtraction that wraps in the original is
//! written with an explicit wrapping op at the width of the Solidity type: `U256` for
//! `uint256`, `truncate_u160` for `uint160`, `i64` sign-extended from 56 bits for `int56`, and
//! `u32` for `uint32`. Checked operations appear only where the Solidity uses `SafeCast` or
//! `LiquidityMath`.
//!
//! Fallible functions compute the new tick first and write it back only on success, so an
//! `Err` leaves the map untouched, matching the revert semantics of the EVM.
//!
//! A tick missing from [`Ticks`] is the zeroed `Tick.Info`, as in an unset storage mapping slot.

use std::collections::BTreeMap;

use alloy_primitives::U256;

use crate::error::{MathError, Result};
use crate::math::liquidity_math::add_delta;
use crate::math::tick_math::{MAX_TICK, MIN_TICK};
use crate::math::truncate_u160;

/// Mirrors `Tick.Info`, fields in declaration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "borsh",
    derive(borsh::BorshSerialize, borsh::BorshDeserialize)
)]
pub struct TickInfo {
    /// `uint128 liquidityGross`
    pub liquidity_gross: u128,
    /// `int128 liquidityNet`
    pub liquidity_net: i128,
    /// `int128 stakedLiquidityNet`
    pub staked_liquidity_net: i128,
    /// `uint256 feeGrowthOutside0X128`
    pub fee_growth_outside0_x128: U256,
    /// `uint256 feeGrowthOutside1X128`
    pub fee_growth_outside1_x128: U256,
    /// `uint256 rewardGrowthOutsideX128`
    pub reward_growth_outside_x128: U256,
    /// `int56 tickCumulativeOutside`
    pub tick_cumulative_outside: i64,
    /// `uint160 secondsPerLiquidityOutsideX128`
    pub seconds_per_liquidity_outside_x128: U256,
    /// `uint32 secondsOutside`
    pub seconds_outside: u32,
    /// `bool initialized`
    pub initialized: bool,
}

/// Initialized ticks by tick index. A missing key is the default (zeroed) tick.
pub type Ticks = BTreeMap<i32, TickInfo>;

/// Mirrors `Tick.LiquidityNets`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiquidityNets {
    pub liquidity_net: i128,
    pub staked_liquidity_net: i128,
}

/// `int56` arithmetic wraps at 56 bits: keep the low 56 bits and sign-extend them.
#[inline]
fn wrap_i56(x: i64) -> i64 {
    (x << 8) >> 8
}

/// `SafeCast.toInt128(int256(net) +/- delta)`. The `int256` arithmetic in the Solidity cannot
/// overflow for `int128` inputs, so only the final cast can revert.
#[inline]
fn apply_net(net: i128, delta: i128, upper: bool) -> Result<i128> {
    let v = if upper {
        net.checked_sub(delta)
    } else {
        net.checked_add(delta)
    };
    v.ok_or(MathError::Overflow)
}

/// `tickSpacingToMaxLiquidityPerTick(int24 tickSpacing)`.
///
/// Reverts (`Err`) on a zero spacing, and on the wrap case where the uint24 tick count is zero.
pub fn tick_spacing_to_max_liquidity_per_tick(tick_spacing: i32) -> Result<u128> {
    if tick_spacing == 0 {
        return Err(MathError::DivisionByZero);
    }
    let min_tick = (MIN_TICK / tick_spacing) * tick_spacing;
    let max_tick = (MAX_TICK / tick_spacing) * tick_spacing;
    // `uint24(int24)` keeps the low 24 bits, and `+ 1` wraps at 24 bits too.
    let span = ((max_tick - min_tick) / tick_spacing) as u32 & 0xff_ffff;
    let num_ticks = span.wrapping_add(1) & 0xff_ffff;
    if num_ticks == 0 {
        return Err(MathError::DivisionByZero);
    }
    Ok(u128::MAX / u128::from(num_ticks))
}

/// `getFeeGrowthInside`. Read-only, and the Solidity cannot revert (all subtractions wrap).
pub fn get_fee_growth_inside(
    ticks: &Ticks,
    tick_lower: i32,
    tick_upper: i32,
    tick_current: i32,
    fee_growth_global0_x128: U256,
    fee_growth_global1_x128: U256,
) -> (U256, U256) {
    let default = TickInfo::default();
    let lower = ticks.get(&tick_lower).unwrap_or(&default);
    let upper = ticks.get(&tick_upper).unwrap_or(&default);
    let g0 = fee_growth_global0_x128;
    let g1 = fee_growth_global1_x128;

    let (below0, below1) = if tick_current >= tick_lower {
        (
            lower.fee_growth_outside0_x128,
            lower.fee_growth_outside1_x128,
        )
    } else {
        (
            g0.wrapping_sub(lower.fee_growth_outside0_x128),
            g1.wrapping_sub(lower.fee_growth_outside1_x128),
        )
    };

    let (above0, above1) = if tick_current < tick_upper {
        (
            upper.fee_growth_outside0_x128,
            upper.fee_growth_outside1_x128,
        )
    } else {
        (
            g0.wrapping_sub(upper.fee_growth_outside0_x128),
            g1.wrapping_sub(upper.fee_growth_outside1_x128),
        )
    };

    (
        g0.wrapping_sub(below0).wrapping_sub(above0),
        g1.wrapping_sub(below1).wrapping_sub(above1),
    )
}

/// `getRewardGrowthInside`. Read-only, and the Solidity cannot revert.
pub fn get_reward_growth_inside(
    ticks: &Ticks,
    tick_lower: i32,
    tick_upper: i32,
    tick_current: i32,
    reward_growth_global_x128: U256,
) -> U256 {
    let default = TickInfo::default();
    let lower = ticks.get(&tick_lower).unwrap_or(&default);
    let upper = ticks.get(&tick_upper).unwrap_or(&default);
    let g = reward_growth_global_x128;

    let below = if tick_current >= tick_lower {
        lower.reward_growth_outside_x128
    } else {
        g.wrapping_sub(lower.reward_growth_outside_x128)
    };

    let above = if tick_current < tick_upper {
        upper.reward_growth_outside_x128
    } else {
        g.wrapping_sub(upper.reward_growth_outside_x128)
    };

    g.wrapping_sub(below).wrapping_sub(above)
}

/// `update`: adds `liquidity_delta` to the tick's gross liquidity and updates the net liquidity.
/// Returns whether the tick flipped between initialized and uninitialized.
///
/// Reverts with `LiquidityAdd`/`LiquiditySub` (from `addDelta`), `LiquidityOverflow` (`"LO"`),
/// or `Overflow` if `liquidityNet` does not fit in `int128`.
#[allow(clippy::too_many_arguments)]
pub fn update(
    ticks: &mut Ticks,
    tick: i32,
    tick_current: i32,
    liquidity_delta: i128,
    fee_growth_global0_x128: U256,
    fee_growth_global1_x128: U256,
    seconds_per_liquidity_cumulative_x128: U256,
    tick_cumulative: i64,
    time: u32,
    upper: bool,
    max_liquidity: u128,
) -> Result<bool> {
    let mut info = ticks.get(&tick).cloned().unwrap_or_default();

    let liquidity_gross_before = info.liquidity_gross;
    let liquidity_gross_after = add_delta(liquidity_gross_before, liquidity_delta)?;

    if liquidity_gross_after > max_liquidity {
        return Err(MathError::LiquidityOverflow);
    }

    let flipped = (liquidity_gross_after == 0) != (liquidity_gross_before == 0);

    if liquidity_gross_before == 0 {
        // by convention, all growth before a tick was initialized happened below the tick
        if tick <= tick_current {
            info.fee_growth_outside0_x128 = fee_growth_global0_x128;
            info.fee_growth_outside1_x128 = fee_growth_global1_x128;
            info.seconds_per_liquidity_outside_x128 = seconds_per_liquidity_cumulative_x128;
            info.tick_cumulative_outside = tick_cumulative;
            info.seconds_outside = time;
        }
        info.initialized = true;
    }

    info.liquidity_gross = liquidity_gross_after;
    info.liquidity_net = apply_net(info.liquidity_net, liquidity_delta, upper)?;

    ticks.insert(tick, info);
    Ok(flipped)
}

/// `updateStake`: adds or removes staked liquidity on the tick's net staked liquidity.
///
/// Reverts with `Overflow` if the result does not fit in `int128`.
pub fn update_stake(
    ticks: &mut Ticks,
    tick: i32,
    staked_liquidity_delta: i128,
    upper: bool,
) -> Result<()> {
    let mut info = ticks.get(&tick).cloned().unwrap_or_default();
    info.staked_liquidity_net =
        apply_net(info.staked_liquidity_net, staked_liquidity_delta, upper)?;
    ticks.insert(tick, info);
    Ok(())
}

/// `clear`: resets the tick to the zeroed default.
pub fn clear(ticks: &mut Ticks, tick: i32) {
    ticks.remove(&tick);
}

/// `cross`: flips the outside accumulators of the tick when price crosses it, and returns the
/// net liquidity of the tick. The Solidity has no revert path, so this is infallible.
#[allow(clippy::too_many_arguments)]
pub fn cross(
    ticks: &mut Ticks,
    tick: i32,
    fee_growth_global0_x128: U256,
    fee_growth_global1_x128: U256,
    seconds_per_liquidity_cumulative_x128: U256,
    tick_cumulative: i64,
    time: u32,
    reward_growth_global_x128: U256,
) -> LiquidityNets {
    let mut info = ticks.get(&tick).cloned().unwrap_or_default();

    info.fee_growth_outside0_x128 =
        fee_growth_global0_x128.wrapping_sub(info.fee_growth_outside0_x128);
    info.fee_growth_outside1_x128 =
        fee_growth_global1_x128.wrapping_sub(info.fee_growth_outside1_x128);
    info.reward_growth_outside_x128 =
        reward_growth_global_x128.wrapping_sub(info.reward_growth_outside_x128);
    info.seconds_per_liquidity_outside_x128 = truncate_u160(
        seconds_per_liquidity_cumulative_x128.wrapping_sub(info.seconds_per_liquidity_outside_x128),
    );
    info.tick_cumulative_outside =
        wrap_i56(tick_cumulative.wrapping_sub(info.tick_cumulative_outside));
    info.seconds_outside = time.wrapping_sub(info.seconds_outside);

    let nets = LiquidityNets {
        liquidity_net: info.liquidity_net,
        staked_liquidity_net: info.staked_liquidity_net,
    };
    ticks.insert(tick, info);
    nets
}
