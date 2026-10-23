// SPDX-License-Identifier: BUSL-1.1 OR GPL-3.0-or-later
// NOTE: The upstream Business Source License change date has passed; this fork is distributed under GPL terms. See LICENSE.md and NOTICE.md for details.
pragma solidity 0.8.19;

import {IManagedRewardsFactory} from "../interfaces/factories/IManagedRewardsFactory.sol";
import {FreeManagedReward} from "../rewards/FreeManagedReward.sol";
import {LockedManagedReward} from "../rewards/LockedManagedReward.sol";

/// @title Managed Rewards Factory
/// @author BTB Finance - https://btb.finance
/// @notice Factory for creating managed rewards
contract ManagedRewardsFactory is IManagedRewardsFactory {
    string public constant PROTOCOL = "Wind Swap v3.0";
    string public constant TEAM = "BTB Finance $BTB - https://btb.finance";
    /// @inheritdoc IManagedRewardsFactory
    function createRewards(
        address _forwarder,
        address _voter
    ) external returns (address lockedManagedReward, address freeManagedReward) {
        lockedManagedReward = address(new LockedManagedReward(_forwarder, _voter));
        freeManagedReward = address(new FreeManagedReward(_forwarder, _voter));
        emit ManagedRewardCreated(_voter, lockedManagedReward, freeManagedReward);
    }
}
