// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IRewardsDistributor} from "../../windv2/contracts/interfaces/IRewardsDistributor.sol";

/// @notice Test double for the windv2 `Minter` as seen by `RewardsDistributor`: it exposes `activePeriod()`
/// and forwards rebases the way `Minter.updatePeriod` does (transfer WIND to the distributor, then
/// `checkpointToken()`). The generator runs two of these so `setMinter` can hand the role over.
contract MockRebaseMinter {
    IERC20 public immutable wind;
    uint256 public activePeriod;

    constructor(IERC20 _wind) {
        wind = _wind;
    }

    /// @notice Simulates `Minter.updatePeriod` setting the active period.
    function setActivePeriod(uint256 period) external {
        activePeriod = period;
    }

    /// @notice Transfers `amount` of WIND to `distributor` and calls `checkpointToken()` as this minter.
    function rebase(address distributor, uint256 amount) external {
        wind.transfer(distributor, amount);
        IRewardsDistributor(distributor).checkpointToken();
    }

    /// @notice Calls `setMinter(newMinter)` on `distributor` as this minter.
    function handOver(address distributor, address newMinter) external {
        IRewardsDistributor(distributor).setMinter(newMinter);
    }
}
