//! Tests for the ve(3,3) tokenomics: locks, voting, gauges, rewards, rebase and the weekly epoch.
//! The mint is simulated: `distribute` returns the mint promise, and the test calls the callback
//! with the mint result, as the runtime would.

use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, Gas, NearToken};
use windswap_engine::minter::WEEK;

use super::*;
use crate::accounts::{action_message, owner_hex};
use crate::accounts_tests::{evm_sign, evm_wallet, hex, sk};

const OWNER: &str = "owner.testnet";
const SELF: &str = "windswapv4-btb.testnet";
const ALICE: &str = "alice.testnet";
const BOB: &str = "bob.testnet";
const WIND: &str = "wind.testnet";
const USDC: &str = "usdc.testnet";
const WNEAR: &str = "wnear.testnet";
const TICK_SPACING: i32 = 3;
const Q96: &str = "79228162514264337593543950336";
const E18: u128 = 1_000_000_000_000_000_000;
const HOUR: u64 = 3_600;
/// The start of an epoch. A multiple of WEEK, so each epoch starts on it.
const T0: u64 = 100 * WEEK;
/// The genesis WIND supply that `setup` mints to the team.
const GENESIS: u128 = 1_000_000 * E18;
const EXPIRES_OFFSET: u64 = 1_000;

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}

/// Sets the caller and the time (block timestamp and height). The contract account is `SELF`.
fn ctx(predecessor: &str, secs: u64, deposit: u128) {
    let mut builder = VMContextBuilder::new();
    builder
        .current_account_id(acct(SELF))
        .predecessor_account_id(acct(predecessor))
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .prepaid_gas(Gas::from_tgas(300))
        .block_timestamp(secs * 1_000_000_000)
        .block_height(secs);
    testing_env!(
        builder.build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        vec![]
    );
}

/// The owner creates the contract, the WIND token, and a USDC/WNEAR pool (pool 0). USDC is
/// whitelisted for bribes.
fn setup() -> Contract {
    ctx(OWNER, T0, 0);
    let mut c = Contract::new(acct(OWNER));
    c.add_token(acct(USDC));
    c.add_token(acct(WIND));
    c.set_wind_token(acct(WIND));
    let pool = c.create_pool(USDC.into(), WNEAR.into(), TICK_SPACING, Q96.into());
    assert_eq!(pool, 0);
    c.whitelist_bribe_token(USDC.into(), true);
    // The genesis supply: the first epoch's growth formula divides by the WIND supply.
    let _ = c.genesis_mint(U128(GENESIS));
    ctx(SELF, T0, 0);
    c.on_genesis_minted(U128(GENESIS), Ok(()));
    assert_eq!(c.get_epoch().wind_minted.0, GENESIS);
    c
}

fn fund(c: &mut Contract, who: &str, token: &str, amount: u128) {
    ctx(OWNER, T0, 0);
    c.test_credit(acct(who), token.into(), U128(amount));
}

fn bal(c: &Contract, who: &str, token: &str) -> u128 {
    c.get_balance(acct(who), token.to_string()).0
}

fn owner_hex_of(name: &str) -> String {
    owner_hex(&owner_of_account(&acct(name)))
}

/// Runs the epoch flip at `secs`: `distribute`, then the mint callback with the amount that
/// `distribute` asked to mint. Returns that amount.
fn run_epoch_flip(c: &mut Contract, secs: u64) -> u128 {
    ctx(BOB, secs, 0);
    let _ = c.distribute();
    let amount = c
        .pending_epoch
        .as_ref()
        .map(|p| to_u128(p.emission.mint_required))
        .unwrap_or(0);
    ctx(SELF, secs, 0);
    let minted = c.on_distribute_minted(U128(amount), Ok(()));
    assert_eq!(minted.0, amount);
    amount
}

fn amount_of(paid: &[(String, U128)], token: &str) -> u128 {
    paid.iter()
        .find(|(t, _)| t == token)
        .map(|(_, a)| a.0)
        .unwrap_or(0)
}

/// Gives ALICE a 1000 WIND lock for four weeks, and LP in the pool. Returns the token id.
fn alice_locks_and_provides(c: &mut Contract) -> u64 {
    fund(c, ALICE, WIND, 1_000 * E18);
    ctx(ALICE, T0 + 10, 0);
    let token = c.create_lock(U128(1_000 * E18), 4 * WEEK);
    fund(c, ALICE, USDC, 10 * E18);
    fund(c, ALICE, WNEAR, 10 * E18);
    ctx(ALICE, T0 + 20, 0);
    c.mint_position(0, -300, 300, U128(E18));
    token
}

#[test]
fn weekly_cycle_locks_votes_stakes_claims_and_rebases() {
    let mut c = setup();
    ctx(OWNER, T0, 0);
    let gauge = c.create_gauge(0);
    assert_eq!(gauge, 1);

    let token = alice_locks_and_provides(&mut c);
    assert_eq!(token, 1);
    assert_eq!(bal(&c, ALICE, WIND), 0, "the lock pulled the WIND");
    let lock = c.get_lock(token).expect("lock exists");
    assert_eq!(lock.owner, owner_hex_of(ALICE));
    assert_eq!(lock.amount.0, 1_000 * E18);
    assert!(!lock.permanent);
    assert_eq!(c.get_locks_of(owner_hex_of(ALICE)), vec![token]);

    // ALICE stakes her position in the gauge.
    ctx(ALICE, T0 + 30, 0);
    c.stake_position(0, -300, 300);
    assert_eq!(c.get_gauge(0).map(|g| g.gauge_id), Some(gauge));

    // Vote inside the voting window of epoch 0 (after the first hour).
    ctx(ALICE, T0 + 2 * HOUR, 0);
    c.vote(token, vec![0], vec![U128(1)]);
    let votes = c.get_votes(token);
    assert_eq!(votes.len(), 1);
    assert!(votes[0].1 .0 > 0, "the vote carries the lock's power");

    // BOB trades (which earns the gauge fees) and bribes the gauge.
    fund(&mut c, BOB, USDC, 100 * E18);
    fund(&mut c, BOB, WNEAR, 100 * E18);
    ctx(BOB, T0 + 3 * HOUR, 0);
    // A small trade, so the price stays inside ALICE's range (a large one would move it out).
    c.swap(0, true, "1000000000000000".into(), "4295128740".into());
    let before = bal(&c, BOB, USDC);
    c.bribe(0, USDC.into(), U128(5 * E18));
    assert_eq!(before - bal(&c, BOB, USDC), 5 * E18);

    // The epoch flips after the week. Emissions reach the voter and the gauge.
    let minted = run_epoch_flip(&mut c, T0 + WEEK + 2 * HOUR);
    assert!(minted > 0, "the first epoch mints the weekly emission");
    let epoch = c.get_epoch();
    assert_eq!(epoch.active_period, T0 + WEEK);
    assert_eq!(epoch.epoch_count, 1);
    assert_eq!(epoch.wind_minted.0, GENESIS + minted);
    assert!(!epoch.distribution_pending);
    assert!(
        c.get_gauge(0).expect("gauge").reward_balance.0 > 0,
        "the gauge got the emission"
    );

    // ALICE claims her gauge emissions for the hour since the flip.
    ctx(ALICE, T0 + WEEK + 3 * HOUR, 0);
    let emissions = c.claim_gauge_rewards(0, -300, 300).0;
    assert!(emissions > 0, "a staked position earns emissions");
    assert_eq!(bal(&c, ALICE, WIND), emissions);

    // Rewards of an epoch are claimable once it has ended, so the next flip comes first.
    let minted2 = run_epoch_flip(&mut c, T0 + 2 * WEEK + 2 * HOUR);
    assert!(minted2 > 0);
    ctx(ALICE, T0 + 2 * WEEK + 3 * HOUR, 0);

    // Fees from BOB's swap, collected at the first flip, reach the voter.
    let fees = c.claim_fees(token, vec![gauge]);
    assert!(
        amount_of(&fees, USDC) > 0 || amount_of(&fees, WNEAR) > 0,
        "the voter gets the gauge's fees"
    );

    // The bribe deposited in epoch 0 reaches the voter.
    let bribes = c.claim_bribes(token, vec![gauge], vec![USDC.into()]);
    assert!(amount_of(&bribes, USDC) > 0, "the voter gets the bribe");

    let rebase = c.claim_rebase(token).0;
    assert_eq!(c.claimable_rebase(token).0, 0, "nothing left after a claim");
    assert!(
        rebase <= minted + minted2,
        "a rebase is part of the minted growth"
    );
}

#[test]
fn vote_twice_in_one_epoch_fails() {
    let mut c = setup();
    ctx(OWNER, T0, 0);
    c.create_gauge(0);
    let token = alice_locks_and_provides(&mut c);
    ctx(ALICE, T0 + 2 * HOUR, 0);
    c.vote(token, vec![0], vec![U128(1)]);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        c.vote(token, vec![0], vec![U128(1)]);
    }));
    let msg = result
        .expect_err("the second vote panics")
        .downcast::<String>()
        .map(|s| *s)
        .unwrap_or_default();
    assert!(msg.contains("AlreadyVotedOrDeposited"), "{msg}");
}

#[test]
#[should_panic(expected = "LockNotExpired")]
fn withdraw_before_expiry_fails() {
    let mut c = setup();
    let token = alice_locks_and_provides(&mut c);
    ctx(ALICE, T0 + 30, 0);
    c.withdraw_lock(token);
}

#[test]
fn withdraw_after_expiry_credits_the_wind_back() {
    let mut c = setup();
    fund(&mut c, ALICE, WIND, 50 * E18);
    ctx(ALICE, T0 + 10, 0);
    let token = c.create_lock(U128(50 * E18), WEEK);
    ctx(ALICE, T0 + WEEK + 10, 0);
    assert_eq!(c.withdraw_lock(token).0, 50 * E18);
    assert_eq!(bal(&c, ALICE, WIND), 50 * E18);
    assert!(c.get_lock(token).is_none(), "the withdrawn lock is gone");
}

#[test]
#[should_panic(expected = "NotApprovedOrOwner")]
fn only_the_owner_can_change_a_lock() {
    let mut c = setup();
    let token = alice_locks_and_provides(&mut c);
    fund(&mut c, BOB, WIND, 5 * E18);
    ctx(BOB, T0 + 30, 0);
    c.increase_amount(token, U128(E18));
}

#[test]
#[should_panic(expected = "position is already staked")]
fn staking_the_same_range_twice_fails() {
    let mut c = setup();
    ctx(OWNER, T0, 0);
    c.create_gauge(0);
    alice_locks_and_provides(&mut c);
    ctx(ALICE, T0 + 30, 0);
    c.stake_position(0, -300, 300);
    c.stake_position(0, -300, 300);
}

#[test]
fn unstake_returns_the_position_and_it_can_be_staked_again() {
    let mut c = setup();
    ctx(OWNER, T0, 0);
    c.create_gauge(0);
    alice_locks_and_provides(&mut c);
    ctx(ALICE, T0 + 30, 0);
    c.stake_position(0, -300, 300);
    ctx(ALICE, T0 + 60, 0);
    c.unstake_position(0, -300, 300);
    assert_eq!(
        c.claimable_gauge_rewards(owner_hex_of(ALICE), 0, -300, 300)
            .0,
        0
    );
    ctx(ALICE, T0 + 90, 0);
    c.stake_position(0, -300, 300);
}

#[test]
fn distribute_is_a_no_op_within_the_same_epoch() {
    let mut c = setup();
    ctx(OWNER, T0, 0);
    c.create_gauge(0);
    run_epoch_flip(&mut c, T0 + WEEK + 2 * HOUR);
    let before = c.get_epoch();
    ctx(BOB, T0 + WEEK + 3 * HOUR, 0);
    assert!(matches!(
        c.distribute(),
        near_sdk::PromiseOrValue::Value(U128(0))
    ));
    let after = c.get_epoch();
    assert_eq!(before.active_period, after.active_period);
    assert_eq!(before.wind_minted.0, after.wind_minted.0);
}

#[test]
#[should_panic(expected = "already in progress")]
fn distribute_while_a_mint_is_in_flight_panics() {
    let mut c = setup();
    ctx(OWNER, T0, 0);
    c.create_gauge(0);
    ctx(BOB, T0 + WEEK + 2 * HOUR, 0);
    let _ = c.distribute();
    let _ = c.distribute();
}

#[test]
fn a_failed_mint_leaves_the_epoch_uncommitted_and_retryable() {
    let mut c = setup();
    ctx(OWNER, T0, 0);
    c.create_gauge(0);
    ctx(BOB, T0 + WEEK + 2 * HOUR, 0);
    let _ = c.distribute();
    assert!(c.get_epoch().distribution_pending);
    ctx(SELF, T0 + WEEK + 2 * HOUR, 0);
    let minted = c.on_distribute_minted(U128(0), Err(near_sdk::PromiseError::Failed));
    assert_eq!(minted.0, 0);
    let epoch = c.get_epoch();
    assert!(
        !epoch.distribution_pending,
        "the failed mint clears the flag"
    );
    assert_eq!(epoch.active_period, T0, "the epoch did not advance");
    assert_eq!(epoch.wind_minted.0, GENESIS);
    // The retry works: the same flip, with the mint succeeding this time.
    let minted = run_epoch_flip(&mut c, T0 + WEEK + 2 * HOUR);
    assert!(minted > 0);
    assert_eq!(c.get_epoch().active_period, T0 + WEEK);
}

#[test]
fn a_wind_deposit_with_a_lock_message_makes_a_lock() {
    let mut c = setup();
    ctx(WIND, T0 + 10, 0);
    let msg = format!("{{\"lock\":{{\"duration\":{}}}}}", 4 * WEEK);
    let result = c.ft_on_transfer(acct(ALICE), U128(7 * E18), msg);
    assert!(matches!(result, near_sdk::PromiseOrValue::Value(U128(0))));
    let locks = c.get_locks_of(owner_hex_of(ALICE));
    assert_eq!(locks.len(), 1);
    assert_eq!(c.get_lock(locks[0]).expect("lock").amount.0, 7 * E18);
    assert_eq!(bal(&c, ALICE, WIND), 0, "the deposit went into the lock");
}

#[test]
fn a_wind_deposit_for_a_wallet_with_a_lock_message_locks_for_the_wallet() {
    let mut c = setup();
    let key = sk(61);
    let wallet = evm_wallet(&key).to_string();
    let owner = c.resolve_wallet(wallet.clone());
    ctx(WIND, T0 + 10, 0);
    let msg = format!(
        "{{\"owner\":\"{wallet}\",\"lock\":{{\"duration\":{}}}}}",
        4 * WEEK
    );
    let _ = c.ft_on_transfer(acct(BOB), U128(3 * E18), msg);
    assert_eq!(c.get_locks_of(owner).len(), 1);
    assert_eq!(bal(&c, BOB, WIND), 0);
}

#[test]
fn wallet_only_evm_user_locks_and_votes_by_signature() {
    let mut c = setup();
    ctx(OWNER, T0, 0);
    c.create_gauge(0);
    let key = sk(62);
    let wallet = evm_wallet(&key).to_string();
    let owner = c.resolve_wallet(wallet.clone());

    // The wallet's WIND arrives by deposit, credited to its own owner id.
    ctx(WIND, T0 + 10, 0);
    let _ = c.ft_on_transfer(
        acct(BOB),
        U128(20 * E18),
        format!("{{\"owner\":\"{wallet}\"}}"),
    );
    assert_eq!(
        c.get_balance_of_owner(owner.clone(), WIND.into()).0,
        20 * E18
    );

    // Lock 20 WIND, signed by the wallet, relayed by BOB.
    let lock_action = SignedAction::CreateLock {
        amount: U128(20 * E18),
        duration: 4 * WEEK,
    };
    ctx(BOB, T0 + 20, 0);
    let sig = signed_by(&key, &lock_action, 1, T0 + 20 + EXPIRES_OFFSET);
    let token = match c.execute(
        wallet.clone(),
        lock_action,
        1,
        T0 + 20 + EXPIRES_OFFSET,
        sig,
    ) {
        ActionResult::TokenId(t) => t,
        _ => panic!("expected a token id"),
    };
    assert_eq!(c.get_locks_of(owner.clone()), vec![token]);

    // Vote for pool 0 with that lock, signed.
    let vote_action = SignedAction::Vote {
        token_id: token,
        pools: vec![0],
        weights: vec![U128(1)],
    };
    ctx(BOB, T0 + 2 * HOUR, 0);
    let expires = T0 + 2 * HOUR + EXPIRES_OFFSET;
    let sig = signed_by(&key, &vote_action, 2, expires);
    c.execute(wallet, vote_action, 2, expires, sig);
    assert_eq!(c.get_votes(token).len(), 1);
}

/// The wallet's signature over an action, as `execute` checks it.
fn signed_by(
    key: &k256::ecdsa::SigningKey,
    action: &SignedAction,
    nonce: u64,
    expires: u64,
) -> String {
    let json = near_sdk::serde_json::to_string(action).unwrap();
    let msg = action_message(&acct(SELF), &evm_wallet(key), nonce, expires, &json);
    format!("0x{}", hex(&evm_sign(key, &msg)))
}
