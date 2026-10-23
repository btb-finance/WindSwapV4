//! Unit tests run on the host with near-sdk's mocked blockchain.

use super::*;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, PromiseResult};
use windswap_engine::error::MathError;
use windswap_engine::fees::ZERO_FEE_INDICATOR;

const OWNER: &str = "owner.testnet";
const ALICE: &str = "alice.testnet";
const BOB: &str = "bob.testnet";
const USDC: &str = "usdc.fakes.testnet";
const WNEAR: &str = "wnear.fakes.testnet";
/// A token contract that is never whitelisted.
const JUNK: &str = "junk.fakes.testnet";
/// Tick spacing 3 is enabled by default in `FeeConfig` (fee 0.03%).
const TICK_SPACING: i32 = 3;
/// `sqrt(1.0) * 2^96`, i.e. tick 0.
const Q96_STR: &str = "79228162514264337593543950336";
/// Engine `MIN_SQRT_RATIO + 1` and `MAX_SQRT_RATIO - 1`: the widest valid price limits.
const MIN_LIMIT: &str = "4295128740";
const MAX_LIMIT: &str = "1461446703485210103287273052203988822378723970341";
const FUNDS: u128 = 1_000_000_000_000_000_000_000_000; // 1e24
/// USDT: the third token, used by the two-pool routes (USDC -> WNEAR -> USDT).
const USDT: &str = "usdt.fakes.testnet";
/// Liquidity minted in each route test pool, around tick 0.
const LIQUIDITY: u128 = 1_000_000_000_000_000_000; // 1e18
/// Input the trader gets in each route test.
const TRADER_FUNDS: u128 = 1_000_000_000_000_000_000; // 1e18
const AMOUNT_IN: u128 = 1_000_000_000_000_000; // 1e15
const AMOUNT_OUT: u128 = 100_000_000_000_000; // 1e14

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}

/// Sets the caller, block time, attached deposit and promise results.
/// `testing_env!` keeps the contract storage between calls.
fn set_ctx_with(predecessor: &str, block_secs: u64, deposit: u128, results: Vec<PromiseResult>) {
    let mut builder = VMContextBuilder::new();
    builder
        .predecessor_account_id(acct(predecessor))
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .block_timestamp(block_secs * 1_000_000_000);
    testing_env!(
        builder.build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        results
    );
}

fn set_ctx(predecessor: &str, block_secs: u64) {
    set_ctx_with(predecessor, block_secs, 0, vec![]);
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

/// Credits `amount` of `token` to `who` through the real deposit path: the owner whitelists
/// the token (if needed), then the token contract calls `ft_on_transfer`.
fn fund(c: &mut Contract, who: &str, token: &str, amount: u128) {
    set_ctx(OWNER, 1_000);
    if !c.get_tokens().contains(&acct(token)) {
        c.add_token(acct(token));
    }
    set_ctx(token, 1_000);
    match c.ft_on_transfer(acct(who), U128(amount), String::new()) {
        PromiseOrValue::Value(U128(0)) => {}
        _ => panic!("whitelisted deposit must not be refunded"),
    }
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
#[should_panic(expected = "invalid token id")]
fn create_pool_rejects_invalid_token_id() {
    let mut c = setup();
    c.create_pool(
        "Not A Valid Id".into(),
        WNEAR.into(),
        TICK_SPACING,
        Q96_STR.into(),
    );
}

#[cfg(feature = "testnet-faucet")]
#[test]
#[should_panic(expected = "only owner")]
fn test_credit_rejects_non_owner() {
    let mut c = setup();
    set_ctx(ALICE, 1_000);
    c.test_credit(acct(ALICE), USDC.into(), U128(1));
}

#[cfg(feature = "testnet-faucet")]
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
    fund(&mut c, ALICE, USDC, FUNDS);
    fund(&mut c, ALICE, WNEAR, FUNDS);
    set_ctx(ALICE, 1_010);
    c.mint_position(pool, -600, 600, U128(0));
}

#[test]
fn mint_swap_burn_collect_moves_balances() {
    let mut c = setup();
    let pool = c.create_pool(USDC.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    fund(&mut c, ALICE, USDC, FUNDS);
    fund(&mut c, ALICE, WNEAR, FUNDS);

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
    fund(&mut c, ALICE, USDC, FUNDS);
    fund(&mut c, ALICE, WNEAR, FUNDS);
    set_ctx(ALICE, 1_010);
    c.mint_position(pool, -600, 600, U128(10u128.pow(18)));

    // Bob has no balance at all, so the exact-input debit must fail.
    set_ctx(BOB, 1_020);
    c.swap(pool, true, 1_000u128.to_string(), MIN_LIMIT.into());
}

#[test]
fn balances_are_per_account_and_per_token() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 3);
    assert_eq!(balance(&c, ALICE, WNEAR), 0);
    assert_eq!(balance(&c, BOB, USDC), 0);
    assert_eq!(balance(&c, ALICE, USDC), 3);
}

// ---- Deposits (NEP-141 receiver) and the token whitelist ----

#[test]
fn ft_on_transfer_credits_whitelisted_token() {
    let mut c = setup();
    set_ctx(OWNER, 1_000);
    c.add_token(acct(USDC));
    set_ctx(USDC, 1_000);
    let unused = c.ft_on_transfer(acct(ALICE), U128(5), String::new());
    assert!(matches!(unused, PromiseOrValue::Value(U128(0))));
    assert_eq!(balance(&c, ALICE, USDC), 5);
    assert_eq!(balance(&c, ALICE, WNEAR), 0);
    assert_eq!(balance(&c, BOB, USDC), 0);
}

#[test]
fn ft_on_transfer_ignores_msg_for_now() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 0);
    set_ctx(USDC, 1_000);
    let unused = c.ft_on_transfer(acct(ALICE), U128(9), "{\"swap\":true}".into());
    assert!(matches!(unused, PromiseOrValue::Value(U128(0))));
    assert_eq!(balance(&c, ALICE, USDC), 9);
}

#[test]
fn ft_on_transfer_refunds_non_whitelisted_token() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 0);
    set_ctx(JUNK, 1_000);
    let unused = c.ft_on_transfer(acct(ALICE), U128(77), String::new());
    match unused {
        PromiseOrValue::Value(U128(refund)) => assert_eq!(refund, 77),
        _ => panic!("non-whitelisted deposit must be refunded"),
    }
    assert_eq!(balance(&c, ALICE, JUNK), 0);
}

#[test]
fn removed_token_is_refunded_on_deposit() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 10);
    set_ctx(OWNER, 1_000);
    c.remove_token(acct(USDC));
    set_ctx(USDC, 1_000);
    let unused = c.ft_on_transfer(acct(ALICE), U128(4), String::new());
    assert!(matches!(unused, PromiseOrValue::Value(U128(4))));
    // The existing balance is untouched by the refund.
    assert_eq!(balance(&c, ALICE, USDC), 10);
}

#[test]
fn add_and_remove_token_updates_whitelist() {
    let mut c = setup();
    assert!(c.get_tokens().is_empty());
    set_ctx(OWNER, 1_000);
    c.add_token(acct(USDC));
    c.add_token(acct(WNEAR));
    assert_eq!(c.get_tokens(), vec![acct(USDC), acct(WNEAR)]);
    c.remove_token(acct(USDC));
    assert_eq!(c.get_tokens(), vec![acct(WNEAR)]);
}

#[test]
#[should_panic(expected = "only owner")]
fn add_token_rejects_non_owner() {
    let mut c = setup();
    set_ctx(ALICE, 1_000);
    c.add_token(acct(USDC));
}

#[test]
#[should_panic(expected = "only owner")]
fn remove_token_rejects_non_owner() {
    let mut c = setup();
    set_ctx(OWNER, 1_000);
    c.add_token(acct(USDC));
    set_ctx(ALICE, 1_000);
    c.remove_token(acct(USDC));
}

#[test]
#[should_panic(expected = "token already allowed")]
fn add_token_rejects_duplicate() {
    let mut c = setup();
    c.add_token(acct(USDC));
    c.add_token(acct(USDC));
}

#[test]
#[should_panic(expected = "token not allowed")]
fn remove_token_rejects_unknown() {
    let mut c = setup();
    c.remove_token(acct(USDC));
}

// ---- Withdrawals ----

#[test]
#[should_panic(expected = "Requires attached deposit of exactly 1 yoctoNEAR")]
fn withdraw_requires_one_yocto() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 100);
    set_ctx(ALICE, 1_000);
    let _ = c.withdraw(acct(USDC), U128(10));
}

#[test]
#[should_panic(expected = "Requires attached deposit of exactly 1 yoctoNEAR")]
fn withdraw_rejects_two_yocto() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 100);
    set_ctx_with(ALICE, 1_000, 2, vec![]);
    let _ = c.withdraw(acct(USDC), U128(10));
}

#[test]
fn withdraw_debits_balance_before_transfer() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 100);
    set_ctx_with(ALICE, 1_000, 1, vec![]);
    let _promise = c.withdraw(acct(USDC), U128(40));
    assert_eq!(balance(&c, ALICE, USDC), 60);
    assert_eq!(balance(&c, BOB, USDC), 0);
}

#[test]
fn withdraw_full_balance_clears_entry() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 100);
    set_ctx_with(ALICE, 1_000, 1, vec![]);
    let _promise = c.withdraw(acct(USDC), U128(100));
    assert_eq!(balance(&c, ALICE, USDC), 0);
}

#[test]
#[should_panic(expected = "insufficient")]
fn withdraw_panics_on_insufficient_balance() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 100);
    set_ctx_with(ALICE, 1_000, 1, vec![]);
    let _ = c.withdraw(acct(USDC), U128(101));
}

#[test]
#[should_panic(expected = "amount must be positive")]
fn withdraw_rejects_zero_amount() {
    let mut c = setup();
    set_ctx_with(ALICE, 1_000, 1, vec![]);
    let _ = c.withdraw(acct(USDC), U128(0));
}

#[test]
fn on_withdraw_failure_recredits_balance() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 100);
    set_ctx_with(ALICE, 1_000, 1, vec![]);
    let _promise = c.withdraw(acct(USDC), U128(40));
    assert_eq!(balance(&c, ALICE, USDC), 60);

    // The ft_transfer failed, so the callback sees a failed promise and restores the debit.
    set_ctx_with(ALICE, 1_000, 0, vec![PromiseResult::Failed]);
    c.on_withdraw(acct(ALICE), acct(USDC), U128(40));
    assert_eq!(balance(&c, ALICE, USDC), 100);
}

#[test]
fn on_withdraw_success_keeps_debit() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 100);
    set_ctx_with(ALICE, 1_000, 1, vec![]);
    let _promise = c.withdraw(acct(USDC), U128(40));

    set_ctx_with(ALICE, 1_000, 0, vec![PromiseResult::Successful(vec![])]);
    c.on_withdraw(acct(ALICE), acct(USDC), U128(40));
    assert_eq!(balance(&c, ALICE, USDC), 60);
}

#[test]
fn withdraw_works_after_token_is_removed() {
    let mut c = setup();
    fund(&mut c, ALICE, USDC, 100);
    set_ctx(OWNER, 1_000);
    c.remove_token(acct(USDC));
    set_ctx_with(ALICE, 1_000, 1, vec![]);
    let _promise = c.withdraw(acct(USDC), U128(100));
    assert_eq!(balance(&c, ALICE, USDC), 0);
}

// ---- Upgrade from the v0 layout ----

#[test]
fn migrate_reads_v0_state() {
    set_ctx(OWNER, 1_000);
    {
        // Write a state in the old layout, using the same storage prefixes as v0.
        let mut balances = LookupMap::new(StorageKey::Balances);
        balances.insert((acct(ALICE), USDC.to_string()), 42u128);
        let old = ContractV0 {
            owner: acct(OWNER),
            fee_config: FeeConfig::default(),
            pools: IterableMap::new(StorageKey::Pools),
            pool_tokens: LookupMap::new(StorageKey::PoolTokens),
            next_pool_id: 3,
            balances,
        };
        env::state_write(&old);
    }

    let mut c = Contract::migrate();
    assert_eq!(c.owner, acct(OWNER));
    assert_eq!(c.next_pool_id, 3);
    assert_eq!(balance(&c, ALICE, USDC), 42);
    assert!(c.get_tokens().is_empty(), "whitelist starts empty");

    // The migrated contract works: the owner can whitelist and deposits are credited.
    c.add_token(acct(USDC));
    set_ctx(USDC, 1_000);
    let _ = c.ft_on_transfer(acct(ALICE), U128(8), String::new());
    assert_eq!(balance(&c, ALICE, USDC), 50);
}

// ---- Multi-hop routes, quotes and fee admin ----

/// Pool 0 is USDC/WNEAR and pool 1 is WNEAR/USDT, both at price 1 with liquidity from ALICE.
/// BOB holds `TRADER_FUNDS` of USDC. Block time is 1020 for everything after this.
fn two_pools() -> (Contract, u64, u64) {
    let mut c = setup();
    let p0 = c.create_pool(USDC.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    let p1 = c.create_pool(WNEAR.into(), USDT.into(), TICK_SPACING, Q96_STR.into());
    for token in [USDC, WNEAR, USDT] {
        fund(&mut c, ALICE, token, FUNDS);
    }
    set_ctx(ALICE, 1_010);
    c.mint_position(p0, -600, 600, U128(LIQUIDITY));
    c.mint_position(p1, -600, 600, U128(LIQUIDITY));
    fund(&mut c, BOB, USDC, TRADER_FUNDS);
    set_ctx(BOB, 1_020);
    (c, p0, p1)
}

fn hop_in(pool_id: u64, token_in: &str) -> HopInput {
    HopInput {
        pool_id,
        token_in: acct(token_in),
    }
}

/// USDC -> WNEAR through `p0`, then WNEAR -> USDT through `p1`.
fn two_hop_path(p0: u64, p1: u64) -> Vec<HopInput> {
    vec![hop_in(p0, USDC), hop_in(p1, WNEAR)]
}

fn pool_state(c: &Contract, pool_id: u64) -> ClPool {
    c.pool_ref(pool_id).clone()
}

#[test]
fn two_hop_exact_input_matches_quote() {
    let (mut c, p0, p1) = two_pools();
    let path = two_hop_path(p0, p1);
    let quoted = c.quote_exact_input(path.clone(), U128(AMOUNT_IN)).0;
    assert!(quoted > 0);

    let usdc_before = balance(&c, BOB, USDC);
    let out = c.swap_exact_input(path, U128(AMOUNT_IN), U128(0)).0;
    assert_eq!(out, quoted);
    assert_eq!(balance(&c, BOB, USDC), usdc_before - AMOUNT_IN);
    assert_eq!(balance(&c, BOB, USDT), out);
    // The intermediate token never reaches the trader's balance.
    assert_eq!(balance(&c, BOB, WNEAR), 0);
    // Both pools were written back: the price moved down in each.
    assert!(c.get_pool(p0).slot0.tick < 0);
    assert!(c.get_pool(p1).slot0.tick < 0);
}

#[test]
fn quotes_do_not_change_state() {
    let (c, p0, p1) = two_pools();
    let path = two_hop_path(p0, p1);
    let pool0_before = pool_state(&c, p0);
    let pool1_before = pool_state(&c, p1);
    let bob_before = (balance(&c, BOB, USDC), balance(&c, BOB, USDT));

    let q_in = c.quote_exact_input(path.clone(), U128(AMOUNT_IN)).0;
    let q_out = c.quote_exact_output(path.clone(), U128(AMOUNT_OUT)).0;
    assert!(q_in > 0 && q_out > 0);
    // Quoting is deterministic.
    assert_eq!(c.quote_exact_input(path, U128(AMOUNT_IN)).0, q_in);

    assert_eq!(pool_state(&c, p0), pool0_before);
    assert_eq!(pool_state(&c, p1), pool1_before);
    assert_eq!((balance(&c, BOB, USDC), balance(&c, BOB, USDT)), bob_before);
}

#[test]
fn exact_output_delivers_exact_amount_within_max() {
    let (mut c, p0, p1) = two_pools();
    let path = two_hop_path(p0, p1);
    let quoted_in = c.quote_exact_output(path.clone(), U128(AMOUNT_OUT)).0;
    let max_in = quoted_in + 1_000_000;

    let usdc_before = balance(&c, BOB, USDC);
    let paid = c.swap_exact_output(path, U128(AMOUNT_OUT), U128(max_in)).0;
    assert_eq!(paid, quoted_in);
    assert!(paid <= max_in);
    assert_eq!(balance(&c, BOB, USDC), usdc_before - paid);
    assert_eq!(balance(&c, BOB, USDT), AMOUNT_OUT);
    assert_eq!(balance(&c, BOB, WNEAR), 0);
}

#[test]
#[should_panic(expected = "TooLittleReceived")]
fn exact_input_slippage_panics() {
    let (mut c, p0, p1) = two_pools();
    let path = two_hop_path(p0, p1);
    let quoted = c.quote_exact_input(path.clone(), U128(AMOUNT_IN)).0;
    c.swap_exact_input(path, U128(AMOUNT_IN), U128(quoted + 1));
}

#[test]
#[should_panic(expected = "TooMuchRequested")]
fn exact_output_above_max_panics() {
    let (mut c, p0, p1) = two_pools();
    let path = two_hop_path(p0, p1);
    let quoted_in = c.quote_exact_output(path.clone(), U128(AMOUNT_OUT)).0;
    c.swap_exact_output(path, U128(AMOUNT_OUT), U128(quoted_in - 1));
}

/// A slippage failure is an `Err` from the router, and `swap_exact_input` panics on it. In a
/// unit test a panic aborts the test, so this checks the same route through the internal
/// helper. The helper runs on working copies, and only a successful swap writes them back.
#[test]
fn failed_route_leaves_contract_state_unchanged() {
    let (c, p0, p1) = two_pools();
    let path = two_hop_path(p0, p1);
    let quoted = c.quote_exact_input(path.clone(), U128(AMOUNT_IN)).0;
    let pool0_before = pool_state(&c, p0);
    let pool1_before = pool_state(&c, p1);
    let bob_before = (balance(&c, BOB, USDC), balance(&c, BOB, USDT));

    let resolved = c.resolve_path(&path);
    let mut working = c.working_pools(&resolved.hops);
    let result = c.route_exact_input(
        &resolved,
        &mut working,
        engine_account(&acct(BOB)),
        AMOUNT_IN,
        quoted + 1,
    );
    assert_eq!(result, Err(MathError::TooLittleReceived));

    assert_eq!(pool_state(&c, p0), pool0_before);
    assert_eq!(pool_state(&c, p1), pool1_before);
    assert_eq!((balance(&c, BOB, USDC), balance(&c, BOB, USDT)), bob_before);
}

#[test]
#[should_panic(expected = "empty path")]
fn empty_path_panics() {
    let (mut c, _, _) = two_pools();
    c.swap_exact_input(vec![], U128(AMOUNT_IN), U128(0));
}

#[test]
#[should_panic(expected = "token not in pool")]
fn token_not_in_pool_panics() {
    let (mut c, p0, _) = two_pools();
    // USDT is not in pool 0.
    c.swap_exact_input(vec![hop_in(p0, USDT)], U128(AMOUNT_IN), U128(0));
}

#[test]
#[should_panic(expected = "discontinuous path at hop 1")]
fn discontinuous_path_panics() {
    let (mut c, p0, p1) = two_pools();
    // Hop 0 outputs WNEAR, but hop 1 takes USDT.
    let path = vec![hop_in(p0, USDC), hop_in(p1, USDT)];
    c.swap_exact_input(path, U128(AMOUNT_IN), U128(0));
}

#[test]
#[should_panic(expected = "pool 9 does not exist")]
fn unknown_pool_in_path_panics() {
    let (c, _, _) = two_pools();
    c.quote_exact_input(vec![hop_in(9, USDC)], U128(AMOUNT_IN));
}

#[test]
#[should_panic(expected = "only owner")]
fn enable_tick_spacing_rejects_non_owner() {
    let mut c = setup();
    set_ctx(ALICE, 1_000);
    c.enable_tick_spacing(7, 500);
}

#[test]
#[should_panic(expected = "only owner")]
fn set_custom_swap_fee_rejects_non_owner() {
    let (mut c, p0, _) = two_pools();
    set_ctx(ALICE, 1_020);
    c.set_custom_swap_fee(p0, 1_000);
}

#[test]
#[should_panic(expected = "only owner")]
fn set_custom_unstaked_fee_rejects_non_owner() {
    let (mut c, p0, _) = two_pools();
    set_ctx(ALICE, 1_020);
    c.set_custom_unstaked_fee(p0, 1_000);
}

#[test]
#[should_panic(expected = "only owner")]
fn set_fee_whitelist_rejects_non_owner() {
    let (mut c, _, _) = two_pools();
    set_ctx(ALICE, 1_020);
    c.set_fee_whitelist(acct(ALICE), true);
}

#[test]
fn enable_tick_spacing_is_usable_by_new_pools() {
    let mut c = setup();
    c.enable_tick_spacing(7, 500);
    let pool = c.create_pool(USDC.into(), WNEAR.into(), 7, Q96_STR.into());
    assert_eq!(c.get_fee(pool, acct(BOB)), 500);
}

#[test]
#[should_panic(expected = "InvalidInput")]
fn enable_tick_spacing_rejects_enabled_spacing() {
    let mut c = setup();
    c.enable_tick_spacing(TICK_SPACING, 500);
}

#[test]
#[should_panic(expected = "pool 9 does not exist")]
fn set_custom_swap_fee_rejects_unknown_pool() {
    let mut c = setup();
    c.set_custom_swap_fee(9, 1_000);
}

#[test]
fn get_fee_defaults_to_tick_spacing_fee() {
    let (c, p0, _) = two_pools();
    assert_eq!(c.get_fee(p0, acct(BOB)), 300);
}

#[test]
fn custom_swap_fee_changes_get_fee_and_swap_output() {
    let (mut c, p0, p1) = two_pools();
    let path = two_hop_path(p0, p1);
    let before = c.quote_exact_input(path.clone(), U128(AMOUNT_IN)).0;

    set_ctx(OWNER, 1_020);
    c.set_custom_swap_fee(p0, 1_000);
    assert_eq!(c.get_fee(p0, acct(BOB)), 1_000);
    assert_eq!(c.get_fee(p1, acct(BOB)), 300, "other pools keep their fee");

    let after = c.quote_exact_input(path.clone(), U128(AMOUNT_IN)).0;
    assert!(after < before, "a higher fee gives less output");

    set_ctx(BOB, 1_020);
    let out = c.swap_exact_input(path, U128(AMOUNT_IN), U128(0)).0;
    assert_eq!(out, after);
}

#[test]
fn whitelisted_trader_pays_zero_fee() {
    let (mut c, p0, p1) = two_pools();
    let path = two_hop_path(p0, p1);

    // Reference: 0% on both pools (the 420 marker). The views quote a non-whitelisted trader,
    // so this is the output a whitelisted trader should get.
    set_ctx(OWNER, 1_020);
    c.set_custom_swap_fee(p0, ZERO_FEE_INDICATOR);
    c.set_custom_swap_fee(p1, ZERO_FEE_INDICATOR);
    let zero_fee_out = c.quote_exact_input(path.clone(), U128(AMOUNT_IN)).0;

    c.set_custom_swap_fee(p0, 0);
    c.set_custom_swap_fee(p1, 0);
    let normal_out = c.quote_exact_input(path.clone(), U128(AMOUNT_IN)).0;
    assert!(normal_out < zero_fee_out);

    c.set_fee_whitelist(acct(BOB), true);
    assert_eq!(c.get_fee(p0, acct(BOB)), 0);
    assert_eq!(c.get_fee(p1, acct(BOB)), 0);
    assert_eq!(c.get_fee(p0, acct(ALICE)), 300, "others still pay");

    set_ctx(BOB, 1_020);
    let out = c.swap_exact_input(path, U128(AMOUNT_IN), U128(0)).0;
    assert_eq!(out, zero_fee_out);
}
