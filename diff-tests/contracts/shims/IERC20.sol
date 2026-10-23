// SPDX-License-Identifier: MIT
pragma solidity =0.7.6;

/// @notice 0.7.6 stand-in for OpenZeppelin 3.4 `token/ERC20/IERC20.sol`. The repo's OZ copy in
/// `windv2-deps` is 0.8-only, so the gauge differential test imports this one instead.
interface IERC20 {
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    function totalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);
    function transfer(address recipient, uint256 amount) external returns (bool);
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transferFrom(address sender, address recipient, uint256 amount) external returns (bool);
}
