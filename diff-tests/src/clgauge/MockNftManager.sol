// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity =0.7.6;
pragma abicoder v2;

import {INonfungiblePositionManager} from "contracts/periphery/interfaces/INonfungiblePositionManager.sol";

interface IMockPoolNft {
    function gauge() external view returns (address);
    function mint(address recipient, int24 tickLower, int24 tickUpper, uint128 amount, bytes calldata data)
        external
        returns (uint256, uint256);
    function burn(int24 tickLower, int24 tickUpper, uint128 amount, address owner)
        external
        returns (uint256, uint256);
    function collect(
        address recipient,
        int24 tickLower,
        int24 tickUpper,
        uint128 amount0Requested,
        uint128 amount1Requested,
        address owner
    ) external returns (uint128, uint128);
}

interface IMockMintable {
    function mint(address to, uint256 amount) external;
}

/// @notice Stands in for NonfungiblePositionManager (the `nft` of the CLPool and the CLGauge).
/// Holds the positions and owners of NFTs and mints liquidity into the pool under its own address,
/// as the real manager does. The ERC721 surface is minimal:
///  - `ownerOf` reverts for unknown ids, `positions` reverts for unknown ids (as the real one does);
///  - `safeTransferFrom` requires the caller to be the owner or approved for all by the owner.
///    It does not call `onERC721Received` (the gauge's receiver is a no-op).
///  - `collect` requires the caller to be owner or approved (the real manager does the same). It mirrors
///    `NonfungiblePositionManager.collect`: when the position has liquidity it calls `burn(0)` so the pool
///    syncs the owed amounts, on the gauge's pool position if the gauge holds the NFT and on its own
///    otherwise, then pays out the requested owed amounts from that same pool position. The real
///    manager pays per-NFT `tokensOwed` (fee-growth snapshots); this stand-in pays the pool position's
///    owed amounts directly. Only the payout amounts differ; the gauge logic does not read them.
/// `WETH9()` returns zero: the gauge only records it.
contract MockNftManager {
    struct Position {
        address token0;
        address token1;
        int24 tickSpacing;
        int24 tickLower;
        int24 tickUpper;
        uint128 liquidity;
    }

    IMockPoolNft public pool;
    address public token0;
    address public token1;
    uint256 public nextId = 1;

    mapping(uint256 => address) internal _owners;
    mapping(uint256 => Position) internal _positions;
    mapping(address => mapping(address => bool)) public isApprovedForAll;

    /// @dev Wires the pool and its tokens. Called once by the harness after the pool is initialized.
    function setPool(address _pool, address _token0, address _token1) external {
        pool = IMockPoolNft(_pool);
        token0 = _token0;
        token1 = _token1;
    }

    function WETH9() external pure returns (address) {
        return address(0);
    }

    function ownerOf(uint256 tokenId) public view returns (address) {
        address o = _owners[tokenId];
        require(o != address(0), "ERC721: invalid token ID");
        return o;
    }

    /// @dev Harness helper: owner or zero for unknown ids (no revert).
    function ownerOrZero(uint256 tokenId) external view returns (address) {
        return _owners[tokenId];
    }

    /// @dev Harness helper: tick range and existence of a token, without reverting.
    function rangeOf(uint256 tokenId) external view returns (int24 tickLower, int24 tickUpper, bool exists) {
        if (_owners[tokenId] == address(0)) return (0, 0, false);
        Position storage p = _positions[tokenId];
        return (p.tickLower, p.tickUpper, true);
    }

    function positions(uint256 tokenId)
        external
        view
        returns (
            uint96 nonce,
            address operator,
            address _token0,
            address _token1,
            int24 tickSpacing,
            int24 tickLower,
            int24 tickUpper,
            uint128 liquidity,
            uint256 feeGrowthInside0LastX128,
            uint256 feeGrowthInside1LastX128,
            uint128 tokensOwed0,
            uint128 tokensOwed1
        )
    {
        require(_owners[tokenId] != address(0), "Invalid token ID");
        Position storage p = _positions[tokenId];
        return (0, address(0), p.token0, p.token1, p.tickSpacing, p.tickLower, p.tickUpper, p.liquidity, 0, 0, 0, 0);
    }

    function setApprovalForAll(address operator, bool approved) external {
        isApprovedForAll[msg.sender][operator] = approved;
    }

    function _isAuthorized(address spender, uint256 tokenId) internal view returns (bool) {
        address owner = ownerOf(tokenId);
        return owner == spender || isApprovedForAll[owner][spender];
    }

    /// @notice Mints a position for `owner`: liquidity goes into the pool under this manager's address.
    function mintPosition(address owner, int24 tickSpacing, int24 tickLower, int24 tickUpper, uint128 liquidity)
        external
        returns (uint256 tokenId, uint256 amount0, uint256 amount1)
    {
        (amount0, amount1) = pool.mint(address(this), tickLower, tickUpper, liquidity, "");
        tokenId = nextId++;
        _owners[tokenId] = owner;
        _positions[tokenId] = Position(token0, token1, tickSpacing, tickLower, tickUpper, liquidity);
    }

    function safeTransferFrom(address from, address to, uint256 tokenId) external {
        require(ownerOf(tokenId) == from, "ERC721: transfer of token that is not own");
        require(_isAuthorized(msg.sender, tokenId), "ERC721: caller is not token owner or approved");
        _owners[tokenId] = to;
    }

    function collect(INonfungiblePositionManager.CollectParams calldata params)
        external
        payable
        returns (uint256 amount0, uint256 amount1)
    {
        require(_isAuthorized(msg.sender, params.tokenId), "Not approved");
        Position storage p = _positions[params.tokenId];
        address poolOwner = ownerOf(params.tokenId) == pool.gauge() ? pool.gauge() : address(this);
        if (p.liquidity > 0) {
            pool.burn(p.tickLower, p.tickUpper, 0, poolOwner);
        }
        (uint128 a0, uint128 a1) = pool.collect(
            params.recipient, p.tickLower, p.tickUpper, params.amount0Max, params.amount1Max, poolOwner
        );
        return (a0, a1);
    }

    /// @dev Pays the owed tokens of a mint to the pool (the pool checks its balance afterwards).
    function uniswapV3MintCallback(uint256 amount0Owed, uint256 amount1Owed, bytes calldata) external {
        if (amount0Owed > 0) IMockMintable(token0).mint(msg.sender, amount0Owed);
        if (amount1Owed > 0) IMockMintable(token1).mint(msg.sender, amount1Owed);
    }
}
