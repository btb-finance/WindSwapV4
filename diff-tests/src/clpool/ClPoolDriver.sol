// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity =0.7.6;

// Forces CLPool.sol (and its imports) into the Foundry build, so vm.deployCode("CLPool.sol:CLPool") works.
import "contracts/core/CLPool.sol";
import "./MockERC20.sol";

interface ICLPoolOps {
    function setGaugeAndPositionManager(address gauge, address nft) external;
    function mint(address recipient, int24 tickLower, int24 tickUpper, uint128 amount, bytes calldata data)
        external
        returns (uint256 amount0, uint256 amount1);
    function burn(int24 tickLower, int24 tickUpper, uint128 amount, address owner)
        external
        returns (uint256 amount0, uint256 amount1);
    function collect(
        address recipient,
        int24 tickLower,
        int24 tickUpper,
        uint128 amount0Requested,
        uint128 amount1Requested,
        address owner
    ) external returns (uint128 amount0, uint128 amount1);
    function swap(
        address recipient,
        bool zeroForOne,
        int256 amountSpecified,
        uint160 sqrtPriceLimitX96,
        bytes calldata data
    ) external returns (int256 amount0, int256 amount1);
}

/// @notice The NFT manager, gauge factory and payer of the CLPool under test. It is `nft` for the
/// owner-taking burn/collect variants, the gauge factory that calls setGaugeAndPositionManager, and
/// it pays the mint and swap callbacks by minting exactly the owed mock tokens to the pool.
contract ClPoolDriver {
    ICLPoolOps public pool;
    address public token0;
    address public token1;

    function setup(address _pool, address _token0, address _token1) external {
        pool = ICLPoolOps(_pool);
        token0 = _token0;
        token1 = _token1;
    }

    function setGauge(address gauge) external {
        pool.setGaugeAndPositionManager(gauge, address(this));
    }

    function mint(address recipient, int24 tickLower, int24 tickUpper, uint128 amount)
        external
        returns (uint256, uint256)
    {
        return pool.mint(recipient, tickLower, tickUpper, amount, "");
    }

    function burn(int24 tickLower, int24 tickUpper, uint128 amount) external returns (uint256, uint256) {
        return pool.burn(tickLower, tickUpper, amount, address(this));
    }

    function collect(
        address recipient,
        int24 tickLower,
        int24 tickUpper,
        uint128 amount0Requested,
        uint128 amount1Requested,
        address owner
    ) external returns (uint128, uint128) {
        return pool.collect(recipient, tickLower, tickUpper, amount0Requested, amount1Requested, owner);
    }

    function swap(address recipient, bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96)
        external
        returns (int256, int256)
    {
        return pool.swap(recipient, zeroForOne, amountSpecified, sqrtPriceLimitX96, "");
    }

    function uniswapV3MintCallback(uint256 amount0Owed, uint256 amount1Owed, bytes calldata) external {
        if (amount0Owed > 0) MockERC20(token0).mint(msg.sender, amount0Owed);
        if (amount1Owed > 0) MockERC20(token1).mint(msg.sender, amount1Owed);
    }

    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata) external {
        if (amount0Delta > 0) MockERC20(token0).mint(msg.sender, uint256(amount0Delta));
        if (amount1Delta > 0) MockERC20(token1).mint(msg.sender, uint256(amount1Delta));
    }
}
