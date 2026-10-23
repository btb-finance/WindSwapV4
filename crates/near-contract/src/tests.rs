//! Unit tests run on the host with near-sdk's mocked blockchain.

use super::*;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::testing_env;

const OWNER: &str = "owner.testnet";
const ALICE: &str = "alice.testnet";
const BOB: &str = "bob.testnet";
const USDC: &str = "usdc.fakes.testnet";
const WNEAR: &str = "wnear.fakes.testnet";
/// Tick spacing 3 is enabled by default in `FeeConfig` (fee 0.03%).
const TICK_SPACING: i32 = 3;
/// `sqrt(1.0) * 2^96`, i.e. tick 0.
const Q96_STR: &str = "79228162514264337593543950336";
/// Engine `MIN_SQRT_RATIO + 1` and `MAX_SQRT_RATIO - 1`: the widest valid price limits.
const MIN_LIMIT: &str = "4295128740";
const MAX_LIMIT: &str = "1461446703485210103287273052203988822378723970341";
const FUNDS: u128 = 1_000_000_000_000_000_000_000_000; // 1e24

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}

/// Sets the caller and block time. `testing_env!` keeps the contract storage between calls.
fn set_ctx(predecessor: &str, block_secs: u64) {
    let mut builder = VMContextBuilder::new();
    builder
        .predecessor_account_id(acct(predecessor))
        .block_timestamp(block_secs * 1_000_000_000);
    testing_env!(builder.build());
}

fn setup() -> Contract {
    set_ctx(OWNER, 1_000);
    Contract::new(acct(OWNER))
}

fn balance(c: &Contract, who: &str, token: &str) -> u128 {
    c.get_balance(acct(who), token.to_string()).0
}

fn num(s: &str) -> u128 {
    s.parse().unwrap()
}

fn signed(s: &str) -> I256 {
    parse_i256(s)
}

#[test]
fn create_pool_initializes_state() {
    let mut c = setup();
    let id = c.create_pool(USDC.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    assert_eq!(id, 0);
    assert_eq!(
        c.create_pool(USDC.into(), WNEAR.into(), 5, Q96_STR.into()),
        1
    );

    let view = c.get_pool(id);
    assert!(view.initialized);
    assert_eq!(view.token0, USDC);
    assert_eq!(view.token1, WNEAR);
    assert_eq!(view.tick_spacing, TICK_SPACING);
    assert_eq!(view.slot0.sqrt_price_x96, Q96_STR);
    assert_eq!(view.slot0.tick, 0);
    assert_eq!(view.liquidity.0, 0);
}

#[test]
#[should_panic(expected = "only owner")]
fn create_pool_rejects_non_owner() {
    let mut c = setup();
    set_ctx(ALICE, 1_000);
    c.create_pool(USDC.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
}

#[test]
#[should_panic(expected = "tick spacing 7 is not enabled")]
fn create_pool_rejects_disabled_tick_spacing() {
    let mut c = setup();
    c.create_pool(USDC.into(), WNEAR.into(), 7, Q96_STR.into());
}

#[test]
#[should_panic(expected = "token0 and token1 must differ")]
fn create_pool_rejects_identical_tokens() {
    let mut c = setup();
    c.create_pool(USDC.into(), USDC.into(), TICK_SPACING, Q96_STR.into());
}

#[test]
#[should_panic(expected = "only owner")]
fn test_credit_rejects_non_owner() {
    let mut c = setup();
    set_ctx(ALICE, 1_000);
    c.test_credit(acct(ALICE), USDC.into(), U128(1));
}

#[test]
fn test_credit_adds_balance() {
    let mut c = setup();
    c.test_credit(acct(ALICE), USDC.into(), U128(5));
    c.test_credit(acct(ALICE), USDC.into(), U128(7));
    assert_eq!(balance(&c, ALICE, USDC), 12);
    assert_eq!(balance(&c, BOB, USDC), 0);
}

#[test]
#[should_panic(expected = "insufficient")]
fn mint_panics_without_balance() {
    let mut c = setup();
    let pool = c.create_pool(USDC.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    set_ctx(BOB, 1_010);
    c.mint_position(pool, -600, 600, U128(10u128.pow(18)));
}

#[test]
#[should_panic(expected = "ZeroAmount")]
fn engine_error_becomes_panic() {
    let mut c = setup();
    let pool = c.create_pool(USDC.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    c.test_credit(acct(ALICE), USDC.into(), U128(FUNDS));
    c.test_credit(acct(ALICE), WNEAR.into(), U128(FUNDS));
    set_ctx(ALICE, 1_010);
    c.mint_position(pool, -600, 600, U128(0));
}

#[test]
fn mint_swap_burn_collect_moves_balances() {
    let mut c = setup();
    let pool = c.create_pool(USDC.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    c.test_credit(acct(ALICE), USDC.into(), U128(FUNDS));
    c.test_credit(acct(ALICE), WNEAR.into(), U128(FUNDS));

    // Mint a position around the current price.
    set_ctx(ALICE, 1_010);
    let liquidity = 10u128.pow(18);
    let [m0, m1] = c.mint_position(pool, -600, 600, U128(liquidity));
    let (m0, m1) = (num(&m0), num(&m1));
    assert!(m0 > 0 && m1 > 0, "in-range mint needs both tokens");
    assert_eq!(balance(&c, ALICE, USDC), FUNDS - m0);
    assert_eq!(balance(&c, ALICE, WNEAR), FUNDS - m1);
    assert_eq!(
        c.get_position(pool, acct(ALICE), -600, 600).liquidity.0,
        liquidity
    );
    assert_eq!(c.get_pool(pool).liquidity.0, liquidity);

    // Swap token0 -> token1 with exact input.
    let usdc_before = balance(&c, ALICE, USDC);
    let wnear_before = balance(&c, ALICE, WNEAR);
    let input = 10u128.pow(15);
    let [s0, s1] = c.swap(pool, true, input.to_string(), MIN_LIMIT.into());
    assert_eq!(num(&s0), input, "exact input is fully consumed");
    assert!(signed(&s1).is_negative(), "output is paid to the trader");
    let out = signed(&s1).unsigned_abs().to::<u128>();
    assert_eq!(balance(&c, ALICE, USDC), usdc_before - input);
    assert_eq!(balance(&c, ALICE, WNEAR), wnear_before + out);
    assert!(c.get_pool(pool).slot0.tick < 0, "price moved down");

    // Swap token1 -> token0 with exact input (the other direction).
    let usdc_before = balance(&c, ALICE, USDC);
    let wnear_before = balance(&c, ALICE, WNEAR);
    let input = 10u128.pow(14);
    let [s0, s1] = c.swap(pool, false, input.to_string(), MAX_LIMIT.into());
    assert!(signed(&s0).is_negative(), "output is paid to the trader");
    assert_eq!(num(&s1), input, "exact input is fully consumed");
    let out = signed(&s0).unsigned_abs().to::<u128>();
    assert_eq!(balance(&c, ALICE, USDC), usdc_before + out);
    assert_eq!(balance(&c, ALICE, WNEAR), wnear_before - input);

    // Burn: the withdrawn amounts become owed to the position, balances do not move yet.
    let usdc_before_burn = balance(&c, ALICE, USDC);
    let wnear_before_burn = balance(&c, ALICE, WNEAR);
    let [b0, b1] = c.burn_position(pool, -600, 600, U128(liquidity));
    assert!(num(&b0) > 0 && num(&b1) > 0);
    assert_eq!(balance(&c, ALICE, USDC), usdc_before_burn);
    assert_eq!(balance(&c, ALICE, WNEAR), wnear_before_burn);
    let position = c.get_position(pool, acct(ALICE), -600, 600);
    assert_eq!(position.liquidity.0, 0);
    assert!(
        position.tokens_owed0.0 >= num(&b0),
        "owed includes the principal"
    );
    assert!(position.tokens_owed1.0 >= num(&b1));

    // Collect pays out exactly what the position reports as owed.
    let [c0, c1] = c.collect(pool, -600, 600);
    assert_eq!(c0, position.tokens_owed0);
    assert_eq!(c1, position.tokens_owed1);
    assert_eq!(balance(&c, ALICE, USDC), usdc_before_burn + c0.0);
    assert_eq!(balance(&c, ALICE, WNEAR), wnear_before_burn + c1.0);
    assert_eq!(c.get_pool(pool).liquidity.0, 0);

    // Nothing left to collect.
    let [z0, z1] = c.collect(pool, -600, 600);
    assert_eq!((z0.0, z1.0), (0, 0));
}

#[test]
#[should_panic(expected = "insufficient")]
fn swap_panics_without_input_balance() {
    let mut c = setup();
    let pool = c.create_pool(USDC.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    c.test_credit(acct(ALICE), USDC.into(), U128(FUNDS));
    c.test_credit(acct(ALICE), WNEAR.into(), U128(FUNDS));
    set_ctx(ALICE, 1_010);
    c.mint_position(pool, -600, 600, U128(10u128.pow(18)));

    // Bob has no balance at all, so the exact-input debit must fail.
    set_ctx(BOB, 1_020);
    c.swap(pool, true, 1_000u128.to_string(), MIN_LIMIT.into());
}

#[test]
fn balances_are_per_account_and_per_token() {
    let mut c = setup();
    c.test_credit(acct(ALICE), USDC.into(), U128(3));
    assert_eq!(balance(&c, ALICE, WNEAR), 0);
    assert_eq!(balance(&c, BOB, USDC), 0);
    assert_eq!(balance(&c, ALICE, USDC), 3);
}
