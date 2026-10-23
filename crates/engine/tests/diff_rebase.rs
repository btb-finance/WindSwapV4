//! Differential test: the Rust `RewardsDistributor` port (with the ported `VotingEscrow`) must match the
//! windv2 Solidity `RewardsDistributor` and `VotingEscrow`.
//!
//! Vectors come from `diff-tests/` (Foundry): `cd diff-tests && forge test --match-test testGenerateRebase`
//! writes `vectors/rebase.txt`. The generator runs about 1500 random calls on the real contracts, with a mock
//! WIND token and two mock minters, and logs the outcome and full post-state after each call.
//!
//! The file is replayed in order on one `World` (escrow, distributor, a WIND ledger, and the active period).
//! Each call runs on a clone, so an `Err` from Rust discards it (Solidity revert). Every call is checked
//! against the Solidity outcome (`ok` with its return value, or `err` with a revert reason) and the full
//! post-state: distributor state, the digest of `tokensPerWeek`, the WIND balances, and each token's lock,
//! user point, `timeCursorOf` and `claimable`.
//!
//! Coverage is tallied by op and by what the claim paid (`deposit`, `transfer`, `none`). The test asserts
//! that the paths in scope occur, including the two loop caps.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use alloy_primitives::{keccak256, B256};
use windswap_engine::error::Result;
use windswap_engine::fees::AccountId;
use windswap_engine::rebase::{Payout, RewardsDistributor, CHECKPOINT_WEEK_CAP, CLAIM_WEEK_CAP};
use windswap_engine::ve::{VotingEscrow, WEEK};
use windswap_engine::{MathError, U256};

/// The Solidity generator's escrow, team and distributor accounts. The ledger keys for the escrow and the
/// distributor are internal to the test.
const TEAM: u64 = 777;
const VE_ACCOUNT: u64 = 4001;
const DIST_ACCOUNT: u64 = 4002;
const MINTER_BASE: u64 = 9000;
const USER_BASE: u64 = 1000;
const NUSERS: u64 = 5;
const OUTSIDER: u64 = 5000;
const INITIAL_FUNDS: u128 = 1 << 127;

fn read_vectors() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../diff-tests/vectors")
        .join("rebase.txt");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Account `n` maps to the 32-byte id with `n` in the low 8 bytes, matching the generator's `uint160` addresses.
fn acct(n: u64) -> AccountId {
    let mut a = [0u8; 32];
    a[24..].copy_from_slice(&n.to_be_bytes());
    a
}

fn acct_num(a: &AccountId) -> u64 {
    assert!(a[..24].iter().all(|&b| b == 0), "unexpected account {a:?}");
    u64::from_be_bytes(a[24..].try_into().unwrap())
}

fn minter_acct(idx: u64) -> AccountId {
    acct(MINTER_BASE + idx)
}

fn u(s: &str) -> U256 {
    s.parse().unwrap()
}

fn n(s: &str) -> u64 {
    s.parse().unwrap()
}

fn week_floor(t: u64) -> u64 {
    (t / WEEK) * WEEK
}

/// The distributor state the Solidity digest covers, with the escrow state, the ledger and the period.
#[derive(Clone)]
struct World {
    ve: VotingEscrow,
    dist: RewardsDistributor,
    wind: BTreeMap<AccountId, U256>,
    active_period: u64,
}

impl World {
    fn new(now: u64, block: u64) -> Self {
        let mut wind = BTreeMap::new();
        for i in 0..NUSERS {
            wind.insert(acct(USER_BASE + i), U256::from(INITIAL_FUNDS));
        }
        wind.insert(acct(OUTSIDER), U256::from(INITIAL_FUNDS));
        Self {
            ve: VotingEscrow::new(acct(TEAM), now, block),
            dist: RewardsDistributor::new(now, minter_acct(0)),
            wind,
            active_period: week_floor(now),
        }
    }

    fn balance(&self, who: u64) -> U256 {
        self.wind.get(&acct(who)).copied().unwrap_or(U256::ZERO)
    }

    fn debit(&mut self, who: AccountId, amount: U256) {
        let slot = self.wind.entry(who).or_default();
        *slot = slot.checked_sub(amount).expect("ledger underflow");
    }

    fn credit(&mut self, who: AccountId, amount: U256) {
        let slot = self.wind.entry(who).or_default();
        *slot = slot.checked_add(amount).expect("ledger overflow");
    }

    /// Moves the WIND a payout took out of the distributor: into the lock (`Deposited`) or to the owner.
    fn settle(&mut self, payout: &Payout) {
        match payout {
            Payout::None => {}
            Payout::Deposited { amount, .. } => {
                self.debit(acct(DIST_ACCOUNT), *amount);
                self.credit(acct(VE_ACCOUNT), *amount);
            }
            Payout::Transferred { to, amount } => {
                self.debit(acct(DIST_ACCOUNT), *amount);
                self.credit(*to, *amount);
            }
        }
    }
}

/// `keccak256` of `tokensPerWeek[w]` (32-byte big-endian words) for every week from `startTime` to the week of
/// `now`. This is what the generator's `digest()` computes.
fn digest(world: &World, now: u64) -> B256 {
    let end = week_floor(now);
    let mut buf = Vec::new();
    let mut w = world.dist.start_time;
    while w <= end {
        buf.extend_from_slice(&world.dist.tokens_in_week(w).to_be_bytes::<32>());
        w += WEEK;
    }
    keccak256(buf)
}

/// `lastTokenTime,tokenLastBalance,minter,activePeriod,digest,supply,epoch,dist,ve,u0..u4,outsider`.
fn gstate(world: &World, now: u64) -> String {
    let d = &world.dist;
    let minter_idx = acct_num(&d.minter) - MINTER_BASE;
    let users: Vec<String> = (0..NUSERS)
        .map(|i| world.balance(USER_BASE + i).to_string())
        .collect();
    format!(
        "{},{},{},{},{:#x},{},{},{},{},{},{}",
        d.last_token_time,
        d.token_last_balance,
        minter_idx,
        world.active_period,
        digest(world, now),
        world.ve.supply,
        world.ve.epoch,
        world.balance_or_zero(DIST_ACCOUNT),
        world.balance_or_zero(VE_ACCOUNT),
        users.join(","),
        world.balance(OUTSIDER),
    )
}

impl World {
    fn balance_or_zero(&self, who: u64) -> U256 {
        self.balance(who)
    }
}

/// `id:owner,amount,end,isPermanent,userEpoch,bias,slope,ts,blk,permanent,timeCursorOf,claimable` per token.
fn tokens(world: &World) -> String {
    let ve = &world.ve;
    (1..=ve.token_id)
        .map(|id| {
            let l = ve.locked_of(id);
            let ue = ve.user_point_epoch.get(&id).copied().unwrap_or(0);
            let up = ve
                .user_point_history
                .get(&(id, ue))
                .copied()
                .unwrap_or_default();
            let claimable = world
                .dist
                .claimable(id, ve)
                .unwrap_or_else(|e| panic!("claimable({id}) failed in Rust: {e:?}"));
            format!(
                "{id}:{},{},{},{},{ue},{},{},{},{},{},{},{claimable}",
                acct_num(&ve.owner(id)),
                l.amount,
                l.end,
                u8::from(l.is_permanent),
                up.bias,
                up.slope,
                up.ts,
                up.blk,
                up.permanent,
                world.dist.time_cursor(id),
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Whether a Rust error is the revert reason the Solidity call reported.
/// `Panic17` is the checked arithmetic panic (overflow or underflow), and `Panic18` is division by zero.
fn reason_matches(reason: &str, err: MathError) -> bool {
    use MathError::*;
    match reason {
        "Panic17" => matches!(err, Overflow | Underflow),
        "Panic18" => err == DivisionByZero,
        "SafeCastOverflow" => err == SafeCastOverflow,
        "SafeCastUnderflow" => err == SafeCastUnderflow,
        "AlreadyVoted" => err == AlreadyVoted,
        "AmountTooBig" => err == AmountTooBig,
        "LockDurationNotInFuture" => err == LockDurationNotInFuture,
        "LockDurationTooLong" => err == LockDurationTooLong,
        "LockExpired" => err == LockExpired,
        "LockNotExpired" => err == LockNotExpired,
        "NoLockFound" => err == NoLockFound,
        "NotApprovedOrOwner" => err == NotApprovedOrOwner,
        "NotOwner" => err == NotOwner,
        "NotPermanentLock" => err == NotPermanentLock,
        "NotTeam" => err == NotTeam,
        "NotVoter" => err == NotVoter,
        "OwnershipChange" => err == OwnershipChange,
        "PermanentLock" => err == PermanentLock,
        "SameNFT" => err == SameNft,
        "SplitNoOwner" => err == SplitNoOwner,
        "SplitNotAllowed" => err == SplitNotAllowed,
        "ZeroAmount" => err == ZeroAmount,
        "NotMinter" => err == NotMinter,
        "UpdatePeriod" => err == UpdatePeriod,
        "TransferToZeroAddress" => err == TransferToZeroAddress,
        _ => false,
    }
}

/// Coverage label for a payout.
fn kind(p: &Payout) -> &'static str {
    match p {
        Payout::None => "none",
        Payout::Deposited { .. } => "deposit",
        Payout::Transferred { .. } => "transfer",
    }
}

/// Executes one vector line on `world`. Returns the Rust output in the generator's format, and the payouts.
fn apply(
    world: &mut World,
    op: &str,
    now: u64,
    block: u64,
    a: &[&str],
) -> Result<(String, Vec<Payout>)> {
    let caller = || acct(n(a[0]));
    match op {
        "createLock" => {
            let (id, pulled) = world
                .ve
                .create_lock(caller(), now, block, u(a[1]), n(a[2]))?;
            world.debit(caller(), pulled);
            world.credit(acct(VE_ACCOUNT), pulled);
            Ok((format!("{id}:{pulled}"), vec![]))
        }
        "createLockFor" => {
            let (id, pulled) =
                world
                    .ve
                    .create_lock_for(acct(n(a[1])), now, block, u(a[2]), n(a[3]))?;
            world.debit(caller(), pulled);
            world.credit(acct(VE_ACCOUNT), pulled);
            Ok((format!("{id}:{pulled}"), vec![]))
        }
        "depositFor" => {
            let pulled = world.ve.deposit_for(now, block, n(a[1]), u(a[2]))?;
            world.debit(caller(), pulled);
            world.credit(acct(VE_ACCOUNT), pulled);
            Ok((pulled.to_string(), vec![]))
        }
        "increaseAmount" => {
            let pulled = world
                .ve
                .increase_amount(caller(), now, block, n(a[1]), u(a[2]))?;
            world.debit(caller(), pulled);
            world.credit(acct(VE_ACCOUNT), pulled);
            Ok((pulled.to_string(), vec![]))
        }
        "increaseUnlockTime" => {
            world
                .ve
                .increase_unlock_time(caller(), now, block, n(a[1]), n(a[2]))?;
            Ok((String::new(), vec![]))
        }
        "withdraw" => {
            let amount = world.ve.withdraw(caller(), now, block, n(a[1]))?;
            world.debit(acct(VE_ACCOUNT), amount);
            world.credit(caller(), amount);
            Ok((amount.to_string(), vec![]))
        }
        "lockPermanent" => {
            world.ve.lock_permanent(caller(), now, block, n(a[1]))?;
            Ok((String::new(), vec![]))
        }
        "unlockPermanent" => {
            world.ve.unlock_permanent(caller(), now, block, n(a[1]))?;
            Ok((String::new(), vec![]))
        }
        "transferFrom" => {
            world
                .ve
                .transfer_from(caller(), block, acct(n(a[1])), acct(n(a[2])), n(a[3]))?;
            Ok((String::new(), vec![]))
        }
        "rebase" => {
            // The minter's `transfer` lands first, then `checkpointToken` runs as the minter.
            let amount = u(a[1]);
            world.credit(acct(DIST_ACCOUNT), amount);
            let balance = world.balance(DIST_ACCOUNT);
            world
                .dist
                .checkpoint_token(minter_acct(n(a[0])), now, balance)?;
            Ok((String::new(), vec![]))
        }
        "setMinter" => {
            world
                .dist
                .set_minter(minter_acct(n(a[0])), minter_acct(n(a[1])))?;
            Ok((String::new(), vec![]))
        }
        "updatePeriod" => {
            world.active_period = n(a[0]);
            Ok((String::new(), vec![]))
        }
        "claim" => {
            let payout =
                world
                    .dist
                    .claim(now, block, world.active_period, n(a[0]), &mut world.ve)?;
            world.settle(&payout);
            Ok((payout.amount().to_string(), vec![payout]))
        }
        "claimMany" => {
            let tokens: Vec<u64> = a.iter().map(|t| n(t)).collect();
            let payouts =
                world
                    .dist
                    .claim_many(now, block, world.active_period, &tokens, &mut world.ve)?;
            for p in &payouts {
                world.settle(p);
            }
            Ok(("true".to_string(), payouts))
        }
        other => panic!("unknown op {other}"),
    }
}

/// Replays `rebase.txt`. Every line's Solidity outcome and post-state must match the Rust port.
#[test]
fn rebase_stateful_replay() {
    let text = read_vectors();
    let mut lines = text.lines().filter(|l| !l.is_empty());

    let init: Vec<&str> = lines
        .next()
        .expect("empty vector file")
        .split('|')
        .collect();
    assert_eq!(init.len(), 8, "init line");
    assert_eq!(init[0], "init");
    let (now0, block0) = (n(init[1]), n(init[2]));
    let mut world = World::new(now0, block0);
    assert_eq!(gstate(&world, now0), init[6], "init state");
    assert_eq!(tokens(&world), init[7], "init tokens");

    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    let mut bump = |key: String| *tally.entry(key).or_default() += 1;
    let mut checked = 0usize;

    for line in lines {
        let f: Vec<&str> = line.split('|').collect();
        assert_eq!(f.len(), 8, "bad line: {line}");
        let (op, now, block, status, out, g, t) = (f[0], n(f[1]), n(f[2]), f[4], f[5], f[6], f[7]);
        let args: Vec<&str> = if f[3].is_empty() {
            Vec::new()
        } else {
            f[3].split(',').collect()
        };

        // Coverage facts known before the call.
        let gap_weeks = now.saturating_sub(world.dist.last_token_time) / WEEK;
        let cursor_before: Option<(u64, u64)> = match op {
            "claim" => Some((n(args[0]), world.dist.time_cursor(n(args[0])))),
            _ => None,
        };

        let mut next = world.clone();
        let res = apply(&mut next, op, now, block, &args);
        match status {
            "ok" => {
                let (got, payouts) =
                    res.unwrap_or_else(|e| panic!("{line}\nsolidity ok, rust returned {e:?}"));
                assert_eq!(got, out, "output mismatch\n{line}");
                world = next;
                if op == "rebase" && gap_weeks > CHECKPOINT_WEEK_CAP as u64 {
                    bump("rebase [gap>20 weeks] ok".into());
                }
                if let Some((token, before)) = cursor_before {
                    let advanced = world.dist.time_cursor(token) - before;
                    if advanced == CLAIM_WEEK_CAP as u64 * WEEK {
                        bump("claim [claim cap 50 weeks hit] ok".into());
                    }
                }
                if payouts.is_empty() {
                    bump(format!("{op} ok"));
                } else {
                    for p in &payouts {
                        bump(format!("{op} paid {}", kind(p)));
                    }
                }
            }
            "err" => {
                let e = res.expect_err(&format!(
                    "{line}\nsolidity reverted {out}, rust returned ok"
                ));
                assert!(
                    reason_matches(out, e),
                    "reason mismatch\n{line}\nrust: {e:?}"
                );
                bump(format!("{op} err {out}"));
            }
            other => panic!("bad status {other}\n{line}"),
        }

        assert_eq!(gstate(&world, now), g, "global state mismatch\n{line}");
        assert_eq!(tokens(&world), t, "token state mismatch\n{line}");
        checked += 1;
    }

    eprintln!("rebase replay: {checked} calls matched Solidity");
    eprintln!("coverage:");
    for (k, v) in &tally {
        eprintln!("  {v:>5}  {k}");
    }

    let required = [
        "rebase ok",
        "rebase err NotMinter",
        "setMinter ok",
        "setMinter err NotMinter",
        "rebase [gap>20 weeks] ok",
        "claim [claim cap 50 weeks hit] ok",
        "claim paid deposit",
        "claim paid transfer",
        "claim paid none",
        "claimMany paid deposit",
        "claimMany paid transfer",
        "claim err UpdatePeriod",
        "withdraw ok",
    ];
    for key in required {
        let hits = tally.get(key).copied().unwrap_or(0);
        assert!(hits > 0, "required path never exercised: {key}");
    }
    // The payout to a burnt token (owner zero) reverts. Either entry point may exercise it.
    let burnt_reverts = tally
        .get("claim err TransferToZeroAddress")
        .copied()
        .unwrap_or(0)
        + tally
            .get("claimMany err TransferToZeroAddress")
            .copied()
            .unwrap_or(0);
    assert!(
        burnt_reverts > 0,
        "required path never exercised: payout to a burnt token"
    );
}
