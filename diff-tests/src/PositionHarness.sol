// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity =0.7.6;

import "./libraries/Position.sol";

/// @notice Exposes the original Position library so the generator can drive it statefully.
/// Positions are keyed exactly like the original pool: keccak256(abi.encodePacked(owner, tickLower, tickUpper)).
contract PositionHarness {
    mapping(bytes32 => Position.Info) positions;

    function get(address owner, int24 tickLower, int24 tickUpper) internal view returns (Position.Info storage) {
        return Position.get(positions, owner, tickLower, tickUpper);
    }

    function update(
        address owner,
        int24 tickLower,
        int24 tickUpper,
        int128 liquidityDelta,
        uint256 feeGrowthInside0X128,
        uint256 feeGrowthInside1X128,
        bool staked
    ) external {
        Position.update(
            get(owner, tickLower, tickUpper), liquidityDelta, feeGrowthInside0X128, feeGrowthInside1X128, staked
        );
    }

    function info(address owner, int24 tickLower, int24 tickUpper)
        external
        view
        returns (
            uint128 liquidity,
            uint256 feeGrowthInside0LastX128,
            uint256 feeGrowthInside1LastX128,
            uint128 tokensOwed0,
            uint128 tokensOwed1
        )
    {
        Position.Info storage p = get(owner, tickLower, tickUpper);
        return (p.liquidity, p.feeGrowthInside0LastX128, p.feeGrowthInside1LastX128, p.tokensOwed0, p.tokensOwed1);
    }
}
