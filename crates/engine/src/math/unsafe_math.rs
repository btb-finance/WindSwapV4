//! Port of `UnsafeMath.sol`.

use alloy_primitives::U256;

/// `ceil(x / y)`. Like the EVM, division by zero returns 0 instead of reverting.
#[inline]
pub fn div_rounding_up(x: U256, y: U256) -> U256 {
    if y.is_zero() {
        return U256::ZERO;
    }
    let q = x / y;
    if (x % y).is_zero() {
        q
    } else {
        q + U256::from(1)
    }
}
