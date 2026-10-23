//! Port of `FullMath.sol`: 512-bit intermediate multiply-divide.

use alloy_primitives::ruint::UintTryFrom;
use alloy_primitives::{U256, U512};

use crate::error::{MathError, Result};

#[inline]
fn widen(x: U256) -> U512 {
    U512::from(x)
}

#[inline]
fn narrow(x: U512) -> Option<U256> {
    U256::uint_try_from(x).ok()
}

/// `floor(a * b / denominator)` with full precision.
/// Reverts if `denominator == 0` or the result overflows `uint256`.
pub fn mul_div(a: U256, b: U256, denominator: U256) -> Result<U256> {
    if denominator.is_zero() {
        return Err(MathError::DivisionByZero);
    }
    narrow(widen(a) * widen(b) / widen(denominator)).ok_or(MathError::Overflow)
}

/// `ceil(a * b / denominator)` with full precision.
/// Reverts if `denominator == 0` or the result overflows `uint256`.
pub fn mul_div_rounding_up(a: U256, b: U256, denominator: U256) -> Result<U256> {
    let result = mul_div(a, b, denominator)?;
    if !a.mul_mod(b, denominator).is_zero() {
        if result == U256::MAX {
            return Err(MathError::Overflow);
        }
        return Ok(result + U256::from(1));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Q128;

    #[test]
    fn reverts_on_zero_denominator() {
        assert_eq!(
            mul_div(Q128, Q128, U256::ZERO),
            Err(MathError::DivisionByZero)
        );
        assert_eq!(
            mul_div_rounding_up(Q128, Q128, U256::ZERO),
            Err(MathError::DivisionByZero)
        );
    }

    #[test]
    fn reverts_on_overflow() {
        assert_eq!(mul_div(Q128, Q128, U256::from(1)), Err(MathError::Overflow));
        assert_eq!(
            mul_div(U256::MAX, U256::MAX, U256::MAX - U256::from(1)),
            Err(MathError::Overflow)
        );
    }

    #[test]
    fn max_inputs() {
        assert_eq!(mul_div(U256::MAX, U256::MAX, U256::MAX), Ok(U256::MAX));
        assert_eq!(
            mul_div_rounding_up(U256::MAX, U256::MAX, U256::MAX),
            Ok(U256::MAX)
        );
    }

    #[test]
    fn accurate_without_phantom_overflow() {
        let result = Q128 / U256::from(3);
        let b = U256::from(50) * Q128 / U256::from(100);
        let d = U256::from(150) * Q128 / U256::from(100);
        assert_eq!(mul_div(Q128, b, d), Ok(result));
        assert_eq!(mul_div_rounding_up(Q128, b, d), Ok(result + U256::from(1)));
    }

    #[test]
    fn accurate_with_phantom_overflow() {
        let result = U256::from(4375) * Q128 / U256::from(1000);
        let b = U256::from(35) * Q128;
        let d = U256::from(8) * Q128;
        assert_eq!(mul_div(Q128, b, d), Ok(result));
        assert_eq!(mul_div_rounding_up(Q128, b, d), Ok(result));
    }

    #[test]
    fn rounding_up_reverts_when_result_is_max_and_rounds() {
        let a = "535006138814359".parse::<U256>().unwrap();
        let b = "432862656469423142931042426214547535783388063929571229938474969"
            .parse::<U256>()
            .unwrap();
        assert_eq!(
            mul_div_rounding_up(a, b, U256::from(2)),
            Err(MathError::Overflow)
        );
    }
}
