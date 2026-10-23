//! Port of `Oracle.sol`: the observation ring buffer that pools use for time-weighted
//! tick and in-range liquidity accumulators.
//!
//! Solidity 0.7 does not check overflow, so each operation mirrors the width of the
//! original type: `uint32` timestamps wrap modulo 2^32 (see [`lte`]), `int56` tick
//! accumulators wrap modulo 2^56 and are sign-extended, `uint160` seconds-per-liquidity
//! accumulators wrap modulo 2^160, and `uint16` indices and cardinalities wrap modulo 2^16.
//!
//! The Solidity array has 65535 slots. Here it is a sparse map: a missing index is the
//! zeroed (uninitialized) observation, so the state stays proportional to the cardinality.

use std::collections::BTreeMap;

use alloy_primitives::U256;

use crate::error::{MathError, Result};
use crate::math::truncate_u160;

/// `Oracle.Observation`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "borsh",
    derive(borsh::BorshSerialize, borsh::BorshDeserialize)
)]
pub struct Observation {
    /// `uint32 blockTimestamp`.
    pub block_timestamp: u32,
    /// `int56 tickCumulative`, stored sign-extended.
    pub tick_cumulative: i64,
    /// `uint160 secondsPerLiquidityCumulativeX128`.
    pub seconds_per_liquidity_cumulative_x128: U256,
    /// `bool initialized`.
    pub initialized: bool,
}

/// Sparse `Observation[65535]`: a missing index is `Observation::default()`.
pub type Observations = BTreeMap<u16, Observation>;

const TWO_POW_32: u64 = 1 << 32;

fn get(observations: &Observations, index: u16) -> Observation {
    observations.get(&index).copied().unwrap_or_default()
}

/// Keeps the low 56 bits of `x`, sign-extended (Solidity `int56` wrap-around).
fn wrap_i56(x: i64) -> i64 {
    (x << 8) >> 8
}

/// `transform`: advances `last` to `block_timestamp` at the given tick and liquidity.
fn transform(last: Observation, block_timestamp: u32, tick: i32, liquidity: u128) -> Observation {
    let delta = block_timestamp.wrapping_sub(last.block_timestamp);
    let tick_delta = i64::from(tick) * i64::from(delta);
    let divisor = if liquidity > 0 {
        U256::from(liquidity)
    } else {
        U256::from(1_u8)
    };
    let spl_delta = truncate_u160(U256::from(delta) << 128_usize) / divisor;
    Observation {
        block_timestamp,
        tick_cumulative: wrap_i56(last.tick_cumulative.wrapping_add(tick_delta)),
        seconds_per_liquidity_cumulative_x128: truncate_u160(
            last.seconds_per_liquidity_cumulative_x128
                .wrapping_add(spl_delta),
        ),
        initialized: true,
    }
}

/// Wrap-aware `a <= b` for timestamps that are chronologically at or before `time`.
fn lte(time: u32, a: u32, b: u32) -> bool {
    if a <= time && b <= time {
        return a <= b;
    }
    let a_adjusted = if a > time {
        u64::from(a)
    } else {
        u64::from(a) + TWO_POW_32
    };
    let b_adjusted = if b > time {
        u64::from(b)
    } else {
        u64::from(b) + TWO_POW_32
    };
    a_adjusted <= b_adjusted
}

/// `Oracle.initialize`: writes the first slot. Returns `(cardinality, cardinalityNext)`.
pub fn initialize(observations: &mut Observations, time: u32) -> (u16, u16) {
    observations.insert(
        0,
        Observation {
            block_timestamp: time,
            tick_cumulative: 0,
            seconds_per_liquidity_cumulative_x128: U256::ZERO,
            initialized: true,
        },
    );
    (1, 1)
}

/// `Oracle.write`: records an observation, at most once per block.
/// Returns `(indexUpdated, cardinalityUpdated)`. Reverts (division by zero) when
/// `cardinality` is zero; state is left unchanged in that case.
pub fn write(
    observations: &mut Observations,
    index: u16,
    block_timestamp: u32,
    tick: i32,
    liquidity: u128,
    cardinality: u16,
    cardinality_next: u16,
) -> Result<(u16, u16)> {
    let last = get(observations, index);
    // early return if an observation was already written this block
    if last.block_timestamp == block_timestamp {
        return Ok((index, cardinality));
    }
    let cardinality_updated =
        if cardinality_next > cardinality && index == cardinality.wrapping_sub(1) {
            cardinality_next
        } else {
            cardinality
        };
    let index_updated = index
        .wrapping_add(1)
        .checked_rem(cardinality_updated)
        .ok_or(MathError::DivisionByZero)?;
    observations.insert(
        index_updated,
        transform(last, block_timestamp, tick, liquidity),
    );
    Ok((index_updated, cardinality_updated))
}

/// `Oracle.grow`: marks slots `current..next` as available (timestamp 1, uninitialized).
/// Returns the new cardinality next. Reverts with `"I"` when `current` is zero.
pub fn grow(observations: &mut Observations, current: u16, next: u16) -> Result<u16> {
    if current == 0 {
        return Err(MathError::OracleNotInitialized);
    }
    if next <= current {
        return Ok(current);
    }
    for slot in current..next {
        observations.entry(slot).or_default().block_timestamp = 1;
    }
    Ok(next)
}

/// `Oracle.binarySearch`. The caller guarantees `cardinality > 0` and that the target lies
/// inside the stored range. Index arithmetic is done in `u64` (Solidity `uint256`); the only
/// place it could underflow, `r = i - 1` at `i == 0`, is unreachable under those preconditions.
fn binary_search(
    observations: &Observations,
    time: u32,
    target: u32,
    index: u16,
    cardinality: u16,
) -> (Observation, Observation) {
    let card = u64::from(cardinality);
    // oldest observation
    let mut l = u64::from(index.wrapping_add(1) % cardinality);
    // newest observation
    let mut r = l + card - 1;
    loop {
        let i = l.wrapping_add(r) / 2;

        let before_or_at = get(observations, (i % card) as u16);

        // landed on an uninitialized slot: keep searching towards newer observations
        if !before_or_at.initialized {
            l = i.wrapping_add(1);
            continue;
        }

        let at_or_after = get(observations, ((i.wrapping_add(1)) % card) as u16);

        let target_at_or_after = lte(time, before_or_at.block_timestamp, target);

        // found the answer
        if target_at_or_after && lte(time, target, at_or_after.block_timestamp) {
            return (before_or_at, at_or_after);
        }

        if target_at_or_after {
            l = i.wrapping_add(1);
        } else {
            r = i.wrapping_sub(1);
        }
    }
}

/// `Oracle.getSurroundingObservations`: the observations at or before and at or after `target`.
fn get_surrounding_observations(
    observations: &Observations,
    time: u32,
    target: u32,
    tick: i32,
    index: u16,
    liquidity: u128,
    cardinality: u16,
) -> Result<(Observation, Observation)> {
    // optimistically assume the newest observation
    let newest = get(observations, index);

    // the target is at or after the newest observation: no search needed
    if lte(time, newest.block_timestamp, target) {
        if newest.block_timestamp == target {
            return Ok((newest, Observation::default()));
        }
        return Ok((newest, transform(newest, target, tick, liquidity)));
    }

    // otherwise the oldest observation (falling back to slot 0 when that slot is uninitialized)
    let oldest_slot = index
        .wrapping_add(1)
        .checked_rem(cardinality)
        .ok_or(MathError::DivisionByZero)?;
    let mut oldest = get(observations, oldest_slot);
    if !oldest.initialized {
        oldest = get(observations, 0);
    }
    if !lte(time, oldest.block_timestamp, target) {
        return Err(MathError::ObservationTooOld);
    }

    Ok(binary_search(
        observations,
        time,
        target,
        index,
        cardinality,
    ))
}

/// `Oracle.observeSingle`: the tick and seconds-per-liquidity accumulators `seconds_ago`
/// seconds before `time`, interpolating between the surrounding observations.
/// Reverts with `"OLD"` (`ObservationTooOld`) when the target predates the oldest observation.
pub fn observe_single(
    observations: &Observations,
    time: u32,
    seconds_ago: u32,
    tick: i32,
    index: u16,
    liquidity: u128,
    cardinality: u16,
) -> Result<(i64, U256)> {
    if seconds_ago == 0 {
        let mut last = get(observations, index);
        if last.block_timestamp != time {
            last = transform(last, time, tick, liquidity);
        }
        return Ok((
            last.tick_cumulative,
            last.seconds_per_liquidity_cumulative_x128,
        ));
    }

    let target = time.wrapping_sub(seconds_ago);
    let (before, after) = get_surrounding_observations(
        observations,
        time,
        target,
        tick,
        index,
        liquidity,
        cardinality,
    )?;

    if target == before.block_timestamp {
        // at the left boundary
        return Ok((
            before.tick_cumulative,
            before.seconds_per_liquidity_cumulative_x128,
        ));
    }
    if target == after.block_timestamp {
        // at the right boundary
        return Ok((
            after.tick_cumulative,
            after.seconds_per_liquidity_cumulative_x128,
        ));
    }

    // in the middle: interpolate
    let observation_time_delta = after.block_timestamp.wrapping_sub(before.block_timestamp);
    let target_delta = target.wrapping_sub(before.block_timestamp);
    if observation_time_delta == 0 {
        return Err(MathError::DivisionByZero);
    }
    let divisor = i64::from(observation_time_delta);

    let tick_quotient =
        wrap_i56(after.tick_cumulative.wrapping_sub(before.tick_cumulative)) / divisor;
    let tick_step = wrap_i56(tick_quotient.wrapping_mul(i64::from(target_delta)));
    let tick_cumulative = wrap_i56(before.tick_cumulative.wrapping_add(tick_step));

    let spl_diff = truncate_u160(
        after
            .seconds_per_liquidity_cumulative_x128
            .wrapping_sub(before.seconds_per_liquidity_cumulative_x128),
    );
    let spl_quotient =
        truncate_u160((spl_diff * U256::from(target_delta)) / U256::from(observation_time_delta));
    let seconds_per_liquidity = truncate_u160(
        before
            .seconds_per_liquidity_cumulative_x128
            .wrapping_add(spl_quotient),
    );

    Ok((tick_cumulative, seconds_per_liquidity))
}

/// `Oracle.observe`: accumulator values for each entry of `seconds_agos`.
/// Reverts with `"I"` (`OracleNotInitialized`) when `cardinality` is zero, and fails as a whole
/// if any entry fails.
pub fn observe(
    observations: &Observations,
    time: u32,
    seconds_agos: &[u32],
    tick: i32,
    index: u16,
    liquidity: u128,
    cardinality: u16,
) -> Result<(Vec<i64>, Vec<U256>)> {
    if cardinality == 0 {
        return Err(MathError::OracleNotInitialized);
    }
    let mut tick_cumulatives = Vec::with_capacity(seconds_agos.len());
    let mut seconds_per_liquidity = Vec::with_capacity(seconds_agos.len());
    for &seconds_ago in seconds_agos {
        let (tick_cumulative, spl) = observe_single(
            observations,
            time,
            seconds_ago,
            tick,
            index,
            liquidity,
            cardinality,
        )?;
        tick_cumulatives.push(tick_cumulative);
        seconds_per_liquidity.push(spl);
    }
    Ok((tick_cumulatives, seconds_per_liquidity))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_i56_sign_extends() {
        assert_eq!(wrap_i56((1 << 55) - 1), (1 << 55) - 1);
        assert_eq!(wrap_i56(1 << 55), -(1 << 55));
        assert_eq!(wrap_i56(-1), -1);
    }

    #[test]
    fn lte_handles_wraparound() {
        assert!(lte(10, 5, 7));
        assert!(!lte(10, 7, 5));
        // time has wrapped to 2; u32::MAX - 10 is 12 seconds before it, 1 is 1 second before it
        assert!(lte(2, u32::MAX - 10, 1));
        assert!(!lte(2, 1, u32::MAX - 10));
    }

    #[test]
    fn uninitialized_cardinality_zero_errors() {
        let obs = Observations::new();
        assert_eq!(
            observe(&obs, 100, &[0], 0, 0, 1, 0),
            Err(MathError::OracleNotInitialized)
        );
        let mut obs = Observations::new();
        assert_eq!(grow(&mut obs, 0, 4), Err(MathError::OracleNotInitialized));
    }
}
