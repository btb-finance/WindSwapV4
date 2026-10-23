// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

interface Vm {
    function deployCode(string calldata artifactPath) external returns (address);
    function writeLine(string calldata path, string calldata data) external;
    function writeFile(string calldata path, string calldata data) external;
    function warp(uint256 newTimestamp) external;
    function toString(uint256 value) external pure returns (string memory);
    function toString(int256 value) external pure returns (string memory);
    function toString(address value) external pure returns (string memory);
}

interface IClPool {
    function initialize(
        address factory,
        address token0,
        address token1,
        int24 tickSpacing,
        address factoryRegistry,
        uint160 sqrtPriceX96
    ) external;
    function slot0() external view returns (uint160, int24, uint16, uint16, uint16, bool);
    function liquidity() external view returns (uint128);
    function stakedLiquidity() external view returns (uint128);
    function feeGrowthGlobal0X128() external view returns (uint256);
    function feeGrowthGlobal1X128() external view returns (uint256);
    function rewardGrowthGlobalX128() external view returns (uint256);
    function gaugeFees() external view returns (uint128, uint128);
    function rewardRate() external view returns (uint256);
    function rewardReserve() external view returns (uint256);
    function periodFinish() external view returns (uint256);
    function rollover() external view returns (uint256);
    function lastUpdated() external view returns (uint32);
    function maxLiquidityPerTick() external view returns (uint128);
    function ticks(int24 tick)
        external
        view
        returns (uint128, int128, int128, uint256, uint256, uint256, int56, uint160, uint32, bool);
    function positions(bytes32 key) external view returns (uint128, uint256, uint256, uint128, uint128);
    function increaseObservationCardinalityNext(uint16 observationCardinalityNext) external;
}

interface IClDriver {
    function setup(address pool, address token0, address token1) external;
    function setGauge(address gauge) external;
    function mint(address recipient, int24 tickLower, int24 tickUpper, uint128 amount)
        external
        returns (uint256, uint256);
    function burn(int24 tickLower, int24 tickUpper, uint128 amount) external returns (uint256, uint256);
    function collect(
        address recipient,
        int24 tickLower,
        int24 tickUpper,
        uint128 amount0Requested,
        uint128 amount1Requested,
        address owner
    ) external returns (uint128, uint128);
    function swap(address recipient, bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96)
        external
        returns (int256, int256);
}

interface IClGauge {
    function setPool(address pool) external;
    function stake(int128 stakedLiquidityDelta, int24 tickLower, int24 tickUpper, bool positionUpdate) external;
    function syncReward(uint256 rewardRate, uint256 rewardReserve, uint256 periodFinish) external;
    function collectFees() external returns (uint128, uint128);
    function updateRewardsGrowthGlobal() external;
}

interface IMockERC20 {
    function mint(address to, uint256 amount) external;
}

interface IMockFactory {
    function setFees(uint24 swapFee, uint24 unstakedFee) external;
}

interface IMockRegistry {
    function setGaugeFactory(address gaugeFactory) external;
}

struct TickView {
    uint128 liquidityGross;
    int128 liquidityNet;
    int128 stakedLiquidityNet;
    uint256 feeGrowthOutside0X128;
    uint256 feeGrowthOutside1X128;
    uint256 rewardGrowthOutsideX128;
    int56 tickCumulativeOutside;
    uint160 secondsPerLiquidityOutsideX128;
    uint32 secondsOutside;
    bool initialized;
}

struct PosView {
    uint128 liquidity;
    uint256 feeGrowthInside0LastX128;
    uint256 feeGrowthInside1LastX128;
    uint128 tokensOwed0;
    uint128 tokensOwed1;
}

struct Pos {
    address owner;
    int24 tickLower;
    int24 tickUpper;
}

/// @notice Differential fuzzer for the original `CLPool.sol`. Deploys a fresh pool per segment and
/// runs random mint / burn / collect / swap / stake / gauge ops, logging after each op the outcome
/// and the full pool state that the Rust `ClPool` must reproduce.
///
/// Line format (fields separated by `|`):
///   `op|time|inputs|status|outputs|S|T|P`
///  - `inputs` / `outputs`: comma-separated, fixed layout per op (see the `emit*` calls).
///  - `status`: `ok` or `err` (a revert in the Solidity pool, state rolled back).
///  - `S=`: slot0 sqrtPriceX96,tick,observationIndex,observationCardinality,observationCardinalityNext,
///    liquidity,stakedLiquidity,feeGrowthGlobal0X128,feeGrowthGlobal1X128,rewardGrowthGlobalX128,
///    gaugeFees0,gaugeFees1,rewardRate,rewardReserve,rollover,lastUpdated,periodFinish.
///  - `T=`: tick records `;`-separated: tick,liquidityGross,liquidityNet,stakedLiquidityNet,
///    feeGrowthOutside0X128,feeGrowthOutside1X128,rewardGrowthOutsideX128,tickCumulativeOutside,
///    secondsPerLiquidityOutsideX128,secondsOutside,initialized (touched ticks, or all anchors on swap).
///  - `P=`: position records `;`-separated: owner,tickLower,tickUpper,liquidity,
///    feeGrowthInside0LastX128,feeGrowthInside1LastX128,tokensOwed0,tokensOwed1 (touched positions).
/// Run: `forge test --match-test testGenerateClPool`
contract GenerateClPoolVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    string constant FILE = "vectors/cl_pool.txt";
    uint256 constant SEGMENTS = 5;
    uint256 constant OPS_PER_SEG = 1100;
    uint256 constant BASE_TIME = 1_700_000_000;
    uint256 constant DONATION = 1 << 160;
    uint160 constant MIN_SQRT = 4295128739;
    uint160 constant MAX_SQRT = 1461446703485210103287273052203988822378723970342;
    int256 constant MIN_TICK = -887272;
    int256 constant MAX_TICK = 887272;

    uint256 seed = 0x434c504f4f4c; // "CLPOOL"

    IClPool pool;
    IClDriver driver;
    IClGauge gauge;
    IMockFactory factory;
    IMockRegistry registry;
    address token0;
    address token1;
    int24 spacing;
    int24[] anchors;
    bool hasLastDm;
    int24 lastLo;
    int24 lastUp;
    uint128 lastAmt;
    // ranges where the driver has successfully minted in the current segment
    int24[] dLo;
    int24[] dUp;
    // gauge segments: driver-biased mints, and burn/collect against the driver's own positions
    bool gaugeSeg;
    // the range of the last successful driver burn, so the next collect can drain it
    bool pend;
    int24 pendLo;
    int24 pendUp;

    function rand() internal returns (uint256) {
        seed = uint256(keccak256(abi.encode(seed)));
        return seed;
    }

    /// Uniform random value of 1..maxBits bits.
    function randBits(uint256 maxBits) internal returns (uint256) {
        uint256 k = 1 + rand() % maxBits;
        return rand() >> (256 - k);
    }

    function eoa(uint256 i) internal pure returns (address) {
        return address(uint160(0xE0A0 + i));
    }

    function b2s(bool b) internal pure returns (string memory) {
        if (b) return "1";
        return "0";
    }

    function statusOf(bool ok) internal pure returns (string memory) {
        if (ok) return "ok";
        return "err";
    }

    function floorDiv(int256 a, int256 b) internal pure returns (int256) {
        int256 q = a / b;
        if (a % b != 0 && ((a < 0) != (b < 0))) q -= 1;
        return q;
    }

    // ------------------------------------------------------------------ state dumps

    function dumpS() internal view returns (string memory s) {
        (uint160 sq, int24 tk, uint16 oi, uint16 oc, uint16 ocn, ) = pool.slot0();
        (uint128 g0, uint128 g1) = pool.gaugeFees();
        s = string.concat(
            "S=",
            vm.toString(uint256(sq)),
            ",",
            vm.toString(int256(tk)),
            ",",
            vm.toString(uint256(oi)),
            ",",
            vm.toString(uint256(oc)),
            ",",
            vm.toString(uint256(ocn))
        );
        s = string.concat(
            s,
            ",",
            vm.toString(uint256(pool.liquidity())),
            ",",
            vm.toString(uint256(pool.stakedLiquidity())),
            ",",
            vm.toString(pool.feeGrowthGlobal0X128()),
            ",",
            vm.toString(pool.feeGrowthGlobal1X128())
        );
        s = string.concat(
            s,
            ",",
            vm.toString(pool.rewardGrowthGlobalX128()),
            ",",
            vm.toString(uint256(g0)),
            ",",
            vm.toString(uint256(g1)),
            ",",
            vm.toString(pool.rewardRate())
        );
        s = string.concat(
            s,
            ",",
            vm.toString(pool.rewardReserve()),
            ",",
            vm.toString(pool.rollover()),
            ",",
            vm.toString(uint256(pool.lastUpdated())),
            ",",
            vm.toString(pool.periodFinish())
        );
    }

    function tickRec(int24 t) internal view returns (string memory) {
        (bool ok, bytes memory d) = address(pool).staticcall(abi.encodeWithSelector(IClPool.ticks.selector, t));
        require(ok, "TICKS");
        TickView memory v = abi.decode(d, (TickView));
        string memory s = string.concat(
            vm.toString(int256(t)),
            ",",
            vm.toString(uint256(v.liquidityGross)),
            ",",
            vm.toString(int256(v.liquidityNet)),
            ",",
            vm.toString(int256(v.stakedLiquidityNet))
        );
        s = string.concat(
            s,
            ",",
            vm.toString(v.feeGrowthOutside0X128),
            ",",
            vm.toString(v.feeGrowthOutside1X128),
            ",",
            vm.toString(v.rewardGrowthOutsideX128)
        );
        s = string.concat(
            s,
            ",",
            vm.toString(int256(v.tickCumulativeOutside)),
            ",",
            vm.toString(uint256(v.secondsPerLiquidityOutsideX128)),
            ",",
            vm.toString(uint256(v.secondsOutside)),
            ",",
            b2s(v.initialized)
        );
        return s;
    }

    function posRec(Pos memory p) internal view returns (string memory) {
        bytes32 key = keccak256(abi.encodePacked(p.owner, p.tickLower, p.tickUpper));
        (bool ok, bytes memory d) = address(pool).staticcall(abi.encodeWithSelector(IClPool.positions.selector, key));
        require(ok, "POS");
        PosView memory v = abi.decode(d, (PosView));
        string memory s = string.concat(
            vm.toString(p.owner),
            ",",
            vm.toString(int256(p.tickLower)),
            ",",
            vm.toString(int256(p.tickUpper)),
            ",",
            vm.toString(uint256(v.liquidity))
        );
        s = string.concat(
            s,
            ",",
            vm.toString(v.feeGrowthInside0LastX128),
            ",",
            vm.toString(v.feeGrowthInside1LastX128),
            ",",
            vm.toString(uint256(v.tokensOwed0)),
            ",",
            vm.toString(uint256(v.tokensOwed1))
        );
        return s;
    }

    function dumpT(int24[] memory ts) internal view returns (string memory s) {
        for (uint256 i = 0; i < ts.length; i++) {
            if (i > 0) s = string.concat(s, ";");
            s = string.concat(s, tickRec(ts[i]));
        }
    }

    function dumpP(Pos[] memory ps) internal view returns (string memory s) {
        for (uint256 i = 0; i < ps.length; i++) {
            if (i > 0) s = string.concat(s, ";");
            s = string.concat(s, posRec(ps[i]));
        }
    }

    function emitOp(string memory op, string memory ins, bool ok, string memory outs, int24[] memory ts, Pos[] memory ps)
        internal
    {
        string memory head = string.concat(op, "|", vm.toString(block.timestamp), "|", ins);
        head = string.concat(head, "|", statusOf(ok), "|", outs, "|");
        string memory tail = string.concat(dumpS(), "|", dumpT(ts));
        vm.writeLine(FILE, string.concat(head, tail, "|", dumpP(ps)));
    }

    function outsU(bool ok, uint256 a, uint256 b) internal view returns (string memory) {
        if (!ok) return "";
        return string.concat(vm.toString(a), ",", vm.toString(b));
    }

    function outsI(bool ok, int256 a, int256 b) internal view returns (string memory) {
        if (!ok) return "";
        return string.concat(vm.toString(a), ",", vm.toString(b));
    }

    function noTicks() internal pure returns (int24[] memory) {
        return new int24[](0);
    }

    function noPos() internal pure returns (Pos[] memory) {
        return new Pos[](0);
    }

    // ------------------------------------------------------------------ random inputs

    function pickRecipient() internal returns (address) {
        uint256 p = rand() % 10;
        if (p < 3) return eoa(p);
        if (p < 7) return address(driver);
        return address(gauge);
    }

    function randAmount() internal returns (uint128) {
        uint256 p = rand() % 100;
        if (p < 20) return uint128(1 + rand() % 1000);
        if (p < 45) return uint128(randBits(40));
        if (p < 70) return uint128(randBits(90));
        if (p < 97) return uint128(randBits(120));
        return type(uint128).max;
    }

    function randReq() internal returns (uint128) {
        uint256 p = rand() % 4;
        if (p == 0) return 0;
        if (p == 1) return type(uint128).max;
        return uint128(randBits(128));
    }

    /// Aligned anchors around the starting tick, plus the extreme aligned ticks.
    function buildAnchors(int24 tk) internal {
        delete anchors;
        int256 s = int256(spacing);
        int256 base = floorDiv(int256(tk), s) * s;
        int256 minA = -floorDiv(-MIN_TICK, s) * s;
        int256 maxA = floorDiv(MAX_TICK, s) * s;
        for (uint256 k = 0; k < 16; k++) {
            int256 v;
            if (k == 14) {
                v = minA;
            } else if (k == 15) {
                v = maxA;
            } else {
                int256 span = k < 8 ? int256(10) : (k < 12 ? int256(200) : int256(3000));
                v = base + (int256(rand() % uint256(2 * span + 1)) - span) * s;
                if (v < minA) v = minA;
                if (v > maxA) v = maxA;
            }
            bool dup;
            for (uint256 q = 0; q < anchors.length; q++) {
                if (int256(anchors[q]) == v) dup = true;
            }
            if (!dup) anchors.push(int24(v));
        }
    }

    /// Mostly anchor pairs; sometimes unaligned or out-of-bounds ticks, which must revert in both.
    function pickRange() internal returns (int24 lo, int24 up) {
        uint256 p = rand() % 100;
        if (p < 4) {
            int256 a = int256(rand() % uint256(2 * 887272)) - 887272;
            int256 b = a + 1 + int256(rand() % 5000);
            if (p == 0) b = MAX_TICK + 1 + int256(rand() % 3);
            if (p == 1) a = MIN_TICK - 1 - int256(rand() % 3);
            return (int24(a), int24(b));
        }
        uint256 n = anchors.length;
        uint256 i = rand() % n;
        uint256 j = rand() % n;
        if (i == j) j = (i + 1) % n;
        if (i > j) (i, j) = (j, i);
        return (anchors[i], anchors[j]);
    }

    function pickAmount() internal returns (int256) {
        if (rand() % 100 < 3) return 0;
        uint256 mag = randBits(1 + rand() % 200);
        return rand() % 100 < 60 ? int256(mag) : -int256(mag);
    }

    function pickLimit(uint160 sq, bool zfo) internal returns (uint160) {
        uint256 p = rand() % 100;
        uint256 cur = uint256(sq);
        if (p < 30) return zfo ? MIN_SQRT + 1 : MAX_SQRT - 1;
        if (p < 70) {
            uint256 d = cur / (1 + rand() % 4096);
            uint256 lim;
            if (zfo) {
                lim = d >= cur ? 0 : cur - d;
                if (lim <= MIN_SQRT) lim = MIN_SQRT + 1;
            } else {
                lim = cur + d;
                if (lim >= MAX_SQRT) lim = MAX_SQRT - 1;
            }
            return uint160(lim);
        }
        if (p < 85) return uint160(uint256(MIN_SQRT) + rand() % (uint256(MAX_SQRT) - uint256(MIN_SQRT)));
        // wrong side of the current price: must revert with SPL
        return zfo ? MAX_SQRT - 1 : MIN_SQRT + 1;
    }

    function feeOf(uint256 k) internal pure returns (uint24) {
        if (k == 0) return 100;
        if (k == 1) return 500;
        if (k == 2) return 3000;
        return 10000;
    }

    function unstakedFeeOf(uint256 k) internal pure returns (uint24) {
        if (k == 0) return 0;
        if (k == 1) return 100000;
        return 500000;
    }

    function randRate() internal returns (uint256) {
        uint256 p = rand() % 5;
        if (p == 0) return 0;
        if (p == 1) return randBits(40);
        if (p == 2) return randBits(128);
        return 1 + rand() % 1e12;
    }

    // ------------------------------------------------------------------ ops

    function doMint() internal {
        address rec = gaugeSeg && rand() % 10 < 7 ? address(driver) : pickRecipient();
        (int24 lo, int24 up) = pickRange();
        uint128 amt = randAmount();
        bool ok;
        uint256 a0;
        uint256 a1;
        try driver.mint(rec, lo, up, amt) returns (uint256 x, uint256 y) {
            ok = true;
            a0 = x;
            a1 = y;
        } catch {}
        if (ok && rec == address(driver)) {
            hasLastDm = true;
            lastLo = lo;
            lastUp = up;
            lastAmt = amt;
            recordDriverRange(lo, up);
        }
        int24[] memory ts = new int24[](2);
        ts[0] = lo;
        ts[1] = up;
        Pos[] memory ps = new Pos[](1);
        ps[0] = Pos(rec, lo, up);
        string memory ins = string.concat(
            vm.toString(rec),
            ",",
            vm.toString(int256(lo)),
            ",",
            vm.toString(int256(up)),
            ",",
            vm.toString(uint256(amt))
        );
        emitOp("mint", ins, ok, outsU(ok, a0, a1), ts, ps);
    }

    function stepGauge(uint256 r) internal {
        if (r < 24) doMint();
        else if (r < 60) doDriverBurn();
        else if (r < 86) doDriverCollect();
        else if (r < 92) doSwap();
        else if (r < 96) doStake();
        else if (r < 97) doSync();
        else if (r < 98) doUpdate();
        else if (r < 99) doCollectFees();
        else doIncrease();
    }

    function recordDriverRange(int24 lo, int24 up) internal {
        if (dLo.length < 24) {
            dLo.push(lo);
            dUp.push(up);
        } else {
            uint256 i = rand() % 24;
            dLo[i] = lo;
            dUp[i] = up;
        }
    }

    function pickDriverRange() internal returns (int24, int24) {
        if (dLo.length == 0) return pickRange();
        uint256 i = rand() % dLo.length;
        return (dLo[i], dUp[i]);
    }

    /// Liquidity and tokens owed of a position, read from the pool before choosing an amount.
    function posOf(address owner, int24 lo, int24 up) internal view returns (uint128, uint128, uint128) {
        (uint128 liq, uint256 f0, uint256 f1, uint128 o0, uint128 o1) =
            pool.positions(keccak256(abi.encodePacked(owner, lo, up)));
        f0;
        f1;
        return (liq, o0, o1);
    }

    /// Burns from the driver's own positions: full liquidity, partial, zero (NP once emptied), or random.
    function liqOf(address owner, int24 lo, int24 up) internal view returns (uint128) {
        (uint128 liq, , , , ) = pool.positions(keccak256(abi.encodePacked(owner, lo, up)));
        return liq;
    }

    function doDriverBurn() internal {
        (int24 lo, int24 up) = pickDriverRange();
        uint128 liq = liqOf(address(driver), lo, up);
        uint256 mode = rand() % 100;
        uint128 amt;
        if (mode < 35) amt = liq;
        else if (mode < 60) amt = uint128((uint256(liq) * (1 + rand() % 15)) / 16);
        else if (mode < 75) amt = 0;
        else amt = uint128(randBits(1 + rand() % 60));
        driverBurnOp(lo, up, amt);
    }

    function driverBurnOp(int24 lo, int24 up, uint128 amt) internal {
        bool ok;
        uint256 a0;
        uint256 a1;
        try driver.burn(lo, up, amt) returns (uint256 x, uint256 y) {
            ok = true;
            a0 = x;
            a1 = y;
        } catch {}
        int24[] memory ts = new int24[](2);
        ts[0] = lo;
        ts[1] = up;
        Pos[] memory ps = new Pos[](1);
        ps[0] = Pos(address(driver), lo, up);
        string memory ins = string.concat(
            vm.toString(address(driver)),
            ",",
            vm.toString(int256(lo)),
            ",",
            vm.toString(int256(up)),
            ",",
            vm.toString(uint256(amt))
        );
        emitOp("burn", ins, ok, outsU(ok, a0, a1), ts, ps);
        if (ok) {
            pend = true;
            pendLo = lo;
            pendUp = up;
        }
    }

    /// Collects the driver's own positions: exact owed amounts (mostly), half of them, or random requests.
    function doDriverCollect() internal {
        int24 lo;
        int24 up;
        if (pend && rand() % 3 != 0) {
            lo = pendLo;
            up = pendUp;
            pend = false;
        } else {
            (lo, up) = pickOwedRange();
        }
        (uint128 r0, uint128 r1) = chooseCollect(lo, up);
        driverCollectOp(pickRecipient(), lo, up, r0, r1);
    }

    /// A driver range that currently owes tokens (a few tries), so collects usually pay out.
    function pickOwedRange() internal returns (int24, int24) {
        (int24 lo, int24 up) = pickDriverRange();
        for (uint256 k = 0; k < 4; k++) {
            (, uint128 o0, uint128 o1) = posOf(address(driver), lo, up);
            if (o0 > 0 || o1 > 0) break;
            (lo, up) = pickDriverRange();
        }
        return (lo, up);
    }

    /// Requests: the exact owed amounts (mostly), half of them, or random values.
    function chooseCollect(int24 lo, int24 up) internal returns (uint128, uint128) {
        (, uint128 o0, uint128 o1) = posOf(address(driver), lo, up);
        uint256 mode = rand() % 100;
        if (mode < 75) return (o0, o1);
        if (mode < 90) return (o0 / 2, o1 / 2);
        return (randReq(), randReq());
    }

    function driverCollectOp(address rec, int24 lo, int24 up, uint128 r0, uint128 r1) internal {
        bool ok;
        uint256 c0;
        uint256 c1;
        try driver.collect(rec, lo, up, r0, r1, address(driver)) returns (uint128 x, uint128 y) {
            ok = true;
            c0 = x;
            c1 = y;
        } catch {}
        Pos[] memory ps = new Pos[](1);
        ps[0] = Pos(address(driver), lo, up);
        string memory ins = string.concat(
            vm.toString(rec),
            ",",
            vm.toString(int256(lo)),
            ",",
            vm.toString(int256(up)),
            ",",
            vm.toString(uint256(r0))
        );
        ins = string.concat(ins, ",", vm.toString(uint256(r1)), ",", vm.toString(address(driver)));
        emitOp("collect", ins, ok, outsU(ok, c0, c1), noTicks(), ps);
    }

    function doBurn() internal {
        int24 lo;
        int24 up;
        uint128 amt;
        if (hasLastDm && rand() % 3 == 0) {
            lo = lastLo;
            up = lastUp;
            amt = lastAmt;
        } else {
            (lo, up) = pickRange();
            amt = uint128(randBits(1 + rand() % 120));
        }
        bool ok;
        uint256 a0;
        uint256 a1;
        try driver.burn(lo, up, amt) returns (uint256 x, uint256 y) {
            ok = true;
            a0 = x;
            a1 = y;
        } catch {}
        int24[] memory ts = new int24[](2);
        ts[0] = lo;
        ts[1] = up;
        Pos[] memory ps = new Pos[](1);
        ps[0] = Pos(address(driver), lo, up);
        string memory ins = string.concat(
            vm.toString(address(driver)),
            ",",
            vm.toString(int256(lo)),
            ",",
            vm.toString(int256(up)),
            ",",
            vm.toString(uint256(amt))
        );
        emitOp("burn", ins, ok, outsU(ok, a0, a1), ts, ps);
    }

    function doCollect() internal {
        address rec = pickRecipient();
        address owner = rand() % 100 < 85 ? address(driver) : eoa(rand() % 3);
        (int24 lo, int24 up) = pickRange();
        uint128 r0 = randReq();
        uint128 r1 = randReq();
        bool ok;
        uint256 c0;
        uint256 c1;
        try driver.collect(rec, lo, up, r0, r1, owner) returns (uint128 x, uint128 y) {
            ok = true;
            c0 = x;
            c1 = y;
        } catch {}
        Pos[] memory ps = new Pos[](1);
        ps[0] = Pos(owner, lo, up);
        string memory ins = string.concat(
            vm.toString(rec),
            ",",
            vm.toString(int256(lo)),
            ",",
            vm.toString(int256(up)),
            ",",
            vm.toString(uint256(r0))
        );
        ins = string.concat(
            ins,
            ",",
            vm.toString(uint256(r1)),
            ",",
            vm.toString(owner)
        );
        emitOp("collect", ins, ok, outsU(ok, c0, c1), noTicks(), ps);
    }

    function doSwap() internal {
        (uint160 sq, int24 tk, , , , ) = pool.slot0();
        bool zfo = rand() % 2 == 0;
        int256 amt = pickAmount();
        uint160 lim = pickLimit(sq, zfo);
        uint24 fee = feeOf(rand() % 4);
        uint24 uf = unstakedFeeOf(rand() % 3);
        factory.setFees(fee, uf);
        address rec = pickRecipient();
        string memory ins = string.concat(
            vm.toString(rec),
            ",",
            b2s(zfo),
            ",",
            vm.toString(amt),
            ",",
            vm.toString(uint256(lim))
        );
        ins = string.concat(ins, ",", vm.toString(uint256(fee)), ",", vm.toString(uint256(uf)));
        swapOp(rec, zfo, amt, lim, tk, ins);
    }

    function swapOp(address rec, bool zfo, int256 amt, uint160 lim, int24 startTk, string memory ins) internal {
        bool ok;
        int256 a0;
        int256 a1;
        try driver.swap(rec, zfo, amt, lim) returns (int256 x, int256 y) {
            ok = true;
            a0 = x;
            a1 = y;
        } catch {}
        (, int24 endTk, , , , ) = pool.slot0();
        emitOp("swap", ins, ok, outsI(ok, a0, a1), swapTicks(startTk, endTk), noPos());
    }

    /// Anchors the swap crossed (start, end] plus the nearest anchor on each side of the move.
    /// Crossed or adjacent ticks are where a swap can change tick state, so only these are dumped.
    function swapTicks(int24 startTk, int24 endTk) internal view returns (int24[] memory) {
        int24 lo = startTk < endTk ? startTk : endTk;
        int24 hi = startTk < endTk ? endTk : startTk;
        bool hasBelow;
        bool hasAbove;
        int24 below;
        int24 above;
        for (uint256 i = 0; i < anchors.length; i++) {
            int24 a = anchors[i];
            if (a <= lo && (!hasBelow || a > below)) {
                below = a;
                hasBelow = true;
            }
            if (a > hi && (!hasAbove || a < above)) {
                above = a;
                hasAbove = true;
            }
        }
        uint256 n;
        for (uint256 i = 0; i < anchors.length; i++) {
            if (keepAnchor(anchors[i], lo, hi, hasBelow, below, hasAbove, above)) n++;
        }
        int24[] memory out = new int24[](n);
        uint256 k;
        for (uint256 i = 0; i < anchors.length; i++) {
            if (keepAnchor(anchors[i], lo, hi, hasBelow, below, hasAbove, above)) out[k++] = anchors[i];
        }
        return out;
    }

    function keepAnchor(int24 a, int24 lo, int24 hi, bool hasBelow, int24 below, bool hasAbove, int24 above)
        internal
        pure
        returns (bool)
    {
        if (a > lo && a <= hi) return true;
        if (hasBelow && a == below) return true;
        return hasAbove && a == above;
    }

    function doStake() internal {
        int128 delta;
        int24 lo;
        int24 up;
        bool pu = rand() % 3 != 0;
        uint256 p = rand() % 4;
        if (hasLastDm && p < 2) {
            delta = int128(lastAmt);
            lo = lastLo;
            up = lastUp;
        } else if (p == 2) {
            delta = -int128(uint128(randBits(1 + rand() % 100)));
            (lo, up) = pickRange();
        } else {
            uint256 mag = randBits(1 + rand() % 90);
            delta = rand() % 2 == 0 ? int128(int256(mag)) : -int128(int256(mag));
            (lo, up) = pickRange();
        }
        bool ok;
        try gauge.stake(delta, lo, up, pu) {
            ok = true;
        } catch {}
        int24[] memory ts = new int24[](2);
        ts[0] = lo;
        ts[1] = up;
        Pos[] memory ps = new Pos[](2);
        ps[0] = Pos(address(driver), lo, up);
        ps[1] = Pos(address(gauge), lo, up);
        string memory ins = string.concat(
            vm.toString(int256(delta)),
            ",",
            vm.toString(int256(lo)),
            ",",
            vm.toString(int256(up)),
            ",",
            b2s(pu)
        );
        emitOp("stake", ins, ok, "", ts, ps);
    }

    function doSync() internal {
        uint256 rate = randRate();
        uint256 reserve = rand() % 4 == 0 ? 0 : randBits(100);
        uint256 finish = rand() % 2 == 0 ? block.timestamp + rand() % (7 days) : randBits(64);
        bool ok;
        try gauge.syncReward(rate, reserve, finish) {
            ok = true;
        } catch {}
        string memory ins = string.concat(
            vm.toString(rate),
            ",",
            vm.toString(reserve),
            ",",
            vm.toString(finish)
        );
        emitOp("syncReward", ins, ok, "", noTicks(), noPos());
    }

    function doUpdate() internal {
        bool ok;
        try gauge.updateRewardsGrowthGlobal() {
            ok = true;
        } catch {}
        emitOp("updateRewardsGrowthGlobal", "", ok, "", noTicks(), noPos());
    }

    function doCollectFees() internal {
        bool ok;
        uint256 a;
        uint256 b;
        try gauge.collectFees() returns (uint128 x, uint128 y) {
            ok = true;
            a = x;
            b = y;
        } catch {}
        emitOp("collectFees", "", ok, outsU(ok, a, b), noTicks(), noPos());
    }

    function doIncrease() internal {
        uint16 next = uint16(1 + rand() % 40);
        bool ok;
        try pool.increaseObservationCardinalityNext(next) {
            ok = true;
        } catch {}
        emitOp("increaseObservationCardinalityNext", vm.toString(uint256(next)), ok, "", noTicks(), noPos());
    }

    function step() internal {
        vm.warp(block.timestamp + (rand() % 10 == 0 ? 0 : rand() % 3601));
        uint256 r = rand() % 100;
        if (gaugeSeg) {
            stepGauge(r);
            return;
        }
        if (r < 25) doMint();
        else if (r < 35) doBurn();
        else if (r < 45) doCollect();
        else if (r < 75) doSwap();
        else if (r < 85) doStake();
        else if (r < 90) doSync();
        else if (r < 94) doUpdate();
        else if (r < 97) doCollectFees();
        else doIncrease();
    }

    function initialSqrtPrice() internal returns (uint160) {
        uint256 p = rand() % 10;
        if (p == 0) return MIN_SQRT + 1;
        if (p == 1) return MAX_SQRT - 1;
        return uint160(2 ** 80 + rand() % (2 ** 112 - 2 ** 80));
    }

    /// Deploys a fresh CLPool with its mocks, initializes it, and logs the `init` (and, for even
    /// segments, the early `setGauge`) lines.
    function newSegment(uint256 seg) internal {
        int24[5] memory spacings = [int24(1), int24(10), int24(60), int24(200), int24(10)];
        spacing = spacings[seg];
        delete anchors;
        delete dLo;
        delete dUp;
        hasLastDm = false;
        // gauge (setGauge) in segments 0, 2, 4; no gauge in 1, 3
        gaugeSeg = seg % 2 == 0;
        pend = false;
        vm.warp(BASE_TIME + seg * 10_000_000);

        address ta = vm.deployCode("MockERC20.sol:MockERC20");
        address tb = vm.deployCode("MockERC20.sol:MockERC20");
        if (ta < tb) {
            token0 = ta;
            token1 = tb;
        } else {
            token0 = tb;
            token1 = ta;
        }
        factory = IMockFactory(vm.deployCode("MockFactory.sol:MockFactory"));
        registry = IMockRegistry(vm.deployCode("MockFactoryRegistry.sol:MockFactoryRegistry"));
        pool = IClPool(vm.deployCode("CLPool.sol:CLPool"));
        driver = IClDriver(vm.deployCode("ClPoolDriver.sol:ClPoolDriver"));
        gauge = IClGauge(vm.deployCode("ClPoolGauge.sol:ClPoolGauge"));
        gauge.setPool(address(pool));
        driver.setup(address(pool), token0, token1);
        registry.setGaugeFactory(address(driver));

        uint160 sqrtPrice = initialSqrtPrice();
        pool.initialize(address(factory), token0, token1, spacing, address(registry), sqrtPrice);
        IMockERC20(token0).mint(address(pool), DONATION);
        IMockERC20(token1).mint(address(pool), DONATION);

        (, int24 tk, , , , ) = pool.slot0();
        buildAnchors(tk);

        string memory ins = string.concat(vm.toString(int256(spacing)), ",", vm.toString(uint256(sqrtPrice)));
        emitOp("init", ins, true, vm.toString(uint256(pool.maxLiquidityPerTick())), noTicks(), noPos());

        if (gaugeSeg) {
            bool ok;
            try driver.setGauge(address(gauge)) {
                ok = true;
            } catch {}
            string memory gins = string.concat(vm.toString(address(gauge)), ",", vm.toString(address(driver)));
            emitOp("setGauge", gins, ok, "", noTicks(), noPos());
        }
    }

    /// One segment per external call: the memory used by the string logging is released between
    /// segments, so the EVM memory limit is not reached.
    function runSegment(uint256 seg) external {
        require(msg.sender == address(this), "SELF");
        newSegment(seg);
        for (uint256 i = 0; i < OPS_PER_SEG; i++) {
            step();
        }
    }

    function testGenerateClPool() public {
        vm.writeFile(FILE, "");
        for (uint256 seg = 0; seg < SEGMENTS; seg++) {
            this.runSegment(seg);
        }
    }
}
