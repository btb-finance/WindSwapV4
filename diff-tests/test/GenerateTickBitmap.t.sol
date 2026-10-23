// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

interface Vm {
    function deployCode(string calldata artifactPath) external returns (address);
    function writeLine(string calldata path, string calldata data) external;
    function writeFile(string calldata path, string calldata data) external;
    function toString(int256 value) external pure returns (string memory);
}

interface ITickBitmapHarness {
    function flipTick(int24 tick, int24 tickSpacing) external;
    function nextInitializedTickWithinOneWord(int24 tick, int24 tickSpacing, bool lte)
        external
        view
        returns (int24, bool);
}

/// @notice Generates a stateful differential test sequence for the original TickBitmap library.
/// Output: `vectors/tick_bitmap.txt`, one step per line:
///   `reset|<tickSpacing>`                          fresh harness with this tick spacing
///   `flip|<tick>|ok` / `flip|<tick>|err`           flipTick (err = revert)
///   `next|<tick>|<lte>|ok|<next>|<initialized>`    nextInitializedTickWithinOneWord
/// Run: `forge test --match-test testGenerateTickBitmap`
contract GenerateTickBitmapVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    string constant OUT = "vectors/tick_bitmap.txt";
    int256 constant MAX_TICK = 887272;
    uint256 constant SEGMENTS = 4;
    uint256 constant STEPS_PER_SEGMENT = 750;

    ITickBitmapHarness h;
    uint256 seed = 0x5449434b; // "TICK"

    function rand() internal returns (uint256) {
        seed = uint256(keccak256(abi.encode(seed)));
        return seed;
    }

    function s(int256 v) internal pure returns (string memory) {
        return vm.toString(v);
    }

    function b(bool v) internal pure returns (string memory) {
        return v ? "1" : "0";
    }

    function line(string memory l) internal {
        vm.writeLine(OUT, l);
    }

    /// Random tick aligned to `spacing`, anywhere in the usable range.
    function alignedTick(int256 spacing) internal returns (int256) {
        int256 maxK = MAX_TICK / spacing;
        int256 k = int256(rand() % uint256(2 * maxK + 1)) - maxK;
        return k * spacing;
    }

    function testGenerateTickBitmap() public {
        vm.writeFile(OUT, "");
        int256[4] memory spacings = [int256(1), 10, 60, 200];
        for (uint256 seg = 0; seg < SEGMENTS; seg++) {
            int256 spacing = spacings[seg];
            h = ITickBitmapHarness(vm.deployCode("TickBitmapHarness.sol:TickBitmapHarness"));
            line(string(abi.encodePacked("reset|", s(spacing))));
            runSegment(spacing);
        }
    }

    function runSegment(int256 spacing) internal {
        // Ticks that were flipped successfully, used to bias queries towards initialized ticks.
        int256[] memory flipped = new int256[](STEPS_PER_SEGMENT);
        uint256 nFlipped = 0;

        for (uint256 i = 0; i < STEPS_PER_SEGMENT; i++) {
            if (rand() % 100 < 40) {
                // flipTick: mostly aligned, sometimes misaligned to exercise the revert.
                int256 tick;
                uint256 pick = rand() % 10;
                if (nFlipped > 0 && pick < 4) {
                    // Re-flip near an existing tick (toggles it back or sets a neighbour).
                    int256 base = flipped[rand() % nFlipped];
                    tick = base + (int256(rand() % 5) - 2) * spacing;
                    if (tick > MAX_TICK || tick < -MAX_TICK) tick = base;
                } else {
                    tick = alignedTick(spacing);
                }
                if (spacing > 1 && rand() % 10 == 0) {
                    tick += int256(rand() % uint256(spacing - 1)) + 1; // misaligned
                }
                int24 t = int24(tick);
                try h.flipTick(t, int24(spacing)) {
                    line(string(abi.encodePacked("flip|", s(tick), "|ok")));
                    if (nFlipped < STEPS_PER_SEGMENT) flipped[nFlipped++] = tick;
                } catch {
                    line(string(abi.encodePacked("flip|", s(tick), "|err")));
                }
            } else {
                // nextInitializedTickWithinOneWord: mostly near flipped ticks, sometimes anywhere.
                int256 tick;
                uint256 pick = rand() % 10;
                if (nFlipped > 0 && pick < 7) {
                    int256 base = flipped[rand() % nFlipped];
                    int256 range = 3 * spacing + 2;
                    tick = base + int256(rand() % uint256(2 * uint256(range) + 1)) - range;
                } else {
                    tick = int256(rand() % uint256(2 * uint256(MAX_TICK) + 1)) - MAX_TICK;
                }
                if (tick > MAX_TICK) tick = MAX_TICK;
                if (tick < -MAX_TICK) tick = -MAX_TICK;
                bool lte = rand() % 2 == 0;
                (int24 next, bool initialized) = h.nextInitializedTickWithinOneWord(int24(tick), int24(spacing), lte);
                line(
                    string(
                        abi.encodePacked(
                            "next|", s(tick), "|", b(lte), "|ok|", s(next), "|", b(initialized)
                        )
                    )
                );
            }
        }
    }
}
