// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity =0.7.6;

import "./libraries/FullMath.sol";
import "./libraries/UnsafeMath.sol";
import "./libraries/BitMath.sol";
import "./libraries/LiquidityMath.sol";
import "./libraries/TickMath.sol";
import "./libraries/SqrtPriceMath.sol";
import "./libraries/SwapMath.sol";

/// @notice Exposes the original Wind Swap (windv3swap) math libraries as external
/// functions so the generator can call them with try/catch.
contract Harness {
    function mulDiv(uint256 a, uint256 b, uint256 d) external pure returns (uint256) {
        return FullMath.mulDiv(a, b, d);
    }

    function mulDivRoundingUp(uint256 a, uint256 b, uint256 d) external pure returns (uint256) {
        return FullMath.mulDivRoundingUp(a, b, d);
    }

    function divRoundingUp(uint256 x, uint256 y) external pure returns (uint256) {
        return UnsafeMath.divRoundingUp(x, y);
    }

    function mostSignificantBit(uint256 x) external pure returns (uint8) {
        return BitMath.mostSignificantBit(x);
    }

    function leastSignificantBit(uint256 x) external pure returns (uint8) {
        return BitMath.leastSignificantBit(x);
    }

    function addDelta(uint128 x, int128 y) external pure returns (uint128) {
        return LiquidityMath.addDelta(x, y);
    }

    function getSqrtRatioAtTick(int24 tick) external pure returns (uint160) {
        return TickMath.getSqrtRatioAtTick(tick);
    }

    function getTickAtSqrtRatio(uint160 sqrtPriceX96) external pure returns (int24) {
        return TickMath.getTickAtSqrtRatio(sqrtPriceX96);
    }

    function getNextSqrtPriceFromInput(uint160 p, uint128 l, uint256 amount, bool zeroForOne)
        external
        pure
        returns (uint160)
    {
        return SqrtPriceMath.getNextSqrtPriceFromInput(p, l, amount, zeroForOne);
    }

    function getNextSqrtPriceFromOutput(uint160 p, uint128 l, uint256 amount, bool zeroForOne)
        external
        pure
        returns (uint160)
    {
        return SqrtPriceMath.getNextSqrtPriceFromOutput(p, l, amount, zeroForOne);
    }

    function getAmount0Delta(uint160 a, uint160 b, uint128 l, bool roundUp) external pure returns (uint256) {
        return SqrtPriceMath.getAmount0Delta(a, b, l, roundUp);
    }

    function getAmount1Delta(uint160 a, uint160 b, uint128 l, bool roundUp) external pure returns (uint256) {
        return SqrtPriceMath.getAmount1Delta(a, b, l, roundUp);
    }

    function getAmount0DeltaSigned(uint160 a, uint160 b, int128 l) external pure returns (int256) {
        return SqrtPriceMath.getAmount0Delta(a, b, l);
    }

    function getAmount1DeltaSigned(uint160 a, uint160 b, int128 l) external pure returns (int256) {
        return SqrtPriceMath.getAmount1Delta(a, b, l);
    }

    function computeSwapStep(uint160 current, uint160 target, uint128 l, int256 remaining, uint24 feePips)
        external
        pure
        returns (uint160 next, uint256 amountIn, uint256 amountOut, uint256 feeAmount)
    {
        return SwapMath.computeSwapStep(current, target, l, remaining, feePips);
    }
}
