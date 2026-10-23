// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

interface Vm {
    function deployCode(string calldata artifactPath) external returns (address);
    function writeLine(string calldata path, string calldata data) external;
    function writeFile(string calldata path, string calldata data) external;
    function toString(uint256 value) external pure returns (string memory);
    function toString(int256 value) external pure returns (string memory);
}

interface IOracleHarness {
    function initialize(uint32 time) external returns (uint16, uint16);
    function write(uint32 time, int24 tick, uint128 liquidity) external returns (uint16, uint16);
    function grow(uint16 next) external returns (uint16);
    function observeSingle(uint32 time, uint32 secondsAgo, int24 tick, uint128 liquidity)
        external
        view
        returns (int56, uint160);
    function observe(uint32 time, uint32[] memory secondsAgos, int24 tick, uint128 liquidity)
        external
        view
        returns (int56[] memory, uint160[] memory);
    function state() external view returns (uint16, uint16, uint16);
    function observation(uint16 i) external view returns (uint32, int56, uint160, bool);
}

/// @notice Generates a stateful replay log for the Oracle library.
/// Output: `vectors/oracle.txt`. Each op line is `op|args|ok|result` or `op|args|err|REVERT_REASON`
/// (empty reason = no string reason, e.g. a division by zero), followed by a `st|` line with
/// index|cardinality|cardinalityNext|ts|tick|spl|initialized of the observation at index.
/// `reset` starts a fresh harness; `dump|k:ts:tick:spl:init|...` lists every slot below cardinalityNext.
/// Run: `forge test --match-test testGenerateOracle`
contract GenerateOracleVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    string constant FILE = "vectors/oracle.txt";
    uint256 constant STEPS = 2000;
    uint256 constant SEGMENT = 500;
    uint256 constant TWO_32 = 4294967296;

    uint256 seed = 0x4f52434c; // "ORCL"
    IOracleHarness h;
    uint32 t; // current block time
    uint32 start; // initialization time of the current segment
    bool extreme; // extreme segments use a constant max-magnitude tick and huge time steps,
    int24 fixedTick; // so the tick and seconds-per-liquidity accumulators overflow their widths

    function rand() internal returns (uint256) {
        seed = uint256(keccak256(abi.encode(seed)));
        return seed;
    }

    function testGenerateOracle() public {
        vm.writeFile(FILE, "");
        for (uint256 step = 0; step < STEPS; step++) {
            if (step % SEGMENT == 0) newSegment((step / SEGMENT) % 2 == 1);
            randomOp();
            if ((step + 1) % 50 == 0) dump();
        }
    }

    // ---------- segments and random choices ----------

    function newSegment(bool extremeMode) internal {
        h = IOracleHarness(vm.deployCode("OracleHarness.sol:OracleHarness"));
        line("reset");
        uint256 t0 = rand() % 3 == 0 ? TWO_32 - 1 - (rand() % 20000) : rand() % TWO_32;
        t = uint32(t0);
        start = t;
        extreme = extremeMode;
        fixedTick = rand() % 2 == 0 ? int24(887272) : int24(-887272);

        // before initialize: observe and grow must revert with "I"
        uint32[] memory agos = new uint32[](2);
        agos[0] = 1;
        agos[1] = 60;
        opObs(t, agos, 0, 1);
        opGrow(5);

        (uint16 c, uint16 n) = h.initialize(t);
        line(string(abi.encodePacked("init|", su(t), "|ok|", su(c), ",", su(n))));
        logState();
    }

    function advance(uint256 dt) internal {
        t = uint32(uint256(t) + dt);
    }

    /// Seconds since the segment was initialized, modulo 2^32.
    function elapsed() internal view returns (uint256) {
        return (uint256(t) + TWO_32 - uint256(start)) % TWO_32;
    }

    function randomOp() internal {
        uint256 pick = rand() % 100;
        if (pick < 25) {
            uint256 dt = rand() % 4 == 0 ? 0 : (extreme ? rand() % (TWO_32 / 4) : rand() % 10001);
            advance(dt);
            opWrite(t, randTick(), randLiq());
        } else if (pick < 35) {
            opGrow(uint16(1 + rand() % 50));
        } else if (pick < 80) {
            uint32 tq = uint32(uint256(t) + (rand() % 3 == 0 ? rand() % 500 : 0));
            opObsOne(tq, randAgo(), randTick(), randLiq());
        } else {
            uint32 tq = uint32(uint256(t) + (rand() % 3 == 0 ? rand() % 500 : 0));
            uint256 n = 1 + rand() % 5;
            uint32[] memory agos = new uint32[](n);
            for (uint256 k = 0; k < n; k++) {
                agos[k] = randAgo();
            }
            opObs(tq, agos, randTick(), randLiq());
        }
    }

    function randTick() internal returns (int24) {
        if (extreme) return fixedTick;
        return int24(int256(rand() % 1774545) - 887272);
    }

    function randLiq() internal returns (uint128) {
        uint256 pick = rand() % 10;
        if (pick == 0) return 0;
        if (pick == 1) return 1;
        if (pick == 2) return type(uint128).max;
        return uint128(rand() >> (rand() % 128));
    }

    function randAgo() internal returns (uint32) {
        uint256 m = rand() % 10;
        if (m == 0) return 0;
        if (m <= 3) return uint32(rand() % 20000);
        if (m <= 6) return uint32(rand() % (elapsed() + 1));
        if (m == 7) return uint32(elapsed() + rand() % 500);
        return uint32(rand());
    }

    // ---------- logging ----------

    function line(string memory s) internal {
        vm.writeLine(FILE, s);
    }

    function su(uint256 v) internal pure returns (string memory) {
        return vm.toString(v);
    }

    function si(int256 v) internal pure returns (string memory) {
        return vm.toString(v);
    }

    function b(bool v) internal pure returns (string memory) {
        return v ? "1" : "0";
    }

    function joinU32(uint32[] memory xs) internal pure returns (string memory out) {
        for (uint256 k = 0; k < xs.length; k++) {
            out = string(abi.encodePacked(out, k == 0 ? "" : ",", su(xs[k])));
        }
    }

    function joinI(int56[] memory xs) internal pure returns (string memory out) {
        for (uint256 k = 0; k < xs.length; k++) {
            out = string(abi.encodePacked(out, k == 0 ? "" : ",", si(xs[k])));
        }
    }

    function joinU(uint160[] memory xs) internal pure returns (string memory out) {
        for (uint256 k = 0; k < xs.length; k++) {
            out = string(abi.encodePacked(out, k == 0 ? "" : ",", su(xs[k])));
        }
    }

    /// Observation at slot `i` as `ts|tick|spl|init` with the given separator.
    function obsStr(uint16 i, string memory sep) internal view returns (string memory) {
        (uint32 ts, int56 tk, uint160 spl, bool ini) = h.observation(i);
        string memory head = string.concat(su(ts), sep, si(tk));
        return string.concat(head, sep, su(spl), sep, b(ini));
    }

    function logState() internal {
        (uint16 idx, uint16 card, uint16 next) = h.state();
        line(string(abi.encodePacked("st|", su(idx), "|", su(card), "|", su(next), "|", obsStr(idx, "|"))));
    }

    function dump() internal {
        (, , uint16 next) = h.state();
        string memory out = "dump";
        for (uint16 k = 0; k < next; k++) {
            out = string(abi.encodePacked(out, "|", su(k), ":", obsStr(k, ":")));
        }
        line(out);
    }

    // ---------- ops ----------
    // Calls go through low-level `call` so a revert is data (`ok == false`), never an exception.

    /// Writes one log record: `op|args|status|payload`.
    function rec(string memory op, string memory args, string memory status, string memory payload) internal {
        line(string(abi.encodePacked(op, "|", args, "|", status, "|", payload)));
    }

    function opWrite(uint32 time, int24 tick, uint128 liq) internal {
        string memory args = string(abi.encodePacked(su(time), "|", si(tick), "|", su(liq)));
        (bool ok, bytes memory ret) = address(h).call(abi.encodeCall(IOracleHarness.write, (time, tick, liq)));
        if (ok) {
            (uint16 idx, uint16 card) = abi.decode(ret, (uint16, uint16));
            rec("write", args, "ok", string(abi.encodePacked(su(idx), ",", su(card))));
        } else {
            rec("write", args, "err", revertReason(ret));
        }
        logState();
    }

    function opGrow(uint16 next) internal {
        (bool ok, bytes memory ret) = address(h).call(abi.encodeCall(IOracleHarness.grow, (next)));
        if (ok) {
            rec("grow", su(next), "ok", su(abi.decode(ret, (uint16))));
        } else {
            rec("grow", su(next), "err", revertReason(ret));
        }
        logState();
    }

    function opObsOne(uint32 time, uint32 ago, int24 tick, uint128 liq) internal {
        string memory args = string(abi.encodePacked(su(time), "|", su(ago), "|", si(tick), "|", su(liq)));
        (bool ok, bytes memory ret) =
            address(h).call(abi.encodeCall(IOracleHarness.observeSingle, (time, ago, tick, liq)));
        if (ok) {
            (int56 tk, uint160 spl) = abi.decode(ret, (int56, uint160));
            rec("obsone", args, "ok", string(abi.encodePacked(si(tk), ",", su(spl))));
        } else {
            rec("obsone", args, "err", revertReason(ret));
        }
        logState();
    }

    function opObs(uint32 time, uint32[] memory agos, int24 tick, uint128 liq) internal {
        string memory args = string(abi.encodePacked(su(time), "|", si(tick), "|", su(liq), "|", joinU32(agos)));
        (bool ok, bytes memory ret) = address(h).call(abi.encodeCall(IOracleHarness.observe, (time, agos, tick, liq)));
        if (ok) {
            (int56[] memory tks, uint160[] memory spls) = abi.decode(ret, (int56[], uint160[]));
            rec("obs", args, "ok", string(abi.encodePacked(joinI(tks), ";", joinU(spls))));
        } else {
            rec("obs", args, "err", revertReason(ret));
        }
        logState();
    }

    /// Decodes `Error(string)` revert data; anything else (e.g. an invalid opcode) is an empty reason.
    function revertReason(bytes memory ret) internal pure returns (string memory) {
        if (ret.length < 4 || ret[0] != 0x08 || ret[1] != 0xc3 || ret[2] != 0x79 || ret[3] != 0xa0) return "";
        bytes memory body = new bytes(ret.length - 4);
        for (uint256 k = 0; k < body.length; k++) {
            body[k] = ret[k + 4];
        }
        return abi.decode(body, (string));
    }
}
