// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

interface INudgeable {
    function nudge() external;
}

/// @notice Test double for windv2 `EpochGovernor`. `result()` returns the state the generator set,
/// as a `ProposalState` value (uint8). `callNudge` is the only way to reach `Minter.nudge`, so the
/// call comes from the epoch governor, as in production.
contract MockEpochGovernor {
    uint8 public state;

    function setState(uint8 _state) external {
        state = _state;
    }

    function result() external view returns (uint8) {
        return state;
    }

    function callNudge(address minter) external {
        INudgeable(minter).nudge();
    }
}
