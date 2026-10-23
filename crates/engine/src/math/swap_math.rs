//! Port of `SwapMath.sol`: a single step of a swap within one tick range.

use alloy_primitives::{I256, U256};

use super::full_math::{mul_div, mul_div_rounding_up};
use super::sqrt_price_math::{
    get_amount0_delta, get_amount1_delta, get_next_sqrt_price_from_input,
    get_next_sqrt_price_from_output,
};
use crate::error::Result;

/// Fee denominator: fees are expressed in hundredths of a bip (1e-6).
pub const FEE_DENOMINATOR: u32 = 1_000_000;

/// Result of [`compute_swap_step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapStep {
    pub sqrt_ratio_next_x96: U256,
    pub amount_in: U256,
    pub amount_out: U256,
    pub fee_amount: U256,
}

/// Computes the result of swapping some amount in or out, given the swap parameters.
///
/// `amount_remaining` is positive for exact input and negative for exact output.
/// `fee_pips` is the fee in hundredths of a bip.
#[allow(clippy::nonminimal_bool)] // keeps the same shape as SwapMath.sol
pub fn compute_swap_step(
    sqrt_ratio_current_x96: U256,
    sqrt_ratio_target_x96: U256,
    liquidity: u128,
    amount_remaining: I256,
    fee_pips: u32,
) -> Result<SwapStep> {
    let zero_for_one = sqrt_ratio_current_x96 >= sqrt_ratio_target_x96;
    let exact_in = !amount_remaining.is_negative();
    // `1e6 - feePips` is uint24 arithmetic in Solidity 0.7
    let fee_complement = U256::from(FEE_DENOMINATOR.wrapping_sub(fee_pips) & 0xFF_FFFF);
    let fee = U256::from(fee_pips);
    let denom = U256::from(FEE_DENOMINATOR);
    let remaining_abs = amount_remaining.wrapping_neg().into_raw();

    let sqrt_ratio_next_x96;
    let mut amount_in = U256::ZERO;
    let mut amount_out = U256::ZERO;

    if exact_in {
        let amount_remaining_less_fee =
            mul_div(amount_remaining.into_raw(), fee_complement, denom)?;
        amount_in = if zero_for_one {
            get_amount0_delta(
                sqrt_ratio_target_x96,
                sqrt_ratio_current_x96,
                liquidity,
                true,
            )?
        } else {
            get_amount1_delta(
                sqrt_ratio_current_x96,
                sqrt_ratio_target_x96,
                liquidity,
                true,
            )?
        };
        sqrt_ratio_next_x96 = if amount_remaining_less_fee >= amount_in {
            sqrt_ratio_target_x96
        } else {
            get_next_sqrt_price_from_input(
                sqrt_ratio_current_x96,
                liquidity,
                amount_remaining_less_fee,
                zero_for_one,
            )?
        };
    } else {
        amount_out = if zero_for_one {
            get_amount1_delta(
                sqrt_ratio_target_x96,
                sqrt_ratio_current_x96,
                liquidity,
                false,
            )?
        } else {
            get_amount0_delta(
                sqrt_ratio_current_x96,
                sqrt_ratio_target_x96,
                liquidity,
                false,
            )?
        };
        sqrt_ratio_next_x96 = if remaining_abs >= amount_out {
            sqrt_ratio_target_x96
        } else {
            get_next_sqrt_price_from_output(
                sqrt_ratio_current_x96,
                liquidity,
                remaining_abs,
                zero_for_one,
            )?
        };
    }

    let max = sqrt_ratio_target_x96 == sqrt_ratio_next_x96;

    // get the input/output amounts
    if zero_for_one {
        if !(max && exact_in) {
            amount_in =
                get_amount0_delta(sqrt_ratio_next_x96, sqrt_ratio_current_x96, liquidity, true)?;
        }
        if !(max && !exact_in) {
            amount_out = get_amount1_delta(
                sqrt_ratio_next_x96,
                sqrt_ratio_current_x96,
                liquidity,
                false,
            )?;
        }
    } else {
        if !(max && exact_in) {
            amount_in =
                get_amount1_delta(sqrt_ratio_current_x96, sqrt_ratio_next_x96, liquidity, true)?;
        }
        if !(max && !exact_in) {
            amount_out = get_amount0_delta(
                sqrt_ratio_current_x96,
                sqrt_ratio_next_x96,
                liquidity,
                false,
            )?;
        }
    }

    // cap the output amount to not exceed the remaining output amount
    if !exact_in && amount_out > remaining_abs {
        amount_out = remaining_abs;
    }

    let fee_amount = if exact_in && sqrt_ratio_next_x96 != sqrt_ratio_target_x96 {
        // we didn't reach the target, so take the remainder of the maximum input as fee
        amount_remaining.into_raw().wrapping_sub(amount_in)
    } else {
        mul_div_rounding_up(amount_in, fee, fee_complement)?
    };

    Ok(SwapStep {
        sqrt_ratio_next_x96,
        amount_in,
        amount_out,
        fee_amount,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::sqrt_price_math::get_next_sqrt_price_from_input;
    use crate::math::Q96;

    fn i(x: &str) -> I256 {
        I256::from_dec_str(x).unwrap()
    }

    #[test]
    fn exact_in_fully_spent_one_for_zero() {
        let price = Q96;
        let target: U256 = "250541448375047931186413801569".parse().unwrap(); // ~1000
        let step = compute_swap_step(
            price,
            target,
            2 * 10u128.pow(18),
            i("1000000000000000000"),
            600,
        )
        .unwrap();
        assert_eq!(
            step.amount_in,
            "999400000000000000".parse::<U256>().unwrap()
        );
        assert_eq!(step.fee_amount, "600000000000000".parse::<U256>().unwrap());
        assert_eq!(
            step.amount_out,
            "666399946655997866".parse::<U256>().unwrap()
        );
        let expected =
            get_next_sqrt_price_from_input(price, 2 * 10u128.pow(18), step.amount_in, false)
                .unwrap();
        assert_eq!(step.sqrt_ratio_next_x96, expected);
    }

    #[test]
    fn amount_out_capped_at_desired() {
        let step = compute_swap_step(
            "417332158212080721273783715441582".parse().unwrap(),
            "1452870262520218020823638996".parse().unwrap(),
            159344665391607089467575320103,
            i("-1"),
            1,
        )
        .unwrap();
        assert_eq!(step.amount_in, U256::from(1));
        assert_eq!(step.fee_amount, U256::from(1));
        assert_eq!(step.amount_out, U256::from(1));
        assert_eq!(
            step.sqrt_ratio_next_x96,
            "417332158212080721273783715441581".parse::<U256>().unwrap()
        );
    }

    #[test]
    fn entire_input_used_for_fee() {
        let step = compute_swap_step(
            U256::from(2413),
            "79887613182836312".parse().unwrap(),
            1985041575832132834610021537970,
            i("10"),
            1872,
        )
        .unwrap();
        assert_eq!(step.amount_in, U256::ZERO);
        assert_eq!(step.fee_amount, U256::from(10));
        assert_eq!(step.amount_out, U256::ZERO);
        assert_eq!(step.sqrt_ratio_next_x96, U256::from(2413));
    }
}
