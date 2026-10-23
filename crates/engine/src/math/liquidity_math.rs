//! Port of `LiquidityMath.sol`.

use crate::error::{MathError, Result};

/// Adds a signed liquidity delta to liquidity, reverting on overflow or underflow.
pub fn add_delta(x: u128, y: i128) -> Result<u128> {
    if y < 0 {
        let z = x.wrapping_sub(y.unsigned_abs());
        if z < x {
            Ok(z)
        } else {
            Err(MathError::LiquiditySub)
        }
    } else {
        let z = x.wrapping_add(y as u128);
        if z >= x {
            Ok(z)
        } else {
            Err(MathError::LiquidityAdd)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_delta_cases() {
        assert_eq!(add_delta(1, 0), Ok(1));
        assert_eq!(add_delta(1, -1), Ok(0));
        assert_eq!(add_delta(1, 1), Ok(2));
        assert_eq!(add_delta(u128::MAX - 14, 15), Err(MathError::LiquidityAdd));
        assert_eq!(add_delta(3, -4), Err(MathError::LiquiditySub));
        assert_eq!(add_delta(0, i128::MIN), Err(MathError::LiquiditySub));
    }
}
