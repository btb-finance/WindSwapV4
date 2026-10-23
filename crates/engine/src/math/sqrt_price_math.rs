//! Port of `SqrtPriceMath.sol`.
//!
//! Solidity 0.7 arithmetic does not check overflow, so the wrapping operations
//! below mirror the original code exactly; `checked_*` mirrors `LowGasSafeMath`.

use alloy_primitives::{I256, U256};

use super::full_math::{mul_div, mul_div_rounding_up};
use super::unsafe_math::div_rounding_up;
use super::{to_u160, truncate_u160, MAX_U160, Q96, RESOLUTION};
use crate::error::{MathError, Result};

/// Next sqrt price given a delta of token0, always rounding up.
pub fn get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_px96: U256,
    liquidity: u128,
    amount: U256,
    add: bool,
) -> Result<U256> {
    if amount.is_zero() {
        return Ok(sqrt_px96);
    }
    let numerator1 = U256::from(liquidity) << RESOLUTION;

    if add {
        let product = amount.wrapping_mul(sqrt_px96);
        if product / amount == sqrt_px96 {
            let denominator = numerator1.wrapping_add(product);
            if denominator >= numerator1 {
                // always fits in 160 bits
                return Ok(truncate_u160(mul_div_rounding_up(
                    numerator1,
                    sqrt_px96,
                    denominator,
                )?));
            }
        }
        if sqrt_px96.is_zero() {
            return Err(MathError::DivisionByZero);
        }
        let denominator = (numerator1 / sqrt_px96)
            .checked_add(amount)
            .ok_or(MathError::Overflow)?;
        Ok(truncate_u160(div_rounding_up(numerator1, denominator)))
    } else {
        let product = amount.wrapping_mul(sqrt_px96);
        if product / amount != sqrt_px96 || numerator1 <= product {
            return Err(MathError::PriceOutOfBounds);
        }
        let denominator = numerator1 - product;
        to_u160(mul_div_rounding_up(numerator1, sqrt_px96, denominator)?)
    }
}

/// Next sqrt price given a delta of token1, always rounding down.
pub fn get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_px96: U256,
    liquidity: u128,
    amount: U256,
    add: bool,
) -> Result<U256> {
    let liquidity = U256::from(liquidity);
    if add {
        let quotient = if amount <= MAX_U160 {
            if liquidity.is_zero() {
                return Err(MathError::DivisionByZero);
            }
            (amount << RESOLUTION) / liquidity
        } else {
            mul_div(amount, Q96, liquidity)?
        };
        to_u160(sqrt_px96.checked_add(quotient).ok_or(MathError::Overflow)?)
    } else {
        let quotient = if amount <= MAX_U160 {
            div_rounding_up(amount << RESOLUTION, liquidity)
        } else {
            mul_div_rounding_up(amount, Q96, liquidity)?
        };
        if sqrt_px96 <= quotient {
            return Err(MathError::PriceOutOfBounds);
        }
        Ok(truncate_u160(sqrt_px96 - quotient))
    }
}

/// Next sqrt price given an input amount of token0 or token1.
pub fn get_next_sqrt_price_from_input(
    sqrt_px96: U256,
    liquidity: u128,
    amount_in: U256,
    zero_for_one: bool,
) -> Result<U256> {
    if sqrt_px96.is_zero() || liquidity == 0 {
        return Err(MathError::InvalidInput);
    }
    if zero_for_one {
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_px96, liquidity, amount_in, true)
    } else {
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_px96, liquidity, amount_in, true)
    }
}

/// Next sqrt price given an output amount of token0 or token1.
pub fn get_next_sqrt_price_from_output(
    sqrt_px96: U256,
    liquidity: u128,
    amount_out: U256,
    zero_for_one: bool,
) -> Result<U256> {
    if sqrt_px96.is_zero() || liquidity == 0 {
        return Err(MathError::InvalidInput);
    }
    if zero_for_one {
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_px96, liquidity, amount_out, false)
    } else {
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_px96, liquidity, amount_out, false)
    }
}

/// Amount of token0 between two prices: `liquidity / sqrt(lower) - liquidity / sqrt(upper)`.
pub fn get_amount0_delta(
    mut sqrt_ratio_ax96: U256,
    mut sqrt_ratio_bx96: U256,
    liquidity: u128,
    round_up: bool,
) -> Result<U256> {
    if sqrt_ratio_ax96 > sqrt_ratio_bx96 {
        core::mem::swap(&mut sqrt_ratio_ax96, &mut sqrt_ratio_bx96);
    }
    let numerator1 = U256::from(liquidity) << RESOLUTION;
    let numerator2 = sqrt_ratio_bx96 - sqrt_ratio_ax96;
    if sqrt_ratio_ax96.is_zero() {
        return Err(MathError::InvalidInput);
    }
    if round_up {
        Ok(div_rounding_up(
            mul_div_rounding_up(numerator1, numerator2, sqrt_ratio_bx96)?,
            sqrt_ratio_ax96,
        ))
    } else {
        Ok(mul_div(numerator1, numerator2, sqrt_ratio_bx96)? / sqrt_ratio_ax96)
    }
}

/// Amount of token1 between two prices: `liquidity * (sqrt(upper) - sqrt(lower))`.
pub fn get_amount1_delta(
    mut sqrt_ratio_ax96: U256,
    mut sqrt_ratio_bx96: U256,
    liquidity: u128,
    round_up: bool,
) -> Result<U256> {
    if sqrt_ratio_ax96 > sqrt_ratio_bx96 {
        core::mem::swap(&mut sqrt_ratio_ax96, &mut sqrt_ratio_bx96);
    }
    let diff = sqrt_ratio_bx96 - sqrt_ratio_ax96;
    if round_up {
        mul_div_rounding_up(U256::from(liquidity), diff, Q96)
    } else {
        mul_div(U256::from(liquidity), diff, Q96)
    }
}

/// `SafeCast.toInt256`
fn to_i256(x: U256) -> Result<I256> {
    I256::try_from(x).map_err(|_| MathError::Overflow)
}

/// Signed token0 delta: positive (rounded up) when adding liquidity, negative (rounded down) when removing.
pub fn get_amount0_delta_signed(
    sqrt_ratio_ax96: U256,
    sqrt_ratio_bx96: U256,
    liquidity: i128,
) -> Result<I256> {
    if liquidity < 0 {
        Ok(-to_i256(get_amount0_delta(
            sqrt_ratio_ax96,
            sqrt_ratio_bx96,
            liquidity.unsigned_abs(),
            false,
        )?)?)
    } else {
        to_i256(get_amount0_delta(
            sqrt_ratio_ax96,
            sqrt_ratio_bx96,
            liquidity as u128,
            true,
        )?)
    }
}

/// Signed token1 delta: positive (rounded up) when adding liquidity, negative (rounded down) when removing.
pub fn get_amount1_delta_signed(
    sqrt_ratio_ax96: U256,
    sqrt_ratio_bx96: U256,
    liquidity: i128,
) -> Result<I256> {
    if liquidity < 0 {
        Ok(-to_i256(get_amount1_delta(
            sqrt_ratio_ax96,
            sqrt_ratio_bx96,
            liquidity.unsigned_abs(),
            false,
        )?)?)
    } else {
        to_i256(get_amount1_delta(
            sqrt_ratio_ax96,
            sqrt_ratio_bx96,
            liquidity as u128,
            true,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e18(n: u64) -> U256 {
        U256::from(n) * U256::from(10u64.pow(18))
    }

    fn encode_price_sqrt(reserve1: u64, reserve0: u64) -> U256 {
        // sqrt(reserve1 / reserve0) * 2^96, for exact squares used in tests
        let r = ((reserve1 as f64) / (reserve0 as f64)).sqrt();
        assert_eq!(r.fract(), 0.0);
        Q96 * U256::from(r as u64)
    }

    #[test]
    fn next_price_from_input_reverts() {
        assert!(get_next_sqrt_price_from_input(U256::ZERO, 0, U256::from(1), false).is_err());
        assert!(get_next_sqrt_price_from_input(U256::from(1), 0, U256::from(1), true).is_err());
        // overflow on price
        assert!(get_next_sqrt_price_from_input(MAX_U160, 1024, U256::from(1024), false).is_err());
    }

    #[test]
    fn next_price_from_input_zero_amount() {
        let price = encode_price_sqrt(1, 1);
        assert_eq!(
            get_next_sqrt_price_from_input(price, 10u128.pow(17), U256::ZERO, true),
            Ok(price)
        );
        assert_eq!(
            get_next_sqrt_price_from_input(price, 10u128.pow(17), U256::ZERO, false),
            Ok(price)
        );
    }

    #[test]
    fn next_price_from_input_known_values() {
        let price = encode_price_sqrt(1, 1);
        let liq = 10u128.pow(18);
        assert_eq!(
            get_next_sqrt_price_from_input(price, liq, e18(1) / U256::from(10), false),
            Ok("87150978765690771352898345369".parse().unwrap())
        );
        assert_eq!(
            get_next_sqrt_price_from_input(price, liq, e18(1) / U256::from(10), true),
            Ok("72025602285694852357767227579".parse().unwrap())
        );
    }

    #[test]
    fn amount_deltas() {
        let p1 = encode_price_sqrt(1, 1);
        assert_eq!(get_amount0_delta(p1, p1, 0, true), Ok(U256::ZERO));
        assert_eq!(get_amount1_delta(p1, p1, 0, true), Ok(U256::ZERO));
        // price 1 -> 1.21 (sqrt 1.1), liquidity 1e18
        let p121: U256 = "87150978765690771352898345369".parse().unwrap();
        let a0 = get_amount0_delta(p1, p121, 10u128.pow(18), true).unwrap();
        assert_eq!(a0, "90909090909090910".parse::<U256>().unwrap());
        let a0_down = get_amount0_delta(p1, p121, 10u128.pow(18), false).unwrap();
        assert_eq!(a0_down, a0 - U256::from(1));
        let a1 = get_amount1_delta(p1, p121, 10u128.pow(18), true).unwrap();
        assert_eq!(a1, "100000000000000000".parse::<U256>().unwrap());
        let a1_down = get_amount1_delta(p1, p121, 10u128.pow(18), false).unwrap();
        assert_eq!(a1_down, a1 - U256::from(1));
    }
}
