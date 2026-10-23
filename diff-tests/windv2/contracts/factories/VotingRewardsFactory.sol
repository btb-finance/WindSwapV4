// SPDX-License-Identifier: BUSL-1.1 OR GPL-3.0-or-later
// NOTE: The upstream Business Source License change date has passed; this fork is distributed under GPL terms. See LICENSE.md and NOTICE.md for details.
pragma solidity 0.8.19;

import {IVotingRewardsFactory} from "../interfaces/factories/IVotingRewardsFactory.sol";
import {FeesVotingReward} from "../rewards/FeesVotingReward.sol";
import {BribeVotingReward} from "../rewards/BribeVotingReward.sol";

/// @title Voting Rewards Factory
/// @author BTB Finance - https://btb.finance
/// @notice Factory for creating voting rewards
contract VotingRewardsFactory is IVotingRewardsFactory {
    string public constant PROTOCOL = "Wind Swap v3.0";
    string public constant TEAM = "BTB Finance $BTB - https://btb.finance";
    /// @inheritdoc IVotingRewardsFactory
    function createRewards(
        address _forwarder,
        address[] memory _rewards
    ) external returns (address feesVotingReward, address bribeVotingReward) {
        feesVotingReward = address(new FeesVotingReward(_forwarder, msg.sender, _rewards));
        bribeVotingReward = address(new BribeVotingReward(_forwarder, msg.sender, _rewards));
    }
}
