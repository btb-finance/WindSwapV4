//! Port of `BitMath.sol`.

use alloy_primitives::U256;

use crate::error::{MathError, Result};

/// Index of the most significant set bit. Reverts on zero.
pub fn most_significant_bit(x: U256) -> Result<u8> {
    if x.is_zero() {
        return Err(MathError::InvalidInput);
    }
    Ok((255 - x.leading_zeros()) as u8)
}

/// Index of the least significant set bit. Reverts on zero.
pub fn least_significant_bit(x: U256) -> Result<u8> {
    if x.is_zero() {
        return Err(MathError::InvalidInput);
    }
    Ok(x.trailing_zeros() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msb_and_lsb() {
        assert_eq!(
            most_significant_bit(U256::ZERO),
            Err(MathError::InvalidInput)
        );
        assert_eq!(
            least_significant_bit(U256::ZERO),
            Err(MathError::InvalidInput)
        );
        assert_eq!(most_significant_bit(U256::from(1)), Ok(0));
        assert_eq!(least_significant_bit(U256::from(1)), Ok(0));
        assert_eq!(most_significant_bit(U256::MAX), Ok(255));
        assert_eq!(least_significant_bit(U256::MAX), Ok(0));
        for i in 0..256 {
            let x = U256::from(1) << i;
            assert_eq!(most_significant_bit(x), Ok(i as u8));
            assert_eq!(least_significant_bit(x), Ok(i as u8));
        }
    }
}
