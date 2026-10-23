// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @notice Test double for windv2 `Voter.notifyRewardAmount`: pulls `amount` from the minter with
/// `transferFrom`, as the real Voter does, so the minter's approval is consumed. Records the amount.
contract MockVoter {
    IERC20 public immutable rewardToken;
    address public minter;
    address public epochGovernor;
    uint256 public lastNotify;
    uint256 public notifyCount;

    constructor(address _token) {
        rewardToken = IERC20(_token);
    }

    function setMinter(address _minter) external {
        minter = _minter;
    }

    function setEpochGovernor(address _epochGovernor) external {
        epochGovernor = _epochGovernor;
    }

    function notifyRewardAmount(uint256 _amount) external {
        require(msg.sender == minter, "NotMinter");
        require(rewardToken.transferFrom(msg.sender, address(this), _amount), "transferFrom");
        lastNotify = _amount;
        notifyCount++;
    }
}
