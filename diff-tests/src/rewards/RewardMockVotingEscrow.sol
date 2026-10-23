// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

/// @notice Test double for the windv2 `VotingEscrow` as used by `VotingReward.getReward`:
/// `ownerOf(tokenId)` (payout recipient) and `isApprovedOrOwner(spender, tokenId)`.
/// Owner of token `id` is `address(uint160(0x1000 + id))`. Approvals are set by the generator.
contract RewardMockVotingEscrow {
    mapping(uint256 => mapping(address => bool)) public approved;

    function ownerOf(uint256 tokenId) public pure returns (address) {
        return address(uint160(0x1000 + tokenId));
    }

    function setApproved(uint256 tokenId, address operator, bool value) external {
        approved[tokenId][operator] = value;
    }

    function isApprovedOrOwner(address spender, uint256 tokenId) external view returns (bool) {
        return spender == ownerOf(tokenId) || approved[tokenId][spender];
    }
}
