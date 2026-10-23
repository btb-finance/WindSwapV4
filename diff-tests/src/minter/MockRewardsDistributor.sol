// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

/// @notice Test double for windv2 `RewardsDistributor`: `Minter` only calls `checkpointToken()`.
contract MockRewardsDistributor {
    uint256 public checkpoints;

    function checkpointToken() external {
        checkpoints++;
    }
}
