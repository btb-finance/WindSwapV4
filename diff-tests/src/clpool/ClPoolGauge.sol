// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity =0.7.6;

interface ICLPoolGaugeOps {
    function stake(int128 stakedLiquidityDelta, int24 tickLower, int24 tickUpper, bool positionUpdate) external;
    function syncReward(uint256 rewardRate, uint256 rewardReserve, uint256 periodFinish) external;
    function collectFees() external returns (uint128 amount0, uint128 amount1);
    function updateRewardsGrowthGlobal() external;
}

/// @notice The `gauge` of the pool. CLPool only accepts the gauge-only calls from this address,
/// so every gauge op goes through this proxy and the pool sees it as msg.sender.
contract ClPoolGauge {
    ICLPoolGaugeOps public pool;

    function setPool(address _pool) external {
        pool = ICLPoolGaugeOps(_pool);
    }

    function stake(int128 stakedLiquidityDelta, int24 tickLower, int24 tickUpper, bool positionUpdate) external {
        pool.stake(stakedLiquidityDelta, tickLower, tickUpper, positionUpdate);
    }

    function syncReward(uint256 rewardRate, uint256 rewardReserve, uint256 periodFinish) external {
        pool.syncReward(rewardRate, rewardReserve, periodFinish);
    }

    function collectFees() external returns (uint128 amount0, uint128 amount1) {
        return pool.collectFees();
    }

    function updateRewardsGrowthGlobal() external {
        pool.updateRewardsGrowthGlobal();
    }
}
