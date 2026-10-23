//! Regression test for the fee leak between NFTs that share a pool position.
//!
//! The NFT manager keeps the liquidity of all its NFTs in one pool position per range. The owed
//! amounts of that pool position belong to all of them, so each NFT must be paid only its own
//! share, computed from its fee-growth snapshot and liquidity (`NonfungiblePositionManager`).
//! Before the fix, `collect` paid the whole pool position's owed amounts, so the first owner to
//! collect took the other owners' fees.

use windswap_engine::gauge::{ClGauge, GaugeConfig, PositionManager};
use windswap_engine::math::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};
use windswap_engine::math::Q96;
use windswap_engine::pool::cl_pool::{AccountId, ClPool};
use windswap_engine::{I256, U256};

const SPACING: i32 = 10;
const LOWER: i32 = -100;
const UPPER: i32 = 100;
/// Swap fee in hundredths of a bip (0.3%). `unstaked_fee` is 0, so no fee goes to the gauge.
const FEE: u32 = 3_000;
const T0: u32 = 1_000;
const MAX: u128 = u128::MAX;

const NFT_ACCOUNT: AccountId = [0x90; 32];
const GAUGE: AccountId = [0x91; 32];
const VOTER: AccountId = [0x92; 32];
const ADMIN: AccountId = [0x93; 32];
const TOKEN0: AccountId = [0xA0; 32];
const TOKEN1: AccountId = [0xA1; 32];

fn owner(n: u8) -> AccountId {
    [n; 32]
}

fn new_pool() -> ClPool {
    let mut pool = ClPool::default();
    pool.initialize(SPACING, Q96, T0).unwrap();
    pool.increase_observation_cardinality_next(4).unwrap();
    pool
}

/// One swap of `amount` in the given direction, inside the range.
fn trade(pool: &mut ClPool, zero_for_one: bool, amount: &str, time: u32) {
    let limit = if zero_for_one {
        MIN_SQRT_RATIO + U256::from(1)
    } else {
        MAX_SQRT_RATIO - U256::from(1)
    };
    let amount: I256 = amount.parse().unwrap();
    pool.swap(zero_for_one, amount, limit, FEE, 0, time)
        .unwrap();
}

/// Both directions, so both tokens earn fees.
fn trade_both_ways(pool: &mut ClPool, time: u32) {
    trade(pool, true, "1000000000", time);
    trade(pool, false, "1000000000", time + 30);
}

/// The fees the pool position `NFT_ACCOUNT` holds for the range: a poke (`burn(0)`), then the owed
/// amounts. This is what the NFT manager's pool position owes all of its NFTs together.
fn pool_fees(pool: &mut ClPool, time: u32) -> (u128, u128) {
    pool.burn(NFT_ACCOUNT, LOWER, UPPER, 0, time).unwrap();
    let info = pool.position(&NFT_ACCOUNT, LOWER, UPPER);
    (info.tokens_owed0, info.tokens_owed1)
}

/// Two owners with unstaked NFTs in the same range, liquidity 3:1. Returns the manager and the
/// token ids of A and B.
fn two_owner_range(pool: &mut ClPool) -> (PositionManager, U256, U256) {
    let mut nft = PositionManager::new(NFT_ACCOUNT);
    let (a, b) = (owner(1), owner(2));
    let (ta, _, _) = nft
        .mint(
            pool,
            a,
            TOKEN0,
            TOKEN1,
            SPACING,
            LOWER,
            UPPER,
            3_000_000_000_000,
            T0,
        )
        .unwrap();
    let (tb, _, _) = nft
        .mint(
            pool,
            b,
            TOKEN0,
            TOKEN1,
            SPACING,
            LOWER,
            UPPER,
            1_000_000_000_000,
            T0,
        )
        .unwrap();
    (nft, ta, tb)
}

#[test]
fn owners_of_one_range_collect_only_their_own_fees() {
    let mut pool = new_pool();
    let (mut nft, ta, tb) = two_owner_range(&mut pool);
    let (a, b) = (owner(1), owner(2));

    trade_both_ways(&mut pool, T0 + 60);
    let (total0, total1) = pool_fees(&mut pool, T0 + 100);
    assert!(total0 > 1_000 && total1 > 1_000, "the swaps must earn fees");

    // A collects first. Its share is 3/4 of the range's fees, not all of them.
    let (a0, a1) = nft.collect(&mut pool, a, ta, MAX, MAX, T0 + 100).unwrap();
    assert!(
        a0.abs_diff(total0 / 4 * 3) <= 2 && a1.abs_diff(total1 / 4 * 3) <= 2,
        "A got {a0},{a1}; its share of {total0},{total1} is 3/4"
    );

    // B then collects its 1/4, which the first collect must not have taken.
    let (b0, b1) = nft.collect(&mut pool, b, tb, MAX, MAX, T0 + 100).unwrap();
    assert!(
        b0.abs_diff(total0 / 4) <= 2 && b1.abs_diff(total1 / 4) <= 2,
        "B got {b0},{b1}; its share of {total0},{total1} is 1/4"
    );

    // The two shares add up to the range's fees, up to rounding (each share rounds down).
    assert!(a0 + b0 <= total0 && total0 - (a0 + b0) <= 2);
    assert!(a1 + b1 <= total1 && total1 - (a1 + b1) <= 2);

    // Nothing is left to collect for either, and the pool position has nothing left either.
    assert_eq!(
        nft.collect(&mut pool, a, ta, MAX, MAX, T0 + 100).unwrap(),
        (0, 0)
    );
    assert_eq!(
        nft.collect(&mut pool, b, tb, MAX, MAX, T0 + 100).unwrap(),
        (0, 0)
    );
    let left = pool.position(&NFT_ACCOUNT, LOWER, UPPER);
    assert!(left.tokens_owed0 <= 2 && left.tokens_owed1 <= 2);
}

#[test]
fn fees_earned_after_one_collect_are_still_split_by_liquidity() {
    let mut pool = new_pool();
    let (mut nft, ta, tb) = two_owner_range(&mut pool);
    let (a, b) = (owner(1), owner(2));

    trade_both_ways(&mut pool, T0 + 60);
    let (a0_first, _) = nft.collect(&mut pool, a, ta, MAX, MAX, T0 + 100).unwrap();
    assert!(a0_first > 0);
    // What the pool position still owes after A's collect: B's share of the first round.
    let (left0, left1) = pool_fees(&mut pool, T0 + 100);
    assert!(
        left0 > 1_000 && left1 > 1_000,
        "B's first-round share is still owed"
    );

    // More trading. A's new fees are 3/4 of the second round and B's are 1/4, on top of its
    // first-round share, which A's collect must not have touched.
    trade_both_ways(&mut pool, T0 + 200);
    let (total0, total1) = pool_fees(&mut pool, T0 + 300);
    let (round0, round1) = (total0 - left0, total1 - left1);
    let (a0, a1) = nft.collect(&mut pool, a, ta, MAX, MAX, T0 + 300).unwrap();
    let (b0, b1) = nft.collect(&mut pool, b, tb, MAX, MAX, T0 + 300).unwrap();
    assert!(a0.abs_diff(round0 / 4 * 3) <= 3 && a1.abs_diff(round1 / 4 * 3) <= 3);
    assert!(b0.abs_diff(left0 + round0 / 4) <= 3 && b1.abs_diff(left1 + round1 / 4) <= 3);
}

#[test]
fn unstaking_pays_principal_and_only_the_owners_fees() {
    let mut pool = new_pool();
    let (mut nft, ta, tb) = two_owner_range(&mut pool);
    let (a, b) = (owner(1), owner(2));

    trade_both_ways(&mut pool, T0 + 60);
    let (total0, total1) = pool_fees(&mut pool, T0 + 100);

    // A withdraws all its liquidity. It gets the burned principal and its 3/4 of the fees.
    let (p0, p1) = nft
        .decrease_liquidity(&mut pool, a, ta, 3_000_000_000_000, T0 + 100)
        .unwrap();
    assert!(
        p0 > U256::ZERO && p1 > U256::ZERO,
        "A's principal is in both tokens"
    );
    let (a0, a1) = nft.collect(&mut pool, a, ta, MAX, MAX, T0 + 100).unwrap();
    let (pa0, pa1) = (p0.to::<u128>(), p1.to::<u128>());
    assert!(
        a0.abs_diff(pa0 + total0 / 4 * 3) <= 2,
        "A: principal plus 3/4 of the fees"
    );
    assert!(
        a1.abs_diff(pa1 + total1 / 4 * 3) <= 2,
        "A: principal plus 3/4 of the fees"
    );

    // B withdraws: its principal and its 1/4 of the fees, which A's withdrawal left alone.
    let (q0, q1) = nft
        .decrease_liquidity(&mut pool, b, tb, 1_000_000_000_000, T0 + 100)
        .unwrap();
    let (b0, b1) = nft.collect(&mut pool, b, tb, MAX, MAX, T0 + 100).unwrap();
    let (qb0, qb1) = (q0.to::<u128>(), q1.to::<u128>());
    assert!(
        b0.abs_diff(qb0 + total0 / 4) <= 2,
        "B: principal plus 1/4 of the fees"
    );
    assert!(
        b1.abs_diff(qb1 + total1 / 4) <= 2,
        "B: principal plus 1/4 of the fees"
    );
}

/// Staked positions (the gauge holds the NFTs) earn no LP fees from the pool position: the pool
/// routes the staked fees to the gauge. Two stakers in one range therefore collect no fees at all
/// on withdraw, and the fees are in the gauge.
#[test]
fn staked_positions_in_one_range_collect_no_lp_fees() {
    let mut pool = new_pool();
    pool.set_gauge_and_position_manager(GAUGE, NFT_ACCOUNT)
        .unwrap();
    let mut nft = PositionManager::new(NFT_ACCOUNT);
    let mut gauge = ClGauge::new(GaugeConfig {
        account: GAUGE,
        voter: VOTER,
        notify_admin: ADMIN,
        token0: TOKEN0,
        token1: TOKEN1,
        tick_spacing: SPACING,
        is_pool: true,
    });
    let (a, b) = (owner(1), owner(2));
    let mut tokens = Vec::new();
    for (who, liquidity) in [(a, 3_000_000_000_000u128), (b, 1_000_000_000_000)] {
        let (t, _, _) = nft
            .mint(
                &mut pool, who, TOKEN0, TOKEN1, SPACING, LOWER, UPPER, liquidity, T0,
            )
            .unwrap();
        nft.set_approval_for_all(who, GAUGE, true);
        gauge.deposit(&mut pool, &mut nft, who, t, T0 + 1).unwrap();
        tokens.push((who, t));
    }

    trade_both_ways(&mut pool, T0 + 60);
    let (fees0, fees1) = (pool.gauge_fees0, pool.gauge_fees1);
    assert!(fees0 > 1 && fees1 > 1, "the staked fees go to the gauge");

    for (who, t) in tokens {
        let (w0, w1, _) = gauge
            .withdraw(&mut pool, &mut nft, who, t, T0 + 700)
            .unwrap();
        assert_eq!((w0, w1), (0, 0), "a staked NFT collects no LP fees");
    }
}
