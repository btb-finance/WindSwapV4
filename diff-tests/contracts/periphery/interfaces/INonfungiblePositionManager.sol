// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity =0.7.6;
pragma abicoder v2;

/// @notice Trimmed copy of the Wind Swap `INonfungiblePositionManager` for the CLGauge differential test.
/// It keeps only the members CLGauge calls (`WETH9`, `positions`, `collect`, `ownerOf`,
/// `safeTransferFrom`) with the same selectors and struct layout as the original. The original
/// file imports OpenZeppelin ERC721Enumerable and the full periphery interface set, which are
/// not in `windv2-deps`. The test drives a mock manager (`src/clgauge/MockNftManager.sol`).
interface INonfungiblePositionManager {
    struct CollectParams {
        uint256 tokenId;
        address recipient;
        uint128 amount0Max;
        uint128 amount1Max;
    }

    function WETH9() external view returns (address);

    function ownerOf(uint256 tokenId) external view returns (address);

    function safeTransferFrom(address from, address to, uint256 tokenId) external;

    function positions(uint256 tokenId)
        external
        view
        returns (
            uint96 nonce,
            address operator,
            address token0,
            address token1,
            int24 tickSpacing,
            int24 tickLower,
            int24 tickUpper,
            uint128 liquidity,
            uint256 feeGrowthInside0LastX128,
            uint256 feeGrowthInside1LastX128,
            uint128 tokensOwed0,
            uint128 tokensOwed1
        );

    function collect(CollectParams calldata params) external payable returns (uint256 amount0, uint256 amount1);
}
