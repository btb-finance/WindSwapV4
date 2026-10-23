/** Slippage helpers. Slippage is given in basis points (1 bp = 0.01%). */
import { MAX_SQRT_RATIO, MIN_SQRT_RATIO } from "./constants.js";
import { toBigInt } from "../util.js";

const BPS_DENOMINATOR = 10_000n;

function checkBps(bps: number | bigint | string): bigint {
  const value = toBigInt(bps, "slippageBps");
  if (value < 0n || value > BPS_DENOMINATOR) {
    throw new RangeError(`slippageBps must be between 0 and 10000, got ${value}`);
  }
  return value;
}

/**
 * Lowest acceptable output for an expected `amountOut`:
 * `floor(amountOut * (10000 - slippageBps) / 10000)`. Pass it as `min_amount_out`.
 */
export function minAmountOut(amountOut: bigint | string | number, slippageBps: number | bigint | string): bigint {
  const amount = toBigInt(amountOut, "amountOut");
  const bps = checkBps(slippageBps);
  return (amount * (BPS_DENOMINATOR - bps)) / BPS_DENOMINATOR;
}

/**
 * Highest acceptable input for an expected `amountIn`:
 * `ceil(amountIn * (10000 + slippageBps) / 10000)`. Pass it as `max_amount_in`.
 */
export function maxAmountIn(amountIn: bigint | string | number, slippageBps: number | bigint | string): bigint {
  const amount = toBigInt(amountIn, "amountIn");
  const bps = checkBps(slippageBps);
  const numerator = amount * (BPS_DENOMINATOR + bps);
  return (numerator + BPS_DENOMINATOR - 1n) / BPS_DENOMINATOR;
}

/**
 * A sqrt price limit that never stops a swap, for use as `sqrt_price_limit_x96`:
 * `MIN_SQRT_RATIO + 1` when `zeroForOne` (price moves down), else `MAX_SQRT_RATIO - 1`.
 */
export function defaultSqrtPriceLimit(zeroForOne: boolean): bigint {
  return zeroForOne ? MIN_SQRT_RATIO + 1n : MAX_SQRT_RATIO - 1n;
}
