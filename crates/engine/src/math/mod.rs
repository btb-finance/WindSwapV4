//! Concentrated-liquidity math, ported from `contracts/core/libraries` in windv3swap.

pub mod bit_math;
pub mod full_math;
pub mod liquidity_math;
pub mod sqrt_price_math;
pub mod swap_math;
pub mod tick_math;
pub mod unsafe_math;

use alloy_primitives::{uint, U256};

use crate::error::{MathError, Result};

/// `FixedPoint96.RESOLUTION`
pub const RESOLUTION: usize = 96;
/// `FixedPoint96.Q96`
pub const Q96: U256 = uint!(0x1000000000000000000000000_U256);
/// `FixedPoint128.Q128`
pub const Q128: U256 = uint!(0x100000000000000000000000000000000_U256);
/// `type(uint160).max`
pub const MAX_U160: U256 = uint!(0xffffffffffffffffffffffffffffffffffffffff_U256);

/// Solidity `uint160(x)`: truncates to the low 160 bits.
#[inline]
pub(crate) fn truncate_u160(x: U256) -> U256 {
    x & MAX_U160
}

/// `SafeCast.toUint160`: reverts if `x` does not fit.
#[inline]
pub(crate) fn to_u160(x: U256) -> Result<U256> {
    if x > MAX_U160 {
        Err(MathError::Overflow)
    } else {
        Ok(x)
    }
}
