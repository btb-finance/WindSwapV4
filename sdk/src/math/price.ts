/**
 * Price conversions for a pool whose token0 has `decimals0` and token1 has `decimals1`.
 *
 * A human price is "token1 per token0" in whole-token units, e.g. 2000 means 1 token0 buys
 * 2000 token1. The raw price (base units) is `human * 10^(decimals1 - decimals0)`.
 * `sqrtPriceX96 = sqrt(rawPrice) * 2^96`.
 */
import { MAX_SQRT_RATIO, MAX_TICK, MIN_SQRT_RATIO, MIN_TICK, Q192 } from "./constants.js";
import { getSqrtRatioAtTick, getTickAtSqrtRatio } from "./tickMath.js";
import { assertInteger, isqrt } from "../util.js";

/** Exact rational value of a decimal string such as "1.5", "2000", or "1e-7". */
function parseDecimal(input: string): { num: bigint; den: bigint } {
  const m = /^(\d*)(?:\.(\d*))?(?:[eE]([+-]?\d+))?$/.exec(input.trim());
  if (!m || ((m[1] ?? "") === "" && (m[2] ?? "") === "")) {
    throw new TypeError(`not a decimal number: "${input}"`);
  }
  const digits = (m[1] ?? "") + (m[2] ?? "");
  const fracLength = (m[2] ?? "").length;
  const exponent = Number(m[3] ?? "0");
  const num = BigInt(digits === "" ? "0" : digits);
  const scale = exponent - fracLength; // value = num * 10^scale
  if (scale >= 0) return { num: num * 10n ** BigInt(scale), den: 1n };
  return { num, den: 10n ** BigInt(-scale) };
}

function positiveRational(price: number | string): { num: bigint; den: bigint } {
  const text = typeof price === "number" ? String(price) : price;
  if (typeof price === "number" && !(Number.isFinite(price) && price > 0)) {
    throw new RangeError(`price must be a positive finite number, got ${price}`);
  }
  const r = parseDecimal(text);
  if (r.num === 0n) throw new RangeError("price must be greater than zero");
  return r;
}

/**
 * Converts a human price (token1 per token0) to `sqrtPriceX96`, rounded down.
 * Throws when the result is outside the pool's valid sqrt price range.
 *
 * Pass the price as a decimal string to avoid float error, e.g. `"0.0005"`.
 */
export function priceToSqrtPriceX96(
  price: number | string,
  decimals0: number,
  decimals1: number,
): bigint {
  assertInteger(decimals0, "decimals0");
  assertInteger(decimals1, "decimals1");
  const { num, den } = positiveRational(price);
  // raw = human * 10^(decimals1 - decimals0)
  let n = num;
  let d = den;
  const e = decimals1 - decimals0;
  if (e >= 0) n *= 10n ** BigInt(e);
  else d *= 10n ** BigInt(-e);
  // floor(sqrt(raw) * 2^96) = isqrt(floor(raw * 2^192))
  const sqrtPriceX96 = isqrt((n * Q192) / d);
  if (sqrtPriceX96 < MIN_SQRT_RATIO || sqrtPriceX96 >= MAX_SQRT_RATIO) {
    throw new RangeError(`price ${price} is outside the representable range`);
  }
  return sqrtPriceX96;
}

const DECIMAL_DIGITS = 60n;

/** Human price (token1 per token0) of a `sqrtPriceX96`. Returns a double. */
export function sqrtPriceX96ToPrice(
  sqrtPriceX96: bigint | string,
  decimals0: number,
  decimals1: number,
): number {
  assertInteger(decimals0, "decimals0");
  assertInteger(decimals1, "decimals1");
  const s = typeof sqrtPriceX96 === "string" ? BigInt(sqrtPriceX96) : sqrtPriceX96;
  if (s <= 0n) throw new RangeError("sqrtPriceX96 must be positive");
  // human = s^2 / 2^192 * 10^(decimals0 - decimals1)
  let num = s * s;
  let den = Q192;
  const e = decimals0 - decimals1;
  if (e >= 0) num *= 10n ** BigInt(e);
  else den *= 10n ** BigInt(-e);
  const scaled = (num * 10n ** DECIMAL_DIGITS) / den;
  return Number(`${scaled}e-${DECIMAL_DIGITS}`);
}

/** Human price at the boundary of `tick` (token1 per token0). */
export function tickToPrice(tick: number, decimals0: number, decimals1: number): number {
  return sqrtPriceX96ToPrice(getSqrtRatioAtTick(tick), decimals0, decimals1);
}

/**
 * Greatest tick whose price is at or below `price`. Exact: goes through `sqrtPriceX96`.
 */
export function priceToTick(price: number | string, decimals0: number, decimals1: number): number {
  return getTickAtSqrtRatio(priceToSqrtPriceX96(price, decimals0, decimals1));
}

/**
 * Rounds `tick` to the nearest multiple of `tickSpacing` that stays inside the tick range.
 * Same rounding as Uniswap's `nearestUsableTick` (JS `Math.round`).
 */
export function nearestUsableTick(tick: number, tickSpacing: number): number {
  assertInteger(tick, "tick");
  assertInteger(tickSpacing, "tickSpacing");
  if (tickSpacing <= 0) throw new RangeError("tickSpacing must be positive");
  const rounded = Math.round(tick / tickSpacing) * tickSpacing;
  if (rounded < MIN_TICK) return rounded + tickSpacing;
  if (rounded > MAX_TICK) return rounded - tickSpacing;
  return rounded;
}
