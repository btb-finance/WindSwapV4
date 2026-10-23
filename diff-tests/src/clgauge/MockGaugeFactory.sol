// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity =0.7.6;

import {CLGauge} from "contracts/gauge/CLGauge.sol";

interface IMockGaugeCreatorPool {
    function setGaugeAndPositionManager(address gauge, address nft) external;
}

interface IMockGaugeAdminToken {
    function mint(address to, uint256 amount) external;
    function approve(address spender, uint256 amount) external returns (bool);
}

interface IMockGaugeAdminOps {
    function notifyRewardWithoutClaim(uint256 amount) external;
}

/// @notice Stands in for CLGaugeFactory: deploys the real `CLGauge`, initializes it (so this factory is
/// the gauge's `gaugeFactory` and the pool's registered gauge factory), and acts as the notify admin.
/// The notify admin is this contract: `notifyWithoutClaim` mints and approves the reward tokens itself.
contract MockGaugeFactory {
    address public rewardToken;

    function setRewardToken(address _rewardToken) external {
        rewardToken = _rewardToken;
    }

    function notifyAdmin() external view returns (address) {
        return address(this);
    }

    function createGauge(
        address pool,
        address feesVotingReward,
        address voter,
        address nft,
        address token0,
        address token1,
        int24 tickSpacing,
        bool isPool
    ) external returns (address) {
        CLGauge gauge = new CLGauge();
        gauge.initialize(pool, feesVotingReward, rewardToken, voter, nft, token0, token1, tickSpacing, isPool);
        IMockGaugeCreatorPool(pool).setGaugeAndPositionManager(address(gauge), nft);
        return address(gauge);
    }

    function notifyWithoutClaim(address gauge, uint256 amount) external {
        IMockGaugeAdminToken(rewardToken).mint(address(this), amount);
        IMockGaugeAdminToken(rewardToken).approve(gauge, amount);
        IMockGaugeAdminOps(gauge).notifyRewardWithoutClaim(amount);
    }
}
