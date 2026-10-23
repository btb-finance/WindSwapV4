//! Port of `TickMath.sol`: converts between ticks and sqrt prices (Q64.96).

use alloy_primitives::{uint, I256, U256};

use crate::error::{MathError, Result};

/// Minimum tick: `log base 1.0001 of 2**-128`.
pub const MIN_TICK: i32 = -887272;
/// Maximum tick: `log base 1.0001 of 2**128`.
pub const MAX_TICK: i32 = -MIN_TICK;
/// `getSqrtRatioAtTick(MIN_TICK)`
pub const MIN_SQRT_RATIO: U256 = uint!(4295128739_U256);
/// `getSqrtRatioAtTick(MAX_TICK)`
pub const MAX_SQRT_RATIO: U256 = uint!(1461446703485210103287273052203988822378723970342_U256);

const TICK_FACTORS: [U256; 19] = [
    uint!(0xfff97272373d413259a46990580e213a_U256),
    uint!(0xfff2e50f5f656932ef12357cf3c7fdcc_U256),
    uint!(0xffe5caca7e10e4e61c3624eaa0941cd0_U256),
    uint!(0xffcb9843d60f6159c9db58835c926644_U256),
    uint!(0xff973b41fa98c081472e6896dfb254c0_U256),
    uint!(0xff2ea16466c96a3843ec78b326b52861_U256),
    uint!(0xfe5dee046a99a2a811c461f1969c3053_U256),
    uint!(0xfcbe86c7900a88aedcffc83b479aa3a4_U256),
    uint!(0xf987a7253ac413176f2b074cf7815e54_U256),
    uint!(0xf3392b0822b70005940c7a398e4b70f3_U256),
    uint!(0xe7159475a2c29b7443b29c7fa6e889d9_U256),
    uint!(0xd097f3bdfd2022b8845ad8f792aa5825_U256),
    uint!(0xa9f746462d870fdf8a65dc1f90e061e5_U256),
    uint!(0x70d869a156d2a1b890bb3df62baf32f7_U256),
    uint!(0x31be135f97d08fd981231505542fcfa6_U256),
    uint!(0x9aa508b5b7a84e1c677de54f3e99bc9_U256),
    uint!(0x5d6af8dedb81196699c329225ee604_U256),
    uint!(0x2216e584f5fa1ea926041bedfe98_U256),
    uint!(0x48a170391f7dc42444e8fa2_U256),
];

/// Calculates `sqrt(1.0001^tick) * 2^96`. Reverts if `|tick| > MAX_TICK`.
pub fn get_sqrt_ratio_at_tick(tick: i32) -> Result<U256> {
    let abs_tick = tick.unsigned_abs();
    if abs_tick > MAX_TICK as u32 {
        return Err(MathError::TickOutOfRange);
    }

    let mut ratio = if abs_tick & 0x1 != 0 {
        uint!(0xfffcb933bd6fad37aa2d162d1a594001_U256)
    } else {
        uint!(0x100000000000000000000000000000000_U256)
    };
    for (i, factor) in TICK_FACTORS.iter().enumerate() {
        if abs_tick & (0x2 << i) != 0 {
            ratio = (ratio * *factor) >> 128;
        }
    }

    if tick > 0 {
        ratio = U256::MAX / ratio;
    }

    // Divide by 1<<32 rounding up to go from Q128.128 to Q128.96.
    let round_up = if (ratio & U256::from(u32::MAX)).is_zero() {
        0
    } else {
        1
    };
    Ok((ratio >> 32) + U256::from(round_up))
}

/// Calculates the greatest tick such that `get_sqrt_ratio_at_tick(tick) <= sqrt_price_x96`.
/// Reverts if `sqrt_price_x96` is outside `[MIN_SQRT_RATIO, MAX_SQRT_RATIO)`.
pub fn get_tick_at_sqrt_ratio(sqrt_price_x96: U256) -> Result<i32> {
    if sqrt_price_x96 < MIN_SQRT_RATIO || sqrt_price_x96 >= MAX_SQRT_RATIO {
        return Err(MathError::SqrtPriceOutOfRange);
    }
    let ratio: U256 = sqrt_price_x96 << 32usize;

    let msb = 255 - ratio.leading_zeros();
    let mut r: U256 = if msb >= 128 {
        ratio >> (msb - 127)
    } else {
        ratio << (127 - msb)
    };

    // Integer part of log2 in Q64.64, then 14 bits of fractional precision.
    let mut log_2: i128 = (msb as i128 - 128) << 64;
    for shift in (50..=63).rev() {
        r = (r * r) >> 127usize;
        let f: u32 = if (r >> 128usize).is_zero() { 0 } else { 1 };
        log_2 |= (f as i128) << shift;
        r >>= f as usize;
    }

    // 128.128 number
    let log_sqrt10001 =
        I256::try_from(log_2).unwrap() * I256::from_dec_str("255738958999603826347141").unwrap();

    let tick_low = (log_sqrt10001
        - I256::from_dec_str("3402992956809132418596140100660247210").unwrap())
    .asr(128);
    let tick_hi = (log_sqrt10001
        + I256::from_dec_str("291339464771989622907027621153398088495").unwrap())
    .asr(128);
    let tick_low = i32::try_from(tick_low).map_err(|_| MathError::Overflow)?;
    let tick_hi = i32::try_from(tick_hi).map_err(|_| MathError::Overflow)?;

    if tick_low == tick_hi {
        Ok(tick_low)
    } else if get_sqrt_ratio_at_tick(tick_hi)? <= sqrt_price_x96 {
        Ok(tick_hi)
    } else {
        Ok(tick_low)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Q96;

    #[test]
    fn bounds() {
        assert_eq!(get_sqrt_ratio_at_tick(MIN_TICK), Ok(MIN_SQRT_RATIO));
        assert_eq!(get_sqrt_ratio_at_tick(MAX_TICK), Ok(MAX_SQRT_RATIO));
        assert_eq!(get_sqrt_ratio_at_tick(0), Ok(Q96));
        assert_eq!(
            get_sqrt_ratio_at_tick(MIN_TICK - 1),
            Err(MathError::TickOutOfRange)
        );
        assert_eq!(
            get_sqrt_ratio_at_tick(MAX_TICK + 1),
            Err(MathError::TickOutOfRange)
        );
    }

    #[test]
    fn tick_at_sqrt_ratio_bounds() {
        assert_eq!(get_tick_at_sqrt_ratio(MIN_SQRT_RATIO), Ok(MIN_TICK));
        assert_eq!(
            get_tick_at_sqrt_ratio(MIN_SQRT_RATIO + U256::from(1)),
            Ok(MIN_TICK)
        );
        assert_eq!(
            get_tick_at_sqrt_ratio(MAX_SQRT_RATIO - U256::from(1)),
            Ok(MAX_TICK - 1)
        );
        assert_eq!(
            get_tick_at_sqrt_ratio(MIN_SQRT_RATIO - U256::from(1)),
            Err(MathError::SqrtPriceOutOfRange)
        );
        assert_eq!(
            get_tick_at_sqrt_ratio(MAX_SQRT_RATIO),
            Err(MathError::SqrtPriceOutOfRange)
        );
    }

    #[test]
    fn round_trip() {
        let mut tick = MIN_TICK;
        while tick < MAX_TICK {
            let price = get_sqrt_ratio_at_tick(tick).unwrap();
            assert_eq!(get_tick_at_sqrt_ratio(price), Ok(tick), "tick {tick}");
            if price > MIN_SQRT_RATIO {
                assert_eq!(
                    get_tick_at_sqrt_ratio(price - U256::from(1)),
                    Ok(tick - 1),
                    "tick {tick} - 1 wei"
                );
            }
            tick += 997;
        }
    }
}
