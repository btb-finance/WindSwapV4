// SPDX-License-Identifier: BUSL-1.1 OR GPL-3.0-or-later
// NOTE: The upstream Business Source License change date has passed; this fork is distributed under GPL terms. See LICENSE.md and NOTICE.md for details.
pragma solidity 0.8.19;

import {IVoter} from "../interfaces/IVoter.sol";
import {VotingReward} from "./VotingReward.sol";

/// @title Bribe Voting Reward
/// @author BTB Finance - https://btb.finance
/// @notice Bribes pay out rewards for a given pool based on the votes that were received from the user (goes hand in hand with Voter.vote())
contract BribeVotingReward is VotingReward {
    string public constant BUILD = "BTB-BribeVotingReward-1.0";
    constructor(
        address _forwarder,
        address _voter,
        address[] memory _rewards
    ) VotingReward(_forwarder, _voter, _rewards) {}

    /// @inheritdoc VotingReward
    function notifyRewardAmount(address token, uint256 amount) external override nonReentrant {
        address sender = _msgSender();

        if (!isReward[token]) {
            if (!IVoter(voter).isWhitelistedToken(token)) revert NotWhitelisted();
            isReward[token] = true;
            rewards.push(token);
        }

        _notifyRewardAmount(sender, token, amount);
    }
}
