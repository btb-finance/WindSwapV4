// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity =0.7.6;

import "./libraries/TickBitmap.sol";

/// @notice Holds a tick bitmap and exposes the original TickBitmap library as external
/// functions so the generator can call them with try/catch.
contract TickBitmapHarness {
    using TickBitmap for mapping(int16 => uint256);

    mapping(int16 => uint256) internal bitmap;

    function flipTick(int24 tick, int24 tickSpacing) external {
        bitmap.flipTick(tick, tickSpacing);
    }

    function nextInitializedTickWithinOneWord(int24 tick, int24 tickSpacing, bool lte)
        external
        view
        returns (int24 next, bool initialized)
    {
        return bitmap.nextInitializedTickWithinOneWord(tick, tickSpacing, lte);
    }

    function getWord(int16 wordPos) external view returns (uint256) {
        return bitmap[wordPos];
    }
}
