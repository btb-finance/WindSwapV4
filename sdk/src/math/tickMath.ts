/**
 * Exact BigInt port of Uniswap v3 `TickMath` as implemented in the engine
 * (`crates/engine/src/math/tick_math.rs`). Verified against `diff-tests/vectors`.
 */
import { MAX_SQRT_RATIO, MAX_TICK, MIN_SQRT_RATIO, MIN_TICK, Q128, UINT256_MAX } from "./constants.js";
import { assertInteger, bitLength } from "../util.js";

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

/**
 * `sqrt(1.0001^tick) * 2^96`, rounded up to the next integer.
 * Throws a RangeError when `tick` is not an integer in `[MIN_TICK, MAX_TICK]`.
 */
export function getSqrtRatioAtTick(tick: number): bigint {
  assertInteger(tick, "tick");
  const absTick = Math.abs(tick);
  if (absTick > MAX_TICK) {
    throw new RangeError(`tick ${tick} is outside [${MIN_TICK}, ${MAX_TICK}]`);
  }

  let ratio = (absTick & 0x1) !== 0 ? 0xfffcb933bd6fad37aa2d162d1a594001n : Q128;
  for (let i = 0; i < TICK_FACTORS.length; i++) {
    if ((absTick & (0x2 << i)) !== 0) {
      ratio = (ratio * TICK_FACTORS[i]) >> 128n;
    }
  }
  if (tick > 0) {
    ratio = UINT256_MAX / ratio;
  }
  // Q128.128 -> Q64.96, rounding up.
  const roundUp = (ratio & 0xffffffffn) === 0n ? 0n : 1n;
  return (ratio >> 32n) + roundUp;
}

/**
 * The greatest tick `t` such that `getSqrtRatioAtTick(t) <= sqrtPriceX96`.
 * Throws a RangeError when `sqrtPriceX96` is outside `[MIN_SQRT_RATIO, MAX_SQRT_RATIO)`.
 */
export function getTickAtSqrtRatio(sqrtPriceX96: bigint): number {
  if (sqrtPriceX96 < MIN_SQRT_RATIO || sqrtPriceX96 >= MAX_SQRT_RATIO) {
    throw new RangeError(`sqrtPriceX96 ${sqrtPriceX96} is outside [MIN_SQRT_RATIO, MAX_SQRT_RATIO)`);
  }
  const ratio = sqrtPriceX96 << 32n;
  const msb = bitLength(ratio) - 1;
  let r = msb >= 128 ? ratio >> BigInt(msb - 127) : ratio << BigInt(127 - msb);

  // Integer part of log2 in Q64.64, then 14 bits of fractional precision.
  let log2 = BigInt(msb - 128) << 64n;
  for (let shift = 63; shift >= 50; shift--) {
    r = (r * r) >> 127n;
    const f = (r >> 128n) === 0n ? 0n : 1n;
    log2 |= f << BigInt(shift);
    r >>= f;
  }

  // 128.128 number: log base sqrt(1.0001) of the price.
  const logSqrt10001 = log2 * 255738958999603826347141n;
  const tickLow = Number((logSqrt10001 - 3402992956809132418596140100660247210n) >> 128n);
  const tickHi = Number((logSqrt10001 + 291339464771989622907027621153398088495n) >> 128n);

  if (tickLow === tickHi) return tickLow;
  return getSqrtRatioAtTick(tickHi) <= sqrtPriceX96 ? tickHi : tickLow;
}
