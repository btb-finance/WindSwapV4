// SPDX-License-Identifier: MIT
pragma solidity =0.7.6;

/// @notice 0.7.6 stand-in for OpenZeppelin 3.4 `token/ERC721/ERC721Holder.sol` (same behaviour).
interface IERC721Receiver {
    function onERC721Received(address operator, address from, uint256 tokenId, bytes calldata data)
        external
        returns (bytes4);
}

contract ERC721Holder is IERC721Receiver {
    function onERC721Received(address, address, uint256, bytes memory) public virtual override returns (bytes4) {
        return this.onERC721Received.selector;
    }
}
