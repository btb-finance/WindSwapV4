// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity =0.7.6;

import "./libraries/Oracle.sol";

/// @notice Holds an observation array plus the index/cardinality/cardinalityNext state a pool keeps,
/// and exposes the Oracle library functions as external calls so the generator can use try/catch.
contract OracleHarness {
    Oracle.Observation[65535] observations;
    uint16 index;
    uint16 cardinality;
    uint16 cardinalityNext;

    function initialize(uint32 time) external returns (uint16, uint16) {
        (cardinality, cardinalityNext) = Oracle.initialize(observations, time);
        index = 0;
        return (cardinality, cardinalityNext);
    }

    function write(uint32 time, int24 tick, uint128 liquidity) external returns (uint16, uint16) {
        (index, cardinality) =
            Oracle.write(observations, index, time, tick, liquidity, cardinality, cardinalityNext);
        return (index, cardinality);
    }

    function grow(uint16 next) external returns (uint16) {
        cardinalityNext = Oracle.grow(observations, cardinalityNext, next);
        return cardinalityNext;
    }

    function observeSingle(uint32 time, uint32 secondsAgo, int24 tick, uint128 liquidity)
        external
        view
        returns (int56, uint160)
    {
        return Oracle.observeSingle(observations, time, secondsAgo, tick, index, liquidity, cardinality);
    }

    function observe(uint32 time, uint32[] memory secondsAgos, int24 tick, uint128 liquidity)
        external
        view
        returns (int56[] memory, uint160[] memory)
    {
        return Oracle.observe(observations, time, secondsAgos, tick, index, liquidity, cardinality);
    }

    function state() external view returns (uint16, uint16, uint16) {
        return (index, cardinality, cardinalityNext);
    }

    function observation(uint16 i) external view returns (uint32, int56, uint160, bool) {
        Oracle.Observation memory o = observations[i];
        return (o.blockTimestamp, o.tickCumulative, o.secondsPerLiquidityCumulativeX128, o.initialized);
    }
}
