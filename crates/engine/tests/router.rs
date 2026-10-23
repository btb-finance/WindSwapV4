//! Integration tests for the multi-hop router and quoter in `windswap_engine::router`.
//!
//! Each expected value is computed by calling `ClPool::swap` directly on clones, so the router
//! is checked against the single-pool math it is built on.

use std::collections::BTreeMap;

use windswap_engine::error::MathError;
use windswap_engine::fees::{AccountId, FeeConfig, PoolId};
use windswap_engine::math::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};
use windswap_engine::math::Q96;
use windswap_engine::pool::cl_pool::{ClPool, SwapResult};
use windswap_engine::router::{quote_exact_input, quote_exact_output, Hop, Router};
use windswap_engine::{I256, U256};

const POOL_AB: PoolId = 1;
const POOL_BC: PoolId = 2;
const TICK_SPACING: i32 = 60;
const SWAP_FEE: u32 = 3_000;
const TIME: u32 = 1_000;
const OWNER: AccountId = [7u8; 32];
const TRADER: AccountId = [9u8; 32];
const LIQUIDITY: u128 = 1_000_000_000_000_000_000;

fn fees() -> FeeConfig {
    let mut fees = FeeConfig::default();
    fees.enable_tick_spacing(TICK_SPACING, SWAP_FEE).unwrap();
    fees
}

/// A pool at price 1 with one wide position, so swaps have liquidity on both sides.
fn pool() -> ClPool {
    let mut pool = ClPool::default();
    pool.initialize(TICK_SPACING, Q96, TIME).unwrap();
    pool.mint(OWNER, -6_000, 6_000, LIQUIDITY, TIME).unwrap();
    pool
}

fn store() -> BTreeMap<PoolId, ClPool> {
    BTreeMap::from([(POOL_AB, pool()), (POOL_BC, pool())])
}

fn amount(x: u128) -> U256 {
    U256::from(x)
}

fn signed(x: U256) -> I256 {
    I256::from_raw(x)
}

/// Trader's output for a zero-for-one or one-for-zero swap, from the pool's signed result.
fn output_of(zero_for_one: bool, result: SwapResult) -> U256 {
    let raw = if zero_for_one {
        result.amount1
    } else {
        result.amount0
    };
    (-raw).into_raw()
}

/// Trader's input for the same swap.
fn input_of(zero_for_one: bool, result: SwapResult) -> U256 {
    if zero_for_one {
        result.amount0.into_raw()
    } else {
        result.amount1.into_raw()
    }
}

/// Direct `ClPool::swap` with the same fee inputs the router uses for this store (no gauge).
fn direct(pool: &mut ClPool, zero_for_one: bool, amount_specified: I256) -> SwapResult {
    let limit = if zero_for_one {
        MIN_SQRT_RATIO + U256::from(1)
    } else {
        MAX_SQRT_RATIO - U256::from(1)
    };
    pool.swap(zero_for_one, amount_specified, limit, SWAP_FEE, 0, TIME)
        .unwrap()
}

fn router<'a>(
    store: &'a mut BTreeMap<PoolId, ClPool>,
    fees: &'a FeeConfig,
) -> Router<'a, BTreeMap<PoolId, ClPool>> {
    Router {
        store,
        fees,
        trader: TRADER,
        time: TIME,
    }
}

fn hop(pool_id: PoolId, zero_for_one: bool) -> Hop {
    Hop {
        pool_id,
        zero_for_one,
    }
}

#[test]
fn single_hop_exact_input_matches_direct_swap() {
    let fees = fees();
    let mut direct_pool = pool();
    let expected_out = output_of(
        true,
        direct(&mut direct_pool, true, signed(amount(1_000_000_000))),
    );

    let mut s = store();
    let before = s.clone();
    let out = router(&mut s, &fees)
        .exact_input(&[hop(POOL_AB, true)], amount(1_000_000_000), U256::ZERO)
        .unwrap();

    assert_eq!(out, expected_out);
    assert!(out > U256::ZERO);
    assert_ne!(s, before);
    assert_eq!(s[&POOL_AB], direct_pool);
}

#[test]
fn single_hop_exact_output_matches_direct_swap() {
    let fees = fees();
    let mut direct_pool = pool();
    let wanted = amount(1_000_000_000);
    let direct_result = direct(&mut direct_pool, false, -signed(wanted));
    let expected_in = input_of(false, direct_result);
    assert_eq!(output_of(false, direct_result), wanted);

    let mut s = store();
    let amount_in = router(&mut s, &fees)
        .exact_output(&[hop(POOL_AB, false)], wanted, U256::MAX)
        .unwrap();

    assert_eq!(amount_in, expected_in);
    assert_eq!(s[&POOL_AB], direct_pool);
}

#[test]
fn two_hop_exact_input_equals_chained_single_swaps() {
    let fees = fees();
    let amount_in = amount(1_000_000_000);

    let mut ab = pool();
    let mid = output_of(true, direct(&mut ab, true, signed(amount_in)));
    let mut bc = pool();
    let expected_out = output_of(true, direct(&mut bc, true, signed(mid)));

    let mut s = store();
    let out = router(&mut s, &fees)
        .exact_input(
            &[hop(POOL_AB, true), hop(POOL_BC, true)],
            amount_in,
            U256::ZERO,
        )
        .unwrap();

    assert_eq!(out, expected_out);
    assert_eq!(s[&POOL_AB], ab);
    assert_eq!(s[&POOL_BC], bc);
}

#[test]
fn two_hop_exact_output_delivers_requested_amount() {
    let fees = fees();
    let wanted = amount(1_000_000_000);

    // Reverse chain, as SwapRouter does: the last pool produces `wanted`, and its input is the
    // output the first pool must produce.
    let mut bc = pool();
    let bc_result = direct(&mut bc, true, -signed(wanted));
    assert_eq!(output_of(true, bc_result), wanted);
    let needed_mid = input_of(true, bc_result);

    let mut ab = pool();
    let ab_result = direct(&mut ab, true, -signed(needed_mid));
    assert_eq!(output_of(true, ab_result), needed_mid);
    let expected_in = input_of(true, ab_result);

    let mut s = store();
    let amount_in = router(&mut s, &fees)
        .exact_output(&[hop(POOL_AB, true), hop(POOL_BC, true)], wanted, U256::MAX)
        .unwrap();

    assert_eq!(amount_in, expected_in);
    assert_eq!(s[&POOL_AB], ab);
    assert_eq!(s[&POOL_BC], bc);
}

#[test]
fn slippage_on_exact_input_leaves_all_pools_unchanged() {
    let fees = fees();
    let mut s = store();
    let before = s.clone();

    // Require more output than the route can give. The first hop would succeed, so this
    // checks that its change is rolled back.
    let err = router(&mut s, &fees)
        .exact_input(
            &[hop(POOL_AB, true), hop(POOL_BC, true)],
            amount(1_000_000_000),
            U256::MAX,
        )
        .unwrap_err();

    assert_eq!(err, MathError::TooLittleReceived);
    assert_eq!(s, before);
}

#[test]
fn slippage_on_exact_output_leaves_all_pools_unchanged() {
    let fees = fees();
    let mut s = store();
    let before = s.clone();

    let err = router(&mut s, &fees)
        .exact_output(
            &[hop(POOL_AB, true), hop(POOL_BC, true)],
            amount(1_000_000_000),
            amount(1),
        )
        .unwrap_err();

    assert_eq!(err, MathError::TooMuchRequested);
    assert_eq!(s, before);
}

#[test]
fn quote_matches_execution_and_does_not_mutate() {
    let fees = fees();
    let path = [hop(POOL_AB, true), hop(POOL_BC, true)];
    let mut s = store();
    let before = s.clone();

    let quoted_out =
        quote_exact_input(&s, &fees, TRADER, TIME, &path, amount(1_000_000_000)).unwrap();
    assert_eq!(s, before, "quote_exact_input mutated the store");
    let quoted_in =
        quote_exact_output(&s, &fees, TRADER, TIME, &path, amount(500_000_000)).unwrap();
    assert_eq!(s, before, "quote_exact_output mutated the store");

    // Execution on the same starting state must match the quotes exactly.
    let executed_out = router(&mut s, &fees)
        .exact_input(&path, amount(1_000_000_000), U256::ZERO)
        .unwrap();
    assert_eq!(quoted_out, executed_out);

    let mut fresh = store();
    let executed_in = router(&mut fresh, &fees)
        .exact_output(&path, amount(500_000_000), U256::MAX)
        .unwrap();
    assert_eq!(quoted_in, executed_in);
}

#[test]
fn invalid_paths_are_rejected_without_changes() {
    let fees = fees();
    let mut s = store();
    let before = s.clone();

    let empty = router(&mut s, &fees)
        .exact_input(&[], amount(1_000), U256::ZERO)
        .unwrap_err();
    assert_eq!(empty, MathError::InvalidInput);

    let empty_out = router(&mut s, &fees)
        .exact_output(&[], amount(1_000), U256::MAX)
        .unwrap_err();
    assert_eq!(empty_out, MathError::InvalidInput);

    // Second hop refers to a pool that does not exist. The first pool must stay unchanged.
    let missing = router(&mut s, &fees)
        .exact_input(
            &[hop(POOL_AB, true), hop(99, true)],
            amount(1_000_000),
            U256::ZERO,
        )
        .unwrap_err();
    assert_eq!(missing, MathError::InvalidInput);

    let missing_out = router(&mut s, &fees)
        .exact_output(&[hop(99, true)], amount(1_000), U256::MAX)
        .unwrap_err();
    assert_eq!(missing_out, MathError::InvalidInput);
    assert_eq!(s, before);

    let quote =
        quote_exact_input(&s, &fees, TRADER, TIME, &[hop(99, false)], amount(1_000)).unwrap_err();
    assert_eq!(quote, MathError::InvalidInput);
    assert_eq!(s, before);
}
