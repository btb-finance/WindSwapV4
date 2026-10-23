// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

interface Vm {
    function deployCode(string calldata artifactPath) external returns (address);
    function writeLine(string calldata path, string calldata data) external;
    function writeFile(string calldata path, string calldata data) external;
    function toString(uint256 value) external pure returns (string memory);
    function toString(int256 value) external pure returns (string memory);
}

interface ITickHarness {
    function tickSpacingToMaxLiquidityPerTick(int24 tickSpacing) external pure returns (uint128);
    function getFeeGrowthInside(int24 tickLower, int24 tickUpper, int24 tickCurrent, uint256 g0, uint256 g1)
        external
        view
        returns (uint256, uint256);
    function getRewardGrowthInside(int24 tickLower, int24 tickUpper, int24 tickCurrent, uint256 g)
        external
        view
        returns (uint256);
    function update(
        int24 tick,
        int24 tickCurrent,
        int128 liquidityDelta,
        uint256 g0,
        uint256 g1,
        uint160 secondsPerLiquidity,
        int56 tickCumulative,
        uint32 time,
        bool upper,
        uint128 maxLiquidity
    ) external returns (bool);
    function updateStake(int24 tick, int128 stakedLiquidityDelta, bool upper) external;
    function clear(int24 tick) external;
    function cross(int24 tick, uint256 g0, uint256 g1, uint160 secondsPerLiquidity, int56 tickCumulative, uint32 time, uint256 rewardGlobal)
        external
        returns (int128, int128);
    function getTick(int24 tick)
        external
        view
        returns (uint128, int128, int128, uint256, uint256, uint256, int56, uint160, uint32, bool);
}

/// @notice Generates differential test vectors from the original Wind Swap `Tick` library.
/// Output:
///  - `vectors/tick_spacing_max_liquidity.txt`: `spacing|ok|maxLiquidity` or `spacing|err`.
///  - `vectors/tick.txt`: stateful replay log, one op per line. Every line is
///    `op|inputs...|ok|outputs...|state` or `op|inputs...|err|state` where `state` is the full
///    `Tick.Info` of the touched tick(s) after the call, as
///    `liquidityGross,liquidityNet,stakedLiquidityNet,feeOutside0,feeOutside1,rewardOutside,
///     tickCumulativeOutside,secondsPerLiquidityOutside,secondsOutside,initialized`.
///    `getFee` and `getReward` log the state of both the lower and the upper tick (last two fields).
/// Run: `forge test --match-test testGenerateTick`
contract GenerateTickVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    uint256 constant STEPS = 4000;
    uint256 constant SPACING_MAX = 1000;

    ITickHarness h;
    uint256 seed = 0x54494b; // "TIK"

    struct UpdateArgs {
        int24 tick;
        int24 cur;
        int128 delta;
        uint256 g0;
        uint256 g1;
        uint160 spl;
        int56 tc;
        uint32 time;
        bool upper;
        uint128 maxL;
    }

    function rand() internal returns (uint256) {
        seed = uint256(keccak256(abi.encode(seed)));
        return seed;
    }

    /// Random value with a random bit length up to `maxBits`, plus edge values.
    function randBits(uint256 maxBits) internal returns (uint256) {
        uint256 pick = rand() % 20;
        uint256 max = maxBits == 256 ? type(uint256).max : (uint256(1) << maxBits) - 1;
        if (pick == 0) return 0;
        if (pick == 1) return 1;
        if (pick == 2) return max;
        uint256 k = rand() % maxBits + 1;
        return rand() >> (256 - k);
    }

    /// Fee / reward growth global: half full width, half small so that `global - outside` wraps.
    function randGlobal() internal returns (uint256) {
        if (rand() % 2 == 0) return randBits(256);
        return rand() % 1000;
    }

    function randSpl() internal returns (uint160) {
        if (rand() % 2 == 0) return uint160(randBits(160));
        return uint160(rand() % 100000);
    }

    function randInt56() internal returns (int56) {
        uint256 pick = rand() % 10;
        if (pick == 0) return type(int56).min;
        if (pick == 1) return type(int56).max;
        uint256 v = randBits(55);
        return rand() % 2 == 0 ? int56(int256(v)) : -int56(int256(v));
    }

    function randLiquidityDelta() internal returns (int128) {
        uint256 pick = rand() % 10;
        if (pick == 0) return type(int128).min;
        if (pick == 1) return type(int128).max;
        if (pick < 4) return int128(int256(rand() % 2000)) - 1000;
        uint256 v = randBits(127);
        return rand() % 2 == 0 ? int128(int256(v)) : -int128(int256(v));
    }

    /// Mostly the real limit, sometimes a small one so that "LO" is hit.
    function randMaxLiquidity() internal returns (uint128) {
        if (rand() % 4 == 0) return uint128(rand() % 3000);
        return type(uint128).max;
    }

    function pickTick(int24[8] memory ts) internal returns (int24) {
        return ts[rand() % 8];
    }

    /// Current tick: sometimes one of the tracked ticks, sometimes anywhere in the TickMath range.
    function pickCurrent(int24[8] memory ts) internal returns (int24) {
        if (rand() % 2 == 0) return ts[rand() % 8];
        return int24(int256(rand() % 1774545) - 887272);
    }

    function s(uint256 v) internal pure returns (string memory) {
        return vm.toString(v);
    }

    function si(int256 v) internal pure returns (string memory) {
        return vm.toString(v);
    }

    function b(bool v) internal pure returns (string memory) {
        return v ? "1" : "0";
    }

    function word(bytes memory d, uint256 i) internal pure returns (uint256 w) {
        assembly {
            w := mload(add(add(d, 32), mul(i, 32)))
        }
    }

    /// Full state of `tick` as one comma-separated field (see the header comment for the order).
    function st(int24 t) internal view returns (string memory out) {
        (, bytes memory d) = address(h).staticcall(abi.encodeWithSelector(ITickHarness.getTick.selector, t));
        for (uint256 i = 0; i < 10; i++) {
            uint256 w = word(d, i);
            string memory v;
            // int128 liquidityNet, int128 stakedLiquidityNet, int56 tickCumulativeOutside: sign-extended words.
            if (i == 1 || i == 2 || i == 6) v = si(int256(w));
            else v = s(w);
            out = i == 0 ? v : string.concat(out, ",", v);
        }
    }

    function testGenerateTick() public {
        h = ITickHarness(vm.deployCode("TickHarness.sol:TickHarness"));
        genSpacing();
        genStateful();
    }

    // ---------------------------------------------------------------- tickSpacingToMaxLiquidityPerTick

    function genSpacing() internal {
        string memory f = "vectors/tick_spacing_max_liquidity.txt";
        vm.writeFile(f, "");
        for (uint256 i = 1; i <= SPACING_MAX; i++) {
            writeSpacing(f, int24(int256(i)));
        }
        int24[9] memory extra = [int24(16384), 887272, 8388607, -1, -60, -887272, -8388608, 0, 3];
        for (uint256 i = 0; i < extra.length; i++) {
            writeSpacing(f, extra[i]);
        }
    }

    function writeSpacing(string memory f, int24 sp) internal {
        string memory inp = si(sp);
        try h.tickSpacingToMaxLiquidityPerTick(sp) returns (uint128 r) {
            vm.writeLine(f, string.concat(inp, "|ok|", s(r)));
        } catch {
            vm.writeLine(f, string.concat(inp, "|err"));
        }
    }

    // ---------------------------------------------------------------- stateful replay

    function genStateful() internal {
        string memory f = "vectors/tick.txt";
        vm.writeFile(f, "");
        int24[8] memory ts = [int24(-887272), -6000, -120, -60, 0, 60, 120, 887272];
        for (uint256 i = 0; i < STEPS; i++) {
            uint256 op = rand() % 100;
            if (op < 40) doUpdate(f, ts);
            else if (op < 55) doUpdateStake(f, ts);
            else if (op < 70) doCross(f, ts);
            else if (op < 75) doClear(f, ts);
            else if (op < 90) doFee(f, ts);
            else doReward(f, ts);
        }
    }

    function randUpdate(int24[8] memory ts) internal returns (UpdateArgs memory a) {
        a.tick = pickTick(ts);
        a.cur = pickCurrent(ts);
        a.delta = randLiquidityDelta();
        a.g0 = randGlobal();
        a.g1 = randGlobal();
        a.spl = randSpl();
        a.tc = randInt56();
        a.time = uint32(randBits(32));
        a.upper = rand() % 2 == 0;
        a.maxL = randMaxLiquidity();
    }

    function doUpdate(string memory f, int24[8] memory ts) internal {
        UpdateArgs memory a = randUpdate(ts);
        string memory inp = string.concat(
            "update|", si(a.tick), "|", si(a.cur), "|", si(a.delta), "|", s(a.g0), "|", s(a.g1)
        );
        inp = string.concat(
            inp, "|", s(a.spl), "|", si(a.tc), "|", s(a.time), "|", b(a.upper), "|", s(a.maxL)
        );
        try h.update(a.tick, a.cur, a.delta, a.g0, a.g1, a.spl, a.tc, a.time, a.upper, a.maxL) returns (bool flipped) {
            vm.writeLine(f, string.concat(inp, "|ok|", b(flipped), "|", st(a.tick)));
        } catch {
            vm.writeLine(f, string.concat(inp, "|err|", st(a.tick)));
        }
    }

    function doUpdateStake(string memory f, int24[8] memory ts) internal {
        int24 tick = pickTick(ts);
        int128 delta = randLiquidityDelta();
        bool upper = rand() % 2 == 0;
        string memory inp = string.concat("updateStake|", si(tick), "|", si(delta), "|", b(upper));
        try h.updateStake(tick, delta, upper) {
            vm.writeLine(f, string.concat(inp, "|ok|", st(tick)));
        } catch {
            vm.writeLine(f, string.concat(inp, "|err|", st(tick)));
        }
    }

    struct CrossArgs {
        int24 tick;
        uint256 g0;
        uint256 g1;
        uint160 spl;
        int56 tc;
        uint32 time;
        uint256 rg;
    }

    /// `cross` has no revert path in the Solidity code, so it is always logged as `ok`.
    function doCross(string memory f, int24[8] memory ts) internal {
        CrossArgs memory a;
        a.tick = pickTick(ts);
        a.g0 = randGlobal();
        a.g1 = randGlobal();
        a.spl = randSpl();
        a.tc = randInt56();
        a.time = uint32(randBits(32));
        a.rg = randGlobal();
        string memory inp = string.concat(
            "cross|", si(a.tick), "|", s(a.g0), "|", s(a.g1), "|", s(a.spl), "|", si(a.tc), "|", s(a.time)
        );
        inp = string.concat(inp, "|", s(a.rg));
        try h.cross(a.tick, a.g0, a.g1, a.spl, a.tc, a.time, a.rg) returns (int128 ln, int128 sln) {
            vm.writeLine(f, string.concat(inp, "|ok|", si(ln), "|", si(sln), "|", st(a.tick)));
        } catch {
            vm.writeLine(f, string.concat(inp, "|err|", st(a.tick)));
        }
    }

    function doClear(string memory f, int24[8] memory ts) internal {
        int24 tick = pickTick(ts);
        string memory inp = string.concat("clear|", si(tick));
        try h.clear(tick) {
            vm.writeLine(f, string.concat(inp, "|ok|", st(tick)));
        } catch {
            vm.writeLine(f, string.concat(inp, "|err|", st(tick)));
        }
    }

    /// Read-only; logs the state of both boundary ticks to check that reads do not write.
    function doFee(string memory f, int24[8] memory ts) internal {
        int24 lo = pickTick(ts);
        int24 up = pickTick(ts);
        int24 cur = pickCurrent(ts);
        uint256 g0 = randGlobal();
        uint256 g1 = randGlobal();
        string memory inp = string.concat(
            "getFee|", si(lo), "|", si(up), "|", si(cur), "|", s(g0), "|", s(g1)
        );
        try h.getFeeGrowthInside(lo, up, cur, g0, g1) returns (uint256 o0, uint256 o1) {
            vm.writeLine(f, string.concat(inp, "|ok|", s(o0), "|", s(o1), "|", st(lo), "|", st(up)));
        } catch {
            vm.writeLine(f, string.concat(inp, "|err|", st(lo), "|", st(up)));
        }
    }

    function doReward(string memory f, int24[8] memory ts) internal {
        int24 lo = pickTick(ts);
        int24 up = pickTick(ts);
        int24 cur = pickCurrent(ts);
        uint256 g = randGlobal();
        string memory inp = string.concat("getReward|", si(lo), "|", si(up), "|", si(cur), "|", s(g));
        try h.getRewardGrowthInside(lo, up, cur, g) returns (uint256 o) {
            vm.writeLine(f, string.concat(inp, "|ok|", s(o), "|", st(lo), "|", st(up)));
        } catch {
            vm.writeLine(f, string.concat(inp, "|err|", st(lo), "|", st(up)));
        }
    }
}
