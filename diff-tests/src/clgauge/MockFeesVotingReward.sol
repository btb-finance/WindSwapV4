// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity =0.7.6;

interface IMockPullToken {
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
}

/// @notice Stands in for FeesVotingReward: pulls the fee tokens the gauge approved for it.
/// The harness reads the amounts from the token balances of this contract.
contract MockFeesVotingReward {
    function notifyRewardAmount(address token, uint256 amount) external {
        require(IMockPullToken(token).transferFrom(msg.sender, address(this), amount), "PULL");
    }
}
