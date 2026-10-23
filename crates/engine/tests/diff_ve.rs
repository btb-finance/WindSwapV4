//! Differential test: the Rust `VotingEscrow` core port must match the windv2 Solidity `VotingEscrow`.
//!
//! Vectors come from `diff-tests/` (Foundry). They are generated from the real `VotingEscrow.sol`, with
//! a mock WIND token and no other mocks: `cd diff-tests && forge test --match-test testGenerateVotingEscrow`.
//!
//! The file is replayed in order on one `VotingEscrow`. Each line holds the call, its Solidity outcome
//! (`ok` with its return value, or `err` with a revert reason), the global state after the call, and
//! the touched tokens (owner, lock, user point, slope changes). Every field is compared after every call.
//! An `Err` from Rust discards the call (the state is restored from a clone, as in Solidity revert).
//!
//! Coverage is tallied by op, state class of the arguments (`empty`, `burnt`, `normal`, `expired`,
//! `permanent`) and outcome. The table is printed with `--nocapture`, and the test asserts that the
//! transitions from `VOTINGESCROW.md` in scope actually occur.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use windswap_engine::error::Result;
use windswap_engine::fees::AccountId;
use windswap_engine::ve::{VotingEscrow, ZERO_ACCOUNT};
use windswap_engine::{MathError, U256};

fn read_vectors() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../diff-tests/vectors")
        .join("voting_escrow.txt");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Account `n` maps to the 32-byte id with `n` in the low 8 bytes, matching the generator's
/// `uint160` addresses (`1000 + i` for users, `777` for team, `5000` for the outsider, `0` for zero).
fn acct(n: u64) -> AccountId {
    let mut a = [0u8; 32];
    a[24..].copy_from_slice(&n.to_be_bytes());
    a
}

fn acct_num(a: &AccountId) -> u64 {
    assert!(a[..24].iter().all(|&b| b == 0), "unexpected account {a:?}");
    u64::from_be_bytes(a[24..].try_into().unwrap())
}

fn u(s: &str) -> U256 {
    s.parse().unwrap()
}

fn n(s: &str) -> u64 {
    s.parse().unwrap()
}

fn flag(s: &str) -> bool {
    match s {
        "1" => true,
        "0" => false,
        _ => panic!("bad flag {s}"),
    }
}

/// `epoch,supply,permanentLockBalance,bias,slope,ts,blk,permanentLockBalance` of `pointHistory(epoch)`.
fn gstate(ve: &VotingEscrow) -> String {
    let gp = ve.point_history.get(&ve.epoch).copied().unwrap_or_default();
    format!(
        "{},{},{},{},{},{},{},{}",
        ve.epoch,
        ve.supply,
        ve.permanent_lock_balance,
        gp.bias,
        gp.slope,
        gp.ts,
        gp.blk,
        gp.permanent_lock_balance
    )
}

/// `id:owner,amount,end,isPermanent,userEpoch,bias,slope,ts,blk,permanent,slopeChangePre,slopeChangePost`.
/// `pre_end` is the lock end before the call (zero for a token that did not exist).
fn tok(ve: &VotingEscrow, id: u64, pre_end: u64) -> String {
    let l = ve.locked_of(id);
    let ue = ve.user_point_epoch.get(&id).copied().unwrap_or(0);
    let up = ve
        .user_point_history
        .get(&(id, ue))
        .copied()
        .unwrap_or_default();
    format!(
        "{id}:{},{},{},{},{ue},{},{},{},{},{},{},{}",
        acct_num(&ve.owner(id)),
        l.amount,
        l.end,
        u8::from(l.is_permanent),
        up.bias,
        up.slope,
        up.ts,
        up.blk,
        up.permanent,
        ve.slope_change(pre_end),
        ve.slope_change(l.end),
    )
}

/// Whether a Rust error is the revert reason the Solidity call reported.
/// `Panic17` is the checked arithmetic panic (overflow or underflow), so either variant matches.
fn reason_matches(reason: &str, err: MathError) -> bool {
    use MathError::*;
    match reason {
        "Panic17" => matches!(err, Overflow | Underflow),
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
        _ => false,
    }
}

/// Executes one vector line on `ve`. `a` are the comma-separated args. Returns the Rust output in the
/// same format as the generator (`token:pulled`, `pulled`, `value`, `t1:t2`, or empty).
fn apply(ve: &mut VotingEscrow, op: &str, now: u64, block: u64, a: &[&str]) -> Result<String> {
    let caller = || acct(n(a[0]));
    Ok(match op {
        "init" => String::new(),
        "createLock" => {
            let (id, pulled) = ve.create_lock(caller(), now, block, u(a[1]), n(a[2]))?;
            format!("{id}:{pulled}")
        }
        "createLockFor" => {
            let (id, pulled) = ve.create_lock_for(acct(n(a[1])), now, block, u(a[2]), n(a[3]))?;
            format!("{id}:{pulled}")
        }
        "depositFor" => ve.deposit_for(now, block, n(a[1]), u(a[2]))?.to_string(),
        "increaseAmount" => ve
            .increase_amount(caller(), now, block, n(a[1]), u(a[2]))?
            .to_string(),
        "increaseUnlockTime" => {
            ve.increase_unlock_time(caller(), now, block, n(a[1]), n(a[2]))?;
            String::new()
        }
        "withdraw" => ve.withdraw(caller(), now, block, n(a[1]))?.to_string(),
        "merge" => {
            ve.merge(caller(), now, block, n(a[1]), n(a[2]))?;
            String::new()
        }
        "split" => {
            let (t1, t2) = ve.split(caller(), now, block, n(a[1]), u(a[2]))?;
            format!("{t1}:{t2}")
        }
        "toggleSplit" => {
            ve.toggle_split(caller(), acct(n(a[1])), flag(a[2]))?;
            String::new()
        }
        "lockPermanent" => {
            ve.lock_permanent(caller(), now, block, n(a[1]))?;
            String::new()
        }
        "unlockPermanent" => {
            ve.unlock_permanent(caller(), now, block, n(a[1]))?;
            String::new()
        }
        "transferFrom" => {
            ve.transfer_from(caller(), block, acct(n(a[1])), acct(n(a[2])), n(a[3]))?;
            String::new()
        }
        "voting" => {
            ve.voting(caller(), n(a[1]), flag(a[2]))?;
            String::new()
        }
        "checkpoint" => {
            ve.checkpoint(now, block)?;
            String::new()
        }
        "balanceOfNFT" => ve.balance_of_nft(now, block, n(a[0]))?.to_string(),
        "balanceOfNFTAt" => ve.balance_of_nft_at(n(a[0]), n(a[1]))?.to_string(),
        "totalSupply" => ve.total_supply(now)?.to_string(),
        "totalSupplyAt" => ve.total_supply_at(n(a[0]))?.to_string(),
        other => panic!("unknown op {other}"),
    })
}

/// Coarse state class of a token before the call, used for the coverage table.
fn class_of(ve: &VotingEscrow, id: u64, now: u64) -> &'static str {
    if id == 0 || id > ve.token_id {
        "empty"
    } else if ve.owner(id) == ZERO_ACCOUNT {
        "burnt"
    } else {
        let l = ve.locked_of(id);
        if l.is_permanent {
            "permanent"
        } else if l.end <= now {
            "expired"
        } else {
            "normal"
        }
    }
}

#[derive(Default)]
struct Tally(BTreeMap<String, usize>);

impl Tally {
    fn bump(&mut self, key: String) {
        *self.0.entry(key).or_default() += 1;
    }

    fn get(&self, key: &str) -> usize {
        self.0.get(key).copied().unwrap_or(0)
    }
}

/// Replays `voting_escrow.txt`. Every line's Solidity outcome and post-state must match the Rust port.
#[test]
fn ve_stateful_replay() {
    let text = read_vectors();
    let mut lines = text.lines().filter(|l| !l.is_empty());

    let init: Vec<&str> = lines
        .next()
        .expect("empty vector file")
        .split('|')
        .collect();
    assert_eq!(init.len(), 8, "init line");
    assert_eq!(init[0], "init");
    let mut ve = VotingEscrow::new(acct(777), n(init[1]), n(init[2]));
    assert_eq!(gstate(&ve), init[6], "init");

    let mut tally = Tally::default();
    let mut lines_checked = 0usize;

    for line in lines {
        let f: Vec<&str> = line.split('|').collect();
        assert_eq!(f.len(), 8, "bad line: {line}");
        let (op, now, block, status, out, g, toks) =
            (f[0], n(f[1]), n(f[2]), f[4], f[5], f[6], f[7]);
        let args: Vec<&str> = if f[3].is_empty() {
            Vec::new()
        } else {
            f[3].split(',').collect()
        };

        // Touched token ids, in the order the generator lists them, and their pre-call lock ends.
        let ids: Vec<u64> = toks
            .split(';')
            .filter(|t| !t.is_empty())
            .map(|t| n(t.split(':').next().unwrap()))
            .collect();
        let pre: Vec<u64> = ids.iter().map(|&id| ve.locked_of(id).end).collect();
        let classes = if op == "merge" {
            ids.iter()
                .map(|&id| class_of(&ve, id, now))
                .collect::<Vec<_>>()
                .join("/")
        } else {
            ids.first()
                .map_or("-", |&id| class_of(&ve, id, now))
                .to_string()
        };

        // Each call runs on a clone, so a Rust `Err` leaves `ve` as it was (Solidity revert).
        let mut next = ve.clone();
        let res = apply(&mut next, op, now, block, &args);
        match status {
            "ok" => {
                let got =
                    res.unwrap_or_else(|e| panic!("{line}\nsolidity ok, rust returned {e:?}"));
                assert_eq!(got, out, "output mismatch\n{line}");
                ve = next;
                tally.bump(format!("{op} [{classes}] ok"));
            }
            "err" => {
                let e = res.expect_err(&format!(
                    "{line}\nsolidity reverted {out}, rust returned ok"
                ));
                assert!(
                    reason_matches(out, e),
                    "reason mismatch\n{line}\nrust: {e:?}"
                );
                tally.bump(format!("{op} [{classes}] err {out}"));
            }
            other => panic!("bad status {other}\n{line}"),
        }

        assert_eq!(gstate(&ve), g, "global state mismatch\n{line}");
        let got_toks: Vec<String> = ids
            .iter()
            .zip(&pre)
            .map(|(&id, &pre_end)| tok(&ve, id, pre_end))
            .collect();
        assert_eq!(got_toks.join(";"), toks, "token state mismatch\n{line}");
        lines_checked += 1;
    }

    eprintln!("ve replay: {lines_checked} calls matched Solidity");
    eprintln!("coverage (op [state of args] outcome):");
    for (k, v) in &tally.0 {
        eprintln!("  {v:>5}  {k}");
    }

    // Transitions from VOTINGESCROW.md (in-scope rows) that must occur at least once.
    let required = [
        "createLock [empty] ok",
        "createLockFor [empty] ok",
        "depositFor [normal] ok",
        "depositFor [permanent] ok",
        "increaseAmount [normal] ok",
        "increaseAmount [permanent] ok",
        "increaseUnlockTime [normal] ok",
        "withdraw [expired] ok",
        "merge [normal/normal] ok",
        "merge [normal/permanent] ok",
        "split [normal] ok",
        "split [permanent] ok",
        "lockPermanent [normal] ok",
        "unlockPermanent [permanent] ok",
        "transferFrom [normal] ok",
    ];
    for key in required {
        assert!(
            tally.get(key) > 0,
            "required transition never exercised: {key}"
        );
    }
}
