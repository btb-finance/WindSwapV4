// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

/// @notice Test double for the windv2 `Voter` as seen by `VotingEscrow` and the managed reward contracts:
/// `ve()`, `governor()`, `emergencyCouncil()` and `isWhitelistedToken(token)`. The generator prank-calls as
/// this address for `voting`, `depositManaged` and `withdrawManaged`.
contract VeFullMockVoter {
    address public immutable ve;
    address public immutable governor;
    address public immutable emergencyCouncil;
    mapping(address => bool) public isWhitelistedToken;

    constructor(address _ve, address _governor, address _emergencyCouncil) {
        ve = _ve;
        governor = _governor;
        emergencyCouncil = _emergencyCouncil;
    }

    function setWhitelisted(address token, bool value) external {
        isWhitelistedToken[token] = value;
    }
}

/// @notice Test double for the windv2 `FactoryRegistry`: only `managedRewardsFactory()`.
contract VeFullMockFactoryRegistry {
    address public immutable managedRewardsFactory;

    constructor(address _managedRewardsFactory) {
        managedRewardsFactory = _managedRewardsFactory;
    }
}
