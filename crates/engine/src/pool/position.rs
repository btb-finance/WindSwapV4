//! Port of `Position.sol`: per-owner liquidity and fee bookkeeping for a tick range.
//!
//! Arithmetic follows the Solidity 0.7.6 original: fee-growth subtraction and the
//! `tokensOwed` accumulation wrap around, and `mulDiv` results are truncated to `uint128`.

use std::collections::BTreeMap;

use alloy_primitives::U256;

use crate::error::{MathError, Result};
use crate::math::{full_math, liquidity_math, Q128};

/// Info stored for each position (`Position.Info`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PositionInfo {
    /// The amount of liquidity owned by this position.
    pub liquidity: u128,
    /// Fee growth per unit of liquidity (token0) as of the last update.
    pub fee_growth_inside0_last_x128: U256,
    /// Fee growth per unit of liquidity (token1) as of the last update.
    pub fee_growth_inside1_last_x128: U256,
    /// Fees owed to the position owner in token0.
    pub tokens_owed0: u128,
    /// Fees owed to the position owner in token1.
    pub tokens_owed1: u128,
}

/// Position key. In Solidity it is `keccak256(abi.encodePacked(owner, tickLower, tickUpper))`;
/// here it is a plain struct. `owner` is a 32-byte account id (the 20-byte EVM address is
/// left-padded with zeros).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PositionKey {
    pub owner: [u8; 32],
    pub tick_lower: i32,
    pub tick_upper: i32,
}

/// All positions of a pool.
pub type Positions = BTreeMap<PositionKey, PositionInfo>;

/// Solidity `uint128(x)`: truncates to the low 128 bits.
fn truncate_u128(x: U256) -> u128 {
    (x & U256::from(u128::MAX)).to::<u128>()
}

/// Fees accrued since `fee_growth_last_x128`, for `liquidity` units, as `uint128`.
fn fees_owed(
    fee_growth_inside_x128: U256,
    fee_growth_last_x128: U256,
    liquidity: u128,
) -> Result<u128> {
    // Unchecked `uint256` subtraction in Solidity: wraps.
    let growth = fee_growth_inside_x128.wrapping_sub(fee_growth_last_x128);
    let owed = full_math::mul_div(growth, U256::from(liquidity), Q128)?;
    Ok(truncate_u128(owed))
}

impl PositionInfo {
    /// Credits accumulated fees and applies a liquidity change (`Position.update`).
    ///
    /// Returns `Err` exactly where the Solidity reverts (`"NP"` for a poke on an empty
    /// position, `"LS"`/`"LA"` for liquidity under/overflow). On `Err`, `self` is unchanged.
    pub fn update(
        &mut self,
        liquidity_delta: i128,
        fee_growth_inside0_x128: U256,
        fee_growth_inside1_x128: U256,
        staked: bool,
    ) -> Result<()> {
        // Compute everything first, so that nothing is written on error.
        let liquidity_next = if liquidity_delta == 0 {
            if self.liquidity == 0 {
                return Err(MathError::NoPositionLiquidity);
            }
            self.liquidity
        } else {
            liquidity_math::add_delta(self.liquidity, liquidity_delta)?
        };

        // Staked positions do not accrue fees here.
        let (owed0, owed1) = if staked {
            (0, 0)
        } else {
            // Fees use the liquidity from before this update.
            (
                fees_owed(
                    fee_growth_inside0_x128,
                    self.fee_growth_inside0_last_x128,
                    self.liquidity,
                )?,
                fees_owed(
                    fee_growth_inside1_x128,
                    self.fee_growth_inside1_last_x128,
                    self.liquidity,
                )?,
            )
        };

        self.liquidity = liquidity_next;
        self.fee_growth_inside0_last_x128 = fee_growth_inside0_x128;
        self.fee_growth_inside1_last_x128 = fee_growth_inside1_x128;
        // The Solidity only writes the owed fields when one of them is non-zero. Adding zero is a
        // no-op, so this is equivalent. The addition wraps: "overflow is acceptable" in the original.
        self.tokens_owed0 = self.tokens_owed0.wrapping_add(owed0);
        self.tokens_owed1 = self.tokens_owed1.wrapping_add(owed1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_liquidity(liquidity: u128) -> PositionInfo {
        PositionInfo {
            liquidity,
            ..Default::default()
        }
    }

    #[test]
    fn poke_on_empty_position_reverts() {
        let mut p = PositionInfo::default();
        assert_eq!(
            p.update(0, U256::from(7), U256::from(9), false),
            Err(MathError::NoPositionLiquidity)
        );
        assert_eq!(p, PositionInfo::default());
    }

    #[test]
    fn mint_sets_liquidity_and_checkpoints() {
        // Liquidity is zero before the mint, so no fees are owed.
        let mut p = PositionInfo::default();
        p.update(100, U256::from(5), U256::from(6), false).unwrap();
        assert_eq!(p.liquidity, 100);
        assert_eq!(p.fee_growth_inside0_last_x128, U256::from(5));
        assert_eq!(p.fee_growth_inside1_last_x128, U256::from(6));
        assert_eq!((p.tokens_owed0, p.tokens_owed1), (0, 0));
    }

    #[test]
    fn accrues_fees_on_old_liquidity() {
        // Growth of 3 per unit of liquidity on 100 units -> 300 owed. Liquidity then grows by 50.
        let mut p = with_liquidity(100);
        p.update(50, Q128 * U256::from(3), U256::ZERO, false)
            .unwrap();
        assert_eq!(p.tokens_owed0, 300);
        assert_eq!(p.tokens_owed1, 0);
        assert_eq!(p.liquidity, 150);
    }

    #[test]
    fn staked_position_accrues_no_fees_but_checkpoints() {
        let mut p = with_liquidity(100);
        p.update(0, Q128 * U256::from(3), Q128 * U256::from(4), true)
            .unwrap();
        assert_eq!((p.tokens_owed0, p.tokens_owed1), (0, 0));
        assert_eq!(p.fee_growth_inside0_last_x128, Q128 * U256::from(3));
        assert_eq!(p.fee_growth_inside1_last_x128, Q128 * U256::from(4));
    }

    #[test]
    fn fee_growth_subtraction_wraps() {
        // last = MAX - 4 and inside = 11 give growth = 16 (mod 2^256).
        // 16 * (2^128 - 1) / 2^128 floors to 15.
        let mut p = PositionInfo {
            liquidity: u128::MAX,
            fee_growth_inside0_last_x128: U256::MAX - U256::from(4),
            ..Default::default()
        };
        p.update(0, U256::from(11), U256::ZERO, false).unwrap();
        assert_eq!(p.tokens_owed0, 15);
    }

    #[test]
    fn owed_is_truncated_to_uint128() {
        // growth = 2 * 2^128 and liquidity = 2^128 - 1 give mulDiv = 2^129 - 2.
        // Its low 128 bits are 2^128 - 2.
        let mut p = with_liquidity(u128::MAX);
        p.update(0, Q128 * U256::from(2), U256::ZERO, false)
            .unwrap();
        assert_eq!(p.tokens_owed0, u128::MAX - 1);
    }

    #[test]
    fn tokens_owed_accumulation_wraps() {
        let mut p = PositionInfo {
            liquidity: 1,
            tokens_owed0: u128::MAX - 10,
            ..Default::default()
        };
        p.update(0, Q128 * U256::from(30), U256::ZERO, false)
            .unwrap();
        assert_eq!(p.tokens_owed0, 19);
    }

    #[test]
    fn liquidity_underflow_reverts_without_writing() {
        let mut p = PositionInfo {
            liquidity: 100,
            fee_growth_inside0_last_x128: U256::from(1),
            ..Default::default()
        };
        let before = p.clone();
        assert_eq!(
            p.update(-101, U256::from(9), U256::from(9), false),
            Err(MathError::LiquiditySub)
        );
        assert_eq!(p, before);
    }

    #[test]
    fn liquidity_overflow_reverts_without_writing() {
        let mut p = with_liquidity(u128::MAX);
        let before = p.clone();
        assert_eq!(p.update(1, Q128, Q128, false), Err(MathError::LiquidityAdd));
        assert_eq!(p, before);
    }

    #[test]
    fn withdrawing_all_liquidity_still_pays_old_fees() {
        let mut p = with_liquidity(10);
        p.update(-10, Q128 * U256::from(2), U256::ZERO, false)
            .unwrap();
        assert_eq!(p.liquidity, 0);
        assert_eq!(p.tokens_owed0, 20);
    }
}
