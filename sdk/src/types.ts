/**
 * JSON shapes of the contract's views and arguments (`crates/near-contract/src/lib.rs`).
 * Values wider than 2^53 (U128, U256, I256) are decimal strings. Pool and tick ids are numbers.
 */

/** A decimal integer string. Used for U128, U256 and I256 values. */
export type DecimalString = string;

export interface Slot0View {
  sqrt_price_x96: DecimalString;
  tick: number;
  observation_index: number;
  observation_cardinality: number;
  observation_cardinality_next: number;
}

export interface PoolView {
  pool_id: number;
  token0: string;
  token1: string;
  initialized: boolean;
  tick_spacing: number;
  slot0: Slot0View;
  liquidity: DecimalString;
  staked_liquidity: DecimalString;
  fee_growth_global0_x128: DecimalString;
  fee_growth_global1_x128: DecimalString;
}

export interface PositionView {
  liquidity: DecimalString;
  fee_growth_inside0_last_x128: DecimalString;
  fee_growth_inside1_last_x128: DecimalString;
  tokens_owed0: DecimalString;
  tokens_owed1: DecimalString;
}

/**
 * One hop of a multi-hop route. `token_in` is the token paid into this hop. The output is the
 * other token of the pool. The next hop must take the previous hop's output as its `token_in`.
 */
export interface HopInput {
  pool_id: number;
  token_in: string;
}

/** `[amount0, amount1]` returned by `mint_position` and `burn_position`. */
export type AmountPair = [DecimalString, DecimalString];

/** `[amount0, amount1]` returned by `collect`. */
export type CollectedPair = [DecimalString, DecimalString];

/** `[delta0, delta1]` returned by `swap`. Signed: positive is paid by the caller, negative is paid to the caller. */
export type SwapDeltas = [DecimalString, DecimalString];
