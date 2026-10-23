// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IReward} from "../../windv2/contracts/interfaces/IReward.sol";

/// @notice Gauge double for the voter generator. `left()` and `fail` are set by the generator. `notifyRewardAmount`
/// pulls the reward token from the caller (the Voter, which approves exactly the claimable amount) or reverts when
/// `fail` is set, which exercises the Voter's `try` / restore path.
contract VoterMockGauge {
    IERC20 public immutable rewardToken;
    address public immutable fees;
    uint256 public left;
    bool public fail;

    constructor(address _token, address _fees) {
        rewardToken = IERC20(_token);
        fees = _fees;
    }

    /// @notice Notifies the gauge's `FeesVotingReward` as the gauge (the Voter maps this gauge to `fees`). The
    /// generator mints the token to this contract first, and the approval is set here.
    function notifyFees(address token, uint256 amount) external {
        require(IERC20(token).approve(fees, amount), "approve");
        IReward(fees).notifyRewardAmount(token, amount);
    }

    function setLeft(uint256 _left) external {
        left = _left;
    }

    function setFail(bool _fail) external {
        fail = _fail;
    }

    function notifyRewardAmount(uint256 amount) external {
        require(!fail, "VoterMockGauge: fail");
        require(rewardToken.transferFrom(msg.sender, address(this), amount), "transferFrom");
    }
}

/// @notice Gauge factory double: deploys a `VoterMockGauge` for the reward token passed as `_ve`.
contract VoterMockGaugeFactory {
    function createGauge(address, address, address _fees, address _token, bool) external returns (address) {
        return address(new VoterMockGauge(_token, _fees));
    }
}

/// @notice Pool double exposing `token0` and `token1`, read by the Voter when `isPool` is true.
contract VoterMockPool {
    address public immutable token0;
    address public immutable token1;

    constructor(address _token0, address _token1) {
        token0 = _token0;
        token1 = _token1;
    }
}

/// @notice Pool factory double: `isPool` is set per pool by the generator.
contract VoterMockPoolFactory {
    mapping(address => bool) public isPool;

    function setPool(address pool, bool value) external {
        isPool[pool] = value;
    }
}

/// @notice Factory registry double: approvals and the factory pair per pool factory.
contract VoterMockFactoryRegistry {
    mapping(address => bool) public isPoolFactoryApproved;
    mapping(address => address) public votingRewardsFactoryOf;
    mapping(address => address) public gaugeFactoryOf;

    function setPoolFactory(address poolFactory, address votingRewardsFactory, address gaugeFactory) external {
        isPoolFactoryApproved[poolFactory] = true;
        votingRewardsFactoryOf[poolFactory] = votingRewardsFactory;
        gaugeFactoryOf[poolFactory] = gaugeFactory;
    }

    function factoriesToPoolFactory(address poolFactory) external view returns (address, address) {
        return (votingRewardsFactoryOf[poolFactory], gaugeFactoryOf[poolFactory]);
    }
}

/// @notice Minter double: the Voter only calls `updatePeriod`, which is a no-op here.
contract VoterMockMinter {
    function updatePeriod() external returns (uint256) {
        return 0;
    }
}
