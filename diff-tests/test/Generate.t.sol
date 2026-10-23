// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

interface Vm {
    function deployCode(string calldata artifactPath) external returns (address);
    function writeLine(string calldata path, string calldata data) external;
    function writeFile(string calldata path, string calldata data) external;
    function toString(uint256 value) external pure returns (string memory);
    function toString(int256 value) external pure returns (string memory);
}

interface IHarness {
    function mulDiv(uint256 a, uint256 b, uint256 d) external pure returns (uint256);
    function mulDivRoundingUp(uint256 a, uint256 b, uint256 d) external pure returns (uint256);
    function divRoundingUp(uint256 x, uint256 y) external pure returns (uint256);
    function mostSignificantBit(uint256 x) external pure returns (uint8);
    function leastSignificantBit(uint256 x) external pure returns (uint8);
    function addDelta(uint128 x, int128 y) external pure returns (uint128);
    function getSqrtRatioAtTick(int24 tick) external pure returns (uint160);
    function getTickAtSqrtRatio(uint160 sqrtPriceX96) external pure returns (int24);
    function getNextSqrtPriceFromInput(uint160 p, uint128 l, uint256 amount, bool zeroForOne)
        external
        pure
        returns (uint160);
    function getNextSqrtPriceFromOutput(uint160 p, uint128 l, uint256 amount, bool zeroForOne)
        external
        pure
        returns (uint160);
    function getAmount0Delta(uint160 a, uint160 b, uint128 l, bool roundUp) external pure returns (uint256);
    function getAmount1Delta(uint160 a, uint160 b, uint128 l, bool roundUp) external pure returns (uint256);
    function getAmount0DeltaSigned(uint160 a, uint160 b, int128 l) external pure returns (int256);
    function getAmount1DeltaSigned(uint160 a, uint160 b, int128 l) external pure returns (int256);
    function computeSwapStep(uint160 current, uint160 target, uint128 l, int256 remaining, uint24 feePips)
        external
        pure
        returns (uint160, uint256, uint256, uint256);
}

/// @notice Generates differential test vectors from the original Solidity math.
/// Output: `vectors/<fn>.txt`, one case per line: `inputs...|ok|outputs...` or `inputs...|err`.
/// Run: `forge test --match-test testGenerate`
contract GenerateVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    uint256 constant N = 1500;
    uint160 constant MIN_SQRT = 4295128739;
    uint160 constant MAX_SQRT = 1461446703485210103287273052203988822378723970342;

    IHarness h;
    uint256 seed = 0x57494e44; // "WIND"

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

    function randPrice() internal returns (uint160) {
        uint256 pick = rand() % 10;
        if (pick == 0) return MIN_SQRT;
        if (pick == 1) return MAX_SQRT - 1;
        if (pick < 4) return uint160(randBits(160));
        return uint160(MIN_SQRT + rand() % (uint256(MAX_SQRT) - MIN_SQRT));
    }

    function randInt128() internal returns (int128) {
        uint256 v = randBits(127);
        return rand() % 2 == 0 ? int128(int256(v)) : -int128(int256(v)) - int128(int256(rand() % 2));
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

    function file(string memory name) internal pure returns (string memory) {
        return string(abi.encodePacked("vectors/", name, ".txt"));
    }

    function reset(string memory name) internal {
        vm.writeFile(file(name), "");
    }

    function testGenerate() public {
        h = IHarness(vm.deployCode("Harness.sol:Harness"));
        genFullMath();
        genUnsafeAndBitMath();
        genLiquidityMath();
        genTickMath();
        genSqrtPriceMath();
        genSwapMath();
    }

    function genFullMath() internal {
        reset("mul_div");
        reset("mul_div_rounding_up");
        for (uint256 i = 0; i < N; i++) {
            uint256 a = randBits(256);
            uint256 c = randBits(256);
            uint256 d = randBits(256);
            string memory inp = string(abi.encodePacked(s(a), "|", s(c), "|", s(d)));
            try h.mulDiv(a, c, d) returns (uint256 r) {
                vm.writeLine(file("mul_div"), string(abi.encodePacked(inp, "|ok|", s(r))));
            } catch {
                vm.writeLine(file("mul_div"), string(abi.encodePacked(inp, "|err")));
            }
            try h.mulDivRoundingUp(a, c, d) returns (uint256 r) {
                vm.writeLine(file("mul_div_rounding_up"), string(abi.encodePacked(inp, "|ok|", s(r))));
            } catch {
                vm.writeLine(file("mul_div_rounding_up"), string(abi.encodePacked(inp, "|err")));
            }
        }
    }

    function genUnsafeAndBitMath() internal {
        reset("div_rounding_up");
        reset("msb");
        reset("lsb");
        for (uint256 i = 0; i < N; i++) {
            uint256 x = randBits(256);
            uint256 y = randBits(256);
            vm.writeLine(
                file("div_rounding_up"),
                string(abi.encodePacked(s(x), "|", s(y), "|ok|", s(h.divRoundingUp(x, y))))
            );
            try h.mostSignificantBit(x) returns (uint8 r) {
                vm.writeLine(file("msb"), string(abi.encodePacked(s(x), "|ok|", s(r))));
            } catch {
                vm.writeLine(file("msb"), string(abi.encodePacked(s(x), "|err")));
            }
            try h.leastSignificantBit(x) returns (uint8 r) {
                vm.writeLine(file("lsb"), string(abi.encodePacked(s(x), "|ok|", s(r))));
            } catch {
                vm.writeLine(file("lsb"), string(abi.encodePacked(s(x), "|err")));
            }
        }
    }

    function genLiquidityMath() internal {
        reset("add_delta");
        for (uint256 i = 0; i < N; i++) {
            uint128 x = uint128(randBits(128));
            int128 y = randInt128();
            string memory inp = string(abi.encodePacked(s(x), "|", si(y)));
            try h.addDelta(x, y) returns (uint128 r) {
                vm.writeLine(file("add_delta"), string(abi.encodePacked(inp, "|ok|", s(r))));
            } catch {
                vm.writeLine(file("add_delta"), string(abi.encodePacked(inp, "|err")));
            }
        }
    }

    function genTickMath() internal {
        reset("sqrt_ratio_at_tick");
        reset("tick_at_sqrt_ratio");
        for (uint256 i = 0; i < N; i++) {
            int24 tick = int24(int256(rand() % 1_800_000) - 900_000);
            try h.getSqrtRatioAtTick(tick) returns (uint160 r) {
                vm.writeLine(file("sqrt_ratio_at_tick"), string(abi.encodePacked(si(tick), "|ok|", s(r))));
            } catch {
                vm.writeLine(file("sqrt_ratio_at_tick"), string(abi.encodePacked(si(tick), "|err")));
            }
            uint160 p = randPrice();
            try h.getTickAtSqrtRatio(p) returns (int24 t) {
                vm.writeLine(file("tick_at_sqrt_ratio"), string(abi.encodePacked(s(p), "|ok|", si(t))));
            } catch {
                vm.writeLine(file("tick_at_sqrt_ratio"), string(abi.encodePacked(s(p), "|err")));
            }
        }
    }

    function genSqrtPriceMath() internal {
        reset("next_sqrt_price_from_input");
        reset("next_sqrt_price_from_output");
        reset("amount0_delta");
        reset("amount1_delta");
        reset("amount0_delta_signed");
        reset("amount1_delta_signed");
        for (uint256 i = 0; i < N; i++) {
            uint160 p = randPrice();
            uint128 l = uint128(randBits(128));
            uint256 amount = rand() % 2 == 0 ? randBits(256) : randBits(128);
            bool flag = rand() % 2 == 0;
            string memory inp = string(abi.encodePacked(s(p), "|", s(l), "|", s(amount), "|", b(flag)));
            try h.getNextSqrtPriceFromInput(p, l, amount, flag) returns (uint160 r) {
                vm.writeLine(file("next_sqrt_price_from_input"), string(abi.encodePacked(inp, "|ok|", s(r))));
            } catch {
                vm.writeLine(file("next_sqrt_price_from_input"), string(abi.encodePacked(inp, "|err")));
            }
            try h.getNextSqrtPriceFromOutput(p, l, amount, flag) returns (uint160 r) {
                vm.writeLine(file("next_sqrt_price_from_output"), string(abi.encodePacked(inp, "|ok|", s(r))));
            } catch {
                vm.writeLine(file("next_sqrt_price_from_output"), string(abi.encodePacked(inp, "|err")));
            }

            uint160 q = randPrice();
            string memory dinp = string(abi.encodePacked(s(p), "|", s(q), "|", s(l), "|", b(flag)));
            try h.getAmount0Delta(p, q, l, flag) returns (uint256 r) {
                vm.writeLine(file("amount0_delta"), string(abi.encodePacked(dinp, "|ok|", s(r))));
            } catch {
                vm.writeLine(file("amount0_delta"), string(abi.encodePacked(dinp, "|err")));
            }
            try h.getAmount1Delta(p, q, l, flag) returns (uint256 r) {
                vm.writeLine(file("amount1_delta"), string(abi.encodePacked(dinp, "|ok|", s(r))));
            } catch {
                vm.writeLine(file("amount1_delta"), string(abi.encodePacked(dinp, "|err")));
            }

            int128 sl = randInt128();
            string memory sinp = string(abi.encodePacked(s(p), "|", s(q), "|", si(sl)));
            try h.getAmount0DeltaSigned(p, q, sl) returns (int256 r) {
                vm.writeLine(file("amount0_delta_signed"), string(abi.encodePacked(sinp, "|ok|", si(r))));
            } catch {
                vm.writeLine(file("amount0_delta_signed"), string(abi.encodePacked(sinp, "|err")));
            }
            try h.getAmount1DeltaSigned(p, q, sl) returns (int256 r) {
                vm.writeLine(file("amount1_delta_signed"), string(abi.encodePacked(sinp, "|ok|", si(r))));
            } catch {
                vm.writeLine(file("amount1_delta_signed"), string(abi.encodePacked(sinp, "|err")));
            }
        }
    }

    function genSwapMath() internal {
        reset("compute_swap_step");
        uint24[6] memory fees = [uint24(0), 100, 500, 3000, 10000, 0];
        for (uint256 i = 0; i < N * 2; i++) {
            uint160 cur = randPrice();
            uint160 target = randPrice();
            uint128 l = uint128(randBits(128));
            uint256 mag = randBits(255);
            int256 remaining = rand() % 2 == 0 ? int256(mag) : -int256(mag);
            uint24 fee = fees[rand() % 6];
            if (rand() % 6 == 0) fee = uint24(rand() % 1_000_000);
            string memory inp = string(
                abi.encodePacked(s(cur), "|", s(target), "|", s(l), "|", si(remaining), "|", s(fee))
            );
            try h.computeSwapStep(cur, target, l, remaining, fee) returns (
                uint160 next, uint256 amountIn, uint256 amountOut, uint256 feeAmount
            ) {
                vm.writeLine(
                    file("compute_swap_step"),
                    string(
                        abi.encodePacked(
                            inp, "|ok|", s(next), "|", s(amountIn), "|", s(amountOut), "|", s(feeAmount)
                        )
                    )
                );
            } catch {
                vm.writeLine(file("compute_swap_step"), string(abi.encodePacked(inp, "|err")));
            }
        }
    }
}
