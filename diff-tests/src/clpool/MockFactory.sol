// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity =0.7.6;

/// @notice Stands in for CLFactory: the fee getters CLPool calls, with settable values.
contract MockFactory {
    uint24 public swapFee;
    uint24 public unstakedFee;

    function setFees(uint24 _swapFee, uint24 _unstakedFee) external {
        swapFee = _swapFee;
        unstakedFee = _unstakedFee;
    }

    function getSwapFee(address) external view returns (uint24) {
        return swapFee;
    }

    function getUnstakedFee(address) external view returns (uint24) {
        return unstakedFee;
    }

    /// @dev No hook: CLPool skips the afterSwap call when this is zero.
    function hook() external pure returns (address) {
        return address(0);
    }
}
