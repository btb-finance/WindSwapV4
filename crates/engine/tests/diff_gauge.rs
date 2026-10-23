//! Differential test: the Rust `ClGauge` (with `PositionManager` and `ClPool`) must match the
//! original `CLGauge.sol` on random operations.
//!
//! Vectors come from `diff-tests/test/GenerateClGauge.t.sol`, which deploys the real `CLGauge.sol`
//! and `CLPool.sol` with mocks (tokens, factories, a voter, a fees voting reward, and a mock
//! NonfungiblePositionManager), runs random ops and logs each outcome plus the state the Rust side
//! must reproduce. Regenerate with: `cd diff-tests && forge test --match-test testGenerateClGauge`.
//!
//! Each line is `op|time|inputs|status|outputs|S|G|T|P|K|E`. The replay applies the op to a copy of
//! the world (pool, NFT manager, gauge); on `Err` the copy is discarded, like a Solidity revert.
//! After every op it compares: the outputs, the pool state `S`, the gauge state `G`, the touched
//! ticks `T`, the touched pool positions `P`, the touched gauge tokens `K` and the staked list `E`.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use alloy_primitives::Address;
use windswap_engine::gauge::{ClGauge, GaugeConfig, PositionManager, TokenRewards, WEEK};
use windswap_engine::pool::cl_pool::{AccountId, ClPool};
use windswap_engine::pool::position::PositionInfo;
use windswap_engine::pool::tick::TickInfo;
use windswap_engine::{I256, U256};

fn read_vectors(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../diff-tests/vectors")
        .join(format!("{name}.txt"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Solidity address `0x...` as the 32-byte account id: 12 zero bytes, then the 20 address bytes.
fn id(s: &str) -> AccountId {
    let addr = Address::from_str(s).unwrap_or_else(|e| panic!("bad address {s}: {e}"));
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(addr.as_slice());
    out
}

/// The user account `eoa(i)` used by the generator: `address(uint160(0xE0A0 + i))`.
fn user(i: usize) -> AccountId {
    id(&format!("0x{:040x}", 0xE0A0 + i))
}

fn u256(s: &str) -> U256 {
    s.parse().unwrap_or_else(|e| panic!("bad uint256 {s}: {e}"))
}

fn i32s(s: &str) -> i32 {
    s.parse().unwrap_or_else(|e| panic!("bad int24 {s}: {e}"))
}

fn flag(s: &str) -> bool {
    match s {
        "1" => true,
        "0" => false,
        other => panic!("bad flag {other}"),
    }
}

fn b(v: bool) -> &'static str {
    if v {
        "1"
    } else {
        "0"
    }
}

/// `S=` state: identical to the CLPool vectors (17 fields).
fn fmt_state(p: &ClPool) -> String {
    let s0 = &p.slot0;
    format!(
        "S={},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        s0.sqrt_price_x96,
        s0.tick,
        s0.observation_index,
        s0.observation_cardinality,
        s0.observation_cardinality_next,
        p.liquidity,
        p.staked_liquidity,
        p.fee_growth_global0_x128,
        p.fee_growth_global1_x128,
        p.reward_growth_global_x128,
        p.gauge_fees0,
        p.gauge_fees1,
        p.reward_rate,
        p.reward_reserve,
        p.rollover,
        p.last_updated,
        p.period_finish,
    )
}

/// `G=` state: rewardRate, periodFinish, fees0, fees1, reward balance, left(), rewardRateByEpoch,
/// then the gauge's token0 and token1 balances.
fn fmt_gauge(g: Option<&ClGauge>, now: u32) -> String {
    match g {
        None => "G=0,0,0,0,0,0,0,0,0".to_string(),
        Some(g) => format!(
            "G={},{},{},{},{},{},{},{},{}",
            g.reward_rate,
            g.period_finish,
            g.fees0,
            g.fees1,
            g.reward_balance,
            g.left(now),
            g.reward_rate_for_epoch(now),
            g.fee_balance0,
            g.fee_balance1,
        ),
    }
}

/// Tick record, identical to the CLPool vectors.
fn fmt_tick(tick: i32, t: &TickInfo) -> String {
    format!(
        "{tick},{},{},{},{},{},{},{},{},{},{}",
        t.liquidity_gross,
        t.liquidity_net,
        t.staked_liquidity_net,
        t.fee_growth_outside0_x128,
        t.fee_growth_outside1_x128,
        t.reward_growth_outside_x128,
        t.tick_cumulative_outside,
        t.seconds_per_liquidity_outside_x128,
        t.seconds_outside,
        b(t.initialized),
    )
}

/// Pool position record, identical to the CLPool vectors. `owner` is echoed from the vector.
fn fmt_position(owner: &str, lower: i32, upper: i32, p: &PositionInfo) -> String {
    format!(
        "{owner},{lower},{upper},{},{},{},{},{}",
        p.liquidity,
        p.fee_growth_inside0_last_x128,
        p.fee_growth_inside1_last_x128,
        p.tokens_owed0,
        p.tokens_owed1,
    )
}

/// Gauge token record: `tokenId,rewards,rewardGrowthInside,lastUpdateTime,depositTimestamp,inGauge`.
fn fmt_token(world: &World, token_id: U256) -> String {
    let t: TokenRewards = world
        .gauge
        .as_ref()
        .map(|g| g.token(token_id))
        .unwrap_or_default();
    let in_gauge = match (&world.nft, &world.gauge) {
        (Some(nft), Some(g)) => nft.owner_or_zero(token_id) == Some(g.account),
        _ => false,
    };
    format!(
        "{token_id},{},{},{},{},{}",
        t.rewards,
        t.reward_growth_inside,
        t.last_update_time,
        t.deposit_timestamp,
        b(in_gauge),
    )
}

/// The whole system: pool, NFT manager and gauge. Cloned before each op to model reverts.
#[derive(Clone)]
struct World {
    pool: ClPool,
    nft: Option<PositionManager>,
    gauge: Option<ClGauge>,
}

/// Applies one op. `Ok` carries the outputs formatted like the vector; `Err` means the Solidity
/// call reverted.
fn apply(world: &mut World, op: &str, ins: &[&str], now: u32) -> Result<String, ()> {
    let pair = |a: &dyn Display, b: &dyn Display| format!("{a},{b}");
    let triple = |a: &dyn Display, b: &dyn Display, c: &dyn Display| format!("{a},{b},{c}");
    match op {
        "createGauge" => {
            let gauge_id = id(ins[0]);
            let nft_id = id(ins[1]);
            world
                .pool
                .set_gauge_and_position_manager(gauge_id, nft_id)
                .map_err(|_| ())?;
            world.nft = Some(PositionManager::new(nft_id));
            world.gauge = Some(ClGauge::new(GaugeConfig {
                account: gauge_id,
                voter: id(ins[3]),
                notify_admin: id(ins[2]),
                token0: id(ins[4]),
                token1: id(ins[5]),
                tick_spacing: i32s(ins[6]),
                is_pool: flag(ins[7]),
            }));
            Ok(String::new())
        }
        "mint" => {
            let nft = world.nft.as_mut().ok_or(())?;
            let g = world.gauge.as_ref().ok_or(())?;
            let (token0, token1, spacing) = (g.token0, g.token1, g.tick_spacing);
            nft.mint(
                &mut world.pool,
                id(ins[0]),
                token0,
                token1,
                spacing,
                i32s(ins[1]),
                i32s(ins[2]),
                ins[3].parse().unwrap(),
                now,
            )
            .map(|(tid, a0, a1)| triple(&tid, &a0, &a1))
            .map_err(|_| ())
        }
        "approve" => {
            let nft = world.nft.as_mut().ok_or(())?;
            let g = world.gauge.as_ref().ok_or(())?;
            nft.set_approval_for_all(id(ins[0]), g.account, flag(ins[1]));
            Ok(String::new())
        }
        "deposit" => {
            let nft = world.nft.as_mut().ok_or(())?;
            let g = world.gauge.as_mut().ok_or(())?;
            g.deposit(&mut world.pool, nft, id(ins[0]), u256(ins[1]), now)
                .map(|(a0, a1)| pair(&a0, &a1))
                .map_err(|_| ())
        }
        "withdraw" => {
            let nft = world.nft.as_mut().ok_or(())?;
            let g = world.gauge.as_mut().ok_or(())?;
            g.withdraw(&mut world.pool, nft, id(ins[0]), u256(ins[1]), now)
                .map(|(a0, a1, paid)| triple(&a0, &a1, &paid))
                .map_err(|_| ())
        }
        "getRewardAll" => {
            let nft = world.nft.as_ref().ok_or(())?;
            let g = world.gauge.as_mut().ok_or(())?;
            let caller = g.voter;
            g.get_reward(&mut world.pool, nft, caller, id(ins[0]), now)
                .map(|paid| paid.to_string())
                .map_err(|_| ())
        }
        "getReward" => {
            let nft = world.nft.as_ref().ok_or(())?;
            let g = world.gauge.as_mut().ok_or(())?;
            g.get_reward_token(&mut world.pool, nft, id(ins[0]), u256(ins[1]), now)
                .map(|paid| paid.to_string())
                .map_err(|_| ())
        }
        "earned" => {
            let nft = world.nft.as_ref().ok_or(())?;
            let g = world.gauge.as_ref().ok_or(())?;
            g.earned(&world.pool, nft, id(ins[0]), u256(ins[1]), now)
                .map(|v| v.to_string())
                .map_err(|_| ())
        }
        "notify" => {
            let g = world.gauge.as_mut().ok_or(())?;
            let caller = g.voter;
            g.notify_reward_amount(&mut world.pool, caller, u256(ins[0]), now)
                .map(|(f0, f1)| pair(&f0, &f1))
                .map_err(|_| ())
        }
        "notifyNoClaim" => {
            let g = world.gauge.as_mut().ok_or(())?;
            let caller = g.notify_admin;
            g.notify_reward_without_claim(&mut world.pool, caller, u256(ins[0]), now)
                .map(|()| String::new())
                .map_err(|_| ())
        }
        "donate" => {
            let g = world.gauge.as_mut().ok_or(())?;
            g.donate(u256(ins[0]));
            Ok(String::new())
        }
        "setAlive" => {
            let g = world.gauge.as_mut().ok_or(())?;
            g.set_alive(flag(ins[0]));
            Ok(String::new())
        }
        "swap" => {
            // ins[0] is the recipient, which the Rust pool does not model
            let amount: I256 = ins[2].parse().unwrap();
            world
                .pool
                .atomic(|p| {
                    p.swap(
                        flag(ins[1]),
                        amount,
                        u256(ins[3]),
                        ins[4].parse().unwrap(),
                        ins[5].parse().unwrap(),
                        now,
                    )
                })
                .map(|r| pair(&r.amount0, &r.amount1))
                .map_err(|_| ())
        }
        other => panic!("unknown op {other}"),
    }
}

/// Counts of outcomes, for the summary and the coverage assertions.
#[derive(Default)]
struct Coverage {
    crossed_swaps: usize,
    staked_swaps: usize,
    rollover_seen: bool,
    fees_claimed_to_reward: usize,
    reward_paid: usize,
    zero_payout_claims_within_min_stake: usize,
    notifies_with_rollover: usize,
    fee_threshold_seen: bool,
}

#[test]
fn cl_gauge_matches_solidity_on_random_ops() {
    let text = read_vectors("cl_gauge");
    let mut world = World {
        pool: ClPool::default(),
        nft: None,
        gauge: None,
    };
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut cov = Coverage::default();
    let mut total = 0usize;

    for (n, raw) in text.lines().enumerate() {
        if raw.is_empty() {
            continue;
        }
        let line_no = n + 1;
        total += 1;
        let f: Vec<&str> = raw.split('|').collect();
        assert_eq!(f.len(), 11, "line {line_no}: expected 11 fields: {raw}");
        let (op, expect_status, expect_out) = (f[0], f[3], f[4]);
        let now: u32 = f[1].parse().unwrap();
        let ins: Vec<&str> = if f[2].is_empty() {
            Vec::new()
        } else {
            f[2].split(',').collect()
        };
        let expect_ok = match expect_status {
            "ok" => true,
            "err" => false,
            other => panic!("line {line_no}: bad status {other}"),
        };

        // Forfeit bookkeeping: the token a claim is about, read before the op.
        let claim_token = match op {
            "withdraw" | "getReward" => Some(u256(ins[1])),
            _ => None,
        };
        let pre_claim = claim_token.and_then(|t| {
            world
                .gauge
                .as_ref()
                .map(|g| (t, g.token(t).deposit_timestamp))
        });

        let start_tick = world.pool.slot0.tick;
        let start_staked = world.pool.staked_liquidity;
        let start_rollover = world.pool.rollover;

        let got = if op == "init" {
            let mut fresh = ClPool::default();
            match fresh.initialize(i32s(ins[0]), u256(ins[1]), now) {
                Ok(()) => {
                    let max = fresh.max_liquidity_per_tick.to_string();
                    world = World {
                        pool: fresh,
                        nft: None,
                        gauge: None,
                    };
                    Ok(max)
                }
                Err(_) => Err(()),
            }
        } else {
            // Solidity revert semantics: run on a copy, keep it only on success.
            let mut next = world.clone();
            match apply(&mut next, op, &ins, now) {
                Ok(out) => {
                    world = next;
                    Ok(out)
                }
                Err(()) => Err(()),
            }
        };

        match (expect_ok, &got) {
            (true, Ok(out)) => assert_eq!(out, expect_out, "line {line_no} {op}: outputs\n{raw}"),
            (false, Err(())) => {}
            _ => {
                panic!("line {line_no} {op}: Solidity status {expect_status}, Rust {got:?}\n{raw}")
            }
        }

        let entry = counts.entry(op.to_string()).or_insert((0, 0));
        if expect_ok {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }

        // Full state after the op.
        assert_eq!(
            fmt_state(&world.pool),
            f[5],
            "line {line_no} {op}: state\n{raw}"
        );
        assert_eq!(
            fmt_gauge(world.gauge.as_ref(), now),
            f[6],
            "line {line_no} {op}: gauge state\n{raw}"
        );
        for rec in f[7].split(';').filter(|r| !r.is_empty()) {
            let tick = i32s(rec.split(',').next().unwrap());
            let actual = fmt_tick(
                tick,
                &world.pool.ticks.get(&tick).cloned().unwrap_or_default(),
            );
            assert_eq!(actual, rec, "line {line_no} {op}: tick {tick}\n{raw}");
        }
        for rec in f[8].split(';').filter(|r| !r.is_empty()) {
            let fields: Vec<&str> = rec.split(',').collect();
            assert_eq!(fields.len(), 8, "line {line_no}: position record {rec}");
            let (lower, upper) = (i32s(fields[1]), i32s(fields[2]));
            let info = world.pool.position(&id(fields[0]), lower, upper);
            let actual = fmt_position(fields[0], lower, upper, &info);
            assert_eq!(
                actual, rec,
                "line {line_no} {op}: position {}\n{raw}",
                fields[0]
            );
        }
        for rec in f[9].split(';').filter(|r| !r.is_empty()) {
            let token_id = u256(rec.split(',').next().unwrap());
            assert_eq!(
                fmt_token(&world, token_id),
                rec,
                "line {line_no} {op}: token {token_id}\n{raw}"
            );
        }
        // Staked list of the account in the op: `E=<index>:<ids>` or `E=`.
        let e = f[10];
        if let Some(rest) = e.strip_prefix("E=") {
            if let Some((acc, _)) = rest.split_once(':') {
                let idx: usize = acc.parse().unwrap();
                let ids = world
                    .gauge
                    .as_ref()
                    .map(|g| g.staked_values(user(idx)))
                    .unwrap_or_default();
                let joined: Vec<String> = ids.iter().map(|x| x.to_string()).collect();
                assert_eq!(
                    format!("E={acc}:{}", joined.join(",")),
                    e,
                    "line {line_no} {op}: staked list\n{raw}"
                );
            } else {
                assert_eq!(e, "E=", "line {line_no}: unexpected E field");
            }
        } else {
            panic!("line {line_no}: bad E field {e}");
        }

        // Coverage bookkeeping (only for ops that succeeded).
        if op == "swap" && expect_ok {
            let end_tick = world.pool.slot0.tick;
            let (lo, hi) = (start_tick.min(end_tick), start_tick.max(end_tick));
            if lo < hi
                && world
                    .pool
                    .ticks
                    .range((lo + 1)..=hi)
                    .any(|(_, t)| t.initialized)
            {
                cov.crossed_swaps += 1;
            }
            if start_staked > 0 {
                cov.staked_swaps += 1;
            }
        }
        if !world.pool.rollover.is_zero() {
            cov.rollover_seen = true;
        }
        if op == "notify" && expect_ok {
            let fees_out = expect_out.split(',').any(|x| x != "0");
            if fees_out {
                cov.fees_claimed_to_reward += 1;
            }
            if expect_out.split(',').any(|x| u256(x) > U256::from(WEEK)) {
                cov.fee_threshold_seen = true;
            }
            if !start_rollover.is_zero() {
                cov.notifies_with_rollover += 1;
            }
        }
        if matches!(op, "withdraw" | "getReward" | "getRewardAll") && expect_ok {
            let paid = expect_out.rsplit(',').next().unwrap_or("0");
            if paid != "0" {
                cov.reward_paid += 1;
            } else if let Some((_, dep_ts)) = pre_claim {
                if U256::from(now) < dep_ts + U256::from(600u32) {
                    cov.zero_payout_claims_within_min_stake += 1;
                }
            }
        }
    }

    println!("cl_gauge replay: {total} ops");
    for (op, (ok, err)) in &counts {
        println!("  {op:<18} ok={ok:<5} err={err}");
    }
    println!(
        "  coverage: crossed-tick swaps={} staked swaps={} rollover seen={} notifies with fees to reward={} notifies with rollover={}",
        cov.crossed_swaps, cov.staked_swaps, cov.rollover_seen, cov.fees_claimed_to_reward, cov.notifies_with_rollover
    );
    println!(
        "  reward paid claims={} zero-payout claims inside MIN_STAKE_DURATION={} fee>WEEK seen={}",
        cov.reward_paid, cov.zero_payout_claims_within_min_stake, cov.fee_threshold_seen
    );
    let ok = |op: &str| counts.get(op).map(|c| c.0).unwrap_or(0);
    let err = |op: &str| counts.get(op).map(|c| c.1).unwrap_or(0);

    assert!(total >= 2000, "expected at least 2000 ops, got {total}");
    assert!(ok("deposit") >= 100, "only {} deposits ok", ok("deposit"));
    assert!(
        ok("withdraw") >= 100,
        "only {} withdraws ok",
        ok("withdraw")
    );
    assert!(ok("notify") >= 80, "only {} notifies ok", ok("notify"));
    assert!(
        ok("notifyNoClaim") >= 20,
        "only {} notifyNoClaim ok",
        ok("notifyNoClaim")
    );
    assert!(err("notify") >= 20, "only {} notify reverts", err("notify"));
    assert!(
        ok("getRewardAll") + ok("getReward") >= 100,
        "too few reward claims"
    );
    assert!(
        cov.reward_paid >= 30,
        "only {} claims paid a reward",
        cov.reward_paid
    );
    assert!(cov.crossed_swaps > 0, "no swap crossed an initialized tick");
    assert!(cov.staked_swaps > 0, "no swap ran with staked liquidity");
    assert!(cov.rollover_seen, "rollover never became nonzero");
    assert!(
        cov.fees_claimed_to_reward > 0,
        "no fee claim reached the fees voting reward"
    );
    assert!(
        cov.fee_threshold_seen,
        "no fee claim above one week of fees"
    );
    assert!(
        cov.zero_payout_claims_within_min_stake > 0,
        "no claim was forfeited inside the min stake duration"
    );
}
