// SPDX-License-Identifier: BUSL-1.1 OR GPL-3.0-or-later
pragma solidity ^0.8.0;

interface IVeArtProxy {
    /// @notice Generate a SVG based on veNFT metadata
    /// @param _tokenId Unique veNFT identifier
    /// @return output SVG metadata as HTML tag
    function tokenURI(uint256 _tokenId) external view returns (string memory output);
}
