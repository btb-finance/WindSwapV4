//! Tests of the solvency accounting (`accounting`).
//!
//! The main test is a seeded random sequence of deposits, withdrawals, swaps (single and
//! multi-hop, exact in and exact out), mints, burns, collects, locks and time. After every step
//! it checks, for each token:
//! - the contract's totals equal this test's own ledger, rebuilt from the amounts the calls
//!   returned (balances, pool reserves, locked WIND);
//! - the holding the test simulates (deposits and mints, minus withdrawals) covers `total_owed`,
//!   and exceeds it by no more than `DUST_BOUND`.
//!
//! The mutation tests skip the reserve update of one flow and check that the same sequence fails.

use std::collections::BTreeMap;

use super::*;
use crate::accounting::{fault, Flow};
use crate::accounts::{owner_hex, owner_of_account};
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, PromiseError};
use windswap_engine::minter::WEEK;

const OWNER: &str = "owner.testnet";
const SELF: &str = "windswapv4-btb.testnet";
const ALICE: &str = "alice.testnet";
const BOB: &str = "bob.testnet";
const CAROL: &str = "carol.testnet";
const RELAYER: &str = "relayer.testnet";
const USDC: &str = "usdc.testnet";
const WNEAR: &str = "wnear.testnet";
const WIND: &str = "wind.testnet";
const SPACING: i32 = 3;
const Q96: &str = "79228162514264337593543950336";
const MIN_SQRT_LIMIT: &str = "4295128740";
const MAX_SQRT_LIMIT: &str = "1461446703485210103287273052203988822378723970341";
const E18: u128 = 1_000_000_000_000_000_000;
const T0: u64 = 100 * WEEK;
const TOKENS: [&str; 3] = [USDC, WNEAR, WIND];
const USERS: [&str; 3] = [ALICE, BOB, CAROL];
/// Surplus of the holding over `total_owed` that the checks accept. Every token movement is
/// recorded in the totals, so the measured surplus is zero. Kept at zero, not a tolerance.
const DUST_BOUND: u128 = 0;

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}

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

/// Owner, three tokens (all whitelisted, WIND set), and two pools: 0 is USDC/WNEAR, 1 is
/// WNEAR/WIND.
fn setup() -> Contract {
    ctx(OWNER, T0, 0);
    let mut c = Contract::new(acct(OWNER));
    c.add_token(acct(USDC));
    c.add_token(acct(WNEAR));
    c.add_token(acct(WIND));
    c.set_wind_token(acct(WIND));
    assert_eq!(
        c.create_pool(USDC.into(), WNEAR.into(), SPACING, Q96.into()),
        0
    );
    assert_eq!(
        c.create_pool(WNEAR.into(), WIND.into(), SPACING, Q96.into()),
        1
    );
    c
}

/// splitmix64: a small deterministic generator, so a failing seed can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi`.
    fn range(&mut self, lo: u128, hi: u128) -> u128 {
        lo + (self.next() as u128) % (hi - lo + 1)
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() as usize) % items.len()]
    }
}

#[derive(Clone)]
struct LockRec {
    user: &'static str,
    amount: u128,
    end: u64,
}

/// The test's own bookkeeping. Each entry is updated from what a call returned, not from the
/// contract's totals.
#[derive(Default)]
struct Ledger {
    /// Internal balance of each user per token.
    balances: BTreeMap<(&'static str, &'static str), u128>,
    /// Tokens the contract holds, simulated: deposits and mints in, withdrawals out.
    held: BTreeMap<&'static str, u128>,
    /// Pool reserves per token, from the amounts that entered and left the pools.
    reserves: BTreeMap<&'static str, i128>,
    locks: BTreeMap<u64, LockRec>,
    /// Liquidity each user holds per `(pool, lower, upper)`.
    positions: BTreeMap<(&'static str, u64, i32, i32), u128>,
}

impl Ledger {
    fn balance(&self, user: &str, token: &str) -> u128 {
        self.balances
            .iter()
            .find(|((u, t), _)| *u == user && *t == token)
            .map(|(_, v)| *v)
            .unwrap_or(0)
    }

    fn add(&mut self, user: &'static str, token: &'static str, amount: u128) {
        *self.balances.entry((user, token)).or_default() += amount;
    }

    fn sub(&mut self, user: &'static str, token: &'static str, amount: u128) {
        let entry = self.balances.entry((user, token)).or_default();
        *entry = entry.checked_sub(amount).expect("ledger went negative");
    }

    fn user_total(&self, token: &str) -> u128 {
        self.balances
            .iter()
            .filter(|((_, t), _)| *t == token)
            .map(|(_, v)| *v)
            .sum()
    }

    fn locked_total(&self, token: &str) -> u128 {
        if token != WIND {
            return 0;
        }
        self.locks.values().map(|l| l.amount).sum()
    }

    fn reserve(&self, token: &str) -> i128 {
        self.reserves.get(token).copied().unwrap_or(0)
    }

    fn reserve_add(&mut self, token: &'static str, delta: i128) {
        *self.reserves.entry(token).or_default() += delta;
    }

    fn hold(&mut self, token: &'static str, delta: i128) {
        let next = self.held.get(token).copied().unwrap_or(0) as i128 + delta;
        assert!(next >= 0, "simulated holding went negative");
        self.held.insert(token, next as u128);
    }
}

struct Sim {
    c: Contract,
    l: Ledger,
    rng: Rng,
    now: u64,
    /// How many times each kind of operation ran and changed something.
    ran: BTreeMap<&'static str, u32>,
}

/// A route: its hops, and its input and output tokens.
fn route(kind: u64) -> (Vec<HopInput>, &'static str, &'static str) {
    let hop = |pool_id, token: &str| HopInput {
        pool_id,
        token_in: token.to_string(),
    };
    match kind {
        0 => (vec![hop(0, USDC)], USDC, WNEAR),
        1 => (vec![hop(0, WNEAR)], WNEAR, USDC),
        2 => (vec![hop(1, WNEAR)], WNEAR, WIND),
        3 => (vec![hop(1, WIND)], WIND, WNEAR),
        4 => (vec![hop(0, USDC), hop(1, WNEAR)], USDC, WIND),
        _ => (vec![hop(1, WIND), hop(0, WNEAR)], WIND, USDC),
    }
}

/// Whether a swap in this direction is allowed at the pool's current price. The engine refuses a
/// limit that is not beyond the price, so a pool pushed to a bound by an earlier swap is skipped.
fn direction_allowed(c: &Contract, pool: u64, zero_for_one: bool) -> bool {
    let price: U256 = parse_u256(&c.get_pool(pool).slot0.sqrt_price_x96);
    let min = parse_u256(MIN_SQRT_LIMIT);
    let max = parse_u256(MAX_SQRT_LIMIT);
    if zero_for_one {
        price > min
    } else {
        price < max
    }
}

impl Sim {
    fn new(seed: u64) -> Self {
        Sim {
            c: setup(),
            l: Ledger::default(),
            rng: Rng(seed),
            now: T0,
            ran: BTreeMap::new(),
        }
    }

    fn deposit(&mut self, user: &'static str, token: &'static str, amount: u128) {
        ctx(token, self.now, 0);
        let _ = self
            .c
            .ft_on_transfer(acct(user), U128(amount), String::new());
        self.l.add(user, token, amount);
        self.l.hold(token, amount as i128);
    }

    /// Deposits first if the user's balance of `token` is below `need`.
    fn ensure(&mut self, user: &'static str, token: &'static str, need: u128) {
        let have = self.l.balance(user, token);
        if have < need {
            self.deposit(user, token, need - have + 10 * E18);
        }
    }

    fn withdraw(&mut self, user: &'static str, token: &'static str, amount: u128) {
        ctx(user, self.now, 1);
        let _ = self.c.withdraw(acct(token), U128(amount));
        self.l.sub(user, token, amount);
        self.l.hold(token, -(amount as i128));
    }

    /// Whether `token_in` is token0 of `pool`.
    fn token_zero_for_one(&self, pool: u64) -> &'static str {
        if pool == 0 {
            USDC
        } else {
            WNEAR
        }
    }

    fn swap_route(&mut self, user: &'static str) {
        let kind = self.rng.range(0, 5) as u64;
        let (path, token_in, token_out) = route(kind);
        let exact_input = self.rng.range(0, 1) == 0;
        let all_allowed = path.iter().all(|h| {
            let zfo = h.token_in == self.token_zero_for_one(h.pool_id);
            direction_allowed(&self.c, h.pool_id, zfo)
        });
        if !all_allowed {
            return;
        }
        if exact_input {
            let amount = self.rng.range(E18 / 1_000, E18 / 5);
            self.ensure(user, token_in, amount);
            ctx(user, self.now, 0);
            // Routes that would run out of liquidity now revert (InsufficientLiquidity) instead
            // of charging for unconsumed input; skip them, as a panicking call changes nothing.
            let quote = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.c.quote_exact_input(path.clone(), U128(amount)).0
            }));
            if quote.is_err() {
                return;
            }
            let out = self.c.swap_exact_input(path, U128(amount), U128(0)).0;
            self.l.sub(user, token_in, amount);
            self.l.add(user, token_out, out);
            self.l.reserve_add(token_in, amount as i128);
            self.l.reserve_add(token_out, -(out as i128));
        } else {
            // Exact output needs liquidity on every hop, so the call cannot panic on price.
            if path
                .iter()
                .any(|h| self.c.get_pool(h.pool_id).liquidity.0 == 0)
            {
                return;
            }
            let want = self.rng.range(E18 / 1_000_000, E18 / 10_000);
            ctx(user, self.now, 0);
            let quoted = self.c.quote_exact_output(path.clone(), U128(want)).0;
            if quoted == 0 || self.l.balance(user, token_in) < quoted + quoted / 10 + 1 {
                self.ensure(user, token_in, quoted + quoted / 10 + 1);
                ctx(user, self.now, 0);
            }
            let paid = self
                .c
                .swap_exact_output(path, U128(want), U128(u128::MAX))
                .0;
            self.l.sub(user, token_in, paid);
            self.l.add(user, token_out, want);
            self.l.reserve_add(token_in, paid as i128);
            self.l.reserve_add(token_out, -(want as i128));
        }
    }

    fn single_swap(&mut self, user: &'static str) {
        let pool = self.rng.range(0, 1) as u64;
        let (token0, token1) = if pool == 0 {
            (USDC, WNEAR)
        } else {
            (WNEAR, WIND)
        };
        let zero_for_one = self.rng.range(0, 1) == 0;
        if !direction_allowed(&self.c, pool, zero_for_one) {
            return;
        }
        let token_in = if zero_for_one { token0 } else { token1 };
        let amount = self.rng.range(E18 / 1_000, E18 / 5);
        self.ensure(user, token_in, amount);
        let limit = if zero_for_one {
            MIN_SQRT_LIMIT
        } else {
            MAX_SQRT_LIMIT
        };
        ctx(user, self.now, 0);
        let [a0, a1] = self
            .c
            .swap(pool, zero_for_one, amount.to_string(), limit.into());
        for (token, delta) in [(token0, a0), (token1, a1)] {
            let delta: i128 = delta.parse().expect("signed amount");
            if delta > 0 {
                self.l.sub(user, token, delta as u128);
            } else if delta < 0 {
                self.l.add(user, token, delta.unsigned_abs());
            }
            self.l.reserve_add(token, delta);
        }
    }

    fn mint(&mut self, user: &'static str) {
        let pool = self.rng.range(0, 1) as u64;
        let (token0, token1) = if pool == 0 {
            (USDC, WNEAR)
        } else {
            (WNEAR, WIND)
        };
        let lower = -(SPACING * self.rng.range(1, 200) as i32);
        let upper = SPACING * self.rng.range(1, 200) as i32;
        let liquidity = self.rng.range(E18 / 100, E18);
        self.ensure(user, token0, E18 / 10);
        self.ensure(user, token1, E18 / 10);
        ctx(user, self.now, 0);
        let [a0, a1] = self.c.mint_position(pool, lower, upper, U128(liquidity));
        let (a0, a1): (u128, u128) = (a0.parse().unwrap(), a1.parse().unwrap());
        self.l.sub(user, token0, a0);
        self.l.sub(user, token1, a1);
        self.l.reserve_add(token0, a0 as i128);
        self.l.reserve_add(token1, a1 as i128);
        *self
            .l
            .positions
            .entry((user, pool, lower, upper))
            .or_default() += liquidity;
    }

    fn burn(&mut self, user: &'static str) {
        let held: Vec<_> = self
            .l
            .positions
            .iter()
            .filter(|((u, _, _, _), liq)| *u == user && **liq > 0)
            .map(|(k, v)| (*k, *v))
            .collect();
        let Some(((_, pool, lower, upper), liquidity)) = held.first().cloned() else {
            return;
        };
        let amount = self.rng.range(1, liquidity);
        ctx(user, self.now, 0);
        let _ = self.c.burn_position(pool, lower, upper, U128(amount));
        *self
            .l
            .positions
            .get_mut(&(user, pool, lower, upper))
            .unwrap() -= amount;
        // Burning moves liquidity into owed amounts inside the pool. No token moves yet.
    }

    fn collect(&mut self, user: &'static str) {
        let (pool, lower, upper) = match self.l.positions.keys().find(|(u, _, _, _)| *u == user) {
            Some((_, p, lo, up)) => (*p, *lo, *up),
            None => (0, -SPACING * 3, SPACING * 3),
        };
        ctx(user, self.now, 0);
        let [a0, a1] = self.c.collect(pool, lower, upper);
        let (token0, token1) = if pool == 0 {
            (USDC, WNEAR)
        } else {
            (WNEAR, WIND)
        };
        self.l.add(user, token0, a0.0);
        self.l.add(user, token1, a1.0);
        self.l.reserve_add(token0, -(a0.0 as i128));
        self.l.reserve_add(token1, -(a1.0 as i128));
    }

    fn create_lock(&mut self, user: &'static str) {
        let amount = self.rng.range(E18, 10 * E18);
        let duration = WEEK * self.rng.range(1, 4) as u64;
        self.ensure(user, WIND, amount);
        ctx(user, self.now, 0);
        let token = self.c.create_lock(U128(amount), duration, None);
        let end = self.c.get_lock(token).expect("lock").end;
        self.l.sub(user, WIND, amount);
        self.l.locks.insert(token, LockRec { user, amount, end });
    }

    fn increase_lock(&mut self, user: &'static str) {
        let Some((token, rec)) = self
            .l
            .locks
            .iter()
            .find(|(_, l)| l.user == user && l.end > self.now)
            .map(|(t, l)| (*t, l.clone()))
        else {
            return;
        };
        let amount = self.rng.range(E18 / 10, E18);
        self.ensure(user, WIND, amount);
        ctx(user, self.now, 0);
        self.c.increase_amount(token, U128(amount));
        self.l.sub(user, WIND, amount);
        self.l.locks.get_mut(&token).unwrap().amount = rec.amount + amount;
    }

    fn withdraw_lock(&mut self, user: &'static str) {
        let Some((token, rec)) = self
            .l
            .locks
            .iter()
            .find(|(_, l)| l.user == user && l.end <= self.now)
            .map(|(t, l)| (*t, l.clone()))
        else {
            return;
        };
        ctx(user, self.now, 0);
        let paid = self.c.withdraw_lock(token).0;
        assert_eq!(paid, rec.amount, "the lock returns what it holds");
        self.l.add(user, WIND, paid);
        self.l.locks.remove(&token);
    }

    fn step(&mut self) {
        let user = self.rng.pick(&USERS);
        let kind = match self.rng.range(0, 99) {
            0..=14 => {
                let token = self.rng.pick(&TOKENS);
                let amount = self.rng.range(E18, 50 * E18);
                self.deposit(user, token, amount);
                "deposit"
            }
            15..=21 => {
                let token = self.rng.pick(&TOKENS);
                let have = self.l.balance(user, token);
                if have > 0 {
                    let amount = self.rng.range(1, have);
                    self.withdraw(user, token, amount);
                    "withdraw"
                } else {
                    "withdraw (skipped)"
                }
            }
            22..=35 => {
                self.swap_route(user);
                "route swap"
            }
            36..=42 => {
                self.single_swap(user);
                "single swap"
            }
            43..=56 => {
                self.mint(user);
                "mint"
            }
            57..=63 => {
                self.burn(user);
                "burn"
            }
            64..=70 => {
                self.collect(user);
                "collect"
            }
            71..=77 => {
                self.create_lock(user);
                "lock"
            }
            78..=82 => {
                self.increase_lock(user);
                "increase lock"
            }
            83..=86 => {
                self.withdraw_lock(user);
                "withdraw lock"
            }
            _ => {
                self.now += self.rng.range(3_600, 3 * WEEK as u128) as u64;
                "time"
            }
        };
        *self.ran.entry(kind).or_default() += 1;
    }

    /// Compares the contract's totals with the ledger and the simulated holding.
    fn check(&self) -> Result<(), String> {
        for token in TOKENS {
            let acc = self.c.get_token_accounting(token.to_string());
            let liabilities = self.l.user_total(token);
            if acc.liabilities.0 != liabilities {
                return Err(format!(
                    "{token}: liabilities {} != ledger {liabilities}",
                    acc.liabilities.0
                ));
            }
            let reserves = self.l.reserve(token);
            if acc.pool_reserves.0 as i128 != reserves {
                return Err(format!(
                    "{token}: pool reserves {} != ledger {reserves}",
                    acc.pool_reserves.0
                ));
            }
            let locked = self.l.locked_total(token);
            if acc.locked.0 != locked {
                return Err(format!(
                    "{token}: locked {} != ledger {locked}",
                    acc.locked.0
                ));
            }
            if acc.pending_withdrawals.0 != 0 || acc.unbacked.0 != 0 {
                return Err(format!("{token}: unexpected pending or unbacked"));
            }
            let owed = liabilities + reserves as u128 + locked;
            if acc.total_owed.0 != owed {
                return Err(format!(
                    "{token}: total_owed {} != {owed}",
                    acc.total_owed.0
                ));
            }
            let held = self.l.held.get(token).copied().unwrap_or(0);
            if held < owed {
                return Err(format!("{token}: held {held} < owed {owed}"));
            }
            if held - owed > DUST_BOUND {
                return Err(format!("{token}: surplus {} > bound", held - owed));
            }
        }
        let all = self.c.get_all_tokens_accounting();
        if all.len() != TOKENS.len() {
            return Err(format!(
                "get_all_tokens_accounting lists {} tokens",
                all.len()
            ));
        }
        Ok(())
    }
}

/// Runs `steps` random operations from `seed`. Returns the first failed check, with its step,
/// and how many times each kind of operation ran.
/// Runs on a thread of its own: the mock storage is per thread, so each run starts empty.
/// `skip` is a flow whose reserve update is skipped (fault injection).
fn run(
    seed: u64,
    steps: usize,
    skip: Option<Flow>,
) -> (Result<(), String>, BTreeMap<&'static str, u32>) {
    std::thread::spawn(move || {
        if let Some(flow) = skip {
            fault::skip(flow);
        }
        let mut sim = Sim::new(seed);
        let result = (|| {
            sim.check()?;
            for i in 0..steps {
                sim.step();
                sim.check()
                    .map_err(|e| format!("seed {seed}, step {i}: {e}"))?;
            }
            Ok(())
        })();
        (result, sim.ran)
    })
    .join()
    .unwrap_or_else(|_| {
        (
            Err(format!("seed {seed}: a call panicked")),
            BTreeMap::new(),
        )
    })
}

#[test]
fn random_sequences_keep_totals_equal_to_the_ledger_and_the_holding_covered() {
    let mut ran: BTreeMap<&'static str, u32> = BTreeMap::new();
    for seed in [1, 2, 3, 4, 5, 6] {
        let (result, counts) = run(seed, 300, None);
        if let Err(e) = result {
            panic!("{e}");
        }
        for (kind, n) in counts {
            *ran.entry(kind).or_default() += n;
        }
    }
    // The sequences must actually reach every kind of operation, or the checks prove little.
    for kind in [
        "deposit",
        "withdraw",
        "route swap",
        "single swap",
        "mint",
        "burn",
        "collect",
        "lock",
        "increase lock",
        "withdraw lock",
        "time",
    ] {
        assert!(
            ran.get(kind).copied().unwrap_or(0) >= 5,
            "{kind} ran {:?} times",
            ran.get(kind)
        );
    }
}

#[test]
fn skipping_the_mint_reserve_update_fails_the_checks() {
    let (result, _) = run(1, 300, Some(Flow::Mint));
    assert!(result.is_err(), "a missed mint update must be caught");
}

#[test]
fn skipping_the_collect_reserve_update_fails_the_checks() {
    let (result, _) = run(1, 300, Some(Flow::Collect));
    assert!(result.is_err(), "a missed collect update must be caught");
}

#[test]
fn skipping_the_swap_reserve_update_fails_the_checks() {
    let (result, _) = run(1, 300, Some(Flow::Swap));
    assert!(result.is_err(), "a missed swap update must be caught");
}

#[test]
fn the_totals_are_zero_on_a_fresh_contract() {
    let c = setup();
    for token in TOKENS {
        let acc = c.get_token_accounting(token.to_string());
        assert_eq!(acc.total_owed.0, 0);
        assert!(!acc.migrated, "a fresh contract needs no seeding");
    }
}

/// The weekly cycle of `tokenomics_tests`, with the solvency identity checked after every step.
/// There is no ledger here, so the check is the holding the test simulates against `total_owed`,
/// which must be exact. It covers gauge emissions, fee and bribe claims, stakes and rebases.
#[test]
fn gauge_cycle_keeps_holdings_equal_to_total_owed() {
    const GENESIS: u128 = 1_000_000 * E18;
    let mut c = setup();
    let mut held: BTreeMap<&str, u128> = BTreeMap::new();
    let check = |c: &Contract, held: &BTreeMap<&str, u128>, label: &str| {
        for token in [USDC, WNEAR, WIND] {
            let acc = c.get_token_accounting(token.to_string());
            let h = held.get(token).copied().unwrap_or(0);
            assert_eq!(
                h, acc.total_owed.0,
                "{label}: {token} held {h}, owed {} (liabilities {}, reserves {}, locked {})",
                acc.total_owed.0, acc.liabilities.0, acc.pool_reserves.0, acc.locked.0
            );
        }
    };

    // Genesis supply: minted to the contract, credited to the team.
    ctx(OWNER, T0, 0);
    let _ = c.genesis_mint(U128(GENESIS));
    ctx(SELF, T0, 0);
    c.on_genesis_minted(U128(GENESIS), Ok(()));
    *held.entry(WIND).or_default() += GENESIS;
    check(&c, &held, "genesis");

    let deposit = |c: &mut Contract,
                   held: &mut BTreeMap<&str, u128>,
                   who: &str,
                   token: &'static str,
                   amount: u128| {
        ctx(token, T0, 0);
        let _ = c.ft_on_transfer(acct(who), U128(amount), String::new());
        *held.entry(token).or_default() += amount;
    };

    ctx(OWNER, T0, 0);
    let gauge = c.create_gauge(0);
    assert_eq!(gauge, 1);

    // ALICE locks WIND, provides liquidity and stakes it.
    deposit(&mut c, &mut held, ALICE, WIND, 1_000 * E18);
    ctx(ALICE, T0 + 10, 0);
    let token = c.create_lock(U128(1_000 * E18), 4 * WEEK, None);
    deposit(&mut c, &mut held, ALICE, USDC, 10 * E18);
    deposit(&mut c, &mut held, ALICE, WNEAR, 10 * E18);
    ctx(ALICE, T0 + 20, 0);
    c.mint_position(0, -300, 300, U128(E18));
    check(&c, &held, "alice provides");
    ctx(ALICE, T0 + 30, 0);
    c.stake_position(0, -300, 300);
    check(&c, &held, "alice stakes");

    ctx(ALICE, T0 + 2 * 3_600, 0);
    c.vote(token, vec![0], vec![U128(1)]);

    // BOB trades (fees for the gauge) and bribes it.
    deposit(&mut c, &mut held, BOB, USDC, 100 * E18);
    deposit(&mut c, &mut held, BOB, WNEAR, 100 * E18);
    ctx(BOB, T0 + 3 * 3_600, 0);
    c.swap(0, true, "1000000000000000".into(), MIN_SQRT_LIMIT.into());
    c.swap(0, false, "1000000000000000".into(), MAX_SQRT_LIMIT.into());
    check(&c, &held, "bob swaps");
    c.bribe(0, USDC.into(), U128(5 * E18));
    check(&c, &held, "bob bribes");

    // The first epoch flip mints the weekly emission.
    let minted = flip(&mut c, T0 + WEEK + 2 * 3_600);
    *held.entry(WIND).or_default() += minted;
    check(&c, &held, "first flip");

    ctx(ALICE, T0 + WEEK + 3 * 3_600, 0);
    let emissions = c.claim_gauge_rewards(0, -300, 300).0;
    assert!(emissions > 0, "the staked position earned emissions");
    check(&c, &held, "emissions claimed");

    let minted2 = flip(&mut c, T0 + 2 * WEEK + 2 * 3_600);
    *held.entry(WIND).or_default() += minted2;
    check(&c, &held, "second flip");

    ctx(ALICE, T0 + 2 * WEEK + 3 * 3_600, 0);
    let fees = c.claim_fees(token, vec![gauge]);
    assert!(!fees.is_empty());
    check(&c, &held, "fees claimed");
    let _ = c.claim_bribes(token, vec![gauge], vec![USDC.into()]);
    check(&c, &held, "bribes claimed");
    let _ = c.claim_rebase(token);
    check(&c, &held, "rebase claimed");

    // Unstake returns the position, its emissions and its fees.
    c.unstake_position(0, -300, 300);
    check(&c, &held, "unstaked");

    // BOB withdraws everything he can.
    for token in [USDC, WNEAR] {
        let amount = bal(&c, BOB, token);
        if amount > 0 {
            ctx(BOB, T0 + 2 * WEEK + 4 * 3_600, 1);
            let _ = c.withdraw(acct(token), U128(amount));
            *held.get_mut(token).unwrap() -= amount;
        }
    }
    check(&c, &held, "bob withdraws");
}

fn bal(c: &Contract, who: &str, token: &str) -> u128 {
    c.get_balance(acct(who), token.to_string()).0
}

/// The epoch flip at `secs`, with the mint its `distribute` asks for. Returns the minted amount.
fn flip(c: &mut Contract, secs: u64) -> u128 {
    ctx(BOB, secs, 0);
    let _ = c.distribute();
    let amount = c
        .pending_epoch
        .as_ref()
        .map(|p| to_u128(p.emission.mint_required))
        .unwrap_or(0);
    ctx(SELF, secs, 0);
    c.on_distribute_minted(U128(amount), Ok(()));
    amount
}

// ---- migration and seeding ----

fn owner_hex_of(name: &str) -> String {
    owner_hex(&owner_of_account(&acct(name)))
}

/// Writes a v6 state holding two balances, one pool, and three EVM withdrawals of which two
/// are not yet broadcast.
fn write_v6_state() {
    let alice = owner_of_account(&acct(ALICE));
    let bob = owner_of_account(&acct(BOB));
    let mut balances = LookupMap::new(StorageKey::OwnerBalances);
    balances.insert((bob, USDC.to_string()), 7);
    balances.insert((alice, WIND.to_string()), 100 * E18);
    let mut allowed_tokens = IterableSet::new(StorageKey::AllowedTokens);
    allowed_tokens.insert(acct(USDC));
    allowed_tokens.insert(acct(WIND));
    let mut pools = IterableMap::new(StorageKey::Pools);
    let mut pool = ClPool::default();
    pool.initialize(SPACING, parse_u256(Q96), T0 as u32)
        .expect("initialize");
    pools.insert(0, pool);
    let mut pool_tokens = LookupMap::new(StorageKey::PoolTokens);
    pool_tokens.insert(0, (USDC.to_string(), WNEAR.to_string()));
    let mut withdrawals = LookupMap::new(StorageKey::Withdrawals);
    let w = |status| Withdrawal {
        owner: alice,
        token_id: "base-sepolia:eth".to_string(),
        amount: 0,
        to_chain_id: 84532,
        to_address: [0x22; 20],
        status,
        signing_nonce: None,
    };
    withdrawals.insert(
        0,
        Withdrawal {
            amount: 5,
            ..w(WithdrawalStatus::Pending)
        },
    );
    withdrawals.insert(
        1,
        Withdrawal {
            amount: 9,
            ..w(WithdrawalStatus::Broadcast {
                nonce: 1,
                tx_hash: "0x00".into(),
            })
        },
    );
    withdrawals.insert(
        2,
        Withdrawal {
            amount: 4,
            ..w(WithdrawalStatus::Signed {
                nonce: 2,
                raw_tx: "0x02".into(),
            })
        },
    );
    let (ve, voter, minter, distributor) = tokenomics::fresh_state(&acct(OWNER), T0, T0);
    let old = ContractV6 {
        owner: acct(OWNER),
        fee_config: FeeConfig::default(),
        pools,
        pool_tokens,
        next_pool_id: 1,
        legacy_balances: LookupMap::new(StorageKey::Balances),
        balances,
        allowed_tokens,
        mpc_contract: acct("mpc.example.testnet"),
        links: LinkRegistry::new("links"),
        action_nonces: LookupSet::new(StorageKey::ActionNonces),
        withdrawals,
        next_withdrawal_id: 3,
        relayers: IterableSet::new(StorageKey::Relayers),
        evm_assets: IterableMap::new(StorageKey::EvmAssets),
        evm_asset_by_token: LookupMap::new(StorageKey::EvmAssetByToken),
        evm_chains: LookupMap::new(StorageKey::EvmChains),
        evm_deposits: LookupSet::new(StorageKey::EvmDeposits),
        evm_deposits_by_asset: LookupSet::new(StorageKey::EvmDepositsByAsset),
        has_state: LookupSet::new(StorageKey::HasState),
        wind_token: Some(acct(WIND)),
        ve: LazyOption::new(StorageKey::Ve, Some(ve)),
        voter: LazyOption::new(StorageKey::Voter, Some(voter)),
        minter: LazyOption::new(StorageKey::Minter, Some(minter)),
        distributor: LazyOption::new(StorageKey::Distributor, Some(distributor)),
        gauges: LookupMap::new(StorageKey::Gauges),
        stakes: LookupMap::new(StorageKey::Stakes),
        nft_manager: LazyOption::new(
            StorageKey::NftManager,
            Some(PositionManager::new(tokenomics::nft_manager_account())),
        ),
        wind_genesis_done: true,
        wind_genesis_pending: false,
        wind_minted: 100 * E18,
        pending_epoch: None,
    };
    env::state_write(&old);
}

#[test]
fn migration_seeds_totals_once_and_counts_touched_keys_correctly() {
    write_v6_state();
    ctx(SELF, T0, 0);
    let mut c = Contract::migrate();

    // Before seeding: no balance is counted, but the pending EVM withdrawals are (5 pending, 4 signed).
    let usdc = c.get_token_accounting(USDC.into());
    assert!(usdc.migrated);
    assert_eq!(
        usdc.liabilities.0, 0,
        "nothing counted until touched or seeded"
    );
    let eth = c.get_token_accounting("base-sepolia:eth".into());
    assert_eq!(
        eth.pending_withdrawals.0, 9,
        "broadcast withdrawals are not owed"
    );
    assert_eq!(eth.total_owed.0, 9);

    // Touching BOB's USDC key counts its current value first, then the change.
    ctx(BOB, T0, 1);
    let _ = c.withdraw(acct(USDC), U128(3));
    assert_eq!(c.get_token_accounting(USDC.into()).liabilities.0, 4);

    // Seeding the untouched WIND key adds its balance, once.
    ctx(OWNER, T0, 0);
    assert_eq!(
        c.seed_accounting(vec![(owner_hex_of(ALICE), WIND.into())]),
        1
    );
    assert_eq!(
        c.seed_accounting(vec![(owner_hex_of(ALICE), WIND.into())]),
        0,
        "a key is counted once"
    );
    assert_eq!(c.get_token_accounting(WIND.into()).liabilities.0, 100 * E18);

    // A deposit after seeding is counted once, on top of the seeded value.
    ctx(WIND, T0, 0);
    let _ = c.ft_on_transfer(acct(ALICE), U128(E18), String::new());
    assert_eq!(c.get_token_accounting(WIND.into()).liabilities.0, 101 * E18);

    // Pool reserves: seeded once per (pool, token). A token outside the pool is refused.
    ctx(OWNER, T0, 0);
    assert_eq!(
        c.seed_pool_reserves(vec![
            (0, USDC.into(), U128(50)),
            (0, WNEAR.into(), U128(60)),
        ]),
        2
    );
    assert_eq!(
        c.seed_pool_reserves(vec![(0, USDC.into(), U128(999))]),
        0,
        "a pool reserve is seeded once"
    );
    assert_eq!(
        c.get_pool_reserves(0),
        vec![(USDC.to_string(), U128(50)), (WNEAR.to_string(), U128(60))]
    );
    assert_eq!(c.get_token_accounting(USDC.into()).total_owed.0, 4 + 50);

    // get_evm_owed and check_solvency's guard for EVM assets come from registered assets only;
    // with none registered, the EVM pending amount still shows in the token accounting.
    assert_eq!(
        c.get_token_accounting("base-sepolia:eth".into())
            .total_owed
            .0,
        9
    );
}

#[test]
#[should_panic(expected = "only owner")]
fn seeding_is_owner_only() {
    write_v6_state();
    ctx(SELF, T0, 0);
    let mut c = Contract::migrate();
    ctx(ALICE, T0, 0);
    c.seed_accounting(vec![(owner_hex_of(ALICE), WIND.into())]);
}

#[test]
#[should_panic(expected = "not migrated")]
fn a_fresh_contract_has_nothing_to_seed() {
    let mut c = setup();
    ctx(OWNER, T0, 0);
    c.seed_accounting(vec![(owner_hex_of(ALICE), WIND.into())]);
}

#[test]
#[should_panic(expected = "not a token of pool")]
fn a_reserve_seed_must_name_a_token_of_the_pool() {
    write_v6_state();
    ctx(SELF, T0, 0);
    let mut c = Contract::migrate();
    ctx(OWNER, T0, 0);
    c.seed_pool_reserves(vec![(0, WIND.into(), U128(1))]);
}

#[test]
fn the_pending_totals_follow_the_evm_withdrawal_states() {
    let mut c = setup();
    ctx(OWNER, T0, 0);
    c.set_evm_chain(84532, "ethereum-1".into(), 0, U128(30_000_000_000));
    c.register_evm_asset(
        84532,
        "native".into(),
        "base-sepolia:eth".into(),
        18,
        U128(u128::MAX),
        U128(u128::MAX),
    );
    c.add_relayer(acct(RELAYER));
    let alice = owner_of_account(&acct(ALICE));
    c.credit(&alice, "base-sepolia:eth", 100);
    let owed = |c: &Contract| c.get_evm_owed(84532, "native".into()).0;
    assert_eq!(owed(&c), 100);

    // Recorded: 30 is debited but still owed, so the total does not change.
    ctx(ALICE, T0, 0);
    let first = c.withdraw_evm(
        "base-sepolia:eth".into(),
        U128(30),
        format!("0x{}", "22".repeat(20)),
    );
    assert_eq!(owed(&c), 100);
    assert_eq!(
        c.get_token_accounting("base-sepolia:eth".into())
            .pending_withdrawals
            .0,
        30
    );

    // A second one, then the signature of the first fails with a nonce gap: the first is
    // credited back, so the total is still the same.
    let second = c.withdraw_evm(
        "base-sepolia:eth".into(),
        U128(20),
        format!("0x{}", "22".repeat(20)),
    );
    ctx(RELAYER, T0, 1);
    let _ = c.process_withdrawal(first, U128(20_000_000_000), U128(2_000_000_000));
    ctx(RELAYER, T0, 1);
    let _ = c.process_withdrawal(second, U128(20_000_000_000), U128(2_000_000_000));
    assert_eq!(owed(&c), 100);
    let tx = EvmTx {
        chain_id: 84532,
        nonce: 0,
        to: format!("0x{}", "22".repeat(20)),
        value_wei: U128(30),
        max_fee_per_gas: U128(20_000_000_000),
        max_priority_fee_per_gas: U128(2_000_000_000),
        gas_limit: 21_000,
        data: "0x".into(),
    };
    ctx(SELF, T0, 0);
    let ok = c.on_withdrawal_signed(first, T0 / 86_400, tx, Err(PromiseError::Failed));
    assert!(!ok);
    assert_eq!(owed(&c), 100, "the failed withdrawal is credited back");
    assert_eq!(
        c.get_token_accounting("base-sepolia:eth".into())
            .pending_withdrawals
            .0,
        20
    );
    assert_eq!(c.get_withdrawal(first).expect("kept").status, "Failed");

    // The second one is broadcast: it leaves the owed total, since its tokens go to the user.
    let signed = WithdrawalStatus::Signed {
        nonce: 1,
        raw_tx: "0x02".into(),
    };
    let mut w = c.withdrawals.get(&second).cloned().expect("withdrawal");
    w.status = signed;
    c.withdrawals.insert(second, w);
    ctx(RELAYER, T0, 0);
    c.mark_withdrawal_broadcast(second, format!("0x{}", "ab".repeat(32)));
    assert_eq!(
        c.get_token_accounting("base-sepolia:eth".into())
            .pending_withdrawals
            .0,
        0
    );
    assert_eq!(
        owed(&c),
        80,
        "the 30 credited back is owed; the broadcast 20 is not"
    );
}

#[test]
fn check_solvency_refuses_evm_assets() {
    let mut c = setup();
    ctx(OWNER, T0, 0);
    c.set_evm_chain(84532, "ethereum-1".into(), 0, U128(30_000_000_000));
    c.register_evm_asset(
        84532,
        "native".into(),
        "base-sepolia:eth".into(),
        18,
        U128(u128::MAX),
        U128(u128::MAX),
    );
    let _ = c.check_solvency(USDC.into());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = c.check_solvency("base-sepolia:eth".into());
    }));
    assert!(result.is_err());
}

#[test]
fn solvency_callback_reports_held_owed_and_the_verdict() {
    let mut c = setup();
    ctx(WIND, T0, 0);
    let _ = c.ft_on_transfer(acct(ALICE), U128(40), String::new());
    ctx(SELF, T0, 0);
    let held_ok = c.on_solvency_checked(USDC.into(), Ok(U128(0)));
    assert!(held_ok.solvent, "nothing owed in USDC");
    let view = c.on_solvency_checked(WIND.into(), Ok(U128(40)));
    assert_eq!(view.owed.0, 40);
    assert!(view.solvent);
    let short = c.on_solvency_checked(WIND.into(), Ok(U128(39)));
    assert!(!short.solvent, "one unit short is insolvent");
    assert_eq!(short.held.0, 39);
    let unbacked = {
        ctx(OWNER, T0, 0);
        c.test_credit(acct(ALICE), USDC.into(), U128(5));
        ctx(SELF, T0, 0);
        c.on_solvency_checked(USDC.into(), Ok(U128(0)))
    };
    assert!(
        !unbacked.solvent,
        "a faucet credit is owed without a deposit"
    );
    assert_eq!(unbacked.unbacked.0, 5);
}

#[test]
fn a_route_books_each_hop_in_its_own_pool() {
    let mut c = setup();
    // ALICE provides liquidity in both pools.
    ctx(OWNER, T0, 0);
    for token in [USDC, WNEAR, WIND] {
        c.credit(&owner_of_account(&acct(ALICE)), token, 100 * E18);
    }
    ctx(ALICE, T0, 0);
    c.mint_position(0, -300, 300, U128(10 * E18));
    c.mint_position(1, -300, 300, U128(10 * E18));
    let pool0_before = c.get_pool_reserves(0);
    let pool1_before = c.get_pool_reserves(1);

    // BOB routes USDC -> WNEAR (pool 0) -> WIND (pool 1).
    let bob = owner_of_account(&acct(BOB));
    c.credit(&bob, USDC, E18);
    ctx(BOB, T0, 0);
    let (path, _, _) = route(4);
    let amount_in = E18 / 100;
    let out = c.swap_exact_input(path, U128(amount_in), U128(0)).0;
    assert!(out > 0);

    let pool0_after = c.get_pool_reserves(0);
    let pool1_after = c.get_pool_reserves(1);
    // Pool 0 took the input in USDC and paid out the intermediate WNEAR.
    let mid = pool0_before[1].1 .0 - pool0_after[1].1 .0;
    assert_eq!(pool0_after[0].1 .0, pool0_before[0].1 .0 + amount_in);
    // Pool 1 took that WNEAR in, and paid out the WIND.
    assert_eq!(pool1_after[0].1 .0, pool1_before[0].1 .0 + mid);
    assert_eq!(pool1_before[1].1 .0 - pool1_after[1].1 .0, out);
    // The per-pool reserves add up to the token totals.
    let wneat = c.get_token_accounting(WNEAR.into()).pool_reserves.0;
    assert_eq!(wneat, pool0_after[1].1 .0 + pool1_after[0].1 .0);
}
