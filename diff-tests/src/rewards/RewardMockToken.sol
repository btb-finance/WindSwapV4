// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// @notice Reward token for the rewards generator. Minting is open; the generator mints to the sender
/// before each notify, so the reward contract's `safeTransferFrom` always succeeds.
contract RewardMockToken is ERC20 {
    constructor(string memory _name) ERC20(_name, _name) {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}
