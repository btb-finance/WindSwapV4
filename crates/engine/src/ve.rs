//! Port of the core lock logic of windv2 `VotingEscrow.sol` (veNFT escrow) and of
//! `BalanceLogicLibrary.sol`.
//!
//! Ported: `LockedBalance`, `UserPoint`, `GlobalPoint`, `_checkpoint` (slope and bias, scheduled
//! slope changes, the weekly global point loop), `createLock` / `createLockFor`, `depositFor`,
//! `increaseAmount`, `increaseUnlockTime`, `withdraw`, `merge`, `split` (with `toggleSplit`),
//! `lockPermanent`, `unlockPermanent`, `balanceOfNFT(At)`, `totalSupply(At)`, `checkpoint()`, the point
//! getters, `permanentLockBalance` and `supply`, plus `voting` (sets `voted`).
//!
//! Not ported (out of scope for this module):
//! - Managed veNFTs (`createManagedLockFor`, `depositManaged`, `withdrawManaged`, `setManagedState`) and
//!   the `EscrowType` checks. Every token here is `NORMAL`, so `NotNormalNFT` and `NotManagedOrNormalNFT`
//!   cannot fire.
//! - Delegation and voting checkpoints (`DelegationLogicLibrary`). `_checkpointDelegator` (mint, transfer,
//!   burn) and `_checkpointDelegatee` (merge, split, increase) are skipped. `_delegate`, called by
//!   `unlockPermanent`, is reduced to its one core-visible effect, the `OwnershipChange` revert. With no
//!   delegations, `_delegates` is always zero, so the skipped calls never touch the core state. The
//!   differential generator never calls `delegate` / `delegateBySig`, which is what keeps the vectors
//!   matching. Delegation checkpoints, `numCheckpoints`, `nonces` and `getPastVotes` are not modelled.
//! - ERC-721 approvals, operators, `safeTransferFrom` (and its receiver check), metadata, art proxy,
//!   `team` / `allowedManager` administration beyond `toggleSplit`. Ownership is a plain `owner_of` map,
//!   and the caller must be the owner, which is `_isApprovedOrOwner` with no approvals.
//! - Token balances and events. Each function that pulls or pays WIND returns the amount instead, and the
//!   caller moves the balances. Solidity pulls WIND in `_depositFor` and pays it out in `withdraw`.
//! - The `voter` / `distributor` wiring is limited to the `voter` check in `voting`. `voter` starts as
//!   `team`, as in the constructor.
//!
//! Atomicity: as with ClPool, an `Err` means the call must be discarded. The Solidity call reverts and
//! the state is unchanged, and a caller that needs rollback clones the struct before the call. The
//! differential test does exactly that.
//!
//! Inputs replace `block.timestamp` (`now`), `block.number` (`block`) and `msg.sender` (`caller`). Each
//! method documents which of these it reads.

use std::collections::BTreeMap;

use crate::error::{MathError, Result};
use crate::fees::AccountId;
use crate::U256;

/// `VotingEscrow.WEEK` (`1 weeks`).
pub const WEEK: u64 = 7 * 24 * 60 * 60;
/// `VotingEscrow.MAXTIME` (`4 * 365 * 86400`).
pub const MAXTIME: u64 = 4 * 365 * 86_400;
/// `VotingEscrow.iMAXTIME`: the `int128` form of `MAXTIME`.
const I_MAXTIME: i128 = MAXTIME as i128;
/// `VotingEscrow.MULTIPLIER` (`1 ether`): fixed-point scale of the block-per-second slope.
const MULTIPLIER: u128 = 1_000_000_000_000_000_000;
/// The zero address. `canSplit[address(0)]` is the flag that lets any owner split.
pub const ZERO_ACCOUNT: AccountId = [0u8; 32];

/// `IVotingEscrow.LockedBalance`. `end` is zero for a permanent lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LockedBalance {
    /// `amount` (`int128`).
    pub amount: i128,
    /// `end`: unlock time in seconds, zero for a permanent lock.
    pub end: u64,
    /// `isPermanent`.
    pub is_permanent: bool,
}

/// `IVotingEscrow.UserPoint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UserPoint {
    /// `bias` (`int128`).
    pub bias: i128,
    /// `slope` (`int128`), in negative weight per second.
    pub slope: i128,
    /// `ts`: block timestamp of the checkpoint.
    pub ts: u64,
    /// `blk`: block number of the checkpoint.
    pub blk: u64,
    /// `permanent`: the locked amount while permanent, else zero.
    pub permanent: U256,
}

/// `IVotingEscrow.GlobalPoint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlobalPoint {
    /// `bias` (`int128`).
    pub bias: i128,
    /// `slope` (`int128`).
    pub slope: i128,
    /// `ts`: timestamp of the point.
    pub ts: u64,
    /// `blk`: estimated block number of the point.
    pub blk: u64,
    /// `permanentLockBalance` as of this point.
    pub permanent_lock_balance: U256,
}

/// State of the core `VotingEscrow`. Field names mirror the Solidity state variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VotingEscrow {
    /// `team`: may call `toggleSplit`.
    pub team: AccountId,
    /// `voter`: may call `voting`. Set to `team` by the constructor.
    pub voter: AccountId,
    /// `canSplit`: owners (and `ZERO_ACCOUNT`, meaning any owner) allowed to split.
    pub can_split: BTreeMap<AccountId, bool>,
    /// `tokenId`: the last minted token id. Starts at zero.
    pub token_id: u64,
    /// `idToOwner`. A missing entry is the zero address.
    pub owner_of: BTreeMap<u64, AccountId>,
    /// `ownershipChange`: block of the last transfer of each token.
    pub ownership_change: BTreeMap<u64, u64>,
    /// `voted`.
    pub voted: BTreeMap<u64, bool>,
    /// `_locked`.
    pub locked: BTreeMap<u64, LockedBalance>,
    /// `epoch`: index of the last global point.
    pub epoch: u64,
    /// `supply`: sum of locked amounts.
    pub supply: U256,
    /// `permanentLockBalance`.
    pub permanent_lock_balance: U256,
    /// `_pointHistory`: global points by epoch. Index zero is the constructor point.
    pub point_history: BTreeMap<u64, GlobalPoint>,
    /// `userPointEpoch`.
    pub user_point_epoch: BTreeMap<u64, u64>,
    /// `_userPointHistory`, keyed by `(tokenId, epoch)`. Index zero is always empty.
    pub user_point_history: BTreeMap<(u64, u64), UserPoint>,
    /// `slopeChanges`: scheduled slope change at each lock end.
    pub slope_changes: BTreeMap<u64, i128>,
}

/// `int128` checked add (Solidity panics with `0x11`).
fn add_i(a: i128, b: i128) -> Result<i128> {
    a.checked_add(b).ok_or(MathError::Overflow)
}

/// `int128` checked sub (Solidity panics with `0x11`).
fn sub_i(a: i128, b: i128) -> Result<i128> {
    a.checked_sub(b).ok_or(MathError::Overflow)
}

/// `int128` checked mul (Solidity panics with `0x11`).
fn mul_i(a: i128, b: i128) -> Result<i128> {
    a.checked_mul(b).ok_or(MathError::Overflow)
}

/// `uint256` checked add (Solidity panics with `0x11`).
fn add_u(a: U256, b: U256) -> Result<U256> {
    a.checked_add(b).ok_or(MathError::Overflow)
}

/// `uint256` checked sub (Solidity panics with `0x11`).
fn sub_u(a: U256, b: U256) -> Result<U256> {
    a.checked_sub(b).ok_or(MathError::Underflow)
}

/// `SafeCastLibrary.toInt128(uint256)`: reverts `SafeCastOverflow` above `type(int128).max`.
fn to_int128(value: U256) -> Result<i128> {
    if value > U256::from(i128::MAX as u128) {
        return Err(MathError::SafeCastOverflow);
    }
    Ok(value.to::<u128>() as i128)
}

/// `SafeCastLibrary.toUint256(int128)`: reverts `SafeCastUnderflow` for a negative value.
fn to_uint256(value: i128) -> Result<U256> {
    if value < 0 {
        return Err(MathError::SafeCastUnderflow);
    }
    Ok(U256::from(value as u128))
}

/// `((now + duration) / WEEK) * WEEK`, computed in `u128` so it cannot overflow for `u64` inputs.
/// Used for `createLock` (`_createLock`), `increaseUnlockTime` and `unlockPermanent`.
fn round_to_week(now: u64, duration: u64) -> u128 {
    let week = WEEK as u128;
    ((now as u128 + duration as u128) / week) * week
}

fn to_u64(value: u128) -> Result<u64> {
    u64::try_from(value).map_err(|_| MathError::Overflow)
}

/// `BalanceLogicLibrary.getPastUserPointIndex`: the user point epoch at or before `t`.
fn past_user_point_index(
    user_point_epoch: &BTreeMap<u64, u64>,
    user_point_history: &BTreeMap<(u64, u64), UserPoint>,
    token: u64,
    t: u64,
) -> u64 {
    let user_epoch = user_point_epoch.get(&token).copied().unwrap_or(0);
    if user_epoch == 0 {
        return 0;
    }
    let point = |e: u64| {
        user_point_history
            .get(&(token, e))
            .copied()
            .unwrap_or_default()
    };
    if point(user_epoch).ts <= t {
        return user_epoch;
    }
    if point(1).ts > t {
        return 0;
    }
    let mut lower = 0u64;
    let mut upper = user_epoch;
    while upper > lower {
        let center = upper - (upper - lower) / 2;
        let p = point(center);
        if p.ts == t {
            return center;
        } else if p.ts < t {
            lower = center;
        } else {
            upper = center - 1;
        }
    }
    lower
}

impl VotingEscrow {
    /// Port of the `VotingEscrow` constructor. `team` is also `voter`. Point 0 is recorded at
    /// deployment (`now` for `block.timestamp`, `block` for `block.number`).
    pub fn new(team: AccountId, now: u64, block: u64) -> Self {
        let mut point_history = BTreeMap::new();
        point_history.insert(
            0,
            GlobalPoint {
                ts: now,
                blk: block,
                ..GlobalPoint::default()
            },
        );
        Self {
            team,
            voter: team,
            can_split: BTreeMap::new(),
            token_id: 0,
            owner_of: BTreeMap::new(),
            ownership_change: BTreeMap::new(),
            voted: BTreeMap::new(),
            locked: BTreeMap::new(),
            epoch: 0,
            supply: U256::ZERO,
            permanent_lock_balance: U256::ZERO,
            point_history,
            user_point_epoch: BTreeMap::new(),
            user_point_history: BTreeMap::new(),
            slope_changes: BTreeMap::new(),
        }
    }

    // ---- STORAGE HELPERS ----

    /// `ownerOf` (`_ownerOf`). The zero address if the token does not exist.
    pub fn owner(&self, token: u64) -> AccountId {
        self.owner_of.get(&token).copied().unwrap_or(ZERO_ACCOUNT)
    }

    /// `_locked[token]`.
    pub fn locked_of(&self, token: u64) -> LockedBalance {
        self.locked.get(&token).copied().unwrap_or_default()
    }

    /// `voted[token]`.
    pub fn is_voted(&self, token: u64) -> bool {
        self.voted.get(&token).copied().unwrap_or(false)
    }

    /// `canSplit[account]`.
    pub fn can_split_of(&self, account: &AccountId) -> bool {
        self.can_split.get(account).copied().unwrap_or(false)
    }

    /// `slopeChanges[t]`.
    pub fn slope_change(&self, t: u64) -> i128 {
        self.slope_changes.get(&t).copied().unwrap_or(0)
    }

    /// `ownershipChange[token]`.
    pub fn ownership_change_of(&self, token: u64) -> u64 {
        self.ownership_change.get(&token).copied().unwrap_or(0)
    }

    /// The owner check that `_isApprovedOrOwner` performs. The caller must own the token, as there are
    /// no approvals.
    fn require_owner(&self, caller: AccountId, token: u64) -> Result<()> {
        if self.owner(token) == caller {
            Ok(())
        } else {
            Err(MathError::NotApprovedOrOwner)
        }
    }

    /// `_mint` (`_addTokenTo`). Ownership only, delegation checkpoints are skipped.
    fn mint(&mut self, to: AccountId, token: u64) {
        self.owner_of.insert(token, to);
    }

    /// `_burn` (`_removeTokenFrom`). Checks the caller is the owner, as `_burn` does. Delegation
    /// checkpoints are skipped.
    fn burn(&mut self, caller: AccountId, token: u64) -> Result<()> {
        self.require_owner(caller, token)?;
        self.owner_of.remove(&token);
        Ok(())
    }

    // ---- CHECKPOINT ----

    /// Port of `VotingEscrow._checkpoint(_tokenId, _oldLocked, _newLocked)`.
    ///
    /// Reads `block.timestamp` (`now`) and `block.number` (`block`). With `token == 0` only the global
    /// point is updated (the `checkpoint()` path). Otherwise the user point and the scheduled slope
    /// changes are updated as well.
    ///
    /// Solidity order preserved:
    /// 1. Per-user slopes: `slope = amount / MAXTIME`, `bias = slope * (end - now)`, kept at zero when the
    ///    lock has ended or has no amount. `permanent` is `amount` for a permanent new lock.
    /// 2. `oldDslope = slopeChanges[old.end]`. `newDslope` is `oldDslope` when the ends are equal, else
    ///    `slopeChanges[new.end]` when `new.end != 0`.
    /// 3. Weekly loop (at most 255 weeks): step `t_i` one week at a time, apply the slope change
    ///    scheduled at each step (`t_i` clamped to `now`), subtract `slope * dt` from the bias, and clamp
    ///    the bias and slope at zero. The block number is interpolated with `blockSlope`. Intermediate
    ///    points are written, and the loop stops when `t_i == now`.
    /// 4. For a user checkpoint, add `new - old` to the slope and bias, clamp them at zero, and set
    ///    `permanentLockBalance` to the current value.
    /// 5. Overwrite `pointHistory[epoch]` when it has `ts == now`, otherwise append it and bump `epoch`.
    /// 6. Schedule the slope changes at `old.end` (when in the future) and `new.end`.
    /// 7. Overwrite the last user point if it has `ts == now`, otherwise append one.
    ///
    /// Any arithmetic overflow returns `Err` (`Overflow` / `Underflow`). `int128` casts return
    /// `SafeCastOverflow` / `SafeCastUnderflow`.
    fn checkpoint_inner(
        &mut self,
        now: u64,
        block: u64,
        token: u64,
        old: LockedBalance,
        new: LockedBalance,
    ) -> Result<()> {
        let mut u_old = UserPoint::default();
        let mut u_new = UserPoint::default();
        let mut old_dslope: i128 = 0;
        let mut new_dslope: i128 = 0;

        if token != 0 {
            u_new.permanent = if new.is_permanent {
                to_uint256(new.amount)?
            } else {
                U256::ZERO
            };
            if old.end > now && old.amount > 0 {
                u_old.slope = old.amount / I_MAXTIME;
                u_old.bias = mul_i(u_old.slope, (old.end - now) as i128)?;
            }
            if new.end > now && new.amount > 0 {
                u_new.slope = new.amount / I_MAXTIME;
                u_new.bias = mul_i(u_new.slope, (new.end - now) as i128)?;
            }
            old_dslope = self.slope_change(old.end);
            if new.end != 0 {
                new_dslope = if new.end == old.end {
                    old_dslope
                } else {
                    self.slope_change(new.end)
                };
            }
        }

        // `lastPoint` starts from the last global point, or from `(now, block)` when there is none.
        let mut last = if self.epoch > 0 {
            self.point_history
                .get(&self.epoch)
                .copied()
                .unwrap_or_default()
        } else {
            GlobalPoint {
                ts: now,
                blk: block,
                ..GlobalPoint::default()
            }
        };
        let initial = last;
        let mut last_checkpoint = last.ts;
        // `dblock/dt`, scaled by MULTIPLIER. Zero when `now` is not after the last point.
        let mut block_slope = U256::ZERO;
        if now > last.ts {
            let dblock = block.checked_sub(last.blk).ok_or(MathError::Underflow)?;
            block_slope = U256::from(MULTIPLIER) * U256::from(dblock) / U256::from(now - last.ts);
        }

        // Fill the weekly history and compute the current point.
        let mut epoch = self.epoch;
        let mut t_i = (last_checkpoint / WEEK) * WEEK;
        for _ in 0..255 {
            t_i = t_i.checked_add(WEEK).ok_or(MathError::Overflow)?;
            let mut d_slope: i128 = 0;
            if t_i > now {
                t_i = now;
            } else {
                d_slope = self.slope_change(t_i);
            }
            let dt = t_i
                .checked_sub(last_checkpoint)
                .ok_or(MathError::Underflow)?;
            last.bias = sub_i(last.bias, mul_i(last.slope, dt as i128)?)?;
            last.slope = add_i(last.slope, d_slope)?;
            if last.bias < 0 {
                last.bias = 0;
            }
            if last.slope < 0 {
                last.slope = 0;
            }
            last_checkpoint = t_i;
            last.ts = t_i;
            let blk_offset = block_slope * U256::from(t_i - initial.ts) / U256::from(MULTIPLIER);
            last.blk = to_u64(
                U256::from(initial.blk)
                    .checked_add(blk_offset)
                    .ok_or(MathError::Overflow)?
                    .to::<u128>(),
            )?;
            epoch += 1;
            if t_i == now {
                last.blk = block;
                break;
            } else {
                self.point_history.insert(epoch, last);
            }
        }

        if token != 0 {
            last.slope = add_i(last.slope, sub_i(u_new.slope, u_old.slope)?)?;
            last.bias = add_i(last.bias, sub_i(u_new.bias, u_old.bias)?)?;
            if last.slope < 0 {
                last.slope = 0;
            }
            if last.bias < 0 {
                last.bias = 0;
            }
            last.permanent_lock_balance = self.permanent_lock_balance;
        }

        // Same timestamp as the previous point: overwrite it. Otherwise append and bump `epoch`.
        let prev_ts = if epoch != 1 {
            self.point_history.get(&(epoch - 1)).map_or(0, |p| p.ts)
        } else {
            0
        };
        if epoch != 1 && prev_ts == now {
            self.point_history.insert(epoch - 1, last);
        } else {
            self.epoch = epoch;
            self.point_history.insert(epoch, last);
        }

        if token != 0 {
            // Scheduled slope changes: cancel the old slope at the old end, and schedule the new one.
            if old.end > now {
                old_dslope = add_i(old_dslope, u_old.slope)?;
                if new.end == old.end {
                    old_dslope = sub_i(old_dslope, u_new.slope)?;
                }
                self.slope_changes.insert(old.end, old_dslope);
            }
            if new.end > now && new.end > old.end {
                new_dslope = sub_i(new_dslope, u_new.slope)?;
                self.slope_changes.insert(new.end, new_dslope);
            }

            u_new.ts = now;
            u_new.blk = block;
            let mut user_epoch = self.user_point_epoch.get(&token).copied().unwrap_or(0);
            let same_ts = user_epoch != 0
                && self
                    .user_point_history
                    .get(&(token, user_epoch))
                    .is_some_and(|p| p.ts == now);
            if same_ts {
                self.user_point_history.insert((token, user_epoch), u_new);
            } else {
                user_epoch += 1;
                self.user_point_epoch.insert(token, user_epoch);
                self.user_point_history.insert((token, user_epoch), u_new);
            }
        }
        Ok(())
    }

    /// Port of `VotingEscrow.checkpoint()`: a global-point-only checkpoint (`_checkpoint(0, 0, 0)`).
    /// Reads `now` and `block`.
    pub fn checkpoint(&mut self, now: u64, block: u64) -> Result<()> {
        self.checkpoint_inner(
            now,
            block,
            0,
            LockedBalance::default(),
            LockedBalance::default(),
        )
    }

    // ---- DEPOSITS ----

    /// Port of `VotingEscrow._depositFor(_tokenId, _value, _unlockTime, _oldLocked, _depositType)`.
    ///
    /// Adds `value` to the supply and to the amount (`toInt128` first, then checked add). Sets `end` when
    /// `unlock != 0`, stores the new lock, and runs `_checkpoint(token, old, new)`. Returns `value`, the
    /// WIND the caller pays into the escrow. The Solidity order is preserved. The `safeTransferFrom` only
    /// happens when `value != 0` and is represented by the return value.
    fn deposit_for_inner(
        &mut self,
        now: u64,
        block: u64,
        token: u64,
        value: U256,
        unlock: u64,
        old: LockedBalance,
    ) -> Result<U256> {
        self.supply = add_u(self.supply, value)?;
        let mut new = old;
        new.amount = add_i(old.amount, to_int128(value)?)?;
        if unlock != 0 {
            new.end = unlock;
        }
        self.locked.insert(token, new);
        self.checkpoint_inner(now, block, token, old, new)?;
        Ok(value)
    }

    /// Port of `VotingEscrow._createSplitNFT`: mints a new token holding `new_locked`, checkpoints it
    /// from an empty lock, then mints it to `owner`. Returns the new id.
    fn create_split_nft(
        &mut self,
        owner: AccountId,
        now: u64,
        block: u64,
        new_locked: LockedBalance,
    ) -> Result<u64> {
        self.token_id = self.token_id.checked_add(1).ok_or(MathError::Overflow)?;
        let id = self.token_id;
        self.locked.insert(id, new_locked);
        self.checkpoint_inner(now, block, id, LockedBalance::default(), new_locked)?;
        self.mint(owner, id);
        Ok(id)
    }

    /// Port of `VotingEscrow._createLock(_value, _lockDuration, _to)` (called by `createLock` with
    /// `to = msg.sender`, and by `createLockFor`).
    ///
    /// Checks the value is not zero (`ZeroAmount`), that the unlock time is in the future
    /// (`LockDurationNotInFuture`) and at most `now + MAXTIME` (`LockDurationTooLong`). Then mints the next
    /// id to `to` and deposits. Returns `(token, pulled)`, where `pulled` is the WIND the caller pays.
    /// Reads `now` and `block`. The unlock time is `((now + duration) / WEEK) * WEEK`.
    fn create_lock_inner(
        &mut self,
        now: u64,
        block: u64,
        to: AccountId,
        value: U256,
        lock_duration: u64,
    ) -> Result<(u64, U256)> {
        let unlock = round_to_week(now, lock_duration);
        if value.is_zero() {
            return Err(MathError::ZeroAmount);
        }
        if unlock <= now as u128 {
            return Err(MathError::LockDurationNotInFuture);
        }
        if unlock > now as u128 + MAXTIME as u128 {
            return Err(MathError::LockDurationTooLong);
        }
        self.token_id = self.token_id.checked_add(1).ok_or(MathError::Overflow)?;
        let id = self.token_id;
        self.mint(to, id);
        let pulled = self.deposit_for_inner(
            now,
            block,
            id,
            value,
            to_u64(unlock)?,
            LockedBalance::default(),
        )?;
        Ok((id, pulled))
    }

    /// Port of `VotingEscrow.createLock(_value, _lockDuration)`. The caller is the owner and pays `value`.
    /// Returns `(new token id, WIND pulled from the caller)`. Reads `now` and `block`.
    pub fn create_lock(
        &mut self,
        caller: AccountId,
        now: u64,
        block: u64,
        value: U256,
        lock_duration: u64,
    ) -> Result<(u64, U256)> {
        self.create_lock_inner(now, block, caller, value, lock_duration)
    }

    /// Port of `VotingEscrow.createLockFor(_value, _lockDuration, _to)`. The new token is minted to `to`.
    /// The caller pays `value` (the caller is not checked, so it is not a parameter). Returns
    /// `(token, pulled)`. Reads `now` and `block`.
    pub fn create_lock_for(
        &mut self,
        to: AccountId,
        now: u64,
        block: u64,
        value: U256,
        lock_duration: u64,
    ) -> Result<(u64, U256)> {
        self.create_lock_inner(now, block, to, value, lock_duration)
    }

    /// Port of `_increaseAmountFor` (the body shared by `depositFor` and `increaseAmount`).
    ///
    /// Checks in order: `value != 0` (`ZeroAmount`), `amount > 0` (`NoLockFound`), and the lock has not
    /// expired unless it is permanent (`LockExpired`). A permanent lock adds `value` to
    /// `permanentLockBalance`. The delegate balance update is skipped (no delegations). Returns the WIND
    /// pulled from the caller.
    fn increase_amount_for(
        &mut self,
        now: u64,
        block: u64,
        token: u64,
        value: U256,
    ) -> Result<U256> {
        let old = self.locked_of(token);
        if value.is_zero() {
            return Err(MathError::ZeroAmount);
        }
        if old.amount <= 0 {
            return Err(MathError::NoLockFound);
        }
        if old.end <= now && !old.is_permanent {
            return Err(MathError::LockExpired);
        }
        if old.is_permanent {
            self.permanent_lock_balance = add_u(self.permanent_lock_balance, value)?;
        }
        self.deposit_for_inner(now, block, token, value, 0, old)
    }

    /// Port of `VotingEscrow.depositFor(_tokenId, _value)`. Anyone can deposit into any lock (the
    /// managed-token restriction does not apply). The payer is the caller, and the WIND pulled is
    /// returned. Reads `now` and `block`.
    pub fn deposit_for(&mut self, now: u64, block: u64, token: u64, value: U256) -> Result<U256> {
        self.increase_amount_for(now, block, token, value)
    }

    /// Port of `VotingEscrow.increaseAmount(_tokenId, _value)`. Only the owner may call it
    /// (`NotApprovedOrOwner`). Returns the WIND pulled from the caller. Reads `now` and `block`.
    pub fn increase_amount(
        &mut self,
        caller: AccountId,
        now: u64,
        block: u64,
        token: u64,
        value: U256,
    ) -> Result<U256> {
        self.require_owner(caller, token)?;
        self.increase_amount_for(now, block, token, value)
    }

    /// Port of `VotingEscrow.increaseUnlockTime(_tokenId, _lockDuration)`. Owner only. The lock must be
    /// normal, not permanent, not expired (`LockExpired`), and non-empty. The new unlock time
    /// (`((now + duration) / WEEK) * WEEK`) must be after the current end (`LockDurationNotInFuture`) and at
    /// most `now + MAXTIME` (`LockDurationTooLong`). Reads `now` and `block`.
    pub fn increase_unlock_time(
        &mut self,
        caller: AccountId,
        now: u64,
        block: u64,
        token: u64,
        lock_duration: u64,
    ) -> Result<()> {
        self.require_owner(caller, token)?;
        let old = self.locked_of(token);
        if old.is_permanent {
            return Err(MathError::PermanentLock);
        }
        let unlock = round_to_week(now, lock_duration);
        if old.end <= now {
            return Err(MathError::LockExpired);
        }
        if old.amount <= 0 {
            return Err(MathError::NoLockFound);
        }
        if unlock <= old.end as u128 {
            return Err(MathError::LockDurationNotInFuture);
        }
        if unlock > now as u128 + MAXTIME as u128 {
            return Err(MathError::LockDurationTooLong);
        }
        self.deposit_for_inner(now, block, token, U256::ZERO, to_u64(unlock)?, old)?;
        Ok(())
    }

    // ---- WITHDRAW / MERGE / SPLIT ----

    /// Port of `VotingEscrow.withdraw(_tokenId)`.
    ///
    /// Checks, in order: owner (`NotApprovedOrOwner`), not voted (`AlreadyVoted`), not permanent
    /// (`PermanentLock`), and `now >= end` (`LockNotExpired`). Then burns the token, zeroes the lock,
    /// reduces the supply by the amount, and checkpoints. Returns the WIND paid out to the owner.
    /// Reads `now` and `block`.
    pub fn withdraw(
        &mut self,
        caller: AccountId,
        now: u64,
        block: u64,
        token: u64,
    ) -> Result<U256> {
        self.require_owner(caller, token)?;
        if self.is_voted(token) {
            return Err(MathError::AlreadyVoted);
        }
        let old = self.locked_of(token);
        if old.is_permanent {
            return Err(MathError::PermanentLock);
        }
        if now < old.end {
            return Err(MathError::LockNotExpired);
        }
        let value = to_uint256(old.amount)?;
        self.burn(caller, token)?;
        self.locked.insert(token, LockedBalance::default());
        self.supply = sub_u(self.supply, value)?;
        self.checkpoint_inner(now, block, token, old, LockedBalance::default())?;
        Ok(value)
    }

    /// Port of `VotingEscrow.merge(_from, _to)`.
    ///
    /// Checks, in order: `from` not voted (`AlreadyVoted`), `from != to` (`SameNFT`), the caller owns both
    /// (`NotApprovedOrOwner`), `to` not expired (`LockExpired`), and `from` not permanent
    /// (`PermanentLock`). Then burns `from` and checkpoints it to empty. `to` gets the sum of the amounts.
    /// Its end is the later of the two ends when it is normal. A permanent `to` adds `from`'s amount to
    /// `permanentLockBalance`. Only `to` may be a permanent lock. Reads `now` and `block`.
    pub fn merge(
        &mut self,
        caller: AccountId,
        now: u64,
        block: u64,
        from: u64,
        to: u64,
    ) -> Result<()> {
        if self.is_voted(from) {
            return Err(MathError::AlreadyVoted);
        }
        if from == to {
            return Err(MathError::SameNft);
        }
        self.require_owner(caller, from)?;
        self.require_owner(caller, to)?;
        let old_to = self.locked_of(to);
        if old_to.end <= now && !old_to.is_permanent {
            return Err(MathError::LockExpired);
        }
        let old_from = self.locked_of(from);
        if old_from.is_permanent {
            return Err(MathError::PermanentLock);
        }
        let end = if old_from.end >= old_to.end {
            old_from.end
        } else {
            old_to.end
        };

        self.burn(caller, from)?;
        self.locked.insert(from, LockedBalance::default());
        self.checkpoint_inner(now, block, from, old_from, LockedBalance::default())?;

        let mut new_to = LockedBalance {
            amount: add_i(old_to.amount, old_from.amount)?,
            end: 0,
            is_permanent: old_to.is_permanent,
        };
        if new_to.is_permanent {
            self.permanent_lock_balance =
                add_u(self.permanent_lock_balance, to_uint256(old_from.amount)?)?;
        } else {
            new_to.end = end;
        }
        self.checkpoint_inner(now, block, to, old_to, new_to)?;
        self.locked.insert(to, new_to);
        Ok(())
    }

    /// Port of `VotingEscrow.split(_from, _amount)`. Returns `(token1, token2)`.
    ///
    /// Checks, in order: the owner exists (`SplitNoOwner`), the owner or `ZERO_ACCOUNT` is in `canSplit`
    /// (`SplitNotAllowed`), not voted (`AlreadyVoted`), the caller owns it (`NotApprovedOrOwner`), the lock
    /// is not expired (`LockExpired`), `amount` fits in `int128` (`SafeCastOverflow`), `amount != 0`
    /// (`ZeroAmount`), and `amount < lock amount` (`AmountTooBig`). Then burns `from`, and mints two
    /// tokens to the owner with `amount - split` and `split`, keeping the same end and permanence. Reads
    /// `now` and `block`. Permanent splits leave `permanentLockBalance` unchanged (the sum is the same). The
    /// delegate checks on a permanent lock are skipped (see the module docs).
    pub fn split(
        &mut self,
        caller: AccountId,
        now: u64,
        block: u64,
        from: u64,
        amount: U256,
    ) -> Result<(u64, u64)> {
        let owner = self.owner(from);
        if owner == ZERO_ACCOUNT {
            return Err(MathError::SplitNoOwner);
        }
        if !self.can_split_of(&owner) && !self.can_split_of(&ZERO_ACCOUNT) {
            return Err(MathError::SplitNotAllowed);
        }
        if self.is_voted(from) {
            return Err(MathError::AlreadyVoted);
        }
        self.require_owner(caller, from)?;
        let old = self.locked_of(from);
        if old.end <= now && !old.is_permanent {
            return Err(MathError::LockExpired);
        }
        let split_amount = to_int128(amount)?;
        if split_amount == 0 {
            return Err(MathError::ZeroAmount);
        }
        if old.amount <= split_amount {
            return Err(MathError::AmountTooBig);
        }

        self.burn(caller, from)?;
        self.locked.insert(from, LockedBalance::default());
        self.checkpoint_inner(now, block, from, old, LockedBalance::default())?;

        let mut new_locked = old;
        new_locked.amount = sub_i(old.amount, split_amount)?;
        let token1 = self.create_split_nft(owner, now, block, new_locked)?;
        new_locked.amount = split_amount;
        let token2 = self.create_split_nft(owner, now, block, new_locked)?;
        Ok((token1, token2))
    }

    /// Port of `VotingEscrow.toggleSplit(_account, _bool)`. Only `team` may call it (`NotTeam`).
    pub fn toggle_split(
        &mut self,
        caller: AccountId,
        account: AccountId,
        flag: bool,
    ) -> Result<()> {
        if caller != self.team {
            return Err(MathError::NotTeam);
        }
        self.can_split.insert(account, flag);
        Ok(())
    }

    // ---- PERMANENT LOCKS ----

    /// Port of `VotingEscrow.lockPermanent(_tokenId)`. Converts a normal lock to a permanent one.
    ///
    /// Checks, in order: owner (`NotApprovedOrOwner`), not already permanent (`PermanentLock`), not
    /// expired (`LockExpired`), amount above zero (`NoLockFound`). Then adds the amount to
    /// `permanentLockBalance`, sets `end = 0` and `isPermanent`, and checkpoints. Reads `now` and `block`.
    pub fn lock_permanent(
        &mut self,
        caller: AccountId,
        now: u64,
        block: u64,
        token: u64,
    ) -> Result<()> {
        self.require_owner(caller, token)?;
        let old = self.locked_of(token);
        if old.is_permanent {
            return Err(MathError::PermanentLock);
        }
        if old.end <= now {
            return Err(MathError::LockExpired);
        }
        if old.amount <= 0 {
            return Err(MathError::NoLockFound);
        }
        let amount = to_uint256(old.amount)?;
        self.permanent_lock_balance = add_u(self.permanent_lock_balance, amount)?;
        let new = LockedBalance {
            amount: old.amount,
            end: 0,
            is_permanent: true,
        };
        self.checkpoint_inner(now, block, token, old, new)?;
        self.locked.insert(token, new);
        Ok(())
    }

    /// Port of `VotingEscrow.unlockPermanent(_tokenId)`. Converts a permanent lock back to normal, with
    /// `end = ((now + MAXTIME) / WEEK) * WEEK`.
    ///
    /// Checks, in order: owner (`NotApprovedOrOwner`), not voted (`AlreadyVoted`), permanent
    /// (`NotPermanentLock`). Then subtracts the amount from `permanentLockBalance` (underflow check). Then
    /// `_delegate(token, 0)`, which reverts `OwnershipChange` when the token was transferred in this block.
    /// Without delegations, that is its only core effect. Then checkpoints. Reads `now` and `block`.
    pub fn unlock_permanent(
        &mut self,
        caller: AccountId,
        now: u64,
        block: u64,
        token: u64,
    ) -> Result<()> {
        self.require_owner(caller, token)?;
        if self.is_voted(token) {
            return Err(MathError::AlreadyVoted);
        }
        let old = self.locked_of(token);
        if !old.is_permanent {
            return Err(MathError::NotPermanentLock);
        }
        let amount = to_uint256(old.amount)?;
        self.permanent_lock_balance = sub_u(self.permanent_lock_balance, amount)?;
        let new = LockedBalance {
            amount: old.amount,
            end: to_u64(round_to_week(now, MAXTIME))?,
            is_permanent: false,
        };
        if self.ownership_change_of(token) == block {
            return Err(MathError::OwnershipChange);
        }
        self.checkpoint_inner(now, block, token, old, new)?;
        self.locked.insert(token, new);
        Ok(())
    }

    // ---- OWNERSHIP AND VOTING FLAG ----

    /// Port of `VotingEscrow.transferFrom(_from, _to, _tokenId)` with no approvals. The caller must own
    /// the token (`NotApprovedOrOwner`), and `from` must be the owner (`NotOwner`). Sets the owner and
    /// records `ownershipChange = block` (the flash-NFT guard used by `balanceOfNFT` and `unlockPermanent`).
    /// Reads `block`.
    pub fn transfer_from(
        &mut self,
        caller: AccountId,
        block: u64,
        from: AccountId,
        to: AccountId,
        token: u64,
    ) -> Result<()> {
        self.require_owner(caller, token)?;
        if self.owner(token) != from {
            return Err(MathError::NotOwner);
        }
        self.owner_of.insert(token, to);
        self.ownership_change.insert(token, block);
        Ok(())
    }

    /// Port of `VotingEscrow.voting(_tokenId, _voted)`. Only `voter` may call it (`NotVoter`).
    pub fn voting(&mut self, caller: AccountId, token: u64, flag: bool) -> Result<()> {
        if caller != self.voter {
            return Err(MathError::NotVoter);
        }
        self.voted.insert(token, flag);
        Ok(())
    }

    // ---- BALANCES AND SUPPLY ----

    /// Port of `VotingEscrow.balanceOfNFT(_tokenId)`: zero if the token changed owner in this block,
    /// otherwise `balanceOfNFTAt(token, now)`. Reads `now` and `block`.
    pub fn balance_of_nft(&self, now: u64, block: u64, token: u64) -> Result<U256> {
        if self.ownership_change_of(token) == block {
            return Ok(U256::ZERO);
        }
        self.balance_of_nft_at(token, now)
    }

    /// Port of `VotingEscrow.balanceOfNFTAt` / `BalanceLogicLibrary.balanceOfNFTAt`. Uses the last user
    /// point at or before `t`. A permanent point returns its `permanent` amount. Otherwise the bias
    /// decays by `slope * (t - ts)`, floored at zero.
    pub fn balance_of_nft_at(&self, token: u64, t: u64) -> Result<U256> {
        let epoch =
            past_user_point_index(&self.user_point_epoch, &self.user_point_history, token, t);
        if epoch == 0 {
            return Ok(U256::ZERO);
        }
        let mut last = self
            .user_point_history
            .get(&(token, epoch))
            .copied()
            .unwrap_or_default();
        if !last.permanent.is_zero() {
            return Ok(last.permanent);
        }
        let dt = t.checked_sub(last.ts).ok_or(MathError::Underflow)?;
        last.bias = sub_i(last.bias, mul_i(last.slope, dt as i128)?)?;
        if last.bias < 0 {
            last.bias = 0;
        }
        to_uint256(last.bias)
    }

    /// `BalanceLogicLibrary.getPastGlobalPointIndex`: the global epoch at or before `t`.
    fn past_global_point_index(&self, t: u64) -> u64 {
        if self.epoch == 0 {
            return 0;
        }
        let point = |e: u64| self.point_history.get(&e).copied().unwrap_or_default();
        if point(self.epoch).ts <= t {
            return self.epoch;
        }
        if point(1).ts > t {
            return 0;
        }
        let mut lower = 0u64;
        let mut upper = self.epoch;
        while upper > lower {
            let center = upper - (upper - lower) / 2;
            let p = point(center);
            if p.ts == t {
                return center;
            } else if p.ts < t {
                lower = center;
            } else {
                upper = center - 1;
            }
        }
        lower
    }

    /// Port of `BalanceLogicLibrary.supplyAt`: total voting power at `t`. Starts from the global point at
    /// or before `t`, walks forward one week at a time (at most 255 weeks) applying scheduled slope changes,
    /// then adds `permanentLockBalance` from that point. The walk is the same loop as `_checkpoint`, without
    /// writes.
    fn supply_at(&self, t: u64) -> Result<U256> {
        let epoch = self.past_global_point_index(t);
        if epoch == 0 {
            return Ok(U256::ZERO);
        }
        let point = self.point_history.get(&epoch).copied().unwrap_or_default();
        let mut bias = point.bias;
        let mut slope = point.slope;
        let mut ts = point.ts;
        let mut t_i = (ts / WEEK) * WEEK;
        for _ in 0..255 {
            t_i = t_i.checked_add(WEEK).ok_or(MathError::Overflow)?;
            let mut d_slope: i128 = 0;
            if t_i > t {
                t_i = t;
            } else {
                d_slope = self.slope_change(t_i);
            }
            let dt = t_i.checked_sub(ts).ok_or(MathError::Underflow)?;
            bias = sub_i(bias, mul_i(slope, dt as i128)?)?;
            if t_i == t {
                break;
            }
            slope = add_i(slope, d_slope)?;
            ts = t_i;
        }
        if bias < 0 {
            bias = 0;
        }
        add_u(to_uint256(bias)?, point.permanent_lock_balance)
    }

    /// Port of `VotingEscrow.totalSupply()`: `supplyAt(now)`.
    pub fn total_supply(&self, now: u64) -> Result<U256> {
        self.supply_at(now)
    }

    /// Port of `VotingEscrow.totalSupplyAt(_timestamp)`.
    pub fn total_supply_at(&self, t: u64) -> Result<U256> {
        self.supply_at(t)
    }
}
