//! Differential test: the Rust `Voter` port (with the `VotingEscrow` and voting-reward ports it drives) must match the
//! windv2 `Voter.sol` exactly.
//!
//! Vectors come from `diff-tests/` (Foundry), generated from the real `Voter.sol`, `VotingEscrow.sol`,
//! `FeesVotingReward.sol` / `BribeVotingReward.sol` (via `VotingRewardsFactory.sol`), with mock gauges, pools,
//! pool factory, factory registry and minter: `cd diff-tests && forge test --match-test testGenerateVoter`.
//!
//! Each line is `op|now|block|args...|res|payload|state`:
//! - `res` is `ok` or `err`. `err` carries the Solidity revert reason, which must match the Rust error.
//! - `payload` is the return value (an id, a minter transfer, the owner's payout, an earned value), or `-`.
//! - `state` is the full observable state after the call. The Rust side builds the same string and compares it.
//!
//! An `Err` from Rust discards the call: the voter, the escrow and the gauge mock state are restored from a clone,
//! as a Solidity revert does.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use windswap_engine::error::Result;
use windswap_engine::fees::AccountId;
use windswap_engine::ve::VotingEscrow;
use windswap_engine::voter::{GaugeId, PoolId, Voter};
use windswap_engine::{MathError, U256};

/// The `block.timestamp` and `block.number` of the generator's deployment.
const T0: u64 = 1_780_000_000;
const B0: u64 = 1_000;
/// Role ids, as the generator's addresses (`address(uint160(role))`).
const TEAM: u64 = 777;
const OWNER: u64 = 0x0A0A;
const COUNCIL: u64 = 0xC0C0;
/// The mock minter contract's role.
const MINTER: u64 = 0x1111;
/// The Voter's own address (the escrow's `voter`).
const VOTER_ADDR: u64 = 0xBEEF;
/// Reward tokens `t0..t4` and WIND (`w`), as the generator names them.
const TOKEN_NAMES: [&str; 6] = ["t0", "t1", "t2", "t3", "t4", "w"];

fn read_vectors() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../diff-tests/vectors")
        .join("voter.txt");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Account `n` maps to the 32-byte id with `n` in the low 8 bytes.
fn acct(n: u64) -> AccountId {
    let mut a = [0u8; 32];
    a[24..].copy_from_slice(&n.to_be_bytes());
    a
}

fn u(s: &str) -> U256 {
    U256::from_str(s).unwrap_or_else(|_| panic!("bad uint {s}"))
}

fn n(s: &str) -> u64 {
    s.parse().unwrap_or_else(|_| panic!("bad u64 {s}"))
}

fn list<T: FromStr>(s: &str) -> Vec<T> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(',')
        .map(|x| {
            x.parse()
                .unwrap_or_else(|_| panic!("bad list item {x} in {s}"))
        })
        .collect()
}

fn b(v: bool) -> char {
    if v {
        '1'
    } else {
        '0'
    }
}

/// The full system state: the voter, the escrow that it drives, and the gauge mock settings
/// (`left` and whether `notifyRewardAmount` succeeds), which are inputs to `distribute`.
#[derive(Clone)]
struct World {
    ve: VotingEscrow,
    voter: Voter,
    /// `gauge -> (left, notify_ok)`, set by the generator's `gcfg` lines. Missing means `(0, true)`.
    cfg: BTreeMap<GaugeId, (U256, bool)>,
}

fn setup() -> World {
    // Solidity: `VotingEscrow(forwarder, WIND, registry, TEAM)`, then `setVoterAndDistributor` by TEAM.
    let mut ve = VotingEscrow::new(acct(TEAM), T0, B0);
    ve.voter = acct(VOTER_ADDR);
    // Solidity: `new Voter(forwarder, ve, registry, OWNER)`, `initialize([t0, t1, WIND], minter)` by OWNER,
    // `setEmergencyCouncil(COUNCIL)` by OWNER.
    let mut voter = Voter::new(acct(VOTER_ADDR), acct(OWNER));
    let tokens: Vec<String> = ["t0", "t1", "w"].iter().map(|t| t.to_string()).collect();
    voter
        .initialize(acct(OWNER), &tokens, acct(MINTER))
        .expect("initialize");
    voter
        .set_emergency_council(acct(OWNER), acct(COUNCIL))
        .expect("setEmergencyCouncil");
    World {
        ve,
        voter,
        cfg: BTreeMap::new(),
    }
}

/// The state string, in the layout the generator's `state()` writes:
/// `tw,index,maxVotingNum,length;` `weight,gauge` for pools 1..6 `,`-joined `;` whitelisted bits `;`
/// per token 1..6: `usedWeight,lastVoted,whitelistedNFT,voted,balanceOfNFT,votes(pool 1..6)` `;`
/// per gauge 1..n: `alive,claimable,supplyIndex,fees.totalSupply,bribe.totalSupply,fees.balanceOf(1..6),
/// bribe.balanceOf(1..6)` (the `;` separates groups, the `,` separates values).
fn state(w: &World, now: u64, blk: u64) -> String {
    let v = &w.voter;
    let ve = &w.ve;
    let mut out = format!(
        "{},{},{},{}",
        v.total_weight,
        v.index,
        v.max_voting_num,
        v.length()
    );
    out.push(';');
    let pools: Vec<String> = (1..=6u64)
        .map(|p| format!("{},{}", v.weight_of(p), v.gauge_of(p)))
        .collect();
    out.push_str(&pools.join(","));
    out.push(';');
    for name in TOKEN_NAMES {
        out.push(b(v.whitelisted_token(name)));
    }
    for t in 1..=6u64 {
        out.push(';');
        let balance = ve.balance_of_nft(now, blk, t).expect("balanceOfNFT");
        out.push_str(&format!(
            "{},{},{},{},{}",
            v.used_weight_of(t),
            v.last_voted_of(t),
            b(v.whitelisted_nft(t)),
            b(ve.is_voted(t)),
            balance
        ));
        for p in 1..=6u64 {
            out.push(',');
            out.push_str(&v.votes_of(t, p).to_string());
        }
    }
    for g in 1..=v.gauge_count {
        let r = v.gauge_rewards.get(&g).expect("gauge rewards");
        out.push(';');
        out.push_str(&format!(
            "{},{},{},{},{}",
            b(v.alive(g)),
            v.claimable_of(g),
            v.supply_index_of(g),
            r.fees.total_supply,
            r.bribe.total_supply
        ));
        for t in 1..=6u64 {
            out.push(',');
            out.push_str(&r.fees.balance_of(t).to_string());
        }
        for t in 1..=6u64 {
            out.push(',');
            out.push_str(&r.bribe.balance_of(t).to_string());
        }
    }
    out
}

/// Whether a Rust error is the revert reason the Solidity call reported.
/// `Panic17` is the checked arithmetic panic (overflow or underflow), so either variant matches.
fn reason_matches(reason: &str, err: MathError) -> bool {
    use MathError::*;
    match reason {
        "Panic17" => matches!(err, Overflow | Underflow),
        "Panic18" => err == DivisionByZero,
        "Panic50" => err == IndexOutOfBounds,
        "AlreadyVotedOrDeposited" => err == AlreadyVotedOrDeposited,
        "DistributeWindow" => err == DistributeWindow,
        "NotApprovedOrOwner" => err == NotApprovedOrOwner,
        "UnequalLengths" => err == UnequalLengths,
        "TooManyPools" => err == TooManyPools,
        "NotWhitelistedNFT" => err == NotWhitelistedNft,
        "NotGovernor" => err == NotGovernor,
        "NotEmergencyCouncil" => err == NotEmergencyCouncil,
        "NotMinter" => err == NotMinter,
        "MaximumVotingNumberTooLow" => err == MaximumVotingNumberTooLow,
        "SameValue" => err == SameValue,
        "GaugeDoesNotExist" => err == GaugeDoesNotExist,
        "GaugeNotAlive" => err == GaugeNotAlive,
        "NonZeroVotes" => err == NonZeroVotes,
        "ZeroBalance" => err == ZeroBalance,
        "FactoryPathNotApproved" => err == FactoryPathNotApproved,
        "GaugeExists" => err == GaugeExists,
        "NotAPool" => err == NotAPool,
        "NotWhitelistedToken" => err == NotWhitelistedToken,
        "GaugeAlreadyKilled" => err == GaugeAlreadyKilled,
        "GaugeAlreadyRevived" => err == GaugeAlreadyRevived,
        "NotWhitelisted" => err == NotWhitelisted,
        "ZeroAmount" => err == ZeroAmount,
        "LockDurationTooLong" => err == LockDurationTooLong,
        "LockDurationNotInFuture" => err == LockDurationNotInFuture,
        _ => false,
    }
}

/// Parses `g:tok,tok;g:tok` into `(gauge, tokens)` entries.
fn parse_entries(s: &str) -> Vec<(GaugeId, Vec<String>)> {
    s.split(';')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (g, toks) = p.split_once(':').unwrap_or_else(|| panic!("bad entry {p}"));
            let tokens = toks
                .split(',')
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .collect();
            (n(g), tokens)
        })
        .collect()
}

/// Applies one call. `Ok(payload)` is the value the Solidity call returned, as the generator logs it.
fn apply(w: &mut World, op: &str, now: u64, blk: u64, a: &[&str]) -> Result<String> {
    let World { ve, voter, cfg } = w;
    let gauge_state = |g: GaugeId| cfg.get(&g).copied().unwrap_or((U256::ZERO, true));
    match op {
        "gcfg" => {
            // gauge mock setters: `setLeft(left)`, `setFail(fail)`. `fail` makes `notifyRewardAmount` revert.
            cfg.insert(n(a[0]), (u(a[1]), a[2] == "0"));
            Ok("-".into())
        }
        "lock" => {
            let (id, _) = ve.create_lock(acct(n(a[0])), now, blk, u(a[1]), n(a[2]))?;
            Ok(id.to_string())
        }
        "vote" => {
            let pools: Vec<PoolId> = list(a[2]);
            let wts: Vec<U256> = list(a[3]);
            voter.vote(ve, acct(n(a[1])), n(a[0]), &pools, &wts, now, blk)?;
            Ok("-".into())
        }
        "reset" => {
            voter.reset(ve, acct(n(a[1])), n(a[0]), now)?;
            Ok("-".into())
        }
        "poke" => {
            voter.poke(ve, n(a[0]), now, blk)?;
            Ok("-".into())
        }
        "creategauge" => {
            let pool = n(a[1]);
            // Pools 1..4 are the mock pools over (t_{i-1}, t_i). Pools 5 and 6 are not pools.
            let tokens = (1..=4u64).contains(&pool).then(|| {
                let i = (pool - 1) as usize;
                (TOKEN_NAMES[i].to_owned(), TOKEN_NAMES[i + 1].to_owned())
            });
            let id = voter.create_gauge(acct(n(a[0])), a[2] == "1", pool, tokens)?;
            Ok(id.to_string())
        }
        "notify" => {
            voter.notify_reward_amount(acct(n(a[0])), u(a[1]))?;
            Ok("-".into())
        }
        "bnotify" => {
            let g = n(a[0]);
            let token = a[1];
            let whitelisted = voter.whitelisted_token(token);
            let bribe = &mut voter
                .gauge_rewards
                .get_mut(&g)
                .ok_or(MathError::InvalidInput)?
                .bribe;
            bribe.notify_bribe_reward(token, u(a[2]), now, whitelisted)?;
            Ok("-".into())
        }
        "distr" => {
            let minter = voter.distribute_range(n(a[1]), n(a[2]), gauge_state)?;
            Ok(minter.to_string())
        }
        "distl" => {
            let gauges: Vec<GaugeId> = list(a[0]);
            Ok(voter.distribute_gauges(&gauges, gauge_state)?.to_string())
        }
        "distg" => {
            let minter = voter.distribute_gauges(&[n(a[0])], gauge_state)?;
            Ok(minter.to_string())
        }
        "upl" => {
            let gauges: Vec<GaugeId> = list(a[0]);
            Ok(voter.update_for_gauges(&gauges)?.to_string())
        }
        "upr" => Ok(voter.update_for_range(n(a[0]), n(a[1]))?.to_string()),
        "upg" => Ok(voter.update_for(n(a[0]))?.to_string()),
        "kill" => Ok(voter.kill_gauge(acct(n(a[1])), n(a[0]))?.to_string()),
        "revive" => {
            voter.revive_gauge(acct(n(a[1])), n(a[0]))?;
            Ok("-".into())
        }
        "wltoken" => {
            voter.whitelist_token(acct(n(a[2])), a[0], a[1] == "1")?;
            Ok("-".into())
        }
        "wlnft" => {
            voter.whitelist_nft(acct(n(a[2])), n(a[0]), a[1] == "1")?;
            Ok("-".into())
        }
        "maxvote" => {
            voter.set_max_voting_num(acct(n(a[1])), u(a[0]))?;
            Ok("-".into())
        }
        "claim" => {
            let is_fees = a[0] == "F";
            let entries = parse_entries(a[3]);
            // The generator's reward tokens are `t0..t4` and `w`, so the names are plain strings.
            let entries: Vec<(GaugeId, Vec<String>)> = entries;
            let paid = voter.claim(ve, acct(n(a[2])), n(a[1]), now, is_fees, &entries)?;
            let parts: Vec<String> = TOKEN_NAMES
                .iter()
                .map(|name| paid.get(*name).copied().unwrap_or(U256::ZERO).to_string())
                .collect();
            Ok(parts.join(","))
        }
        "earned" => {
            let reward = voter
                .reward(n(a[1]), a[0] == "F")
                .ok_or(MathError::InvalidInput)?;
            Ok(reward.earned(a[2], n(a[3]), n(a[4]))?.to_string())
        }
        other => panic!("unknown op {other}"),
    }
}

#[derive(Default)]
struct Tally {
    ok: usize,
    err: usize,
    reasons: BTreeMap<String, usize>,
}

#[test]
fn voter_matches_solidity_vectors() {
    let text = read_vectors();
    let mut w = setup();
    let mut per_op: BTreeMap<String, Tally> = BTreeMap::new();
    let mut lines = 0usize;

    for (lineno, line) in text.lines().enumerate() {
        let lineno = lineno + 1;
        let f: Vec<&str> = line.split('|').collect();
        assert!(f.len() >= 4, "line {lineno}: too few fields");
        let op = f[0];
        let now = n(f[1]);
        let blk = n(f[2]);
        let res = f[f.len() - 3];
        let payload = f[f.len() - 2];
        let state_expected = f[f.len() - 1];
        let args = &f[3..f.len() - 3];
        lines += 1;

        if op == "init" {
            assert_eq!(
                state(&w, now, blk),
                state_expected,
                "line {lineno}: init state"
            );
            continue;
        }

        let snapshot = w.clone();
        let outcome = apply(&mut w, op, now, blk, args);
        let tally = per_op.entry(op.to_owned()).or_default();
        match (res, outcome) {
            ("ok", Ok(value)) => {
                assert_eq!(value, payload, "line {lineno}: payload\n{line}");
                tally.ok += 1;
            }
            ("err", Err(e)) => {
                assert!(
                    reason_matches(payload, e),
                    "line {lineno}: expected revert {payload}, rust returned {e:?}\n{line}"
                );
                tally.err += 1;
                *tally.reasons.entry(payload.to_owned()).or_default() += 1;
                w = snapshot;
            }
            ("ok", Err(e)) => panic!("line {lineno}: solidity ok, rust {e:?}\n{line}"),
            ("err", Ok(value)) => {
                panic!("line {lineno}: solidity reverted {payload}, rust ok {value}\n{line}")
            }
            (other, _) => panic!("line {lineno}: bad result field {other}"),
        }
        assert_eq!(
            state(&w, now, blk),
            state_expected,
            "line {lineno}: state after {op}\n{line}"
        );
    }

    println!("voter vectors: {lines} lines");
    println!("{:<12} {:>6} {:>6}  reasons", "op", "ok", "err");
    for (op, t) in &per_op {
        let reasons: Vec<String> = t.reasons.iter().map(|(k, v)| format!("{k}={v}")).collect();
        println!("{:<12} {:>6} {:>6}  {}", op, t.ok, t.err, reasons.join(" "));
    }

    // The run must reach the real logic, not only the revert paths.
    let vote = &per_op["vote"];
    assert!(vote.ok >= 40, "too few successful votes: {}", vote.ok);
    assert!(per_op["poke"].ok >= 50, "too few successful pokes");
    assert!(per_op["reset"].ok >= 50, "too few successful resets");
    assert!(per_op["distr"].ok + per_op["distl"].ok + per_op["distg"].ok >= 50);
    assert!(per_op["claim"].ok >= 50, "too few successful claims");
    assert!(per_op["notify"].ok >= 50, "too few successful notifies");
    for reason in [
        "NotApprovedOrOwner",
        "DistributeWindow",
        "NotWhitelistedNFT",
        "GaugeNotAlive",
        "ZeroBalance",
        "NonZeroVotes",
        "TooManyPools",
        "NotAPool",
        "NotWhitelistedToken",
        "GaugeExists",
        "FactoryPathNotApproved",
        "NotMinter",
        "Panic50",
        "Panic17",
    ] {
        let seen = per_op.values().any(|t| t.reasons.contains_key(reason));
        assert!(seen, "revert path {reason} never exercised");
    }
}
