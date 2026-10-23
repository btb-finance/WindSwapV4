// SPDX-License-Identifier: BUSL-1.1 OR GPL-3.0-or-later
// NOTE: The upstream Business Source License change date has passed; this fork is distributed under GPL terms. See LICENSE.md and NOTICE.md for details.
pragma solidity 0.8.19;

import {ManagedReward} from "./ManagedReward.sol";
import {IVotingEscrow} from "../interfaces/IVotingEscrow.sol";
import {IVoter} from "../interfaces/IVoter.sol";

/// @title Free Managed Reward
/// @author BTB Finance - https://btb.finance
/// @notice Stores rewards that are free to be distributed
/// @dev Rewards are distributed based on weight contribution to managed nft
contract FreeManagedReward is ManagedReward {
    string public constant BUILD = "BTB-FreeManagedReward-1.0";
    constructor(address _forwarder, address _voter) ManagedReward(_forwarder, _voter) {}

    /// @inheritdoc ManagedReward
    function getReward(uint256 tokenId, address[] memory tokens) external override nonReentrant {
        if (!IVotingEscrow(ve).isApprovedOrOwner(_msgSender(), tokenId)) revert NotAuthorized();

        address owner = IVotingEscrow(ve).ownerOf(tokenId);

        _getReward(owner, tokenId, tokens);
    }

    /// @inheritdoc ManagedReward
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
