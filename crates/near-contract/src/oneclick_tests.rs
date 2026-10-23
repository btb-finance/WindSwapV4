//! Contract-level tests for the one-step NEP-141 flows: the `swap` and `add_liquidity` messages of
//! `ft_on_transfer`. The intent tests share the helpers here.

use near_sdk::serde_json::json;
use near_sdk::test_utils::{get_created_receipts, VMContextBuilder};
use near_sdk::{testing_env, AccountId, PromiseResult};

use super::*;
use crate::accounts::{owner_hex, owner_of_account};

pub(crate) const OWNER: &str = "owner.testnet";
pub(crate) const SELF: &str = "windswapv4-btb.testnet";
pub(crate) const ALICE: &str = "alice.testnet";
pub(crate) const BOB: &str = "bob.testnet";
pub(crate) const CAROL: &str = "carol.testnet";
pub(crate) const RELAYER: &str = "relayer.testnet";
pub(crate) const USDC: &str = "usdc.fakes.testnet";
pub(crate) const WNEAR: &str = "wnear.fakes.testnet";
/// Internal ids of registered EVM assets (see `intents_tests`).
pub(crate) const ETH: &str = "base-sepolia:eth";
pub(crate) const USDC_EVM: &str = "base-sepolia:usdc";
pub(crate) const BASE_SEPOLIA: u64 = 84532;
pub(crate) const TICK_SPACING: i32 = 3;
/// Price 1 (sqrt price 2^96).
pub(crate) const Q96_STR: &str = "79228162514264337593543950336";
pub(crate) const NOW: u64 = 1_000;
pub(crate) const EXPIRES: u64 = 2_000;
pub(crate) const LP_FUNDS: u128 = 1_000_000_000_000_000_000_000_000_000; // 1e27
pub(crate) const LIQUIDITY: u128 = 1_000_000_000_000_000_000; // 1e18
pub(crate) const TO_EVM: &str = "0x2222222222222222222222222222222222222222";

pub(crate) fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}

/// Sets the caller, block time, deposit and promise results. The contract account is `SELF`.
pub(crate) fn ctx_with(
    predecessor: &str,
    block_secs: u64,
    deposit: u128,
    results: Vec<PromiseResult>,
) {
    let mut builder = VMContextBuilder::new();
    builder
        .current_account_id(acct(SELF))
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

pub(crate) fn ctx(predecessor: &str, block_secs: u64) {
    ctx_with(predecessor, block_secs, 0, vec![]);
}

pub(crate) fn setup() -> Contract {
    ctx(OWNER, NOW);
    Contract::new(acct(OWNER))
}

pub(crate) fn balance(c: &Contract, who: &str, token: &str) -> u128 {
    c.get_balance(acct(who), token.to_string()).0
}

pub(crate) fn hop(pool_id: u64, token_in: &str) -> HopInput {
    HopInput {
        pool_id,
        token_in: token_in.to_string(),
    }
}

/// Delivers `amount` of `token` from the token contract with `msg`, whitelisting it first if
/// needed. Returns the refund the contract reports.
pub(crate) fn deliver_msg(
    c: &mut Contract,
    token: &str,
    sender: &str,
    amount: u128,
    msg: &str,
) -> u128 {
    ctx(OWNER, NOW);
    if !c.get_tokens().contains(&acct(token)) {
        c.add_token(acct(token));
    }
    ctx(token, NOW);
    match c.ft_on_transfer(acct(sender), U128(amount), msg.to_string()) {
        PromiseOrValue::Value(refund) => refund.0,
        PromiseOrValue::Promise(_) => panic!("ft_on_transfer must not return a promise"),
    }
}

/// `{"swap": {...}}` for `path`, with `to` if given.
pub(crate) fn swap_msg(path: &[HopInput], min_out: u128, to: Option<&str>) -> String {
    let mut swap = json!({
        "path": path,
        "min_amount_out": min_out.to_string(),
    });
    if let Some(to) = to {
        swap["to"] = json!(to);
    }
    json!({ "swap": swap }).to_string()
}

/// `{"add_liquidity": {...}}`.
pub(crate) fn zap_msg(
    pool_id: u64,
    tick_lower: i32,
    tick_upper: i32,
    zap: bool,
    min_liquidity: Option<u128>,
    to: Option<&str>,
) -> String {
    let mut add = json!({
        "pool_id": pool_id,
        "tick_lower": tick_lower,
        "tick_upper": tick_upper,
        "zap": zap,
    });
    if let Some(min) = min_liquidity {
        add["min_liquidity"] = json!(min.to_string());
    }
    if let Some(to) = to {
        add["to"] = json!(to);
    }
    json!({ "add_liquidity": add }).to_string()
}

/// Creates a USDC/WNEAR pool at price 1, with ALICE's liquidity over `[-600, 600]`.
pub(crate) fn usdc_wnear_pool(c: &mut Contract) -> u64 {
    ctx(OWNER, NOW);
    let pool = c.create_pool(USDC.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    deliver_msg(c, USDC, ALICE, LP_FUNDS, "");
    deliver_msg(c, WNEAR, ALICE, LP_FUNDS, "");
    ctx(ALICE, NOW);
    c.mint_position(pool, -600, 600, U128(LIQUIDITY));
    pool
}

/// Number of `ft_transfer` receipts the call created.
pub(crate) fn ft_transfer_count() -> usize {
    use near_sdk::mock::MockAction;
    get_created_receipts()
        .iter()
        .flat_map(|r| r.actions.iter())
        .filter(|a| {
            matches!(
                a,
                MockAction::FunctionCallWeight { method_name, .. } if method_name == b"ft_transfer"
            )
        })
        .count()
}

/// Total owed of `token` equals what the contract holds for it: the deposits the test made, less
/// the tokens it sent, plus the faucet credit that no deposit paid for.
pub(crate) fn assert_owed(c: &Contract, token: &str, held: u128) {
    let acc = c.get_token_accounting(token.to_string());
    assert_eq!(
        acc.total_owed.0,
        held + acc.unbacked.0,
        "total owed of {token} must equal the held tokens plus unbacked credit"
    );
}

/// The JSON of a pool, to compare the whole state of a pool before and after a call.
pub(crate) fn pool_json(c: &Contract, pool_id: u64) -> String {
    near_sdk::serde_json::to_string(&c.get_pool(pool_id)).unwrap()
}

/// Everything a refused one-step must leave unchanged, as one comparable string.
pub(crate) fn snapshot(c: &Contract, pool_id: u64) -> String {
    let tokens = [USDC, WNEAR, ETH, USDC_EVM];
    let accounting: Vec<_> = tokens
        .iter()
        .map(|t| c.get_token_accounting(t.to_string()))
        .map(|a| {
            (
                a.liabilities.0,
                a.pool_reserves.0,
                a.pending_withdrawals.0,
                a.total_owed.0,
            )
        })
        .collect();
    format!(
        "{}|{:?}|{:?}|{}|{}|{}|{}",
        pool_json(c, pool_id),
        c.get_pool_reserves(pool_id),
        accounting,
        balance(c, BOB, USDC),
        balance(c, BOB, WNEAR),
        c.get_pending_withdrawals(0, 100).len(),
        c.get_balances_of_owner(owner_hex(&owner_of_account(&acct(BOB))))
            .len(),
    )
}

#[test]
fn swap_to_internal_credits_the_output() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let amount = 1_000_000u128;
    let path = vec![hop(pool, USDC)];
    let quote = c.quote_exact_input(path.clone(), U128(amount)).0;
    assert!(quote > 0);

    let refund = deliver_msg(
        &mut c,
        USDC,
        BOB,
        amount,
        &swap_msg(&path, 1, Some("internal")),
    );
    assert_eq!(refund, 0);
    assert_eq!(
        balance(&c, BOB, WNEAR),
        quote,
        "the output equals the quote"
    );
    assert_eq!(
        balance(&c, BOB, USDC),
        0,
        "the input never enters a balance"
    );
    // ALICE deposited LP_FUNDS of each token. BOB's input is held on top, and the output has
    // left the pool, so the WNEAR owed is what ALICE deposited, less nothing that was sent.
    assert_owed(&c, USDC, LP_FUNDS + amount);
    assert_owed(&c, WNEAR, LP_FUNDS);
}

#[test]
fn default_receiver_of_a_near_token_is_the_sender() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let amount = 1_000_000u128;
    let path = vec![hop(pool, USDC)];

    // No `to`: the output is a NEP-141 token, so it goes to the sender's NEAR account.
    let refund = deliver_msg(&mut c, USDC, BOB, amount, &swap_msg(&path, 1, None));
    assert_eq!(refund, 0);
    assert_eq!(balance(&c, BOB, WNEAR), 0, "sent out, not credited");
    assert_eq!(ft_transfer_count(), 1, "one ft_transfer to the sender");
}

#[test]
fn swap_to_near_pays_out_and_a_failed_payout_is_credited_back() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let amount = 1_000_000u128;
    let path = vec![hop(pool, USDC)];
    let quote = c.quote_exact_input(path.clone(), U128(amount)).0;

    let to = format!("near:{CAROL}");
    let refund = deliver_msg(&mut c, USDC, BOB, amount, &swap_msg(&path, 1, Some(&to)));
    assert_eq!(refund, 0);
    assert_eq!(balance(&c, BOB, WNEAR), 0, "the output is sent, not kept");
    assert_eq!(ft_transfer_count(), 1);

    // The transfer failed: the callback credits the beneficiary back.
    ctx_with(SELF, NOW, 0, vec![PromiseResult::Failed]);
    c.on_payout(
        owner_hex(&owner_of_account(&acct(BOB))),
        WNEAR.into(),
        U128(quote),
    );
    assert_eq!(balance(&c, BOB, WNEAR), quote);
}

#[test]
fn swap_to_evm_creates_a_pending_withdrawal() {
    let mut c = setup();
    let pool = usdc_eth_pool(&mut c);
    let amount = 1_000_000u128;
    let path = vec![hop(pool, USDC)];
    let quote = c.quote_exact_input(path.clone(), U128(amount)).0;

    let to = format!("evm:{TO_EVM}");
    let refund = deliver_msg(&mut c, USDC, BOB, amount, &swap_msg(&path, 1, Some(&to)));
    assert_eq!(refund, 0);
    let pending = c.get_pending_withdrawals(0, 10);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].token_id, ETH);
    assert_eq!(pending[0].amount.0, quote);
    assert_eq!(
        pending[0].owner_hex,
        owner_hex(&owner_of_account(&acct(BOB)))
    );
    assert_eq!(pending[0].to_address, TO_EVM);
}

/// Makes a pool of USDC (a NEP-141 token) and ETH (an EVM asset) with ALICE's liquidity. The
/// transferred token is USDC, so the one-step tests can pay out to an EVM address.
pub(crate) fn usdc_eth_pool(c: &mut Contract) -> u64 {
    ctx(OWNER, NOW);
    c.register_evm_asset(
        BASE_SEPOLIA,
        "native".into(),
        ETH.into(),
        18,
        U128(LP_FUNDS),
        U128(LP_FUNDS),
    );
    let pool = c.create_pool(USDC.into(), ETH.into(), TICK_SPACING, Q96_STR.into());
    deliver_msg(c, USDC, ALICE, LP_FUNDS, "");
    ctx(OWNER, NOW);
    c.test_credit(acct(ALICE), ETH.into(), U128(LP_FUNDS));
    ctx(ALICE, NOW);
    c.mint_position(pool, -600, 600, U128(LIQUIDITY));
    pool
}

/// Registers the two EVM assets and makes a pool of ETH and USDC_EVM with ALICE's liquidity.
pub(crate) fn evm_pool(c: &mut Contract) -> u64 {
    ctx(OWNER, NOW);
    c.register_evm_asset(
        BASE_SEPOLIA,
        "native".into(),
        ETH.into(),
        18,
        U128(LP_FUNDS),
        U128(LP_FUNDS),
    );
    c.register_evm_asset(
        BASE_SEPOLIA,
        "0x3333333333333333333333333333333333333333".into(),
        USDC_EVM.into(),
        6,
        U128(LP_FUNDS),
        U128(LP_FUNDS),
    );
    let pool = c.create_pool(ETH.into(), USDC_EVM.into(), TICK_SPACING, Q96_STR.into());
    ctx(OWNER, NOW);
    c.test_credit(acct(ALICE), ETH.into(), U128(LP_FUNDS));
    c.test_credit(acct(ALICE), USDC_EVM.into(), U128(LP_FUNDS));
    ctx(ALICE, NOW);
    c.mint_position(pool, -600, 600, U128(LIQUIDITY));
    pool
}

#[test]
fn slippage_refuses_and_changes_nothing() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let amount = 1_000_000u128;
    let path = vec![hop(pool, USDC)];
    let quote = c.quote_exact_input(path.clone(), U128(amount)).0;
    let before = snapshot(&c, pool);

    let refund = deliver_msg(
        &mut c,
        USDC,
        BOB,
        amount,
        &swap_msg(&path, quote + 1, Some("internal")),
    );
    assert_eq!(refund, amount, "the whole transfer is refunded");
    assert_eq!(snapshot(&c, pool), before, "no state changed");
}

#[test]
fn bad_paths_are_refunded_without_a_panic() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let amount = 1_000_000u128;
    let cases: Vec<(&str, Vec<HopInput>)> = vec![
        ("empty path", vec![]),
        (
            "first hop is not the transferred token",
            vec![hop(pool, WNEAR)],
        ),
        ("unknown pool", vec![hop(99, USDC)]),
        (
            "discontinuous route",
            vec![hop(pool, USDC), hop(pool, USDC)],
        ),
        ("token not in pool", vec![hop(pool, ETH)]),
    ];
    for (name, path) in cases {
        let before = snapshot(&c, pool);
        let refund = deliver_msg(
            &mut c,
            USDC,
            BOB,
            amount,
            &swap_msg(&path, 1, Some("internal")),
        );
        assert_eq!(refund, amount, "{name}: refunded");
        assert_eq!(snapshot(&c, pool), before, "{name}: no state changed");
    }
}

#[test]
fn a_zero_amount_is_refunded() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let path = vec![hop(pool, USDC)];
    assert_eq!(
        deliver_msg(&mut c, USDC, BOB, 0, &swap_msg(&path, 0, Some("internal"))),
        0
    );
}

#[test]
fn a_near_receiver_needs_a_nep141_output() {
    let mut c = setup();
    let pool = usdc_eth_pool(&mut c);
    let before = snapshot(&c, pool);
    // The output is an EVM asset, which cannot be sent to a NEAR account.
    let to = format!("near:{CAROL}");
    let refund = deliver_msg(
        &mut c,
        USDC,
        BOB,
        1_000,
        &swap_msg(&[hop(pool, USDC)], 1, Some(&to)),
    );
    assert_eq!(refund, 1_000);
    assert_eq!(snapshot(&c, pool), before);
}

#[test]
fn a_malformed_message_is_refunded() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let path = vec![hop(pool, USDC)];
    // The path must parse, and `to` must be a valid receiver.
    let bad_to = swap_msg(&path, 1, Some("bogus"));
    assert_eq!(deliver_msg(&mut c, USDC, BOB, 1_000, &bad_to), 1_000);
    let both = json!({
        "swap": { "path": path, "min_amount_out": "1" },
        "add_liquidity": { "pool_id": pool, "tick_lower": -300, "tick_upper": 300, "zap": true },
    })
    .to_string();
    assert_eq!(deliver_msg(&mut c, USDC, BOB, 1_000, &both), 1_000);
}

#[test]
fn zap_adds_liquidity_and_credits_the_leftovers() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let amount = 10_000_000_000u128;
    let usdc_before = c.get_pool_reserves(pool)[0].1 .0;
    let wnear_before = c.get_pool_reserves(pool)[1].1 .0;

    let refund = deliver_msg(
        &mut c,
        USDC,
        BOB,
        amount,
        &zap_msg(pool, -300, 300, true, None, None),
    );
    assert_eq!(refund, 0);

    let position = c.get_position(pool, acct(BOB), -300, 300);
    assert!(position.liquidity.0 > 0, "a position was minted for BOB");

    // The deposit is conserved: what BOB keeps plus what the pool took in is the deposit.
    let reserves = c.get_pool_reserves(pool);
    let usdc_in = reserves[0].1 .0 - usdc_before;
    assert_eq!(balance(&c, BOB, USDC) + usdc_in, amount);
    // WNEAR: BOB's leftover is what the pool gave up, net of what the mint took.
    let wnear_delta = reserves[1].1 .0 as i128 - wnear_before as i128;
    assert_eq!(balance(&c, BOB, WNEAR) as i128 + wnear_delta, 0);

    // The leftover is dust, not a meaningful share of the deposit.
    assert!(
        balance(&c, BOB, USDC) < amount / 1_000,
        "USDC leftover is small"
    );
    assert!(
        balance(&c, BOB, WNEAR) < amount / 1_000,
        "WNEAR leftover is small"
    );
    assert_owed(&c, USDC, LP_FUNDS + amount);
}

#[test]
fn zap_without_zap_needs_a_one_sided_range() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let amount = 1_000_000_000u128;

    // In range: the USDC alone cannot make a position, so the transfer is refunded.
    let before = snapshot(&c, pool);
    assert_eq!(
        deliver_msg(
            &mut c,
            USDC,
            BOB,
            amount,
            &zap_msg(pool, -300, 300, false, None, None)
        ),
        amount
    );
    assert_eq!(snapshot(&c, pool), before);

    // Above the price the range holds token0 only, so USDC alone makes the position.
    let refund = deliver_msg(
        &mut c,
        USDC,
        BOB,
        amount,
        &zap_msg(pool, 600, 1200, false, None, None),
    );
    assert_eq!(refund, 0);
    assert!(c.get_position(pool, acct(BOB), 600, 1200).liquidity.0 > 0);
    assert!(balance(&c, BOB, USDC) < 2, "only rounding dust is left");
    assert_eq!(balance(&c, BOB, WNEAR), 0);
}

#[test]
fn zap_refuses_when_min_liquidity_is_not_reached() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let amount = 10_000_000_000u128;
    let before = snapshot(&c, pool);
    let refund = deliver_msg(
        &mut c,
        USDC,
        BOB,
        amount,
        &zap_msg(pool, -300, 300, true, Some(u128::MAX), None),
    );
    assert_eq!(refund, amount);
    assert_eq!(snapshot(&c, pool), before);
}

#[test]
fn zap_refuses_a_misaligned_range_and_a_token_outside_the_pool() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let amount = 10_000_000_000u128;
    let before = snapshot(&c, pool);
    assert_eq!(
        deliver_msg(
            &mut c,
            USDC,
            BOB,
            amount,
            &zap_msg(pool, -301, 300, true, None, None)
        ),
        amount,
        "a tick that is not a multiple of the spacing"
    );
    assert_eq!(
        deliver_msg(
            &mut c,
            USDC,
            BOB,
            amount,
            &zap_msg(pool, 300, -300, true, None, None)
        ),
        amount,
        "a range with its ends swapped"
    );
    assert_eq!(snapshot(&c, pool), before);
}

#[test]
fn zap_sends_the_leftovers_to_a_near_account() {
    zap_leftovers_to_near(10_000_000_000);
}

#[test]
fn zap_sends_the_leftovers_to_a_near_account_odd_amount() {
    zap_leftovers_to_near(7_777_777_777);
}

#[test]
fn zap_sends_the_leftovers_to_a_near_account_small_amount() {
    zap_leftovers_to_near(123_456_789);
}

/// A zap with `to: near:<account>` sends each nonzero leftover, and only those. The zap leaves
/// dust, which can be zero. The leftovers follow from the pool's reserve changes: the USDC pool
/// took `amount - leftover`, and the WNEAR pool gave up `leftover` net of what the mint used.
fn zap_leftovers_to_near(amount: u128) {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let usdc_before = c.get_pool_reserves(pool)[0].1 .0 as i128;
    let wnear_before = c.get_pool_reserves(pool)[1].1 .0 as i128;
    let to = format!("near:{CAROL}");
    let refund = deliver_msg(
        &mut c,
        USDC,
        BOB,
        amount,
        &zap_msg(pool, -300, 300, true, None, Some(&to)),
    );
    assert_eq!(refund, 0);
    assert!(c.get_position(pool, acct(BOB), -300, 300).liquidity.0 > 0);
    assert_eq!(balance(&c, BOB, USDC), 0, "leftover USDC was sent");
    assert_eq!(balance(&c, BOB, WNEAR), 0, "leftover WNEAR was sent");

    let usdc_delta = c.get_pool_reserves(pool)[0].1 .0 as i128 - usdc_before;
    let wnear_delta = c.get_pool_reserves(pool)[1].1 .0 as i128 - wnear_before;
    let left_usdc = amount as i128 - usdc_delta;
    let left_wnear = -wnear_delta;
    assert!(left_usdc >= 0 && left_wnear >= 0);
    let expected = usize::from(left_usdc > 0) + usize::from(left_wnear > 0);
    assert_eq!(
        ft_transfer_count(),
        expected,
        "one transfer per nonzero leftover"
    );
}

#[test]
fn zap_into_an_evm_pool_refuses_an_evm_receiver() {
    let mut c = setup();
    let pool = usdc_eth_pool(&mut c);
    let before = snapshot(&c, pool);
    let to = format!("evm:{TO_EVM}");
    let refund = deliver_msg(
        &mut c,
        USDC,
        BOB,
        1_000_000,
        &zap_msg(pool, -300, 300, true, None, Some(&to)),
    );
    assert_eq!(refund, 1_000_000);
    assert_eq!(snapshot(&c, pool), before);
}

/// Every flow, one after another, keeps the owed total equal to what the contract holds.
#[test]
fn one_step_flows_keep_the_totals_equal_to_the_holdings() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let (mut held_usdc, mut held_wnear) = (0u128, 0u128);
    // The pool's own deposits were made by ALICE, in `usdc_wnear_pool`.
    held_usdc += LP_FUNDS;
    held_wnear += LP_FUNDS;

    let amount = 5_000_000u128;
    let path = vec![hop(pool, USDC)];

    // Swap to internal: USDC in, WNEAR credited.
    assert_eq!(
        deliver_msg(
            &mut c,
            USDC,
            BOB,
            amount,
            &swap_msg(&path, 1, Some("internal"))
        ),
        0
    );
    held_usdc += amount;
    assert_owed(&c, USDC, held_usdc);
    assert_owed(&c, WNEAR, held_wnear);

    // Zap: USDC in, a position minted, leftovers credited.
    let zap_amount = 3_000_000_000u128;
    assert_eq!(
        deliver_msg(
            &mut c,
            USDC,
            BOB,
            zap_amount,
            &zap_msg(pool, -300, 300, true, None, None)
        ),
        0
    );
    held_usdc += zap_amount;
    assert_owed(&c, USDC, held_usdc);
    assert_owed(&c, WNEAR, held_wnear);

    // Swap to near: the output leaves the contract, the payout is assumed to succeed.
    let to = format!("near:{CAROL}");
    let quote = c.quote_exact_input(path.clone(), U128(amount)).0;
    assert_eq!(
        deliver_msg(&mut c, USDC, BOB, amount, &swap_msg(&path, 1, Some(&to))),
        0
    );
    held_usdc += amount;
    held_wnear -= quote;
    assert_owed(&c, USDC, held_usdc);
    assert_owed(&c, WNEAR, held_wnear);
}

#[test]
fn a_one_step_swap_is_counted_in_the_pool_summary() {
    let mut c = setup();
    let pool = usdc_wnear_pool(&mut c);
    let amount = 1_000_000u128;
    let path = vec![hop(pool, USDC)];
    assert_eq!(c.get_pool_summary(pool).unwrap().swap_count, 0);

    let refund = deliver_msg(&mut c, USDC, BOB, amount, &swap_msg(&path, 1, None));
    assert_eq!(refund, 0);
    let s = c.get_pool_summary(pool).unwrap();
    assert_eq!(s.swap_count, 1);
    assert_eq!(s.volume0.0, amount, "token0 in is the amount paid in");
    assert!(s.volume1.0 > 0, "token1 out is counted too");
}
