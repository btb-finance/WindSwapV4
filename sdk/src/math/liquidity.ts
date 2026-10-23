/**
 * Port of Uniswap v3 periphery `LiquidityAmounts.sol` (BigInt). Every division rounds down,
 * as in Solidity, and results that do not fit their Solidity type throw.
 */
import { Q96, UINT128_MAX, UINT160_MAX, UINT256_MAX } from "./constants.js";

/** `FullMath.mulDiv`: floor(a * b / d). Throws on d = 0 or a result that overflows uint256. */
export function mulDiv(a: bigint, b: bigint, d: bigint): bigint {
  if (d === 0n) throw new RangeError("mulDiv: division by zero");
  const result = (a * b) / d;
  if (result > UINT256_MAX) throw new RangeError("mulDiv: result overflows uint256");
  return result;
}

function toUint128(x: bigint): bigint {
  if (x > UINT128_MAX) throw new RangeError("liquidity does not fit in uint128");
  return x;
}

function assertSqrt160(x: bigint, name: string): void {
  if (x < 0n || x > UINT160_MAX) throw new RangeError(`${name} does not fit in uint160`);
}

function sortAsc(a: bigint, b: bigint): [bigint, bigint] {
  return a > b ? [b, a] : [a, b];
}

/** Liquidity received for `amount0` over the range `[sqrtA, sqrtB]`. */
export function getLiquidityForAmount0(sqrtRatioAX96: bigint, sqrtRatioBX96: bigint, amount0: bigint): bigint {
  assertSqrt160(sqrtRatioAX96, "sqrtRatioAX96");
  assertSqrt160(sqrtRatioBX96, "sqrtRatioBX96");
  const [a, b] = sortAsc(sqrtRatioAX96, sqrtRatioBX96);
  const intermediate = mulDiv(a, b, Q96);
  return toUint128(mulDiv(amount0, intermediate, b - a));
}

/** Liquidity received for `amount1` over the range `[sqrtA, sqrtB]`. */
export function getLiquidityForAmount1(sqrtRatioAX96: bigint, sqrtRatioBX96: bigint, amount1: bigint): bigint {
  assertSqrt160(sqrtRatioAX96, "sqrtRatioAX96");
  assertSqrt160(sqrtRatioBX96, "sqrtRatioBX96");
  const [a, b] = sortAsc(sqrtRatioAX96, sqrtRatioBX96);
  return toUint128(mulDiv(amount1, Q96, b - a));
}

/**
 * Maximum liquidity for `amount0` and `amount1` at the current price `sqrtRatioX96` and the
 * range `[sqrtA, sqrtB]`. Use it to size a position from the amounts you want to deposit.
 */
export function getLiquidityForAmounts(
  sqrtRatioX96: bigint,
  sqrtRatioAX96: bigint,
  sqrtRatioBX96: bigint,
  amount0: bigint,
  amount1: bigint,
): bigint {
  assertSqrt160(sqrtRatioX96, "sqrtRatioX96");
  const [a, b] = sortAsc(sqrtRatioAX96, sqrtRatioBX96);
  if (sqrtRatioX96 <= a) {
    return getLiquidityForAmount0(a, b, amount0);
  }
  if (sqrtRatioX96 < b) {
    const liquidity0 = getLiquidityForAmount0(sqrtRatioX96, b, amount0);
    const liquidity1 = getLiquidityForAmount1(a, sqrtRatioX96, amount1);
    return liquidity0 < liquidity1 ? liquidity0 : liquidity1;
  }
  return getLiquidityForAmount1(a, b, amount1);
}

/** Amount of token0 that `liquidity` is worth over `[sqrtA, sqrtB]`, rounded down. */
export function getAmount0ForLiquidity(sqrtRatioAX96: bigint, sqrtRatioBX96: bigint, liquidity: bigint): bigint {
  assertSqrt160(sqrtRatioAX96, "sqrtRatioAX96");
  assertSqrt160(sqrtRatioBX96, "sqrtRatioBX96");
  const [a, b] = sortAsc(sqrtRatioAX96, sqrtRatioBX96);
  return mulDiv(liquidity << 96n, b - a, b) / a;
}

/** Amount of token1 that `liquidity` is worth over `[sqrtA, sqrtB]`, rounded down. */
export function getAmount1ForLiquidity(sqrtRatioAX96: bigint, sqrtRatioBX96: bigint, liquidity: bigint): bigint {
  assertSqrt160(sqrtRatioAX96, "sqrtRatioAX96");
  assertSqrt160(sqrtRatioBX96, "sqrtRatioBX96");
  const [a, b] = sortAsc(sqrtRatioAX96, sqrtRatioBX96);
  return mulDiv(liquidity, b - a, Q96);
}

/**
 * Token amounts that `liquidity` in the range `[sqrtA, sqrtB]` is worth at the current price
 * `sqrtRatioX96`, rounded down. Returns `{ amount0, amount1 }`.
 */
export function getAmountsForLiquidity(
  sqrtRatioX96: bigint,
  sqrtRatioAX96: bigint,
  sqrtRatioBX96: bigint,
  liquidity: bigint,
): { amount0: bigint; amount1: bigint } {
  assertSqrt160(sqrtRatioX96, "sqrtRatioX96");
  const [a, b] = sortAsc(sqrtRatioAX96, sqrtRatioBX96);
  if (sqrtRatioX96 <= a) {
    return { amount0: getAmount0ForLiquidity(a, b, liquidity), amount1: 0n };
  }
  if (sqrtRatioX96 < b) {
    return {
      amount0: getAmount0ForLiquidity(sqrtRatioX96, b, liquidity),
      amount1: getAmount1ForLiquidity(a, sqrtRatioX96, liquidity),
    };
  }
  return { amount0: 0n, amount1: getAmount1ForLiquidity(a, b, liquidity) };
}
