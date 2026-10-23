// SPDX-License-Identifier: MIT
pragma solidity =0.7.6;

/// @notice 0.7.6 stand-in for OpenZeppelin 3.4 `utils/ReentrancyGuard.sol` (same behaviour).
abstract contract ReentrancyGuard {
    uint256 private constant _NOT_ENTERED = 1;
    uint256 private constant _ENTERED = 2;
    uint256 private _status;

    constructor() {
        _status = _NOT_ENTERED;
    }

    modifier nonReentrant() {
        require(_status != _ENTERED, "ReentrancyGuard: reentrant call");
        _status = _ENTERED;
        _;
        _status = _NOT_ENTERED;
    }
}
