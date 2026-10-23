// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity =0.7.6;

import "./libraries/Tick.sol";

/// @notice Exposes the original Wind Swap `Tick` library as external functions over a
/// single `ticks` mapping, so the generator can make stateful calls with try/catch.
contract TickHarness {
    mapping(int24 => Tick.Info) ticks;

    function tickSpacingToMaxLiquidityPerTick(int24 tickSpacing) external pure returns (uint128) {
        return Tick.tickSpacingToMaxLiquidityPerTick(tickSpacing);
    }

    function getFeeGrowthInside(
        int24 tickLower,
        int24 tickUpper,
        int24 tickCurrent,
        uint256 feeGrowthGlobal0X128,
        uint256 feeGrowthGlobal1X128
    ) external view returns (uint256, uint256) {
        return Tick.getFeeGrowthInside(ticks, tickLower, tickUpper, tickCurrent, feeGrowthGlobal0X128, feeGrowthGlobal1X128);
    }

    function getRewardGrowthInside(int24 tickLower, int24 tickUpper, int24 tickCurrent, uint256 rewardGrowthGlobalX128)
        external
        view
        returns (uint256)
    {
        return Tick.getRewardGrowthInside(ticks, tickLower, tickUpper, tickCurrent, rewardGrowthGlobalX128);
    }

    function update(
        int24 tick,
        int24 tickCurrent,
        int128 liquidityDelta,
        uint256 feeGrowthGlobal0X128,
        uint256 feeGrowthGlobal1X128,
        uint160 secondsPerLiquidityCumulativeX128,
        int56 tickCumulative,
        uint32 time,
        bool upper,
        uint128 maxLiquidity
    ) external returns (bool) {
        return Tick.update(
            ticks,
            tick,
            tickCurrent,
            liquidityDelta,
            feeGrowthGlobal0X128,
            feeGrowthGlobal1X128,
            secondsPerLiquidityCumulativeX128,
            tickCumulative,
            time,
            upper,
            maxLiquidity
        );
    }

    function updateStake(int24 tick, int128 stakedLiquidityDelta, bool upper) external {
        Tick.updateStake(ticks, tick, stakedLiquidityDelta, upper);
    }

    function clear(int24 tick) external {
        Tick.clear(ticks, tick);
    }

    function cross(
        int24 tick,
        uint256 feeGrowthGlobal0X128,
        uint256 feeGrowthGlobal1X128,
        uint160 secondsPerLiquidityCumulativeX128,
        int56 tickCumulative,
        uint32 time,
        uint256 rewardGrowthGlobalX128
    ) external returns (int128, int128) {
        Tick.LiquidityNets memory nets = Tick.cross(
            ticks,
            tick,
            feeGrowthGlobal0X128,
            feeGrowthGlobal1X128,
            secondsPerLiquidityCumulativeX128,
            tickCumulative,
            time,
            rewardGrowthGlobalX128
        );
        return (nets.liquidityNet, nets.stakedLiquidityNet);
    }

    /// @notice All fields of the tick, in `Tick.Info` declaration order.
    function getTick(int24 tick)
        external
        view
        returns (uint128, int128, int128, uint256, uint256, uint256, int56, uint160, uint32, bool)
    {
        Tick.Info storage info = ticks[tick];
        return (
            info.liquidityGross,
            info.liquidityNet,
            info.stakedLiquidityNet,
            info.feeGrowthOutside0X128,
            info.feeGrowthOutside1X128,
            info.rewardGrowthOutsideX128,
            info.tickCumulativeOutside,
            info.secondsPerLiquidityOutsideX128,
            info.secondsOutside,
            info.initialized
        );
    }
}
