// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

interface GVm {
    function deployCode(string calldata artifactPath) external returns (address);
    function writeLine(string calldata path, string calldata data) external;
    function writeFile(string calldata path, string calldata data) external;
    function warp(uint256 newTimestamp) external;
    function prank(address sender) external;
    function toString(uint256 value) external pure returns (string memory);
    function toString(int256 value) external pure returns (string memory);
    function toString(address value) external pure returns (string memory);
}

interface IGPool {
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
    function positions(bytes32 key) external view returns (uint128, uint256, uint256, uint128, uint128);
    function increaseObservationCardinalityNext(uint16 observationCardinalityNext) external;
}

interface IGDriver {
    function setup(address pool, address token0, address token1) external;
    function swap(address recipient, bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96)
        external
        returns (int256, int256);
}

interface IGFeeFactory {
    function setFees(uint24 swapFee, uint24 unstakedFee) external;
}

interface IGRegistry {
    function setGaugeFactory(address gaugeFactory) external;
}

interface IGToken {
    function mint(address to, uint256 amount) external;
    function balanceOf(address account) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
}

interface IGNft {
    function setPool(address pool, address token0, address token1) external;
    function setApprovalForAll(address operator, bool approved) external;
    function mintPosition(address owner, int24 tickSpacing, int24 tickLower, int24 tickUpper, uint128 liquidity)
        external
        returns (uint256 tokenId, uint256 amount0, uint256 amount1);
    function ownerOrZero(uint256 tokenId) external view returns (address);
    function rangeOf(uint256 tokenId) external view returns (int24 tickLower, int24 tickUpper, bool exists);
}

interface IGVoter {
    function setRewardToken(address rewardToken) external;
    function setAlive(bool alive) external;
    function notify(address gauge, uint256 amount) external;
    function getReward(address gauge, address account) external;
}

interface IGGaugeFactory {
    function setRewardToken(address rewardToken) external;
    function createGauge(
        address pool,
        address feesVotingReward,
        address voter,
        address nft,
        address token0,
        address token1,
        int24 tickSpacing,
        bool isPool
    ) external returns (address);
    function notifyWithoutClaim(address gauge, uint256 amount) external;
}

interface IGGauge {
    function deposit(uint256 tokenId) external;
    function withdraw(uint256 tokenId) external;
    function getReward(uint256 tokenId) external;
    function earned(address account, uint256 tokenId) external view returns (uint256);
    function rewardRate() external view returns (uint256);
    function periodFinish() external view returns (uint256);
    function fees0() external view returns (uint256);
    function fees1() external view returns (uint256);
    function left() external view returns (uint256);
    function rewardRateByEpoch(uint256 epochStart) external view returns (uint256);
    function rewards(uint256 tokenId) external view returns (uint256);
    function rewardGrowthInside(uint256 tokenId) external view returns (uint256);
    function lastUpdateTime(uint256 tokenId) external view returns (uint256);
    function depositTimestamp(uint256 tokenId) external view returns (uint256);
    function stakedValues(address depositor) external view returns (uint256[] memory);
}

struct GTickView {
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

struct GPos {
    address owner;
    int24 tickLower;
    int24 tickUpper;
}

/// @dev Ops touched by one line: pool ticks and positions to dump, gauge token ids to dump, and the
/// account whose staked list is dumped (-1 for none). The staked tokens of `acc` are always dumped too.
struct GTouch {
    int24[] ts;
    GPos[] ps;
    uint256[] ks;
    int256 acc;
}

/// @notice Differential fuzzer for the original `CLGauge.sol` on top of the real `CLPool.sol`.
///
/// Each segment deploys: two mock tokens, a reward token, the CLPool with its factory, registry and
/// swap driver, a mock NonfungiblePositionManager (`MockNftManager`, the pool's `nft`), a mock gauge
/// factory that deploys the real CLGauge and registers it with the pool, a voter mock, and a
/// fees-voting-reward mock. Then it runs random ops: mint positions, approve, deposit, withdraw,
/// getReward (by token and by account through the voter), earned reads, notifyRewardAmount (voter,
/// mid-period, after the period, zero), notifyRewardWithoutClaim (notify admin), reward donations,
/// voter isAlive toggles and swaps that move the pool price through the staked ranges.
///
/// Line format (fields separated by `|`):
///   `op|time|inputs|status|outputs|S|G|T|P|K|E`
///  - `status`: `ok` or `err` (revert; all state rolled back).
///  - `outputs`: op-specific, comma-separated (see each `do*` function). Amounts are measured as balance
///    deltas of the actor (user / voter / fees reward), so they are what the Rust replay must return.
///  - `S=`: the pool state, same fields as `GenerateClPool.t.sol` (17 fields).
///  - `G=`: rewardRate,periodFinish,fees0,fees1,rewardTokenBalanceOfGauge,left(),rewardRateByEpoch(epochStart(now)).
///  - `T=`: pool tick records `;`-separated (same fields as `GenerateClPool.t.sol`).
///  - `P=`: pool position records `;`-separated: owner,tickLower,tickUpper,liquidity,f0,f1,owed0,owed1.
///  - `K=`: gauge token records `;`-separated: tokenId,rewards,rewardGrowthInside,lastUpdateTime,depositTimestamp,inGauge.
///  - `E=`: `account:tokenIds` (the account's staked list in EnumerableSet order), or empty.
/// Run: `forge test --match-test testGenerateClGauge`
contract GenerateClGaugeVectors {
    GVm constant vm = GVm(address(uint160(uint256(keccak256("hevm cheat code")))));

    string constant FILE = "vectors/cl_gauge.txt";
    uint256 constant SEGMENTS = 4;
    uint256 constant OPS_PER_SEG = 500;
    uint256 constant NUSERS = 4;
    uint256 constant WEEK = 7 days;
    uint256 constant BASE_TIME = 1_700_000_000;
    uint256 constant DONATION = 1 << 160;
    uint160 constant MIN_SQRT = 4295128739;
    uint160 constant MAX_SQRT = 1461446703485210103287273052203988822378723970342;
    int256 constant MIN_TICK = -887272;
    int256 constant MAX_TICK = 887272;

    uint256 seed = 0x47415547; // "GAUG"

    IGPool pool;
    IGDriver driver;
    IGFeeFactory feeFactory;
    IGRegistry registry;
    IGNft nft;
    IGGaugeFactory gaugeFactory;
    IGVoter voter;
    IGGauge gauge;
    IGToken reward;
    IGToken token0;
    IGToken token1;
    address feesVr;
    int24 spacing;
    bool isPoolSeg;
    int24[] anchors;
    uint256[] tokenIds;
    mapping(uint256 => address) depositor;
    uint256 segBase;

    /// Token ids restart in every segment (new NFT manager), so the depositor map is keyed by segment too.
    function depKey(uint256 tid) internal view returns (uint256) {
        return segBase + tid;
    }

    // ------------------------------------------------------------------ random

    function rand() internal returns (uint256) {
        seed = uint256(keccak256(abi.encode(seed)));
        return seed;
    }

    function randBits(uint256 maxBits) internal returns (uint256) {
        uint256 k = 1 + rand() % maxBits;
        return rand() >> (256 - k);
    }

    function eoa(uint256 i) internal pure returns (address) {
        return address(uint160(0xE0A0 + i));
    }

    /// Index of a user address, or -1 if it is not one of the users.
    function userIndex(address a) internal pure returns (int256) {
        for (uint256 i = 0; i < NUSERS; i++) {
            if (eoa(i) == a) return int256(i);
        }
        return -1;
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

    function randAmount() internal returns (uint128) {
        uint256 p = rand() % 100;
        if (p < 20) return uint128(1 + rand() % 1000);
        if (p < 45) return uint128(randBits(40));
        if (p < 70) return uint128(randBits(90));
        if (p < 97) return uint128(randBits(120));
        return type(uint128).max;
    }

    function pickNotifyAmount() internal returns (uint256) {
        uint256 p = rand() % 100;
        if (p < 5) return 0;
        if (p < 35) return 1 + rand() % 1_000_000;
        if (p < 70) return randBits(40);
        if (p < 90) return randBits(70);
        return randBits(100);
    }

    // ------------------------------------------------------------------ dumps

    function s(uint256 x) internal view returns (string memory) {
        return vm.toString(x);
    }

    function dumpS() internal view returns (string memory out) {
        (uint160 sq, int24 tk, uint16 oi, uint16 oc, uint16 ocn, ) = pool.slot0();
        (uint128 g0, uint128 g1) = pool.gaugeFees();
        out = string.concat(
            "S=",
            s(uint256(sq)),
            ",",
            vm.toString(int256(tk)),
            ",",
            s(uint256(oi)),
            ",",
            s(uint256(oc)),
            ",",
            s(uint256(ocn))
        );
        out = string.concat(
            out,
            ",",
            s(uint256(pool.liquidity())),
            ",",
            s(uint256(pool.stakedLiquidity())),
            ",",
            s(pool.feeGrowthGlobal0X128()),
            ",",
            s(pool.feeGrowthGlobal1X128())
        );
        out = string.concat(
            out,
            ",",
            s(pool.rewardGrowthGlobalX128()),
            ",",
            s(uint256(g0)),
            ",",
            s(uint256(g1)),
            ",",
            s(pool.rewardRate())
        );
        out = string.concat(
            out,
            ",",
            s(pool.rewardReserve()),
            ",",
            s(pool.rollover()),
            ",",
            s(uint256(pool.lastUpdated())),
            ",",
            s(pool.periodFinish())
        );
    }

    function dumpG(uint256 ts) internal view returns (string memory out) {
        if (address(gauge) == address(0)) return "G=0,0,0,0,0,0,0,0,0";
        uint256 epoch = ts - (ts % WEEK);
        out = string.concat(
            "G=",
            s(gauge.rewardRate()),
            ",",
            s(gauge.periodFinish()),
            ",",
            s(gauge.fees0()),
            ",",
            s(gauge.fees1())
        );
        out = string.concat(
            out,
            ",",
            s(reward.balanceOf(address(gauge))),
            ",",
            s(gauge.left()),
            ",",
            s(gauge.rewardRateByEpoch(epoch))
        );
        out = string.concat(
            out,
            ",",
            s(token0.balanceOf(address(gauge))),
            ",",
            s(token1.balanceOf(address(gauge)))
        );
    }

    function tickRec(int24 t) internal view returns (string memory) {
        (bool ok, bytes memory d) = address(pool).staticcall(abi.encodeWithSignature("ticks(int24)", t));
        require(ok, "TICKS");
        GTickView memory v = abi.decode(d, (GTickView));
        string memory out = string.concat(
            vm.toString(int256(t)),
            ",",
            s(uint256(v.liquidityGross)),
            ",",
            vm.toString(int256(v.liquidityNet)),
            ",",
            vm.toString(int256(v.stakedLiquidityNet))
        );
        out = string.concat(
            out,
            ",",
            s(v.feeGrowthOutside0X128),
            ",",
            s(v.feeGrowthOutside1X128),
            ",",
            s(v.rewardGrowthOutsideX128)
        );
        out = string.concat(
            out,
            ",",
            vm.toString(int256(v.tickCumulativeOutside)),
            ",",
            s(uint256(v.secondsPerLiquidityOutsideX128)),
            ",",
            s(uint256(v.secondsOutside)),
            ",",
            b2s(v.initialized)
        );
        return out;
    }

    function posRec(GPos memory p) internal view returns (string memory) {
        bytes32 key = keccak256(abi.encodePacked(p.owner, p.tickLower, p.tickUpper));
        (uint128 liq, uint256 f0, uint256 f1, uint128 o0, uint128 o1) = pool.positions(key);
        return string.concat(
            vm.toString(p.owner),
            ",",
            vm.toString(int256(p.tickLower)),
            ",",
            vm.toString(int256(p.tickUpper)),
            ",",
            s(uint256(liq)),
            ",",
            s(f0),
            ",",
            s(f1),
            ",",
            s(uint256(o0)),
            ",",
            s(uint256(o1))
        );
    }

    function join(string memory a, string memory sep, string memory b) internal pure returns (string memory) {
        if (bytes(a).length == 0) return b;
        if (bytes(b).length == 0) return a;
        return string.concat(a, sep, b);
    }

    function dumpT(int24[] memory ts) internal view returns (string memory out) {
        for (uint256 i = 0; i < ts.length; i++) {
            out = join(out, ";", tickRec(ts[i]));
        }
    }

    function dumpP(GPos[] memory ps) internal view returns (string memory out) {
        for (uint256 i = 0; i < ps.length; i++) {
            out = join(out, ";", posRec(ps[i]));
        }
    }

    function tokRec(uint256 tid) internal view returns (string memory) {
        return string.concat(
            s(tid),
            ",",
            s(gauge.rewards(tid)),
            ",",
            s(gauge.rewardGrowthInside(tid)),
            ",",
            s(gauge.lastUpdateTime(tid)),
            ",",
            s(gauge.depositTimestamp(tid)),
            ",",
            b2s(nft.ownerOrZero(tid) == address(gauge))
        );
    }

    function dumpK(uint256[] memory ks) internal view returns (string memory out) {
        for (uint256 i = 0; i < ks.length; i++) {
            out = join(out, ";", tokRec(ks[i]));
        }
    }

    function emitOp(string memory op, string memory ins, bool ok, string memory outs, GTouch memory t) internal {
        uint256[] memory staked = new uint256[](0);
        string memory eStr = "";
        if (t.acc >= 0) {
            staked = gauge.stakedValues(eoa(uint256(t.acc)));
            eStr = string.concat("E=", s(uint256(t.acc)), ":");
            for (uint256 i = 0; i < staked.length; i++) {
                if (i > 0) eStr = string.concat(eStr, ",");
                eStr = string.concat(eStr, s(staked[i]));
            }
        } else {
            eStr = "E=";
        }
        string memory kStr = join(dumpK(t.ks), ";", dumpK(staked));
        string memory head = string.concat(op, "|", s(block.timestamp), "|", ins, "|", statusOf(ok), "|", outs, "|");
        string memory tail = string.concat(dumpS(), "|", dumpG(block.timestamp), "|", dumpT(t.ts));
        tail = string.concat(tail, "|", dumpP(t.ps), "|", kStr, "|", eStr);
        vm.writeLine(FILE, string.concat(head, tail));
    }

    function none() internal pure returns (GTouch memory t) {
        t.ts = new int24[](0);
        t.ps = new GPos[](0);
        t.ks = new uint256[](0);
        t.acc = -1;
    }

    function outsU1(bool ok, uint256 a) internal view returns (string memory) {
        if (!ok) return "";
        return s(a);
    }

    function outsU2(bool ok, uint256 a, uint256 b) internal view returns (string memory) {
        if (!ok) return "";
        return string.concat(s(a), ",", s(b));
    }

    function outsU3(bool ok, uint256 a, uint256 b, uint256 c) internal view returns (string memory) {
        if (!ok) return "";
        return string.concat(s(a), ",", s(b), ",", s(c));
    }

    // ------------------------------------------------------------------ ranges and prices (as in GenerateClPool)

    function buildAnchors(int24 tk) internal {
        delete anchors;
        int256 sp = int256(spacing);
        int256 base = floorDiv(int256(tk), sp) * sp;
        int256 minA = -floorDiv(-MIN_TICK, sp) * sp;
        int256 maxA = floorDiv(MAX_TICK, sp) * sp;
        for (uint256 k = 0; k < 16; k++) {
            int256 v;
            if (k == 14) {
                v = minA;
            } else if (k == 15) {
                v = maxA;
            } else {
                int256 span = k < 8 ? int256(10) : (k < 12 ? int256(200) : int256(3000));
                v = base + (int256(rand() % uint256(2 * span + 1)) - span) * sp;
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
        return zfo ? MAX_SQRT - 1 : MIN_SQRT + 1;
    }

    function pickAmount() internal returns (int256) {
        if (rand() % 100 < 3) return 0;
        uint256 mag = randBits(1 + rand() % 200);
        return rand() % 100 < 60 ? int256(mag) : -int256(mag);
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

    /// Anchors the swap crossed (start, end] plus the nearest anchor on each side of the move.
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

    // ------------------------------------------------------------------ token picking

    function pickToken() internal returns (uint256) {
        uint256 n = tokenIds.length;
        if (n == 0 || rand() % 10 == 0) return randBits(8);
        return tokenIds[rand() % n];
    }

    /// A token that is currently staked in the gauge (by the harness's own bookkeeping), else any token.
    function pickStaked() internal returns (uint256) {
        uint256 n = tokenIds.length;
        uint256 start = n == 0 ? 0 : rand() % n;
        for (uint256 k = 0; k < n; k++) {
            uint256 tid = tokenIds[(start + k) % n];
            if (depositor[depKey(tid)] != address(0)) return tid;
        }
        return pickToken();
    }

    /// A minted token that is not staked, else any token.
    function pickUnstaked() internal returns (uint256) {
        uint256 n = tokenIds.length;
        uint256 start = n == 0 ? 0 : rand() % n;
        for (uint256 k = 0; k < n; k++) {
            uint256 tid = tokenIds[(start + k) % n];
            if (depositor[depKey(tid)] == address(0)) return tid;
        }
        return pickToken();
    }

    // ------------------------------------------------------------------ ops

    /// mint: inputs owner,tickLower,tickUpper,liquidity; outputs tokenId,amount0,amount1.
    function doMint() internal {
        address owner = eoa(rand() % NUSERS);
        (int24 lo, int24 up) = pickRange();
        uint128 liq = randAmount();
        bool ok;
        uint256 tid;
        uint256 a0;
        uint256 a1;
        try nft.mintPosition(owner, spacing, lo, up, liq) returns (uint256 t, uint256 x, uint256 y) {
            ok = true;
            tid = t;
            a0 = x;
            a1 = y;
        } catch {}
        if (ok) tokenIds.push(tid);
        GTouch memory t = none();
        t.ts = new int24[](2);
        t.ts[0] = lo;
        t.ts[1] = up;
        t.ps = new GPos[](2);
        t.ps[0] = GPos(address(nft), lo, up);
        t.ps[1] = GPos(address(gauge), lo, up);
        if (ok) {
            t.ks = new uint256[](1);
            t.ks[0] = tid;
        }
        string memory ins = string.concat(
            vm.toString(owner),
            ",",
            vm.toString(int256(lo)),
            ",",
            vm.toString(int256(up)),
            ",",
            s(uint256(liq))
        );
        emitOp("mint", ins, ok, outsU3(ok, tid, a0, a1), t);
    }

    /// approve: inputs user,flag; the user calls setApprovalForAll(gauge, flag) on the NFT manager.
    function doApprove() internal {
        approveOp(rand() % NUSERS, rand() % 20 == 0 ? 0 : 1);
    }

    function approveOp(uint256 ui, uint256 flag) internal {
        vm.prank(eoa(ui));
        bool ok;
        try nft.setApprovalForAll(address(gauge), flag == 1) {
            ok = true;
        } catch {}
        GTouch memory t = none();
        t.acc = int256(ui);
        emitOp("approve", string.concat(vm.toString(eoa(ui)), ",", s(flag)), ok, "", t);
    }

    function accTouch(int256 acc) internal pure returns (GTouch memory t) {
        t = none();
        t.acc = acc;
    }

    /// Touch set for one gauge token: the token, its pool range (when it exists) and the account's staked list.
    function touchTok(uint256 tid, int256 acc) internal view returns (GTouch memory t) {
        t = accTouch(acc);
        t.ks = new uint256[](1);
        t.ks[0] = tid;
        (int24 lo, int24 up, bool exists) = nft.rangeOf(tid);
        if (exists) {
            t.ts = new int24[](2);
            t.ts[0] = lo;
            t.ts[1] = up;
            t.ps = new GPos[](2);
            t.ps[0] = GPos(address(nft), lo, up);
            t.ps[1] = GPos(address(gauge), lo, up);
        }
    }

    function tryDeposit(uint256 tid) internal returns (bool ok) {
        try gauge.deposit(tid) {
            ok = true;
        } catch {}
    }

    function tryWithdraw(uint256 tid) internal returns (bool ok) {
        try gauge.withdraw(tid) {
            ok = true;
        } catch {}
    }

    function tryGetReward(uint256 tid) internal returns (bool ok) {
        try gauge.getReward(tid) {
            ok = true;
        } catch {}
    }

    function tryGetRewardAll(address u) internal returns (bool ok) {
        try voter.getReward(address(gauge), u) {
            ok = true;
        } catch {}
    }

    /// deposit: inputs user,tokenId; outputs collected token0,token1 paid to the user by the NFT manager.
    function doDeposit() internal {
        uint256 tid = rand() % 10 < 8 ? pickUnstaked() : pickToken();
        address owner = nft.ownerOrZero(tid);
        uint256 ui = rand() % 20 < 19 && userIndex(owner) >= 0 ? uint256(userIndex(owner)) : rand() % NUSERS;
        address u = eoa(ui);
        uint256 b0 = token0.balanceOf(u);
        uint256 b1 = token1.balanceOf(u);
        vm.prank(u);
        bool ok = tryDeposit(tid);
        if (ok) depositor[depKey(tid)] = u;
        string memory outs = ok ? outsU2(true, token0.balanceOf(u) - b0, token1.balanceOf(u) - b1) : "";
        emitOp("deposit", string.concat(vm.toString(u), ",", s(tid)), ok, outs, touchTok(tid, int256(ui)));
    }

    /// withdraw: inputs user,tokenId; outputs collected token0,token1 and the reward paid to the user.
    function doWithdraw() internal {
        uint256 tid = rand() % 10 < 8 ? pickStaked() : pickToken();
        address u = depositor[depKey(tid)] != address(0) && rand() % 10 < 8
            ? depositor[depKey(tid)]
            : eoa(rand() % NUSERS);
        uint256 b0 = token0.balanceOf(u);
        uint256 b1 = token1.balanceOf(u);
        uint256 br = reward.balanceOf(u);
        vm.prank(u);
        bool ok = tryWithdraw(tid);
        if (ok) depositor[depKey(tid)] = address(0);
        string memory outs = ok
            ? outsU3(true, token0.balanceOf(u) - b0, token1.balanceOf(u) - b1, reward.balanceOf(u) - br)
            : "";
        emitOp("withdraw", string.concat(vm.toString(u), ",", s(tid)), ok, outs, touchTok(tid, int256(userIndex(u))));
    }

    /// getRewardAll: inputs account (through the voter); outputs the reward paid to the account.
    function doGetRewardAll() internal {
        uint256 ui = rand() % NUSERS;
        address u = eoa(ui);
        uint256 br = reward.balanceOf(u);
        bool ok = tryGetRewardAll(u);
        emitOp("getRewardAll", vm.toString(u), ok, outsU1(ok, reward.balanceOf(u) - br), accTouch(int256(ui)));
    }

    /// getReward: inputs user,tokenId; outputs the reward paid to the user.
    function doGetReward() internal {
        uint256 tid = rand() % 10 < 7 ? pickStaked() : pickToken();
        address u = depositor[depKey(tid)] != address(0) && rand() % 10 < 8
            ? depositor[depKey(tid)]
            : eoa(rand() % NUSERS);
        uint256 br = reward.balanceOf(u);
        vm.prank(u);
        bool ok = tryGetReward(tid);
        emitOp(
            "getReward",
            string.concat(vm.toString(u), ",", s(tid)),
            ok,
            outsU1(ok, reward.balanceOf(u) - br),
            touchTok(tid, int256(userIndex(u)))
        );
    }

    /// earned: inputs account,tokenId; outputs the view result.
    function doEarned() internal {
        uint256 tid = rand() % 10 < 6 ? pickStaked() : pickToken();
        address u = depositor[depKey(tid)] != address(0) && rand() % 10 < 7
            ? depositor[depKey(tid)]
            : eoa(rand() % NUSERS);
        uint256 ui = uint256(userIndex(u));
        bool ok;
        uint256 v;
        try gauge.earned(u, tid) returns (uint256 x) {
            ok = true;
            v = x;
        } catch {}
        GTouch memory t = none();
        t.acc = int256(ui);
        t.ks = new uint256[](1);
        t.ks[0] = tid;
        emitOp("earned", string.concat(vm.toString(u), ",", s(tid)), ok, outsU1(ok, v), t);
    }

    /// notify: inputs amount (voter); outputs the fee tokens pulled by the fees voting reward (token0,token1).
    function doNotify() internal {
        uint256 amt = pickNotifyAmount();
        uint256 f0 = token0.balanceOf(feesVr);
        uint256 f1 = token1.balanceOf(feesVr);
        bool ok;
        try voter.notify(address(gauge), amt) {
            ok = true;
        } catch {}
        uint256 d0 = ok ? token0.balanceOf(feesVr) - f0 : 0;
        uint256 d1 = ok ? token1.balanceOf(feesVr) - f1 : 0;
        emitOp("notify", s(amt), ok, outsU2(ok, d0, d1), none());
    }

    /// notifyNoClaim: inputs amount (notify admin = gauge factory); no outputs.
    function doNotifyNoClaim() internal {
        uint256 amt = pickNotifyAmount();
        bool ok;
        try gaugeFactory.notifyWithoutClaim(address(gauge), amt) {
            ok = true;
        } catch {}
        emitOp("notifyNoClaim", s(amt), ok, "", none());
    }

    /// donate: a direct transfer of reward tokens to the gauge (always succeeds).
    function doDonate() internal {
        uint256 amt = randBits(60);
        reward.mint(address(gauge), amt);
        emitOp("donate", s(amt), true, "", none());
    }

    /// setAlive: inputs flag (voter isAlive for the gauge).
    function doSetAlive() internal {
        bool a = rand() % 10 != 0;
        voter.setAlive(a);
        emitOp("setAlive", b2s(a), true, "", none());
    }

    /// swap: inputs recipient,zeroForOne,amountSpecified,sqrtPriceLimit,fee,unstakedFee; outputs amount0,amount1.
    function doSwap() internal {
        (uint160 sq, int24 startTk, , , , ) = pool.slot0();
        bool zfo = rand() % 2 == 0;
        int256 amt = pickAmount();
        uint160 lim = pickLimit(sq, zfo);
        uint24 fee = feeOf(rand() % 4);
        uint24 uf = unstakedFeeOf(rand() % 3);
        feeFactory.setFees(fee, uf);
        address rec = eoa(rand() % NUSERS);
        string memory ins = string.concat(
            vm.toString(rec),
            ",",
            b2s(zfo),
            ",",
            vm.toString(amt),
            ",",
            s(uint256(lim)),
            ",",
            s(uint256(fee)),
            ",",
            s(uint256(uf))
        );
        bool ok;
        int256 a0;
        int256 a1;
        try driver.swap(rec, zfo, amt, lim) returns (int256 x, int256 y) {
            ok = true;
            a0 = x;
            a1 = y;
        } catch {}
        (, int24 endTk, , , , ) = pool.slot0();
        GTouch memory t = none();
        t.ts = swapTicks(startTk, endTk);
        string memory outs = ok ? string.concat(vm.toString(a0), ",", vm.toString(a1)) : "";
        emitOp("swap", ins, ok, outs, t);
    }

    function step() internal {
        uint256 d = rand() % 10 == 0 ? 0 : (rand() % 30 == 0 ? rand() % (8 days) : rand() % 3601);
        vm.warp(block.timestamp + d);
        uint256 r = rand() % 100;
        if (r < 12) doMint();
        else if (r < 18) doApprove();
        else if (r < 32) doDeposit();
        else if (r < 42) doWithdraw();
        else if (r < 48) doGetRewardAll();
        else if (r < 54) doGetReward();
        else if (r < 58) doEarned();
        else if (r < 68) doNotify();
        else if (r < 71) doNotifyNoClaim();
        else if (r < 74) doDonate();
        else if (r < 75) doSetAlive();
        else doSwap();
    }

    function initialSqrtPrice() internal returns (uint160) {
        uint256 p = rand() % 10;
        if (p == 0) return MIN_SQRT + 1;
        if (p == 1) return MAX_SQRT - 1;
        return uint160(2 ** 80 + rand() % (2 ** 112 - 2 ** 80));
    }

    /// Deploys the whole system for one segment and logs the `init` and `createGauge` lines.
    function newSegment(uint256 seg) internal {
        int24[4] memory spacings = [int24(1), int24(10), int24(60), int24(10)];
        spacing = spacings[seg];
        isPoolSeg = seg % 2 == 0;
        gauge = IGGauge(address(0));
        segBase = seg << 64;
        delete anchors;
        delete tokenIds;
        vm.warp(BASE_TIME + seg * 10_000_000);

        address ta = vm.deployCode("MockERC20.sol:MockERC20");
        address tb = vm.deployCode("MockERC20.sol:MockERC20");
        if (ta > tb) (ta, tb) = (tb, ta);
        token0 = IGToken(ta);
        token1 = IGToken(tb);
        reward = IGToken(vm.deployCode("MockERC20.sol:MockERC20"));
        feeFactory = IGFeeFactory(vm.deployCode("MockFactory.sol:MockFactory"));
        registry = IGRegistry(vm.deployCode("MockFactoryRegistry.sol:MockFactoryRegistry"));
        pool = IGPool(vm.deployCode("CLPool.sol:CLPool"));
        driver = IGDriver(vm.deployCode("ClPoolDriver.sol:ClPoolDriver"));
        nft = IGNft(vm.deployCode("MockNftManager.sol:MockNftManager"));
        gaugeFactory = IGGaugeFactory(vm.deployCode("MockGaugeFactory.sol:MockGaugeFactory"));
        voter = IGVoter(vm.deployCode("MockGaugeVoter.sol:MockGaugeVoter"));
        feesVr = vm.deployCode("MockFeesVotingReward.sol:MockFeesVotingReward");

        driver.setup(address(pool), ta, tb);
        registry.setGaugeFactory(address(gaugeFactory));
        voter.setRewardToken(address(reward));
        voter.setAlive(true);
        gaugeFactory.setRewardToken(address(reward));

        uint160 sqrtPrice = initialSqrtPrice();
        pool.initialize(address(feeFactory), ta, tb, spacing, address(registry), sqrtPrice);
        token0.mint(address(pool), DONATION);
        token1.mint(address(pool), DONATION);
        nft.setPool(address(pool), ta, tb);

        (, int24 tk, , , , ) = pool.slot0();
        buildAnchors(tk);

        string memory ins = string.concat(vm.toString(int256(spacing)), ",", s(uint256(sqrtPrice)));
        emitOp("init", ins, true, s(uint256(pool.maxLiquidityPerTick())), none());

        gauge = IGGauge(
            gaugeFactory.createGauge(
                address(pool), feesVr, address(voter), address(nft), ta, tb, spacing, isPoolSeg
            )
        );
        string memory gins = string.concat(
            vm.toString(address(gauge)),
            ",",
            vm.toString(address(nft)),
            ",",
            vm.toString(address(gaugeFactory)),
            ",",
            vm.toString(address(voter)),
            ",",
            vm.toString(ta),
            ",",
            vm.toString(tb),
            ",",
            vm.toString(int256(spacing)),
            ",",
            b2s(isPoolSeg)
        );
        emitOp("createGauge", gins, true, "", none());
        for (uint256 i = 0; i < NUSERS; i++) {
            approveOp(i, 1);
        }
    }

    /// One segment per external call: the memory used by the string logging is released between segments.
    function runSegment(uint256 seg) external {
        require(msg.sender == address(this), "SELF");
        newSegment(seg);
        for (uint256 i = 0; i < OPS_PER_SEG; i++) {
            step();
        }
    }

    function testGenerateClGauge() public {
        vm.writeFile(FILE, "");
        for (uint256 seg = 0; seg < SEGMENTS; seg++) {
            this.runSegment(seg);
        }
    }
}
