//! Port of the fee configuration logic: `CLFactory` fee getters and setters,
//! `CustomSwapFeeModule` and `CustomUnstakedFeeModule`.
//!
//! The Solidity contracts spread this state across three contracts and read
//! `tx.origin`, `msg.sender` and external gauge state. Here everything is plain
//! data: the caller supplies the trader, whether the pool's gauge is alive, and
//! is responsible for authorization (`msg.sender` checks) and for pool
//! existence (`factory.isPool`), which the engine does not track.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{MathError, Result};

/// Pool identifier (the engine's stand-in for a pool address).
pub type PoolId = u64;
/// Account identifier (the engine's stand-in for a 20-byte address, widened).
pub type AccountId = [u8; 32];

/// Upper bound (inclusive) on a swap fee, in hundredths of a bip. `CLFactory.getSwapFee`
/// and `_enableTickSpacing` both cap at 100_000 (10%).
pub const MAX_SWAP_FEE: u32 = 100_000;
/// Upper bound on a fee returned by `CLFactory.getUnstakedFee` from the module (`<= 1_000_000`).
pub const MAX_UNSTAKED_FEE: u32 = 1_000_000;
/// Upper bound on `CLFactory.setDefaultUnstakedFee`.
pub const MAX_DEFAULT_UNSTAKED_FEE: u32 = 500_000;
/// Upper bound on a custom swap fee set through `CustomSwapFeeModule.setCustomFee` (3%).
pub const MAX_CUSTOM_SWAP_FEE: u32 = 30_000;
/// Upper bound on a custom unstaked fee set through `CustomUnstakedFeeModule.setCustomFee` (50%).
pub const MAX_CUSTOM_UNSTAKED_FEE: u32 = 500_000;
/// Sentinel stored in a custom fee map meaning "custom fee of 0%". A stored `0`
/// means "no custom fee", so 0% needs an explicit marker.
pub const ZERO_FEE_INDICATOR: u32 = 420;
/// Default unstaked fee used by `CLFactory` when no module is set (10%).
pub const DEFAULT_UNSTAKED_FEE: u32 = 100_000;
/// Fee `CustomUnstakedFeeModule.getFee` returns when no custom fee is set (10%).
pub const MODULE_DEFAULT_UNSTAKED_FEE: u32 = 100_000;
/// Exclusive upper bound on tick spacing (`tickSpacing < 16384` in `_enableTickSpacing`).
pub const MAX_TICK_SPACING: i32 = 16_384;

/// Tick spacings enabled by the `CLFactory` constructor, as `(tick_spacing, fee)`.
const DEFAULT_TICK_SPACINGS: [(i32, u32); 5] = [
    (1, 50),     // 0.005% - Stables
    (2, 10_000), // 1.00% - Exotic
    (3, 300),    // 0.03% - Standard pairs
    (4, 500),    // 0.05% - Medium volatility
    (5, 2_600),  // 0.26% - Volatile
];

/// Fee configuration owned by the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeConfig {
    /// `CLFactory.tickSpacingToFee`. A missing key is the Solidity `0` (not enabled).
    pub tick_spacing_to_fee: BTreeMap<i32, u32>,
    /// `CLFactory.defaultUnstakedFee`.
    pub default_unstaked_fee: u32,
    /// `CustomSwapFeeModule.customFee`. Missing key or `0` means no custom fee.
    pub custom_swap_fee: BTreeMap<PoolId, u32>,
    /// `CustomUnstakedFeeModule.customFee`. Missing key or `0` means no custom fee.
    pub custom_unstaked_fee: BTreeMap<PoolId, u32>,
    /// `CustomSwapFeeModule.whitelist`: accounts that pay a 0 swap fee.
    pub swap_fee_whitelist: BTreeSet<AccountId>,
}

impl Default for FeeConfig {
    /// Matches the `CLFactory` constructor: `defaultUnstakedFee = 100_000` and the
    /// five `_enableTickSpacing` calls. No custom fees, empty whitelist.
    fn default() -> Self {
        Self {
            tick_spacing_to_fee: DEFAULT_TICK_SPACINGS.into_iter().collect(),
            default_unstaked_fee: DEFAULT_UNSTAKED_FEE,
            custom_swap_fee: BTreeMap::new(),
            custom_unstaked_fee: BTreeMap::new(),
            swap_fee_whitelist: BTreeSet::new(),
        }
    }
}

impl FeeConfig {
    /// Port of `CLFactory._enableTickSpacing`.
    ///
    /// Requires `0 < fee <= 100_000`, `0 < tick_spacing < 16384`, and that the
    /// tick spacing is not already enabled. The `enableTickSpacing` owner check
    /// is the caller's responsibility.
    pub fn enable_tick_spacing(&mut self, tick_spacing: i32, fee: u32) -> Result<()> {
        if !(fee > 0 && fee <= MAX_SWAP_FEE) {
            return Err(MathError::InvalidInput);
        }
        if !(tick_spacing > 0 && tick_spacing < MAX_TICK_SPACING) {
            return Err(MathError::InvalidInput);
        }
        if self.tick_spacing_to_fee.contains_key(&tick_spacing) {
            return Err(MathError::InvalidInput);
        }
        self.tick_spacing_to_fee.insert(tick_spacing, fee);
        Ok(())
    }

    /// Port of `CLFactory.setDefaultUnstakedFee`. Requires `fee <= 500_000`.
    pub fn set_default_unstaked_fee(&mut self, fee: u32) -> Result<()> {
        if fee > MAX_DEFAULT_UNSTAKED_FEE {
            return Err(MathError::InvalidInput);
        }
        self.default_unstaked_fee = fee;
        Ok(())
    }

    /// Port of `CustomSwapFeeModule.setCustomFee`. Requires `fee <= 30_000` or `fee == 420`.
    ///
    /// The `factory.isPool(pool)` check is not performed here. A fee of `0` clears
    /// the custom fee, since `0` is the Solidity "unset" value.
    pub fn set_custom_swap_fee(&mut self, pool: PoolId, fee: u32) -> Result<()> {
        if !(fee <= MAX_CUSTOM_SWAP_FEE || fee == ZERO_FEE_INDICATOR) {
            return Err(MathError::InvalidInput);
        }
        set_or_clear(&mut self.custom_swap_fee, pool, fee);
        Ok(())
    }

    /// Port of `CustomUnstakedFeeModule.setCustomFee`. Requires `fee <= 500_000` or `fee == 420`.
    ///
    /// The `factory.isPool(pool)` check is not performed here. A fee of `0` clears
    /// the custom fee.
    pub fn set_custom_unstaked_fee(&mut self, pool: PoolId, fee: u32) -> Result<()> {
        if !(fee <= MAX_CUSTOM_UNSTAKED_FEE || fee == ZERO_FEE_INDICATOR) {
            return Err(MathError::InvalidInput);
        }
        set_or_clear(&mut self.custom_unstaked_fee, pool, fee);
        Ok(())
    }

    /// Port of `CustomSwapFeeModule.setWhitelist`. Adds or removes `account` from the whitelist.
    pub fn set_whitelist(&mut self, account: AccountId, status: bool) {
        if status {
            self.swap_fee_whitelist.insert(account);
        } else {
            self.swap_fee_whitelist.remove(&account);
        }
    }

    /// Swap fee for a trade on `pool` with tick spacing `tick_spacing` by `trader`.
    ///
    /// Combines `CustomSwapFeeModule.getFee` (the WindMEV hook branch is not ported)
    /// with `CLFactory.getSwapFee`, assuming the swap fee module is set:
    /// - `trader` whitelisted: `0`.
    /// - custom fee `420`: `0`.
    /// - nonzero custom fee: that fee. It is at most 30_000, so the factory's
    ///   `<= 100_000` check always passes and the fee is returned as is.
    /// - otherwise: `tickSpacingToFee[tick_spacing]`, or `0` if the spacing is not enabled.
    ///   The module returns this value unchanged, and the factory accepts it since
    ///   enabled fees are at most 100_000.
    pub fn swap_fee(&self, pool: PoolId, tick_spacing: i32, trader: &AccountId) -> u32 {
        if self.swap_fee_whitelist.contains(trader) {
            return 0;
        }
        match custom_fee(&self.custom_swap_fee, pool) {
            ZERO_FEE_INDICATOR => 0,
            0 => self
                .tick_spacing_to_fee
                .get(&tick_spacing)
                .copied()
                .unwrap_or(0),
            fee => fee,
        }
    }

    /// Unstaked fee for `pool`, where `gauge_alive` is `voter.isAlive(voter.gauges(pool))`
    /// with a nonzero gauge.
    ///
    /// Combines `CLFactory.getUnstakedFee` with `CustomUnstakedFeeModule.getFee`,
    /// assuming the unstaked fee module is set (as on mainnet):
    /// - no live gauge: `0`. Checked before the module, so it overrides any custom fee.
    /// - custom fee `420`: `0`.
    /// - nonzero custom fee: that fee. It is at most 500_000, so the factory's
    ///   `<= 1_000_000` check always passes.
    /// - otherwise: `100_000`, the module's own default.
    ///
    /// `default_unstaked_fee` is never returned while the module is set. The module
    /// always yields a value at most 1_000_000, so `CLFactory` never falls back to
    /// `defaultUnstakedFee`. Changing that field does not affect this function.
    pub fn unstaked_fee(&self, pool: PoolId, gauge_alive: bool) -> u32 {
        if !gauge_alive {
            return 0;
        }
        match custom_fee(&self.custom_unstaked_fee, pool) {
            ZERO_FEE_INDICATOR => 0,
            0 => MODULE_DEFAULT_UNSTAKED_FEE,
            fee => fee,
        }
    }
}

/// Reads a custom fee map entry, treating a missing key as `0` (Solidity's default).
fn custom_fee(map: &BTreeMap<PoolId, u32>, pool: PoolId) -> u32 {
    map.get(&pool).copied().unwrap_or(0)
}

/// Stores `fee` for `pool`, removing the entry when `fee` is `0` so the map keeps
/// only meaningful entries.
fn set_or_clear(map: &mut BTreeMap<PoolId, u32>, pool: PoolId, fee: u32) {
    if fee == 0 {
        map.remove(&pool);
    } else {
        map.insert(pool, fee);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POOL: PoolId = 7;
    const OTHER_POOL: PoolId = 8;
    const TRADER: AccountId = [1u8; 32];
    const OTHER_TRADER: AccountId = [2u8; 32];

    fn config() -> FeeConfig {
        FeeConfig::default()
    }

    #[test]
    fn default_matches_factory_constructor() {
        let c = config();
        assert_eq!(c.default_unstaked_fee, 100_000);
        let expected: BTreeMap<i32, u32> = [(1, 50), (2, 10_000), (3, 300), (4, 500), (5, 2_600)]
            .into_iter()
            .collect();
        assert_eq!(c.tick_spacing_to_fee, expected);
        assert!(c.custom_swap_fee.is_empty());
        assert!(c.custom_unstaked_fee.is_empty());
        assert!(c.swap_fee_whitelist.is_empty());
    }

    #[test]
    fn enable_tick_spacing_accepts_boundary_values() {
        let mut c = config();
        assert!(c.enable_tick_spacing(10, 1).is_ok()); // fee min
        assert!(c.enable_tick_spacing(11, 100_000).is_ok()); // fee max
        assert!(c.enable_tick_spacing(16_383, 500).is_ok()); // spacing max (exclusive bound)
        assert_eq!(c.tick_spacing_to_fee[&10], 1);
        assert_eq!(c.tick_spacing_to_fee[&11], 100_000);
        assert_eq!(c.tick_spacing_to_fee[&16_383], 500);
    }

    #[test]
    fn enable_tick_spacing_rejects_zero_fee() {
        let mut c = config();
        assert_eq!(c.enable_tick_spacing(10, 0), Err(MathError::InvalidInput));
        assert!(!c.tick_spacing_to_fee.contains_key(&10));
    }

    #[test]
    fn enable_tick_spacing_rejects_fee_above_max() {
        let mut c = config();
        assert_eq!(
            c.enable_tick_spacing(10, 100_001),
            Err(MathError::InvalidInput)
        );
    }

    #[test]
    fn enable_tick_spacing_rejects_zero_spacing() {
        let mut c = config();
        assert_eq!(c.enable_tick_spacing(0, 500), Err(MathError::InvalidInput));
    }

    #[test]
    fn enable_tick_spacing_rejects_negative_spacing() {
        let mut c = config();
        assert_eq!(c.enable_tick_spacing(-1, 500), Err(MathError::InvalidInput));
    }

    #[test]
    fn enable_tick_spacing_rejects_spacing_at_or_above_16384() {
        let mut c = config();
        assert_eq!(
            c.enable_tick_spacing(16_384, 500),
            Err(MathError::InvalidInput)
        );
        assert_eq!(
            c.enable_tick_spacing(20_000, 500),
            Err(MathError::InvalidInput)
        );
    }

    #[test]
    fn enable_tick_spacing_rejects_already_enabled() {
        let mut c = config();
        assert_eq!(c.enable_tick_spacing(3, 999), Err(MathError::InvalidInput));
        // Original fee is untouched.
        assert_eq!(c.tick_spacing_to_fee[&3], 300);
    }

    #[test]
    fn set_default_unstaked_fee_bounds() {
        let mut c = config();
        assert!(c.set_default_unstaked_fee(500_000).is_ok());
        assert_eq!(c.default_unstaked_fee, 500_000);
        assert!(c.set_default_unstaked_fee(0).is_ok());
        assert_eq!(c.default_unstaked_fee, 0);
        assert_eq!(
            c.set_default_unstaked_fee(500_001),
            Err(MathError::InvalidInput)
        );
        assert_eq!(c.default_unstaked_fee, 0);
    }

    #[test]
    fn set_custom_swap_fee_accepts_max_and_sentinel() {
        let mut c = config();
        assert!(c.set_custom_swap_fee(POOL, 30_000).is_ok());
        assert_eq!(c.custom_swap_fee[&POOL], 30_000);
        assert!(c.set_custom_swap_fee(POOL, ZERO_FEE_INDICATOR).is_ok());
        assert_eq!(c.custom_swap_fee[&POOL], 420);
    }

    #[test]
    fn set_custom_swap_fee_rejects_out_of_range() {
        let mut c = config();
        assert_eq!(
            c.set_custom_swap_fee(POOL, 30_001),
            Err(MathError::InvalidInput)
        );
        // Above the cap and not the sentinel.
        assert_eq!(
            c.set_custom_swap_fee(POOL, 30_420),
            Err(MathError::InvalidInput)
        );
        assert!(!c.custom_swap_fee.contains_key(&POOL));
    }

    #[test]
    fn set_custom_swap_fee_zero_clears_entry() {
        let mut c = config();
        c.set_custom_swap_fee(POOL, 1_000).unwrap();
        c.set_custom_swap_fee(POOL, 0).unwrap();
        assert!(!c.custom_swap_fee.contains_key(&POOL));
    }

    #[test]
    fn set_custom_unstaked_fee_accepts_max_and_sentinel() {
        let mut c = config();
        assert!(c.set_custom_unstaked_fee(POOL, 500_000).is_ok());
        assert_eq!(c.custom_unstaked_fee[&POOL], 500_000);
        assert!(c.set_custom_unstaked_fee(POOL, ZERO_FEE_INDICATOR).is_ok());
        assert_eq!(c.custom_unstaked_fee[&POOL], 420);
    }

    #[test]
    fn set_custom_unstaked_fee_rejects_out_of_range() {
        let mut c = config();
        assert_eq!(
            c.set_custom_unstaked_fee(POOL, 500_001),
            Err(MathError::InvalidInput)
        );
        assert_eq!(
            c.set_custom_unstaked_fee(POOL, 500_420),
            Err(MathError::InvalidInput)
        );
        assert!(!c.custom_unstaked_fee.contains_key(&POOL));
    }

    #[test]
    fn set_custom_unstaked_fee_zero_clears_entry() {
        let mut c = config();
        c.set_custom_unstaked_fee(POOL, 1_000).unwrap();
        c.set_custom_unstaked_fee(POOL, 0).unwrap();
        assert!(!c.custom_unstaked_fee.contains_key(&POOL));
    }

    #[test]
    fn whitelist_add_and_remove() {
        let mut c = config();
        c.set_whitelist(TRADER, true);
        assert!(c.swap_fee_whitelist.contains(&TRADER));
        c.set_whitelist(TRADER, false);
        assert!(!c.swap_fee_whitelist.contains(&TRADER));
        // Removing an absent account is a no-op.
        c.set_whitelist(TRADER, false);
        assert!(c.swap_fee_whitelist.is_empty());
    }

    #[test]
    fn swap_fee_whitelisted_trader_pays_zero() {
        let mut c = config();
        c.set_custom_swap_fee(POOL, 1_000).unwrap();
        c.set_whitelist(TRADER, true);
        assert_eq!(c.swap_fee(POOL, 3, &TRADER), 0);
    }

    #[test]
    fn swap_fee_custom_zero_indicator_pays_zero() {
        let mut c = config();
        c.set_custom_swap_fee(POOL, ZERO_FEE_INDICATOR).unwrap();
        assert_eq!(c.swap_fee(POOL, 3, &OTHER_TRADER), 0);
    }

    #[test]
    fn swap_fee_nonzero_custom_overrides_tick_spacing() {
        let mut c = config();
        c.set_custom_swap_fee(POOL, 1_234).unwrap();
        assert_eq!(c.swap_fee(POOL, 3, &OTHER_TRADER), 1_234);
    }

    #[test]
    fn swap_fee_custom_max_is_returned() {
        let mut c = config();
        c.set_custom_swap_fee(POOL, MAX_CUSTOM_SWAP_FEE).unwrap();
        assert_eq!(c.swap_fee(POOL, 3, &OTHER_TRADER), 30_000);
    }

    #[test]
    fn swap_fee_falls_back_to_tick_spacing() {
        let c = config();
        assert_eq!(c.swap_fee(POOL, 3, &OTHER_TRADER), 300);
        assert_eq!(c.swap_fee(POOL, 5, &OTHER_TRADER), 2_600);
    }

    #[test]
    fn swap_fee_unknown_tick_spacing_is_zero() {
        let c = config();
        assert_eq!(c.swap_fee(POOL, 99, &OTHER_TRADER), 0);
    }

    #[test]
    fn swap_fee_custom_is_per_pool() {
        let mut c = config();
        c.set_custom_swap_fee(POOL, 1_234).unwrap();
        assert_eq!(c.swap_fee(OTHER_POOL, 3, &OTHER_TRADER), 300);
    }

    #[test]
    fn unstaked_fee_dead_gauge_is_zero_even_with_custom_fee() {
        let mut c = config();
        c.set_custom_unstaked_fee(POOL, 1_234).unwrap();
        assert_eq!(c.unstaked_fee(POOL, false), 0);
    }

    #[test]
    fn unstaked_fee_custom_zero_indicator_is_zero() {
        let mut c = config();
        c.set_custom_unstaked_fee(POOL, ZERO_FEE_INDICATOR).unwrap();
        assert_eq!(c.unstaked_fee(POOL, true), 0);
    }

    #[test]
    fn unstaked_fee_nonzero_custom_is_returned() {
        let mut c = config();
        c.set_custom_unstaked_fee(POOL, 1_234).unwrap();
        assert_eq!(c.unstaked_fee(POOL, true), 1_234);
    }

    #[test]
    fn unstaked_fee_custom_max_is_returned() {
        let mut c = config();
        c.set_custom_unstaked_fee(POOL, MAX_CUSTOM_UNSTAKED_FEE)
            .unwrap();
        assert_eq!(c.unstaked_fee(POOL, true), 500_000);
    }

    #[test]
    fn unstaked_fee_without_custom_is_module_default() {
        let c = config();
        assert_eq!(c.unstaked_fee(POOL, true), 100_000);
    }

    #[test]
    fn unstaked_fee_ignores_default_unstaked_fee_when_module_set() {
        let mut c = config();
        c.set_default_unstaked_fee(200_000).unwrap();
        assert_eq!(c.unstaked_fee(POOL, true), 100_000);
        c.set_default_unstaked_fee(0).unwrap();
        assert_eq!(c.unstaked_fee(POOL, true), 100_000);
    }

    #[test]
    fn unstaked_fee_dead_gauge_is_zero_without_custom_fee() {
        let c = config();
        assert_eq!(c.unstaked_fee(POOL, false), 0);
    }
}
