/**
 * BigInt pool math, copied from `sdk/src/math` (tickMath.ts, liquidity.ts, constants.ts) so this
 * tool runs on its own with Node's built-in TypeScript stripping. The copies are checked against
 * the on-chain pool state in compute.ts (slot0.tick must equal getTickAtSqrtRatio(sqrtPrice)).
 *
 * Rounding follows the engine (`crates/engine/src/math/sqrt_price_math.rs`): the amounts a pool
 * holds for a position are rounded UP (as mint does), so `positionAmounts` takes `roundUp = true`.
 */

export const Q96 = 1n << 96n;
export const Q128 = 1n << 128n;
export const UINT256_MAX = (1n << 256n) - 1n;
export const MIN_TICK = -887272;
export const MAX_TICK = 887272;
export const MIN_SQRT_RATIO = 4295128739n;
export const MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342n;

const TICK_FACTORS: readonly bigint[] = [
  0xfff97272373d413259a46990580e213an,
  0xfff2e50f5f656932ef12357cf3c7fdccn,
  0xffe5caca7e10e4e61c3624eaa0941cd0n,
  0xffcb9843d60f6159c9db58835c926644n,
  0xff973b41fa98c081472e6896dfb254c0n,
  0xff2ea16466c96a3843ec78b326b52861n,
  0xfe5dee046a99a2a811c461f1969c3053n,
  0xfcbe86c7900a88aedcffc83b479aa3a4n,
  0xf987a7253ac413176f2b074cf7815e54n,
  0xf3392b0822b70005940c7a398e4b70f3n,
  0xe7159475a2c29b7443b29c7fa6e889d9n,
  0xd097f3bdfd2022b8845ad8f792aa5825n,
  0xa9f746462d870fdf8a65dc1f90e061e5n,
  0x70d869a156d2a1b890bb3df62baf32f7n,
  0x31be135f97d08fd981231505542fcfa6n,
  0x9aa508b5b7a84e1c677de54f3e99bc9n,
  0x5d6af8dedb81196699c329225ee604n,
  0x2216e584f5fa1ea926041bedfe98n,
  0x48a170391f7dc42444e8fa2n,
];

function bitLength(x: bigint): number {
  return x === 0n ? 0 : x.toString(2).length;
}

/** `sqrt(1.0001^tick) * 2^96`, rounded up (`TickMath.getSqrtRatioAtTick`). */
export function getSqrtRatioAtTick(tick: number): bigint {
  if (!Number.isInteger(tick)) throw new RangeError(`tick ${tick} is not an integer`);
  const absTick = Math.abs(tick);
  if (absTick > MAX_TICK) throw new RangeError(`tick ${tick} is outside [${MIN_TICK}, ${MAX_TICK}]`);

  let ratio = (absTick & 0x1) !== 0 ? 0xfffcb933bd6fad37aa2d162d1a594001n : Q128;
  for (let i = 0; i < TICK_FACTORS.length; i++) {
    if ((absTick & (0x2 << i)) !== 0) {
      ratio = (ratio * TICK_FACTORS[i]) >> 128n;
    }
  }
  if (tick > 0) {
    ratio = UINT256_MAX / ratio;
  }
  const roundUp = (ratio & 0xffffffffn) === 0n ? 0n : 1n;
  return (ratio >> 32n) + roundUp;
}

/** The greatest tick `t` with `getSqrtRatioAtTick(t) <= sqrtPriceX96` (`TickMath.getTickAtSqrtRatio`). */
export function getTickAtSqrtRatio(sqrtPriceX96: bigint): number {
  if (sqrtPriceX96 < MIN_SQRT_RATIO || sqrtPriceX96 >= MAX_SQRT_RATIO) {
    throw new RangeError(`sqrtPriceX96 ${sqrtPriceX96} is outside [MIN_SQRT_RATIO, MAX_SQRT_RATIO)`);
  }
  const ratio = sqrtPriceX96 << 32n;
  const msb = bitLength(ratio) - 1;
  let r = msb >= 128 ? ratio >> BigInt(msb - 127) : ratio << BigInt(127 - msb);

  let log2 = BigInt(msb - 128) << 64n;
  for (let shift = 63; shift >= 50; shift--) {
    r = (r * r) >> 127n;
    const f = (r >> 128n) === 0n ? 0n : 1n;
    log2 |= f << BigInt(shift);
    r >>= f;
  }

  const logSqrt10001 = log2 * 255738958999603826347141n;
  const tickLow = Number((logSqrt10001 - 3402992956809132418596140100660247210n) >> 128n);
  const tickHi = Number((logSqrt10001 + 291339464771989622907027621153398088495n) >> 128n);

  if (tickLow === tickHi) return tickLow;
  return getSqrtRatioAtTick(tickHi) <= sqrtPriceX96 ? tickHi : tickLow;
}

function sortAsc(a: bigint, b: bigint): [bigint, bigint] {
  return a > b ? [b, a] : [a, b];
}

function divRoundingUp(x: bigint, d: bigint): bigint {
  return (x + d - 1n) / d;
}

/** `SqrtPriceMath.getAmount0Delta` for a positive liquidity, rounding up or down. */
export function amount0Delta(sqrtA: bigint, sqrtB: bigint, liquidity: bigint, roundUp: boolean): bigint {
  const [a, b] = sortAsc(sqrtA, sqrtB);
  if (a === 0n) throw new RangeError("amount0Delta: sqrt price is zero");
  const numerator1 = liquidity << 96n;
  const numerator2 = b - a;
  if (roundUp) {
    return divRoundingUp(divRoundingUp(numerator1 * numerator2, b), a);
  }
  return (numerator1 * numerator2) / b / a;
}

/** `SqrtPriceMath.getAmount1Delta` for a positive liquidity, rounding up or down. */
export function amount1Delta(sqrtA: bigint, sqrtB: bigint, liquidity: bigint, roundUp: boolean): bigint {
  const [a, b] = sortAsc(sqrtA, sqrtB);
  const diff = b - a;
  return roundUp ? divRoundingUp(liquidity * diff, Q96) : (liquidity * diff) / Q96;
}

/**
 * Tokens a position of `liquidity` in `[tickLower, tickUpper)` holds at the pool's current state
 * (`slot0.tick`, `slot0.sqrtPriceX96`). Same branches as `ClPool::modify_position`.
 */
export function positionAmounts(
  liquidity: bigint,
  tickLower: number,
  tickUpper: number,
  currentTick: number,
  sqrtPriceX96: bigint,
  roundUp: boolean,
): { amount0: bigint; amount1: bigint } {
  if (liquidity === 0n) return { amount0: 0n, amount1: 0n };
  const sqrtLower = getSqrtRatioAtTick(tickLower);
  const sqrtUpper = getSqrtRatioAtTick(tickUpper);
  if (currentTick < tickLower) {
    return { amount0: amount0Delta(sqrtLower, sqrtUpper, liquidity, roundUp), amount1: 0n };
  }
  if (currentTick < tickUpper) {
    return {
      amount0: amount0Delta(sqrtPriceX96, sqrtUpper, liquidity, roundUp),
      amount1: amount1Delta(sqrtLower, sqrtPriceX96, liquidity, roundUp),
    };
  }
  return { amount0: 0n, amount1: amount1Delta(sqrtLower, sqrtUpper, liquidity, roundUp) };
}
