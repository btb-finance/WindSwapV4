// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// @notice Test double for windv2 `Wind`: an ERC20 whose `mint` is restricted to `minter`
/// (as in `Wind.sol`). `testMint` is unrestricted and only used by the generator to move supply.
contract MockWind is ERC20 {
    address public minter;

    constructor() ERC20("Mock Wind", "WIND") {
        minter = msg.sender;
    }

    function setMinter(address _minter) external {
        require(msg.sender == minter, "NotMinter");
        minter = _minter;
    }

    function mint(address account, uint256 amount) external returns (bool) {
        require(msg.sender == minter, "NotMinter");
        _mint(account, amount);
        return true;
    }

    function testMint(address account, uint256 amount) external {
        _mint(account, amount);
    }
}
