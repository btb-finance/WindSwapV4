//! Differential test: the Rust `ClPool` must match the original `CLPool.sol` on random operations.
//!
//! Vectors come from `diff-tests/test/GenerateClPool.t.sol`, which deploys the real `CLPool.sol`
//! with mock tokens, factory, registry, a gauge proxy and an NFT-manager driver, runs random ops
//! and logs each outcome plus the full pool state. Regenerate with:
//! `cd diff-tests && forge test --match-test testGenerateClPool`
//!
//! Each line is `op|time|inputs|status|outputs|S|T|P`. The replay applies the same op to
//! [`ClPool`] inside [`ClPool::atomic`] and compares the outputs and the state strings exactly.
//! The Rust side has no `msg.sender` checks, so the replay repeats the Solidity modifiers
//! (`onlyGauge` needs a gauge, `onlyNftManager` needs an NFT manager) before calling the pool.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use alloy_primitives::Address;
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

/// `S=` state: slot0, global liquidity and fee/reward accumulators, gauge fees, reward schedule.
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

/// Tick record: `tick,liquidityGross,liquidityNet,stakedLiquidityNet,feeOutside0,feeOutside1,
/// rewardOutside,tickCumulativeOutside,secondsPerLiquidityOutside,secondsOutside,initialized`.
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

/// Position record: `owner,tickLower,tickUpper,liquidity,feeGrowthInside0Last,
/// feeGrowthInside1Last,tokensOwed0,tokensOwed1`. `owner` is echoed from the vector.
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

/// Applies one op to the pool. `Ok` carries the outputs formatted like the vector, `Err` means
/// the Solidity call reverted (the pool is left unchanged).
fn apply(pool: &mut ClPool, op: &str, ins: &[&str], time: u32) -> Result<String, ()> {
    let pair = |a: &dyn Display, b: &dyn Display| format!("{a},{b}");
    match op {
        "setGauge" => pool
            .set_gauge_and_position_manager(id(ins[0]), id(ins[1]))
            .map(|()| String::new())
            .map_err(|_| ()),
        "mint" => pool
            .atomic(|p| {
                p.mint(
                    id(ins[0]),
                    i32s(ins[1]),
                    i32s(ins[2]),
                    ins[3].parse().unwrap(),
                    time,
                )
            })
            .map(|(a0, a1)| pair(&a0, &a1))
            .map_err(|_| ()),
        "burn" => {
            // onlyNftManager
            if pool.nft.is_none() {
                return Err(());
            }
            pool.atomic(|p| {
                p.burn(
                    id(ins[0]),
                    i32s(ins[1]),
                    i32s(ins[2]),
                    ins[3].parse().unwrap(),
                    time,
                )
            })
            .map(|(a0, a1)| pair(&a0, &a1))
            .map_err(|_| ())
        }
        "collect" => {
            // onlyNftManager; ins[0] is the recipient, which the Rust pool does not model
            if pool.nft.is_none() {
                return Err(());
            }
            pool.atomic(|p| {
                p.collect(
                    id(ins[5]),
                    i32s(ins[1]),
                    i32s(ins[2]),
                    ins[3].parse().unwrap(),
                    ins[4].parse().unwrap(),
                )
            })
            .map(|(a0, a1)| pair(&a0, &a1))
            .map_err(|_| ())
        }
        "swap" => {
            // ins[0] is the recipient, which the Rust pool does not model
            let amount: I256 = ins[2].parse().unwrap();
            pool.atomic(|p| {
                p.swap(
                    flag(ins[1]),
                    amount,
                    u256(ins[3]),
                    ins[4].parse().unwrap(),
                    ins[5].parse().unwrap(),
                    time,
                )
            })
            .map(|r| pair(&r.amount0, &r.amount1))
            .map_err(|_| ())
        }
        "stake" => {
            // onlyGauge
            if pool.gauge.is_none() {
                return Err(());
            }
            pool.atomic(|p| {
                p.stake(
                    ins[0].parse().unwrap(),
                    i32s(ins[1]),
                    i32s(ins[2]),
                    flag(ins[3]),
                    time,
                )
            })
            .map(|()| String::new())
            .map_err(|_| ())
        }
        "syncReward" => {
            // onlyGauge
            if pool.gauge.is_none() {
                return Err(());
            }
            pool.sync_reward(u256(ins[0]), u256(ins[1]), u256(ins[2]));
            Ok(String::new())
        }
        "updateRewardsGrowthGlobal" => {
            // onlyGauge
            if pool.gauge.is_none() {
                return Err(());
            }
            pool.atomic(|p| p.update_rewards_growth_global(time))
                .map(|()| String::new())
                .map_err(|_| ())
        }
        "collectFees" => {
            // onlyGauge
            if pool.gauge.is_none() {
                return Err(());
            }
            let (a0, a1) = pool.collect_fees();
            Ok(pair(&a0, &a1))
        }
        "increaseObservationCardinalityNext" => pool
            .atomic(|p| p.increase_observation_cardinality_next(ins[0].parse().unwrap()))
            .map(|()| String::new())
            .map_err(|_| ()),
        other => panic!("unknown op {other}"),
    }
}

/// Counts of ok and err outcomes per op, for the summary and the coverage assertions.
#[derive(Default)]
struct Coverage {
    crossed_swaps: usize,
    staked_swaps: usize,
    rollover_seen: bool,
    gauge_fees_seen: bool,
    burns_ok: usize,
    collects_nonzero: usize,
}

#[test]
fn cl_pool_matches_solidity_on_random_ops() {
    let text = read_vectors("cl_pool");
    let mut pool = ClPool::default();
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
        assert_eq!(f.len(), 8, "line {line_no}: expected 8 fields: {raw}");
        let (op, expect_status, expect_out) = (f[0], f[3], f[4]);
        let time: u32 = f[1].parse().unwrap();
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

        let start_tick = pool.slot0.tick;
        let start_staked = pool.staked_liquidity;

        let got = if op == "init" {
            let mut fresh = ClPool::default();
            match fresh.initialize(i32s(ins[0]), u256(ins[1]), time) {
                Ok(()) => {
                    let max = fresh.max_liquidity_per_tick.to_string();
                    pool = fresh;
                    Ok(max)
                }
                Err(_) => Err(()),
            }
        } else {
            apply(&mut pool, op, &ins, time)
        };

        match (expect_ok, &got) {
            (true, Ok(out)) => assert_eq!(out, expect_out, "line {line_no} {op}: outputs"),
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

        // Full state after the op: global state, every touched tick, every touched position.
        let state = fmt_state(&pool);
        assert_eq!(state, f[5], "line {line_no} {op}: state\n{raw}");
        for rec in f[6].split(';').filter(|r| !r.is_empty()) {
            let tick = i32s(rec.split(',').next().unwrap());
            let actual = fmt_tick(tick, &pool.ticks.get(&tick).cloned().unwrap_or_default());
            assert_eq!(actual, rec, "line {line_no} {op}: tick {tick}\n{raw}");
        }
        for rec in f[7].split(';').filter(|r| !r.is_empty()) {
            let fields: Vec<&str> = rec.split(',').collect();
            assert_eq!(fields.len(), 8, "line {line_no}: position record {rec}");
            let (lower, upper) = (i32s(fields[1]), i32s(fields[2]));
            let info = pool.position(&id(fields[0]), lower, upper);
            let actual = fmt_position(fields[0], lower, upper, &info);
            assert_eq!(
                actual, rec,
                "line {line_no} {op}: position {}\n{raw}",
                fields[0]
            );
        }

        // Coverage bookkeeping (only for ops the pool accepted).
        if op == "swap" && expect_ok {
            let end_tick = pool.slot0.tick;
            let (lo, hi) = (start_tick.min(end_tick), start_tick.max(end_tick));
            if lo < hi && pool.ticks.range((lo + 1)..=hi).any(|(_, t)| t.initialized) {
                cov.crossed_swaps += 1;
            }
            if start_staked > 0 {
                cov.staked_swaps += 1;
            }
        }
        if !pool.rollover.is_zero() {
            cov.rollover_seen = true;
        }
        if pool.gauge_fees0 > 1 || pool.gauge_fees1 > 1 {
            cov.gauge_fees_seen = true;
        }
        if op == "burn" && expect_ok {
            cov.burns_ok += 1;
        }
        if op == "collect" && expect_ok && expect_out != "0,0" {
            cov.collects_nonzero += 1;
        }
    }

    println!("cl_pool replay: {total} ops");
    for (op, (ok, err)) in &counts {
        println!("  {op:<36} ok={ok:<5} err={err}");
    }
    println!(
        "  coverage: crossed-tick swaps={} staked swaps={} rollover seen={} gauge fees seen={}",
        cov.crossed_swaps, cov.staked_swaps, cov.rollover_seen, cov.gauge_fees_seen
    );
    println!(
        "  burns ok={} collects with nonzero amounts={}",
        cov.burns_ok, cov.collects_nonzero
    );

    assert!(total >= 3000, "expected at least 3000 ops, got {total}");
    assert!(
        cov.burns_ok >= 300,
        "only {} successful burns",
        cov.burns_ok
    );
    assert!(
        cov.collects_nonzero >= 300,
        "only {} collects with nonzero amounts",
        cov.collects_nonzero
    );
    assert!(cov.crossed_swaps > 0, "no swap crossed an initialized tick");
    assert!(cov.staked_swaps > 0, "no swap ran with staked liquidity");
    assert!(cov.rollover_seen, "rollover never became nonzero");
    assert!(cov.gauge_fees_seen, "gauge fees never accrued");
}
