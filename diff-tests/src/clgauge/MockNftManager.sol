// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity =0.7.6;
pragma abicoder v2;

import {INonfungiblePositionManager} from "contracts/periphery/interfaces/INonfungiblePositionManager.sol";
import {FullMath} from "contracts/core/libraries/FullMath.sol";
import {FixedPoint128} from "contracts/core/libraries/FixedPoint128.sol";

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
    function positions(bytes32 key)
        external
        view
        returns (uint128 liquidity, uint256 feeGrowthInside0LastX128, uint256 feeGrowthInside1LastX128, uint128, uint128);
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
/// The position accounting is the real manager's, per NFT: `mintPosition` snapshots the pool
/// position's fee growth, and `collect` and `decreaseLiquidity` follow `NonfungiblePositionManager.sol`
/// (the `isStaked` branches, `tokensOwed`, the `burn(0)` poke). The differences are only in the
/// entry points: `mintPosition` replaces `mint` (no ETH, no deadline, no pool creation), there are no
/// `amountMin` checks, and the 3-argument `burn`/`collect` are called with the owner argument (the
/// same pool position as `msg.sender`).
/// `WETH9()` returns zero: the gauge only records it.
contract MockNftManager {
    struct Position {
        address token0;
        address token1;
        int24 tickSpacing;
        int24 tickLower;
        int24 tickUpper;
        uint128 liquidity;
        uint256 feeGrowthInside0LastX128;
        uint256 feeGrowthInside1LastX128;
        uint128 tokensOwed0;
        uint128 tokensOwed1;
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
        return (
            0,
            address(0),
            p.token0,
            p.token1,
            p.tickSpacing,
            p.tickLower,
            p.tickUpper,
            p.liquidity,
            p.feeGrowthInside0LastX128,
            p.feeGrowthInside1LastX128,
            p.tokensOwed0,
            p.tokensOwed1
        );
    }

    function setApprovalForAll(address operator, bool approved) external {
        isApprovedForAll[msg.sender][operator] = approved;
    }

    function _isAuthorized(address spender, uint256 tokenId) internal view returns (bool) {
        address owner = ownerOf(tokenId);
        return owner == spender || isApprovedForAll[owner][spender];
    }

    /// @notice Mints a position for `owner`: liquidity goes into the pool under this manager's address,
    /// and the NFT starts in sync with the pool position (`mint` in NonfungiblePositionManager).
    function mintPosition(address owner, int24 tickSpacing, int24 tickLower, int24 tickUpper, uint128 liquidity)
        external
        returns (uint256 tokenId, uint256 amount0, uint256 amount1)
    {
        (amount0, amount1) = pool.mint(address(this), tickLower, tickUpper, liquidity, "");
        (, uint256 feeGrowthInside0LastX128, uint256 feeGrowthInside1LastX128,,) =
            pool.positions(keccak256(abi.encodePacked(address(this), tickLower, tickUpper)));
        tokenId = nextId++;
        _owners[tokenId] = owner;
        _positions[tokenId] = Position(
            token0,
            token1,
            tickSpacing,
            tickLower,
            tickUpper,
            liquidity,
            feeGrowthInside0LastX128,
            feeGrowthInside1LastX128,
            0,
            0
        );
    }

    function safeTransferFrom(address from, address to, uint256 tokenId) external {
        require(ownerOf(tokenId) == from, "ERC721: transfer of token that is not own");
        require(_isAuthorized(msg.sender, tokenId), "ERC721: caller is not token owner or approved");
        _owners[tokenId] = to;
    }

    /// @notice `NonfungiblePositionManager.collect`. Pays `min(max, tokensOwed)` of this NFT from the
    /// pool position that holds it, after syncing that position (`burn(0)`) and this NFT's snapshot.
    function collect(INonfungiblePositionManager.CollectParams calldata params)
        external
        payable
        returns (uint256 amount0, uint256 amount1)
    {
        require(params.amount0Max > 0 || params.amount1Max > 0);
        // allow collecting to the nft position manager address with address 0
        address recipient = params.recipient == address(0) ? address(this) : params.recipient;
        require(_isAuthorized(msg.sender, params.tokenId), "Not approved");

        Position storage position = _positions[params.tokenId];
        (uint128 tokensOwed0, uint128 tokensOwed1) = (position.tokensOwed0, position.tokensOwed1);

        address gauge = pool.gauge();
        bool isStaked = ownerOf(params.tokenId) == gauge;

        // trigger an update of the position fees owed and fee growth snapshots if it has any liquidity
        if (position.liquidity > 0) {
            uint256 feeGrowthInside0LastX128;
            uint256 feeGrowthInside1LastX128;
            if (!isStaked) {
                pool.burn(position.tickLower, position.tickUpper, 0, address(this));

                (, feeGrowthInside0LastX128, feeGrowthInside1LastX128,,) =
                    pool.positions(keccak256(abi.encodePacked(address(this), position.tickLower, position.tickUpper)));

                tokensOwed0 += uint128(
                    FullMath.mulDiv(
                        feeGrowthInside0LastX128 - position.feeGrowthInside0LastX128,
                        position.liquidity,
                        FixedPoint128.Q128
                    )
                );
                tokensOwed1 += uint128(
                    FullMath.mulDiv(
                        feeGrowthInside1LastX128 - position.feeGrowthInside1LastX128,
                        position.liquidity,
                        FixedPoint128.Q128
                    )
                );
            } else {
                pool.burn(position.tickLower, position.tickUpper, 0, gauge);

                (, feeGrowthInside0LastX128, feeGrowthInside1LastX128,,) =
                    pool.positions(keccak256(abi.encodePacked(gauge, position.tickLower, position.tickUpper)));
            }

            position.feeGrowthInside0LastX128 = feeGrowthInside0LastX128;
            position.feeGrowthInside1LastX128 = feeGrowthInside1LastX128;
        }

        // compute the arguments to give to the pool#collect method
        (uint128 amount0Collect, uint128 amount1Collect) = (
            params.amount0Max > tokensOwed0 ? tokensOwed0 : params.amount0Max,
            params.amount1Max > tokensOwed1 ? tokensOwed1 : params.amount1Max
        );

        // the actual amounts collected are returned
        if (!isStaked) {
            (amount0, amount1) = pool.collect(
                recipient, position.tickLower, position.tickUpper, amount0Collect, amount1Collect, address(this)
            );
        } else {
            (amount0, amount1) =
                pool.collect(recipient, position.tickLower, position.tickUpper, amount0Collect, amount1Collect, gauge);
        }

        // sometimes there will be a few less wei than expected due to rounding down in core, but we just subtract the full amount expected
        // instead of the actual amount so we can burn the token
        (position.tokensOwed0, position.tokensOwed1) = (tokensOwed0 - amount0Collect, tokensOwed1 - amount1Collect);
    }

    /// @notice `NonfungiblePositionManager.decreaseLiquidity`, without the `deadline` and `amountMin`
    /// checks. Burns `liquidity` from the pool position that holds the NFT and adds the burned amounts
    /// and the fees since the snapshot to the NFT's `tokensOwed`. Collect them with `collect`.
    function decreaseLiquidity(uint256 tokenId, uint128 liquidity) external returns (uint256 amount0, uint256 amount1) {
        require(liquidity > 0);
        require(_isAuthorized(msg.sender, tokenId), "Not approved");
        Position storage position = _positions[tokenId];

        uint128 positionLiquidity = position.liquidity;
        require(positionLiquidity >= liquidity);

        address gauge = pool.gauge();
        bool isStaked = ownerOf(tokenId) == gauge;
        if (!isStaked) {
            (amount0, amount1) = pool.burn(position.tickLower, position.tickUpper, liquidity, address(this));
        } else {
            (amount0, amount1) = pool.burn(position.tickLower, position.tickUpper, liquidity, gauge);
        }

        // this is now updated to the current transaction
        (, uint256 feeGrowthInside0LastX128, uint256 feeGrowthInside1LastX128,,) = pool.positions(
            isStaked
                ? keccak256(abi.encodePacked(gauge, position.tickLower, position.tickUpper))
                : keccak256(abi.encodePacked(address(this), position.tickLower, position.tickUpper))
        );

        position.tokensOwed0 += uint128(amount0);
        position.tokensOwed1 += uint128(amount1);

        if (!isStaked) {
            position.tokensOwed0 += uint128(
                FullMath.mulDiv(
                    feeGrowthInside0LastX128 - position.feeGrowthInside0LastX128, positionLiquidity, FixedPoint128.Q128
                )
            );
            position.tokensOwed1 += uint128(
                FullMath.mulDiv(
                    feeGrowthInside1LastX128 - position.feeGrowthInside1LastX128, positionLiquidity, FixedPoint128.Q128
                )
            );
        }

        position.feeGrowthInside0LastX128 = feeGrowthInside0LastX128;
        position.feeGrowthInside1LastX128 = feeGrowthInside1LastX128;
        // subtraction is safe because we checked positionLiquidity is gte liquidity
        position.liquidity = positionLiquidity - liquidity;
    }

    /// @dev Pays the owed tokens of a mint to the pool (the pool checks its balance afterwards).
    function uniswapV3MintCallback(uint256 amount0Owed, uint256 amount1Owed, bytes calldata) external {
        if (amount0Owed > 0) IMockMintable(token0).mint(msg.sender, amount0Owed);
        if (amount1Owed > 0) IMockMintable(token1).mint(msg.sender, amount1Owed);
    }
}
