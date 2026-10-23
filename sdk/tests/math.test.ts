import { describe, expect, it } from "vitest";
import {
  defaultSqrtPriceLimit,
  getAmountsForLiquidity,
  getLiquidityForAmount0,
  getLiquidityForAmount1,
  getLiquidityForAmounts,
  getSqrtRatioAtTick,
  getTickAtSqrtRatio,
  maxAmountIn,
  minAmountOut,
  MAX_SQRT_RATIO,
  MIN_SQRT_RATIO,
  nearestUsableTick,
  priceToSqrtPriceX96,
  priceToTick,
  Q96,
  sqrtPriceX96ToPrice,
  tickToPrice,
} from "../src/index.js";

describe("price conversions", () => {
  it("price 1 with equal decimals is sqrtPriceX96 = 2^96", () => {
    expect(priceToSqrtPriceX96(1, 18, 18)).toBe(Q96);
    expect(sqrtPriceX96ToPrice(Q96, 18, 18)).toBe(1);
  });

  it("accounts for decimal differences (tUSDC 6 dec / tWETH 18 dec at 2000 tWETH per tUSDC)", () => {
    // raw price = 2000 * 10^(18-6) = 2e15; sqrtPriceX96 = sqrt(2e15) * 2^96
    const s = priceToSqrtPriceX96("2000", 6, 18);
    const back = sqrtPriceX96ToPrice(s, 6, 18);
    expect(Math.abs(back - 2000) / 2000).toBeLessThan(1e-15);
  });

  it("round-trips decimal prices", () => {
    for (const p of ["0.0005", "1", "3.25", "123456.789", "1e-7"]) {
      const s = priceToSqrtPriceX96(p, 18, 6);
      const back = sqrtPriceX96ToPrice(s, 18, 6);
      expect(Math.abs(back - Number(p)) / Number(p)).toBeLessThan(1e-12);
    }
  });

  it("rejects non-positive prices", () => {
    expect(() => priceToSqrtPriceX96(0, 18, 18)).toThrow(RangeError);
    expect(() => priceToSqrtPriceX96(-1, 18, 18)).toThrow(RangeError);
    expect(() => priceToSqrtPriceX96("abc", 18, 18)).toThrow(TypeError);
  });

  it("tickToPrice and priceToTick are inverse at tick boundaries", () => {
    for (const tick of [-600, -3, 0, 3, 600, 887272 - 1]) {
      const p = tickToPrice(tick, 18, 18);
      // The float price sits just below the exact boundary, so nudge it up before converting.
      expect(priceToTick((p * (1 + 1e-12)).toPrecision(15), 18, 18)).toBe(tick);
      expect(priceToTick((p * (1 - 1e-9)).toPrecision(15), 18, 18)).toBe(tick - 1);
    }
  });

  it("tickToPrice(0) is 1 for equal decimals", () => {
    expect(tickToPrice(0, 18, 18)).toBe(1);
  });
});

describe("nearestUsableTick", () => {
  it("rounds to the nearest multiple of the spacing", () => {
    expect(nearestUsableTick(7, 3)).toBe(6);
    expect(nearestUsableTick(8, 3)).toBe(9);
    expect(nearestUsableTick(-7, 3)).toBe(-6);
    expect(nearestUsableTick(0, 60)).toBe(0);
  });

  it("stays inside the tick range", () => {
    expect(nearestUsableTick(887272, 60)).toBe(887220);
    expect(nearestUsableTick(-887272, 60)).toBe(-887220);
  });

  it("rejects a non-positive spacing", () => {
    expect(() => nearestUsableTick(10, 0)).toThrow(RangeError);
  });
});

describe("liquidity math", () => {
  const sqrtA = getSqrtRatioAtTick(-600);
  const sqrtB = getSqrtRatioAtTick(600);
  const sqrtP = getSqrtRatioAtTick(120);

  it("in-range position: amounts round-trip to a liquidity no larger than the original", () => {
    for (const liquidity of [10n ** 20n, 5n * 10n ** 20n, 123456789012345678901234n]) {
      const { amount0, amount1 } = getAmountsForLiquidity(sqrtP, sqrtA, sqrtB, liquidity);
      expect(amount0).toBeGreaterThan(0n);
      expect(amount1).toBeGreaterThan(0n);
      const back = getLiquidityForAmounts(sqrtP, sqrtA, sqrtB, amount0, amount1);
      expect(back).toBeLessThanOrEqual(liquidity);
      // Rounding down loses at most a few parts per billion here.
      expect(liquidity - back).toBeLessThanOrEqual(liquidity / 1_000_000_000n + 2n);
    }
  });

  it("amounts for the liquidity from amounts never exceed the original amounts", () => {
    const amount0 = 1_000_000_000_000_000_000n;
    const amount1 = 3_000_000_000_000_000_000n;
    const liquidity = getLiquidityForAmounts(sqrtP, sqrtA, sqrtB, amount0, amount1);
    const back = getAmountsForLiquidity(sqrtP, sqrtA, sqrtB, liquidity);
    expect(back.amount0).toBeLessThanOrEqual(amount0);
    expect(back.amount1).toBeLessThanOrEqual(amount1);
  });

  it("below the range only token0 is needed; above the range only token1", () => {
    const below = getAmountsForLiquidity(sqrtA - 1n, sqrtA, sqrtB, 10n ** 20n);
    expect(below.amount1).toBe(0n);
    expect(below.amount0).toBeGreaterThan(0n);
    const above = getAmountsForLiquidity(sqrtB + 1n, sqrtA, sqrtB, 10n ** 20n);
    expect(above.amount0).toBe(0n);
    expect(above.amount1).toBeGreaterThan(0n);
  });

  it("swapping the range bounds does not change the result", () => {
    const x = getAmountsForLiquidity(sqrtP, sqrtA, sqrtB, 10n ** 19n);
    const y = getAmountsForLiquidity(sqrtP, sqrtB, sqrtA, 10n ** 19n);
    expect(x).toEqual(y);
  });

  it("liquidity from amounts is the minimum of the two single-token limits in range", () => {
    const amount0 = 10n ** 18n;
    const amount1 = 10n ** 12n;
    const l = getLiquidityForAmounts(sqrtP, sqrtA, sqrtB, amount0, amount1);
    // In range: token0 alone supports liquidity over [P, B], token1 alone over [A, P].
    const fromAmount0 = getLiquidityForAmount0(sqrtP, sqrtB, amount0);
    const fromAmount1 = getLiquidityForAmount1(sqrtA, sqrtP, amount1);
    expect(l).toBe(fromAmount0 < fromAmount1 ? fromAmount0 : fromAmount1);
  });

  it("throws when a result does not fit in uint128", () => {
    expect(() => getLiquidityForAmounts(sqrtP, MIN_SQRT_RATIO, MAX_SQRT_RATIO - 1n, 1n << 200n, 0n)).toThrow(RangeError);
  });
});

describe("slippage helpers", () => {
  it("minAmountOut rounds down, maxAmountIn rounds up", () => {
    expect(minAmountOut(1000n, 50)).toBe(995n);
    expect(minAmountOut("999", 50)).toBe(994n); // 999 * 9950 / 10000 = 994.005
    expect(maxAmountIn(1000n, 50)).toBe(1005n);
    expect(maxAmountIn(999n, 50)).toBe(1004n); // 999 * 10050 / 10000 = 1003.995 -> ceil 1004
    expect(minAmountOut(1000n, 0)).toBe(1000n);
    expect(maxAmountIn(1000n, 0)).toBe(1000n);
  });

  it("rejects slippage outside 0..10000 bps", () => {
    expect(() => minAmountOut(1n, -1)).toThrow(RangeError);
    expect(() => maxAmountIn(1n, 10001)).toThrow(RangeError);
  });

  it("default sqrt price limits are the exclusive bounds", () => {
    expect(defaultSqrtPriceLimit(true)).toBe(MIN_SQRT_RATIO + 1n);
    expect(defaultSqrtPriceLimit(false)).toBe(MAX_SQRT_RATIO - 1n);
  });
});
