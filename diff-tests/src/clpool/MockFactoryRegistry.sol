// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity =0.7.6;

/// @notice Stands in for FactoryRegistry: only the gauge factory slot CLPool checks.
contract MockFactoryRegistry {
    address public gaugeFactory;

    function setGaugeFactory(address _gaugeFactory) external {
        gaugeFactory = _gaugeFactory;
    }

    function factoriesToPoolFactory(address)
        external
        view
        returns (address votingRewardsFactory, address gaugeFactoryOut)
    {
        return (address(0), gaugeFactory);
    }
}
