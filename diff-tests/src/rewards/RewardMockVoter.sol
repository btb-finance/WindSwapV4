// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

/// @notice Test double for the windv2 `Voter` as seen by the reward contracts: `ve()` (constructor),
/// `gaugeToFees(gauge)` (FeesVotingReward.notifyRewardAmount) and `isWhitelistedToken(token)`
/// (BribeVotingReward.notifyRewardAmount). The generator prank-calls as this address for `_deposit`,
/// `_withdraw` and `getReward` (the voter is an authorized caller).
contract RewardMockVoter {
    address public immutable ve;
    mapping(address => address) private _gaugeToFees;
    mapping(address => bool) private _whitelisted;

    constructor(address _ve) {
        ve = _ve;
    }

    function setGaugeToFees(address gauge, address fees) external {
        _gaugeToFees[gauge] = fees;
    }

    function setWhitelisted(address token, bool value) external {
        _whitelisted[token] = value;
    }

    function gaugeToFees(address gauge) external view returns (address) {
        return _gaugeToFees[gauge];
    }

    function isWhitelistedToken(address token) external view returns (bool) {
        return _whitelisted[token];
    }
}
