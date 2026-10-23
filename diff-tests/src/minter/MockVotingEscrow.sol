// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

/// @notice Test double for windv2 `VotingEscrow`. `Minter` only calls `token()` (constructor) and
/// `totalSupplyAt(t)` (in `calculateGrowth`). `totalSupplyAt` ignores `t` and returns the value the
/// generator set, which is the value the Rust port receives as `ve_total_supply`.
contract MockVotingEscrow {
    address public immutable token;
    uint256 public totalSupplyValue;

    constructor(address _token) {
        token = _token;
    }

    function setTotalSupply(uint256 value) external {
        totalSupplyValue = value;
    }

    function totalSupplyAt(uint256) external view returns (uint256) {
        return totalSupplyValue;
    }
}
