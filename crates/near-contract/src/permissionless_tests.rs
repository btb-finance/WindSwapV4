//! Tests for permissionless listing (v8): auto-registration of EVM ERC-20s, anyone-creates-pool,
//! `register_token`, the pool views, and the v7 -> v8 migration.

use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, PromiseError};

use super::*;
use crate::permissionless::{
    pool_key, FtMetadata, PoolSummary, POOL_STORAGE_DEPOSIT, REGISTER_TOKEN_DEPOSIT,
};

const OWNER: &str = "owner.testnet";
const SELF: &str = "windswapv4-btb.testnet";
const ALICE: &str = "alice.testnet";
const BOB: &str = "bob.testnet";
const RELAYER: &str = "relayer.testnet";
const WNEAR: &str = "wnear.fakes.testnet";
const FAKE: &str = "fake.fakes.testnet";
const CHAIN: u64 = 84532;
const NOW: u64 = 1_000;
const TICK_SPACING: i32 = 3;
const Q96_STR: &str = "79228162514264337593543950336";
const E18: u128 = 1_000_000_000_000_000_000;
/// Base Sepolia USDC, lower case.
const USDC_ADDR: &str = "0x036cbd53842c5426634e7929541ec2318f3dcf7e";
/// A look-alike token with the same symbol and another address.
const FAKE_USDC_ADDR: &str = "0x9999999999999999999999999999999999999999";
const SENDER: &str = "0x2222222222222222222222222222222222222222";
const TX: u8 = 7;

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}

fn ctx(predecessor: &str, deposit: u128) {
    let mut builder = VMContextBuilder::new();
    builder
        .current_account_id(acct(SELF))
        .predecessor_account_id(acct(predecessor))
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .prepaid_gas(Gas::from_tgas(300))
        .block_timestamp(NOW * 1_000_000_000);
    testing_env!(
        builder.build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        vec![]
    );
}

fn setup() -> Contract {
    ctx(OWNER, 0);
    Contract::new(acct(OWNER))
}

fn tx_hash(n: u8) -> String {
    format!("0x{}", format!("{n:02x}").repeat(32))
}

/// Owner setup for the EVM tests: relayer, enabled chain, and default caps.
fn evm_ready() -> Contract {
    let mut c = setup();
    ctx(OWNER, 0);
    c.add_relayer(acct(RELAYER));
    c.set_evm_chain(CHAIN, "ethereum-1".into(), 0, U128(30_000_000_000));
    c.set_default_caps(U128(1_000 * 1_000_000), U128(10_000 * 1_000_000));
    c
}

/// The relayer reports one deposit. Returns the contract's answer.
fn report(
    c: &mut Contract,
    tx: u8,
    asset: &str,
    amount: u128,
    decimals: Option<u8>,
    symbol: Option<&str>,
) -> bool {
    ctx(RELAYER, 0);
    c.report_evm_deposit(
        CHAIN,
        tx_hash(tx),
        0,
        SENDER.into(),
        asset.into(),
        U128(amount),
        decimals,
        symbol.map(str::to_string),
    )
}

/// The internal balance of the wallet `SENDER`.
fn sender_balance(c: &Contract, token: &str) -> u128 {
    let owner = c.resolve_wallet(format!("evm:{SENDER}"));
    c.get_balance_of_owner(owner, token.into()).0
}

fn auto_id(addr: &str) -> String {
    format!("evm:{CHAIN}:{addr}")
}

#[test]
fn unknown_erc20_on_an_enabled_chain_is_registered_by_its_first_deposit() {
    let mut c = evm_ready();
    assert!(report(
        &mut c,
        TX,
        USDC_ADDR,
        50,
        Some(6),
        Some("USD Coin!! very long symbol")
    ));

    let view = c
        .get_evm_asset(CHAIN, USDC_ADDR.into())
        .expect("registered");
    assert_eq!(view.token_id, auto_id(USDC_ADDR));
    assert_eq!(view.decimals, 6);
    assert_eq!(view.per_deposit_cap.0, 1_000 * 1_000_000);
    assert_eq!(view.daily_withdraw_cap.0, 10_000 * 1_000_000);
    assert!(!view.verified, "auto-registered tokens start unverified");
    assert_eq!(
        view.symbol, "USDCoinverylongs",
        "sanitised, at most 16 characters"
    );
    assert_eq!(c.get_evm_assets().len(), 1);
    assert_eq!(sender_balance(&c, &auto_id(USDC_ADDR)), 50);

    // The second deposit of the same token is credited to the registered asset.
    assert!(report(&mut c, 8, USDC_ADDR, 25, None, None));
    assert_eq!(sender_balance(&c, &auto_id(USDC_ADDR)), 75);
}

#[test]
fn a_duplicate_report_of_an_auto_registered_deposit_is_refused() {
    let mut c = evm_ready();
    assert!(report(&mut c, TX, USDC_ADDR, 50, Some(6), None));
    assert!(!report(&mut c, TX, USDC_ADDR, 50, Some(6), None));
    assert_eq!(sender_balance(&c, &auto_id(USDC_ADDR)), 50);
}

#[test]
#[should_panic(expected = "is not registered")]
fn a_disabled_chain_panics_for_an_unknown_asset() {
    let mut c = setup();
    ctx(OWNER, 0);
    c.add_relayer(acct(RELAYER));
    c.set_default_caps(U128(E18), U128(E18));
    report(&mut c, TX, USDC_ADDR, 1, Some(6), None);
}

#[test]
fn missing_decimals_refuse_the_report_and_a_retry_with_decimals_works() {
    let mut c = evm_ready();
    assert!(!report(&mut c, TX, USDC_ADDR, 50, None, None));
    assert!(c.get_evm_asset(CHAIN, USDC_ADDR.into()).is_none());
    assert!(c.get_evm_assets().is_empty());

    // Nothing was recorded, so the same deposit can be reported again with its decimals.
    assert!(report(&mut c, TX, USDC_ADDR, 50, Some(6), None));
    assert_eq!(sender_balance(&c, &auto_id(USDC_ADDR)), 50);
}

#[test]
fn decimals_above_36_are_refused() {
    let mut c = evm_ready();
    assert!(!report(&mut c, TX, USDC_ADDR, 50, Some(37), None));
    assert!(c.get_evm_asset(CHAIN, USDC_ADDR.into()).is_none());
}

#[test]
fn without_default_caps_no_token_is_auto_registered() {
    let mut c = setup();
    ctx(OWNER, 0);
    c.add_relayer(acct(RELAYER));
    c.set_evm_chain(CHAIN, "ethereum-1".into(), 0, U128(30_000_000_000));
    assert!(c.get_default_caps().is_none());
    assert!(!report(&mut c, TX, USDC_ADDR, 50, Some(6), None));
    assert!(c.get_evm_asset(CHAIN, USDC_ADDR.into()).is_none());
}

#[test]
#[should_panic(expected = "is not registered")]
fn native_is_never_auto_registered() {
    let mut c = evm_ready();
    report(&mut c, TX, "native", 1, Some(18), None);
}

#[test]
fn token_ids_come_from_the_address_so_a_look_alike_cannot_take_them() {
    let mut c = evm_ready();
    assert!(report(&mut c, 1, USDC_ADDR, 10, Some(6), Some("USDC")));
    assert!(report(&mut c, 2, FAKE_USDC_ADDR, 10, Some(6), Some("USDC")));
    let real = c.get_evm_asset(CHAIN, USDC_ADDR.into()).unwrap();
    let fake = c.get_evm_asset(CHAIN, FAKE_USDC_ADDR.into()).unwrap();
    assert_ne!(real.token_id, fake.token_id);
    assert_eq!(real.token_id, auto_id(USDC_ADDR));
    assert_eq!(fake.token_id, auto_id(FAKE_USDC_ADDR));
    assert_eq!(real.symbol, "USDC");
}

#[test]
fn the_owner_can_verify_an_asset_and_owner_registered_assets_are_verified() {
    let mut c = evm_ready();
    assert!(report(&mut c, TX, USDC_ADDR, 10, Some(6), Some("USDC")));
    ctx(OWNER, 0);
    c.set_evm_asset_verified(CHAIN, USDC_ADDR.into(), true);
    let view = c.get_evm_asset(CHAIN, USDC_ADDR.into()).unwrap();
    assert!(view.verified);
    assert_eq!(view.symbol, "USDC", "the symbol is kept");

    ctx(OWNER, 0);
    c.register_evm_asset(
        CHAIN,
        "native".into(),
        "base-sepolia:eth".into(),
        18,
        U128(1),
        U128(1),
    );
    assert!(c.get_evm_asset(CHAIN, "native".into()).unwrap().verified);
}

#[test]
#[should_panic(expected = "only owner")]
fn set_default_caps_is_owner_only() {
    let mut c = setup();
    ctx(ALICE, 0);
    c.set_default_caps(U128(1), U128(1));
}

#[test]
#[should_panic(expected = "per_deposit_cap must be positive")]
fn set_default_caps_refuses_a_zero_deposit_cap() {
    let mut c = setup();
    ctx(OWNER, 0);
    c.set_default_caps(U128(0), U128(1));
}

// ---- Pools ----

fn with_pair_tokens() -> Contract {
    let mut c = setup();
    ctx(OWNER, 0);
    c.add_token(acct(WNEAR));
    c
}

#[test]
fn anyone_creates_a_pool_with_the_storage_deposit() {
    let mut c = with_pair_tokens();
    ctx(OWNER, 0);
    c.add_token(acct(FAKE));
    ctx(ALICE, POOL_STORAGE_DEPOSIT + 5);
    let pool = c.create_pool(FAKE.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    assert_eq!(c.get_pool(pool).token0, FAKE);
    assert_eq!(
        c.get_pool_id(FAKE.into(), WNEAR.into(), TICK_SPACING),
        Some(pool)
    );
    // The pair is found in either order.
    assert_eq!(
        c.get_pool_id(WNEAR.into(), FAKE.into(), TICK_SPACING),
        Some(pool)
    );
    assert_eq!(c.get_pool_id(FAKE.into(), WNEAR.into(), 60), None);
}

#[test]
fn a_duplicate_pool_for_the_pair_and_spacing_is_refused_in_either_order() {
    let mut c = with_pair_tokens();
    ctx(OWNER, 0);
    c.add_token(acct("wind.testnet"));
    ctx(ALICE, POOL_STORAGE_DEPOSIT);
    c.create_pool(
        WNEAR.into(),
        "wind.testnet".into(),
        TICK_SPACING,
        Q96_STR.into(),
    );
    ctx(BOB, POOL_STORAGE_DEPOSIT);
    let again = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        c.create_pool(
            "wind.testnet".into(),
            WNEAR.into(),
            TICK_SPACING,
            Q96_STR.into(),
        )
    }));
    assert!(
        again.is_err(),
        "second pool for the same pair and spacing must panic"
    );
    assert_eq!(
        c.get_pool_id(WNEAR.into(), "wind.testnet".into(), TICK_SPACING),
        Some(0)
    );
}

#[test]
#[should_panic(expected = "already exists for this pair and tick spacing")]
fn duplicate_pool_panics_with_the_existing_id() {
    let mut c = with_pair_tokens();
    ctx(OWNER, 0);
    c.add_token(acct("wind.testnet"));
    ctx(ALICE, POOL_STORAGE_DEPOSIT);
    c.create_pool(
        WNEAR.into(),
        "wind.testnet".into(),
        TICK_SPACING,
        Q96_STR.into(),
    );
    ctx(BOB, POOL_STORAGE_DEPOSIT);
    c.create_pool(
        "wind.testnet".into(),
        WNEAR.into(),
        TICK_SPACING,
        Q96_STR.into(),
    );
}

#[test]
#[should_panic(expected = "attach at least")]
fn a_non_owner_without_the_storage_deposit_is_refused() {
    let mut c = with_pair_tokens();
    ctx(ALICE, POOL_STORAGE_DEPOSIT - 1);
    c.create_pool(
        WNEAR.into(),
        "wind.testnet".into(),
        TICK_SPACING,
        Q96_STR.into(),
    );
}

#[test]
#[should_panic(expected = "both tokens must be whitelisted or registered EVM assets")]
fn a_non_owner_needs_known_tokens() {
    let mut c = with_pair_tokens();
    ctx(ALICE, POOL_STORAGE_DEPOSIT);
    c.create_pool(WNEAR.into(), FAKE.into(), TICK_SPACING, Q96_STR.into());
}

#[test]
fn a_non_owner_can_pool_an_auto_registered_evm_asset() {
    let mut c = evm_ready();
    ctx(OWNER, 0);
    c.add_token(acct(WNEAR));
    assert!(report(&mut c, TX, USDC_ADDR, 10, Some(6), None));
    ctx(ALICE, POOL_STORAGE_DEPOSIT);
    let pool = c.create_pool(
        auto_id(USDC_ADDR),
        WNEAR.into(),
        TICK_SPACING,
        Q96_STR.into(),
    );
    assert_eq!(c.get_pool(pool).token1, WNEAR);
}

#[test]
fn the_owner_creates_a_pool_without_a_deposit() {
    let mut c = with_pair_tokens();
    ctx(OWNER, 0);
    let pool = c.create_pool(
        WNEAR.into(),
        "wind.testnet".into(),
        TICK_SPACING,
        Q96_STR.into(),
    );
    assert_eq!(pool, 0);
}

#[test]
fn get_pools_pages_through_every_pool() {
    let mut c = with_pair_tokens();
    ctx(OWNER, 0);
    c.add_token(acct("wind.testnet"));
    c.add_token(acct(FAKE));
    c.create_pool(
        WNEAR.into(),
        "wind.testnet".into(),
        TICK_SPACING,
        Q96_STR.into(),
    );
    c.create_pool(WNEAR.into(), FAKE.into(), TICK_SPACING, Q96_STR.into());
    c.create_pool(
        "wind.testnet".into(),
        FAKE.into(),
        TICK_SPACING,
        Q96_STR.into(),
    );

    let ids = |v: Vec<PoolSummary>| v.iter().map(|p| p.pool_id).collect::<Vec<_>>();
    assert_eq!(ids(c.get_pools(0, 2)), vec![0, 1]);
    assert_eq!(ids(c.get_pools(2, 10)), vec![2]);
    assert!(c.get_pools(3, 10).is_empty());
    assert_eq!(ids(c.get_pools(1, 1)), vec![1]);
    assert_eq!(
        ids(c.get_pools(0, u64::MAX)),
        vec![0, 1, 2],
        "limit is capped, not overflowed"
    );
}

// ---- register_token ----

#[test]
fn register_token_whitelists_a_token_when_its_metadata_succeeds() {
    let mut c = setup();
    ctx(BOB, REGISTER_TOKEN_DEPOSIT);
    let _promise = c.register_token(acct(FAKE));
    ctx(SELF, 0);
    let listed = c.on_register_token(
        acct(FAKE),
        acct(BOB),
        U128(REGISTER_TOKEN_DEPOSIT),
        Ok(FtMetadata { decimals: 24 }),
    );
    assert!(listed);
    assert!(c.get_tokens().contains(&acct(FAKE)));
}

#[test]
fn register_token_refuses_a_token_whose_metadata_fails() {
    let mut c = setup();
    ctx(SELF, 0);
    let listed = c.on_register_token(
        acct(FAKE),
        acct(BOB),
        U128(REGISTER_TOKEN_DEPOSIT),
        Err(PromiseError::Failed),
    );
    assert!(!listed);
    assert!(!c.get_tokens().contains(&acct(FAKE)));
}

#[test]
fn register_token_refuses_decimals_above_36() {
    let mut c = setup();
    ctx(SELF, 0);
    let listed = c.on_register_token(
        acct(FAKE),
        acct(BOB),
        U128(REGISTER_TOKEN_DEPOSIT),
        Ok(FtMetadata { decimals: 37 }),
    );
    assert!(!listed);
    assert!(c.get_tokens().is_empty());
}

#[test]
#[should_panic(expected = "attach at least")]
fn register_token_needs_the_storage_deposit() {
    let mut c = setup();
    ctx(BOB, REGISTER_TOKEN_DEPOSIT - 1);
    let _ = c.register_token(acct(FAKE));
}

#[test]
#[should_panic(expected = "token already allowed")]
fn register_token_refuses_a_whitelisted_token() {
    let mut c = with_pair_tokens();
    ctx(BOB, REGISTER_TOKEN_DEPOSIT);
    let _ = c.register_token(acct(WNEAR));
}

// ---- Migration from the deployed v7 layout ----

#[test]
fn migrate_from_v8_backfills_the_pool_index() {
    let mut c = with_pair_tokens();
    ctx(OWNER, 0);
    c.add_token(acct("wind.testnet"));
    c.add_token(acct(FAKE));
    c.create_pool(
        WNEAR.into(),
        "wind.testnet".into(),
        TICK_SPACING,
        Q96_STR.into(),
    );
    c.create_pool(WNEAR.into(), FAKE.into(), TICK_SPACING, Q96_STR.into());
    // Simulate the deployed layout: no index entries.
    c.permissionless
        .pool_by_key
        .remove(&pool_key(WNEAR, "wind.testnet", TICK_SPACING));
    c.permissionless
        .pool_by_key
        .remove(&pool_key(WNEAR, FAKE, TICK_SPACING));
    env::state_write(&permissionless::into_v8(c));

    let migrated = Contract::migrate();
    assert_eq!(
        migrated.get_pool_id(WNEAR.into(), "wind.testnet".into(), TICK_SPACING),
        Some(0),
        "the first pool of the pair keeps the key"
    );
    assert_eq!(
        migrated.get_pool_id(FAKE.into(), WNEAR.into(), TICK_SPACING),
        Some(1)
    );
    assert_eq!(migrated.get_pools(0, 10).len(), 2);
    assert!(migrated.get_default_caps().is_none());
}

// ---- Pool summaries ----

const MIN_LIMIT: &str = "4295128740";
const LIQUIDITY: u128 = 1_000_000_000_000_000_000;

/// Owner creates a pool and alice funds and mints a position in it. Returns the pool id.
fn pool_with_liquidity(c: &mut Contract, token: &str) -> u64 {
    ctx(OWNER, 0);
    c.add_token(acct(token));
    let pool = c.create_pool(token.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    ctx(OWNER, 0);
    c.test_credit(acct(ALICE), token.into(), U128(1 << 100));
    c.test_credit(acct(ALICE), WNEAR.into(), U128(1 << 100));
    ctx(ALICE, 0);
    c.mint_position(pool, -600, 600, U128(LIQUIDITY));
    pool
}

#[test]
fn the_summary_follows_mints_swaps_and_burns() {
    let mut c = with_pair_tokens();
    let pool = pool_with_liquidity(&mut c, FAKE);
    let s = c.get_pool_summary(pool).expect("summary");
    assert_eq!(s.token0, FAKE);
    assert_eq!(s.liquidity.0, LIQUIDITY);
    assert_eq!(s.swap_count, 0);
    assert!(!s.has_gauge);
    assert_eq!(s.fee, c.get_pool_summary(pool).unwrap().fee);

    ctx(OWNER, 0);
    c.test_credit(acct(BOB), FAKE.into(), U128(1 << 80));
    ctx(BOB, 0);
    let [a0, a1] = c.swap(pool, true, "1000000".into(), MIN_LIMIT.into());
    let s = c.get_pool_summary(pool).unwrap();
    assert_eq!(s.swap_count, 1);
    assert_eq!(
        s.volume0.0,
        a0.trim_start_matches('-').parse::<u128>().unwrap()
    );
    assert_eq!(
        s.volume1.0,
        a1.trim_start_matches('-').parse::<u128>().unwrap()
    );
    assert_eq!(
        s.tick,
        c.get_pool(pool).slot0.tick,
        "live fields match the pool"
    );

    ctx(ALICE, 0);
    c.burn_position(pool, -600, 600, U128(LIQUIDITY / 2));
    assert_eq!(c.get_pool_summary(pool).unwrap().liquidity.0, LIQUIDITY / 2);
    assert_eq!(
        c.get_pool_summary(pool).unwrap().swap_count,
        1,
        "a burn is not a swap"
    );

    ctx(OWNER, 0);
    c.set_custom_swap_fee(pool, 1234);
    assert_eq!(c.get_pool_summary(pool).unwrap().fee, 1234);
}

#[test]
fn get_pools_reads_only_the_summaries() {
    // Twenty pools, each with several positions over many ticks.
    let mut c = setup();
    let mut pools = Vec::new();
    for i in 0..20u32 {
        let token = format!("t{i}.fakes.testnet");
        let pool = pool_with_liquidity(&mut c, &token);
        ctx(ALICE, 0);
        for k in 1..=5 {
            c.mint_position(pool, -60 * k - 60, 60 * k + 60, U128(LIQUIDITY / 100));
        }
        pools.push(pool);
    }
    // Remove the pool states. A view that reads them would fail, but the summaries are enough.
    for pool in &pools {
        c.pools.remove(pool);
    }
    let page = c.get_pools(0, 100);
    assert_eq!(page.len(), 20);
    // Each extra position covers tick 0, so it adds to the active liquidity.
    let active = LIQUIDITY + 5 * (LIQUIDITY / 100);
    assert!(page.iter().all(|p| p.liquidity.0 == active));
    assert_eq!(
        c.get_pools(0, 1000).len(),
        20,
        "limit capped at 100, not an error"
    );
    assert!(c.get_pool_summary(pools[3]).is_some());
}

#[test]
fn migrate_backfills_summaries_for_existing_pools() {
    let mut c = with_pair_tokens();
    ctx(OWNER, 0);
    c.add_token(acct(FAKE));
    c.create_pool(WNEAR.into(), FAKE.into(), TICK_SPACING, Q96_STR.into());
    env::state_write(&permissionless::into_v8(c));
    let migrated = Contract::migrate();
    let s = migrated.get_pool_summary(0).expect("backfilled");
    assert_eq!(s.token1, FAKE);
    assert_eq!(s.swap_count, 0);
    assert_eq!(migrated.get_pools(0, 10).len(), 1);
}
