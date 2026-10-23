//! Differential test, full `VotingEscrow`: voting checkpoints and delegation, managed veNFTs, and their
//! locked and free reward contracts, replayed against the windv2 Solidity.
//!
//! Vectors come from `diff-tests/` (Foundry): `cd diff-tests && forge test --match-test
//! testGenerateVotingEscrowFull`, which writes `vectors/voting_escrow_full.txt`. The core-only replay is
//! `diff_ve.rs`. This one covers the rest of the escrow.
//!
//! Each line is `op|now|block|args|ok|out|G|T|M`. The file is replayed in order on one `VotingEscrow`. A
//! Solidity revert is an `Err` from Rust, and the state is restored from a clone. After every call the
//! global point (`G`), the touched tokens (`T`) and the touched managed veNFTs (`M`) are compared with the
//! Rust state. `T` covers the lock, the user point, the slope changes, the escrow type, the managed link,
//! the voting checkpoint history, and the reward contracts of the token's last managed veNFT.
//!
//! Harness inputs, not escrow logic: the signer recovered by `ecrecover` and the `s`-range result are
//! passed to `delegateBySig` (the NEAR layer owns signature verification). The Voter is modeled as the
//! `voter`, `governor` (888) and `emergency council` (999) values the generator's mock returns. The
//! harness records which managed veNFT each token was last deposited into (`dm`), as the generator does.
//! That mapping is not escrow state, but it selects which reward contracts `T` reads.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use windswap_engine::error::Result;
use windswap_engine::fees::AccountId;
use windswap_engine::rewards::Reward;
use windswap_engine::ve::{EscrowType, VotingEscrow, ZERO_ACCOUNT};
use windswap_engine::{MathError, U256};

/// Governor and emergency council of the mock Voter (see `diff-tests/src/ve/VeFullMocks.sol`).
const GOVERNOR: &str = "888";
const EMERGENCY_COUNCIL: &str = "999";

/// The escrow plus the harness-side deposit record `dm`.
#[derive(Clone)]
struct World {
    ve: VotingEscrow,
    /// Managed veNFT each token was last successfully deposited into.
    deposited: BTreeMap<u64, u64>,
}

fn read_vectors() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../diff-tests/vectors")
        .join("voting_escrow_full.txt");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// An address string (decimal `uint160`, as the generator prints it) as the 32-byte account id.
fn acct(s: &str) -> AccountId {
    U256::from_str_radix(s, 10)
        .unwrap_or_else(|e| panic!("bad address {s}: {e}"))
        .to_be_bytes::<32>()
}

/// The decimal form of an account id, as the generator prints addresses.
fn acct_str(a: &AccountId) -> String {
    U256::from_be_bytes::<32>(*a).to_string()
}

fn u(s: &str) -> U256 {
    s.parse().unwrap_or_else(|e| panic!("bad uint {s}: {e}"))
}

fn n(s: &str) -> u64 {
    s.parse().unwrap_or_else(|e| panic!("bad u64 {s}: {e}"))
}

fn flag(s: &str) -> bool {
    match s {
        "1" => true,
        "0" => false,
        _ => panic!("bad flag {s}"),
    }
}

fn escrow_code(e: EscrowType) -> u8 {
    match e {
        EscrowType::Normal => 0,
        EscrowType::Locked => 1,
        EscrowType::Managed => 2,
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

/// `T` entry of one token. See the module docs for the field order.
fn tok(w: &World, id: u64, pre_end: u64, now: u64) -> Result<String> {
    let ve = &w.ve;
    let l = ve.locked_of(id);
    let ue = ve.user_point_epoch.get(&id).copied().unwrap_or(0);
    let up = ve
        .user_point_history
        .get(&(id, ue))
        .copied()
        .unwrap_or_default();
    let core = format!(
        "{id}:{},{},{},{},{ue},{},{},{},{},{},{},{}",
        acct_str(&ve.owner(id)),
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
    );
    let m = ve.id_to_managed.get(&id).copied().unwrap_or(0);
    let weight = ve.weights.get(&(id, m)).copied().unwrap_or_default();
    let cps = if ve.num_checkpoints(id) == 0 {
        "-".to_owned()
    } else {
        (0..ve.num_checkpoints(id))
            .map(|i| {
                let c = ve.checkpoint_at(id, i);
                format!(
                    "{}.{}.{}.{}",
                    c.from_timestamp,
                    acct_str(&c.owner),
                    c.delegated_balance,
                    c.delegatee
                )
            })
            .collect::<Vec<_>>()
            .join("~")
    };
    let dm = w.deposited.get(&id).copied().unwrap_or(0);
    let rewards = if dm == 0 {
        "0,0,0,0,0,0,0".to_owned()
    } else {
        let rs = ve
            .managed_rewards_of(dm)
            .expect("deposited into a managed veNFT");
        let token = ve.token.as_str();
        let lmr = &rs.locked.reward;
        let fmr = &rs.free.reward;
        format!(
            "{dm},{},{},{},{},{},{}",
            lmr.balance_of(id),
            lmr.num_checkpoints(id),
            lmr.earned(token, id, now)?,
            fmr.balance_of(id),
            fmr.num_checkpoints(id),
            fmr.earned(token, id, now)?,
        )
    };
    Ok(format!(
        "{core},{},{m},{weight},{},{},{cps},{rewards}",
        escrow_code(ve.escrow_type_of(id)),
        ve.delegate_of(id),
        ve.num_checkpoints(id),
    ))
}

/// `M` entry of one managed veNFT: `m:deactivated,lmrSupply,lmrSupplyNumCp,fmrSupply,fmrSupplyNumCp`.
fn managed_entry(ve: &VotingEscrow, m: u64) -> String {
    let rs = ve
        .managed_rewards_of(m)
        .expect("managed veNFT has reward contracts");
    let (lmr, fmr): (&Reward, &Reward) = (&rs.locked.reward, &rs.free.reward);
    format!(
        "{m}:{},{},{},{},{}",
        u8::from(ve.is_deactivated(m)),
        lmr.total_supply,
        lmr.supply_num_checkpoints(),
        fmr.total_supply,
        fmr.supply_num_checkpoints(),
    )
}

/// Unique non-zero ids in order (the generator's `tokList` / `mList` order).
fn unique_ids(ids: &[u64]) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::new();
    for &id in ids {
        if id != 0 && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// Whether a Rust error is the revert reason the Solidity call reported.
/// `Panic17` is checked arithmetic, so either overflow or underflow matches.
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
        "NotNormalNFT" => err == NotNormalNft,
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
        "NonExistentToken" => err == NonExistentToken,
        "InvalidSignature" => err == InvalidSignature,
        "InvalidSignatureS" => err == InvalidSignatureS,
        "InvalidNonce" => err == InvalidNonce,
        "SignatureExpired" => err == SignatureExpired,
        "SameAddress" => err == SameAddress,
        "ZeroAddress" => err == ZeroAddress,
        "NotGovernor" => err == NotGovernor,
        "NotGovernorOrManager" => err == NotGovernorOrManager,
        "NotEmergencyCouncilOrGovernor" => err == NotEmergencyCouncilOrGovernor,
        "NotManagedNFT" => err == NotManagedNft,
        "NotLockedNFT" => err == NotLockedNft,
        "NotManagedOrNormalNFT" => err == NotManagedOrNormalNft,
        "InvalidManagedNFTId" => err == InvalidManagedNftId,
        "SameState" => err == SameState,
        "ZeroBalance" => err == ZeroBalance,
        "NotDistributor" => err == NotDistributor,
        "NotAuthorized" => err == NotAuthorized,
        "NotWhitelisted" => err == NotWhitelisted,
        "NotVotingEscrow" => err == NotVotingEscrow,
        "NotSingleToken" => err == NotSingleToken,
        "NotEscrowToken" => err == NotEscrowToken,
        "InvalidReward" => err == InvalidReward,
        "NotGauge" => err == NotGauge,
        _ => false,
    }
}

/// Executes one vector line on `w`. `a` are the comma-separated args. Returns the output in the generator's
/// format.
fn apply(w: &mut World, op: &str, now: u64, block: u64, a: &[&str]) -> Result<String> {
    let caller = || acct(a[0]);
    let gov = acct(GOVERNOR);
    let ec = acct(EMERGENCY_COUNCIL);
    let ve = &mut w.ve;
    Ok(match op {
        "setVoterAndDistributor" => {
            ve.set_voter_and_distributor(acct(a[0]), acct(a[1]), acct(a[2]))?;
            String::new()
        }
        "setAllowedManager" => {
            ve.set_allowed_manager(acct(a[0]), gov, acct(a[1]))?;
            String::new()
        }
        "createLock" => {
            let (id, pulled) = ve.create_lock(caller(), now, block, u(a[1]), n(a[2]))?;
            format!("{id}:{pulled}")
        }
        "createLockFor" => {
            let (id, pulled) = ve.create_lock_for(acct(a[1]), now, block, u(a[2]), n(a[3]))?;
            format!("{id}:{pulled}")
        }
        "depositFor" => ve
            .deposit_for(caller(), now, block, n(a[1]), u(a[2]))?
            .to_string(),
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
            ve.toggle_split(caller(), acct(a[1]), flag(a[2]))?;
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
            ve.transfer_from(caller(), now, block, acct(a[1]), acct(a[2]), n(a[3]))?;
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
        "delegate" => {
            ve.delegate(caller(), now, block, n(a[1]), n(a[2]))?;
            String::new()
        }
        "delegateBySig" => ve
            .delegate_by_sig(
                now,
                block,
                n(a[1]),
                n(a[2]),
                u(a[3]),
                u(a[4]),
                flag(a[5]),
                acct(a[6]),
            )?
            .to_string(),
        "getPastVotes" => ve
            .get_past_votes(&acct(a[0]), n(a[1]), n(a[2]))?
            .to_string(),
        "createManagedLockFor" => ve
            .create_managed_lock_for(caller(), gov, now, block, acct(a[1]))?
            .to_string(),
        "depositManaged" => {
            let (token, m) = (n(a[1]), n(a[2]));
            ve.deposit_managed(caller(), now, block, token, m)?;
            w.deposited.insert(token, m);
            String::new()
        }
        "withdrawManaged" => {
            let (total, reward) = ve.withdraw_managed(caller(), now, block, n(a[1]))?;
            format!("{total}:{reward}")
        }
        "setManagedState" => {
            ve.set_managed_state(caller(), gov, ec, n(a[1]), flag(a[2]))?;
            String::new()
        }
        "freeNotify" => {
            ve.notify_free_reward(now, n(a[1]), a[2], u(a[3]), flag(a[4]))?;
            String::new()
        }
        "freeClaim" => {
            let tokens: Vec<String> = a[3..].iter().map(|t| (*t).to_owned()).collect();
            ve.claim_free_reward(caller(), now, n(a[1]), n(a[2]), &tokens)?
                .iter()
                .map(|(_, amount)| amount.to_string())
                .collect::<Vec<_>>()
                .join(",")
        }
        other => panic!("unknown op {other}"),
    })
}

/// Replays `voting_escrow_full.txt`. Every line's Solidity outcome and post-state must match the Rust port.
#[test]
fn ve_full_stateful_replay() {
    let text = read_vectors();
    let mut lines = text.lines().filter(|l| !l.is_empty());

    let init: Vec<&str> = lines
        .next()
        .expect("empty vector file")
        .split('|')
        .collect();
    assert_eq!(init.len(), 9, "init line");
    assert_eq!(init[0], "init");
    let mut ve = VotingEscrow::new(acct("777"), n(init[1]), n(init[2]));
    ve.token = init[3].to_owned();
    let mut world = World {
        ve,
        deposited: BTreeMap::new(),
    };
    assert_eq!(gstate(&world.ve), init[6], "init");

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut lines_checked = 0usize;
    for line in lines {
        let f: Vec<&str> = line.split('|').collect();
        assert_eq!(f.len(), 9, "bad line: {line}");
        let (op, now, block, status, out, g, toks, managed_list) =
            (f[0], n(f[1]), n(f[2]), f[4], f[5], f[6], f[7], f[8]);
        let args: Vec<&str> = if f[3].is_empty() {
            Vec::new()
        } else {
            f[3].split(',').collect()
        };

        // Touched ids in the generator's order, with their pre-call lock ends.
        let ids: Vec<u64> = toks
            .split(';')
            .filter(|t| !t.is_empty())
            .map(|t| n(t.split(':').next().unwrap()))
            .collect();
        let pre: Vec<u64> = ids.iter().map(|&id| world.ve.locked_of(id).end).collect();

        // Each call runs on a clone, so a Rust `Err` leaves `world` as it was (Solidity revert).
        let mut next = world.clone();
        let res = apply(&mut next, op, now, block, &args);
        match status {
            "ok" => {
                let got =
                    res.unwrap_or_else(|e| panic!("{line}\nsolidity ok, rust returned {e:?}"));
                assert_eq!(got, out, "output mismatch\n{line}");
                world = next;
                *counts.entry(format!("{op} ok")).or_default() += 1;
            }
            "err" => {
                let e = res.expect_err(&format!(
                    "{line}\nsolidity reverted {out}, rust returned ok"
                ));
                assert!(
                    reason_matches(out, e),
                    "reason mismatch\n{line}\nrust: {e:?}"
                );
                *counts.entry(format!("{op} err {out}")).or_default() += 1;
            }
            other => panic!("bad status {other}\n{line}"),
        }

        assert_eq!(gstate(&world.ve), g, "global state mismatch\n{line}");
        let got_toks: Vec<String> = ids
            .iter()
            .zip(&pre)
            .map(|(&id, &pre_end)| tok(&world, id, pre_end, now))
            .collect::<Result<_>>()
            .unwrap_or_else(|e| panic!("{line}\nearned failed: {e:?}"));
        assert_eq!(got_toks.join(";"), toks, "token state mismatch\n{line}");

        let managed: Vec<u64> = unique_ids(&ids)
            .into_iter()
            .filter(|&id| world.ve.escrow_type_of(id) == EscrowType::Managed)
            .collect();
        let got_managed: Vec<String> = managed
            .iter()
            .map(|&m| managed_entry(&world.ve, m))
            .collect();
        assert_eq!(
            got_managed.join(";"),
            managed_list,
            "managed state mismatch\n{line}"
        );
        lines_checked += 1;
    }

    eprintln!("ve full replay: {lines_checked} calls matched Solidity");
    eprintln!("coverage (op outcome):");
    for (k, v) in &counts {
        eprintln!("  {v:>5}  {k}");
    }

    // Transitions from VOTINGESCROW.md (delegation and managed rows) that must occur at least once.
    let required = [
        "createManagedLockFor ok",
        "depositManaged ok",
        "withdrawManaged ok",
        "setManagedState ok",
        "setAllowedManager ok",
        "freeNotify ok",
        "freeClaim ok",
        "delegate ok",
        "delegateBySig ok",
        "getPastVotes ok",
        "depositFor ok",
        "increaseAmount ok",
    ];
    for key in required {
        assert!(
            counts.get(key).copied().unwrap_or(0) > 0,
            "required transition never exercised: {key}"
        );
    }
    let _ = ZERO_ACCOUNT;
}
