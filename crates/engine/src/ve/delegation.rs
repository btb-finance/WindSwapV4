//! Port of the voting checkpoints and delegation of windv2 `VotingEscrow.sol` and
//! `DelegationLogicLibrary.sol`: `checkpointDelegator`, `checkpointDelegatee`, `_delegate`, `delegate`,
//! `delegateBySig`, `getPastVotes` (with `getPastVotesIndex`), and the `delegates`, `numCheckpoints`,
//! `checkpoints`, `nonces` reads.
//!
//! `delegateBySig` takes the signer that the NEAR layer already recovered. That layer owns the EIP-712
//! digest and `ecrecover`. The port receives the `s`-range check result and the recovered signer
//! (`ZERO_ACCOUNT` when recovery fails), and applies the rest of the Solidity logic in order.
//!
//! Inputs replace `block.timestamp` (`now`) and `block.number` (`block`), as in `ve.rs`.

use crate::error::{MathError, Result};
use crate::fees::AccountId;
use crate::rewards::prior_index;
use crate::ve::{to_uint256, VoteCheckpoint, VotingEscrow, ZERO_ACCOUNT};
use crate::U256;

impl VotingEscrow {
    /// `_isCheckpointInNewBlock(token)`: true unless the last checkpoint already has `now` as its timestamp.
    fn is_checkpoint_in_new_block(&self, now: u64, token: u64) -> bool {
        match self.checkpoints.get(&token).and_then(|cps| cps.last()) {
            Some(last) => last.from_timestamp != now,
            None => true,
        }
    }

    /// The last checkpoint of `token`, or the default when there is none (`_checkpoints[token][0]`).
    fn last_checkpoint(&self, token: u64) -> VoteCheckpoint {
        self.checkpoints
            .get(&token)
            .and_then(|cps| cps.last())
            .copied()
            .unwrap_or_default()
    }

    /// Writes `cp` as the next checkpoint of `token`, or overwrites the last one within the same
    /// timestamp. This is the `_isCheckpointInNewBlock` branch of the Solidity.
    fn push_checkpoint(&mut self, now: u64, token: u64, cp: VoteCheckpoint) {
        let new_block = self.is_checkpoint_in_new_block(now, token);
        let cps = self.checkpoints.entry(token).or_default();
        if new_block {
            cps.push(cp);
        } else if let Some(last) = cps.last_mut() {
            *last = cp;
        }
    }

    /// `DelegationLogicLibrary.checkpointDelegator(_locked, ..., delegator, delegatee, owner)`. Dedelegates
    /// from the old delegatee, then writes a checkpoint with the new delegatee and owner. Reads `now`.
    /// Must run before the token's owner or lock changes, since it reads `_locked[delegator].amount`.
    pub(crate) fn checkpoint_delegator(
        &mut self,
        now: u64,
        delegator: u64,
        delegatee: u64,
        owner: AccountId,
    ) -> Result<()> {
        let delegated_balance = to_uint256(self.locked_of(delegator).amount)?;
        let old = self.last_checkpoint(delegator);
        // Dedelegate from the old delegatee, if any.
        self.checkpoint_delegatee(now, old.delegatee, delegated_balance, false)?;
        let cp = VoteCheckpoint {
            from_timestamp: now,
            owner,
            delegated_balance: old.delegated_balance,
            delegatee,
        };
        self.push_checkpoint(now, delegator, cp);
        self.delegates.insert(delegator, delegatee);
        Ok(())
    }

    /// `DelegationLogicLibrary.checkpointDelegatee`: adds or removes `balance` on the delegatee's latest
    /// delegated balance. Does nothing when `delegatee` is zero. Reads `now`.
    pub(crate) fn checkpoint_delegatee(
        &mut self,
        now: u64,
        delegatee: u64,
        balance: U256,
        increase: bool,
    ) -> Result<()> {
        if delegatee == 0 {
            return Ok(());
        }
        let old = self.last_checkpoint(delegatee);
        let delegated_balance = if increase {
            old.delegated_balance
                .checked_add(balance)
                .ok_or(MathError::Overflow)?
        } else if balance < old.delegated_balance {
            old.delegated_balance - balance
        } else {
            U256::ZERO
        };
        let cp = VoteCheckpoint {
            from_timestamp: now,
            owner: old.owner,
            delegated_balance,
            delegatee: old.delegatee,
        };
        self.push_checkpoint(now, delegatee, cp);
        Ok(())
    }

    /// `delegates(delegator)`.
    pub fn delegate_of(&self, delegator: u64) -> u64 {
        self.delegates.get(&delegator).copied().unwrap_or(0)
    }

    /// `numCheckpoints(tokenId)`.
    pub fn num_checkpoints(&self, token: u64) -> u64 {
        self.checkpoints
            .get(&token)
            .map_or(0, |cps| cps.len() as u64)
    }

    /// `checkpoints(tokenId, index)`. The default checkpoint when `index` is out of range.
    pub fn checkpoint_at(&self, token: u64, index: u64) -> VoteCheckpoint {
        self.checkpoints
            .get(&token)
            .and_then(|cps| cps.get(index as usize))
            .copied()
            .unwrap_or_default()
    }

    /// `nonces(account)`.
    pub fn nonces_of(&self, account: &AccountId) -> U256 {
        self.nonces.get(account).copied().unwrap_or(U256::ZERO)
    }

    /// `_delegate(delegator, delegatee)`. Requires a permanent lock (`NotPermanentLock`), an existing
    /// delegatee (`NonExistentToken`), and no transfer in this block (`OwnershipChange`). A delegator
    /// delegating to itself is treated as zero. Does nothing when the delegate is unchanged. Reads `now`
    /// and `block`.
    pub(crate) fn delegate_inner(
        &mut self,
        now: u64,
        block: u64,
        delegator: u64,
        delegatee: u64,
    ) -> Result<()> {
        let locked = self.locked_of(delegator);
        if !locked.is_permanent {
            return Err(MathError::NotPermanentLock);
        }
        if delegatee != 0 && self.owner(delegatee) == ZERO_ACCOUNT {
            return Err(MathError::NonExistentToken);
        }
        if self.ownership_change_of(delegator) == block {
            return Err(MathError::OwnershipChange);
        }
        let delegatee = if delegatee == delegator { 0 } else { delegatee };
        if self.delegate_of(delegator) == delegatee {
            return Ok(());
        }
        let delegated_balance = to_uint256(locked.amount)?;
        let owner = self.owner(delegator);
        self.checkpoint_delegator(now, delegator, delegatee, owner)?;
        self.checkpoint_delegatee(now, delegatee, delegated_balance, true)?;
        Ok(())
    }

    /// `VotingEscrow.delegate(delegator, delegatee)`. The caller must be the owner (`NotApprovedOrOwner`).
    pub fn delegate(
        &mut self,
        caller: AccountId,
        now: u64,
        block: u64,
        delegator: u64,
        delegatee: u64,
    ) -> Result<()> {
        if !self.is_approved_or_owner(caller, delegator) {
            return Err(MathError::NotApprovedOrOwner);
        }
        self.delegate_inner(now, block, delegator, delegatee)
    }

    /// `VotingEscrow.delegateBySig`, with the signature checked by the caller.
    ///
    /// `s_in_range` is `uint256(s) <= 0x7FFF...5D576E7357A4501DDFE92F46681B20A0` (`InvalidSignatureS`
    /// otherwise). `signatory` is `ecrecover(digest, v, r, s)`, `ZERO_ACCOUNT` when recovery fails. The
    /// checks run in Solidity order: `s`, owner-or-approved of `signatory`, zero signer, nonce (incremented
    /// on success), expiry, then `_delegate`. Returns the signer's nonce after the call.
    #[allow(clippy::too_many_arguments)]
    pub fn delegate_by_sig(
        &mut self,
        now: u64,
        block: u64,
        delegator: u64,
        delegatee: u64,
        nonce: U256,
        expiry: U256,
        s_in_range: bool,
        signatory: AccountId,
    ) -> Result<U256> {
        if !s_in_range {
            return Err(MathError::InvalidSignatureS);
        }
        if !self.is_approved_or_owner(signatory, delegator) {
            return Err(MathError::NotApprovedOrOwner);
        }
        if signatory == ZERO_ACCOUNT {
            return Err(MathError::InvalidSignature);
        }
        let current = self.nonces_of(&signatory);
        if nonce != current {
            return Err(MathError::InvalidNonce);
        }
        let next = current
            .checked_add(U256::from(1))
            .ok_or(MathError::Overflow)?;
        self.nonces.insert(signatory, next);
        if U256::from(now) > expiry {
            return Err(MathError::SignatureExpired);
        }
        self.delegate_inner(now, block, delegator, delegatee)?;
        Ok(next)
    }

    /// `getPastVotesIndex(tokenId, timestamp)`: index of the last checkpoint at or before `t`, `0` when
    /// there is none.
    fn past_votes_index(&self, token: u64, t: u64) -> usize {
        let cps = self.checkpoints.get(&token).map_or(&[][..], Vec::as_slice);
        prior_index(cps.len(), |i| cps[i].from_timestamp, t)
    }

    /// `VotingEscrow.getPastVotes(account, tokenId, timestamp)`. Zero unless `account` owned the token
    /// at `t`. A non-delegating token adds its own balance at `t`. Reads nothing else.
    pub fn get_past_votes(&self, account: &AccountId, token: u64, t: u64) -> Result<U256> {
        let idx = self.past_votes_index(token, t);
        let last = self
            .checkpoints
            .get(&token)
            .and_then(|cps| cps.get(idx))
            .copied()
            .unwrap_or_default();
        if last.from_timestamp > t {
            return Ok(U256::ZERO);
        }
        if *account != last.owner {
            return Ok(U256::ZERO);
        }
        let votes = last.delegated_balance;
        if last.delegatee == 0 {
            votes
                .checked_add(self.balance_of_nft_at(token, t)?)
                .ok_or(MathError::Overflow)
        } else {
            Ok(votes)
        }
    }
}
