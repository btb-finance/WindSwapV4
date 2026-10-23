// SPDX-License-Identifier: BUSL-1.1 OR GPL-3.0-or-later
// NOTE: The upstream Business Source License change date has passed; this fork is distributed under GPL terms. See LICENSE.md and NOTICE.md for details.
pragma solidity 0.8.19;

import {Reward} from "./Reward.sol";
import {IVotingEscrow} from "../interfaces/IVotingEscrow.sol";
import {IVoter} from "../interfaces/IVoter.sol";

/// @title Base managed veNFT reward contract for distribution of rewards by token id
/// @author BTB Finance - https://btb.finance
abstract contract ManagedReward is Reward {
    string public constant VERSION_MR = "BTB-ManagedReward-1.0";
    constructor(address _forwarder, address _voter) Reward(_forwarder, _voter) {
        address _ve = IVoter(_voter).ve();
        address _token = IVotingEscrow(_ve).token();
        rewards.push(_token);
        isReward[_token] = true;

        authorized = _ve;
    }

    /// @inheritdoc Reward
    function getReward(uint256 tokenId, address[] memory tokens) external virtual override {}

    /// @inheritdoc Reward
    function notifyRewardAmount(address token, uint256 amount) external virtual override {}
}
