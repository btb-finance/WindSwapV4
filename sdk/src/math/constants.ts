/** Fixed-point constants used by the Wind Swap v4 pool math (Uniswap v3 compatible). */

/** 2^96: Q64.96 fixed point (sqrt prices are `sqrtPrice * 2^96`). */
export const Q96 = 1n << 96n;
/** 2^128. */
export const Q128 = 1n << 128n;
/** 2^192 = (2^96)^2: the scale of a squared sqrt price. */
export const Q192 = 1n << 192n;
export const UINT128_MAX = (1n << 128n) - 1n;
export const UINT160_MAX = (1n << 160n) - 1n;
export const UINT256_MAX = (1n << 256n) - 1n;

/** Smallest and largest tick the pool supports. */
export const MIN_TICK = -887272;
export const MAX_TICK = 887272;
/** `getSqrtRatioAtTick(MIN_TICK)`. */
export const MIN_SQRT_RATIO = 4295128739n;
/** `getSqrtRatioAtTick(MAX_TICK)`. Valid sqrt prices are `[MIN_SQRT_RATIO, MAX_SQRT_RATIO)`. */
export const MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342n;
