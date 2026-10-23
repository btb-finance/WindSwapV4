// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

interface Vm {
    function deployCode(string calldata artifactPath) external returns (address);
    function writeLine(string calldata path, string calldata data) external;
    function writeFile(string calldata path, string calldata data) external;
    function toString(uint256 value) external pure returns (string memory);
    function toString(int256 value) external pure returns (string memory);
}

interface IPositionHarness {
    function update(
        address owner,
        int24 tickLower,
        int24 tickUpper,
        int128 liquidityDelta,
        uint256 feeGrowthInside0X128,
        uint256 feeGrowthInside1X128,
        bool staked
    ) external;
    function info(address owner, int24 tickLower, int24 tickUpper)
        external
        view
        returns (uint128, uint256, uint256, uint128, uint128);
}

/// @notice Generates a stateful differential trace for the original Position library.
/// Output: `vectors/position.txt`, one step per line:
///   `update|key|liquidityDelta|fg0|fg1|staked|ok|liquidity|fg0Last|fg1Last|owed0|owed1` (state after)
///   `update|key|liquidityDelta|fg0|fg1|staked|err`                                    (state unchanged)
/// Keys: key k -> owner address(0xA000 + k / 3), ticks from `keyTicks(k % 3)`.
/// Run: `forge test --match-test testGeneratePosition`
contract GeneratePositionVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    uint256 constant N = 3000;
    uint256 constant KEYS = 9;
    string constant FILE = "vectors/position.txt";

    IPositionHarness h;
    uint256 seed = 0x504f53; // "POS"
    uint256 fg0;
    uint256 fg1;

    function rand() internal returns (uint256) {
        seed = uint256(keccak256(abi.encode(seed)));
        return seed;
    }

    /// Random value with a random bit length up to `maxBits` (< 256), plus edge values.
    function randBits(uint256 maxBits) internal returns (uint256) {
        uint256 pick = rand() % 20;
        if (pick == 0) return 0;
        if (pick == 1) return 1;
        if (pick == 2) return (uint256(1) << maxBits) - 1;
        uint256 k = rand() % maxBits + 1;
        return rand() >> (256 - k);
    }

    /// Liquidity delta: zeros (pokes), small/large/near-max positives, negatives that underflow, int128 edges.
    function randDelta() internal returns (int128) {
        uint256 pick = rand() % 20;
        if (pick < 4) return 0;
        if (pick == 4) return type(int128).min;
        if (pick == 5) return type(int128).max;
        uint256 bits = pick < 10 ? 20 : (pick < 14 ? 64 : 127);
        int128 v = int128(int256(randBits(bits)));
        return rand() % 3 == 0 ? -v : v;
    }

    function withdrawAll(uint128 cur) internal pure returns (int128) {
        if (cur == 0) return 0;
        if (cur > uint128(type(int128).max)) return type(int128).min;
        return -int128(cur);
    }

    /// Fee growth: mostly increasing (large steps make tokensOwed wrap), sometimes unchanged,
    /// random 256-bit, or decreasing (so `feeGrowthInside - last` wraps).
    function nextFee(uint256 fg) internal returns (uint256) {
        uint256 pick = rand() % 10;
        unchecked {
            if (pick < 5) return fg + randBits(128);
            if (pick < 7) return fg + randBits(200);
            if (pick == 7) return fg;
            if (pick == 8) return rand();
            return fg - randBits(160);
        }
    }

    function keyOwner(uint256 k) internal pure returns (address) {
        return address(uint160(0xA000 + k / 3));
    }

    function keyTicks(uint256 k) internal pure returns (int24, int24) {
        uint256 r = k % 3;
        if (r == 0) return (-120, 120);
        if (r == 1) return (-60, 60);
        return (0, 600);
    }

    function testGeneratePosition() public {
        h = IPositionHarness(vm.deployCode("PositionHarness.sol:PositionHarness"));
        vm.writeFile(FILE, "");
        for (uint256 i = 0; i < N; i++) {
            step();
        }
    }

    function step() internal {
        uint256 k = rand() % KEYS;
        address owner = keyOwner(k);
        (int24 lo, int24 hi) = keyTicks(k);
        (uint128 cur,,,,) = h.info(owner, lo, hi);
        // Every 6th step withdraws all liquidity, so later zero-delta pokes hit "NP".
        int128 d = rand() % 6 == 0 ? withdrawAll(cur) : randDelta();
        fg0 = nextFee(fg0);
        fg1 = nextFee(fg1);
        bool staked = rand() % 2 == 0;

        string memory head = string(
            abi.encodePacked(
                "update|",
                vm.toString(k),
                "|",
                vm.toString(int256(d)),
                "|",
                vm.toString(fg0),
                "|",
                vm.toString(fg1),
                "|",
                staked ? "1" : "0",
                "|"
            )
        );
        try h.update(owner, lo, hi, d, fg0, fg1, staked) {
            logOk(head, owner, lo, hi);
        } catch {
            vm.writeLine(FILE, string(abi.encodePacked(head, "err")));
        }
    }

    function logOk(string memory head, address owner, int24 lo, int24 hi) internal {
        (uint128 l, uint256 f0, uint256 f1, uint128 o0, uint128 o1) = h.info(owner, lo, hi);
        string memory state = string(
            abi.encodePacked(
                "ok|",
                vm.toString(uint256(l)),
                "|",
                vm.toString(f0),
                "|",
                vm.toString(f1),
                "|",
                vm.toString(uint256(o0)),
                "|",
                vm.toString(uint256(o1))
            )
        );
        vm.writeLine(FILE, string(abi.encodePacked(head, state)));
    }
}
