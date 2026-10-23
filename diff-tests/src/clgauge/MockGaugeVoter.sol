// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity =0.7.6;

interface IMockGaugeVoterOps {
    function notifyRewardAmount(uint256 amount) external;
    function getReward(address account) external;
}

interface IMockTokenVoterOps {
    function mint(address to, uint256 amount) external;
    function approve(address spender, uint256 amount) external returns (bool);
}

/// @notice Stands in for the Voter: `isAlive` for the gauge, and the only caller of
/// `notifyRewardAmount` and `getReward(account)` on the gauge (so the gauge sees `msg.sender == voter`).
contract MockGaugeVoter {
    address public rewardToken;
    bool public alive;

    function setRewardToken(address _rewardToken) external {
        rewardToken = _rewardToken;
    }

    function setAlive(bool _alive) external {
        alive = _alive;
    }

    function isAlive(address) external view returns (bool) {
        return alive;
    }

    /// @notice Mints `amount` reward tokens to this voter, approves the gauge for them, and notifies.
    function notify(address gauge, uint256 amount) external {
        IMockTokenVoterOps(rewardToken).mint(address(this), amount);
        IMockTokenVoterOps(rewardToken).approve(gauge, amount);
        IMockGaugeVoterOps(gauge).notifyRewardAmount(amount);
    }

    function getReward(address gauge, address account) external {
        IMockGaugeVoterOps(gauge).getReward(account);
    }
}
