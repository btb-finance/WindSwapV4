//! Port of the managed veNFT logic of windv2 `VotingEscrow.sol`: `createManagedLockFor`, `depositManaged`,
//! `withdrawManaged`, `setAllowedManager`, `setManagedState`, and the `setVoterAndDistributor` wiring they
//! depend on.
//!
//! The Voter is modeled by its inputs: `voter`, `governor` and `emergency_council` are the values that
//! `IVoter(voter)` returns at call time, and they are passed in by the caller. The reward contracts
//! (`LockedManagedReward`, `FreeManagedReward`) are owned by the escrow in `managed_rewards`, keyed by
//! the managed token id. Their token transfers are the caller's job, as in `ve.rs`.
//!
//! Inputs replace `block.timestamp` (`now`), `block.number` (`block`) and `msg.sender` (`caller`).

use crate::error::{MathError, Result};
use crate::fees::AccountId;
use crate::rewards_managed::ManagedRewards;
use crate::ve::{
    add_i, add_u, round_to_week, sub_i, sub_u, to_int128, to_u64, to_uint256, EscrowType,
    LockedBalance, VotingEscrow, MAXTIME, ZERO_ACCOUNT,
};
use crate::U256;

impl VotingEscrow {
    /// The reward pair of managed token `m`. `NotManagedNFT` when `m` has none.
    pub(crate) fn managed_rewards_mut(&mut self, m: u64) -> Result<&mut ManagedRewards> {
        self.managed_rewards
            .get_mut(&m)
            .ok_or(MathError::NotManagedNft)
    }

    /// `setVoterAndDistributor(voter, distributor)`. Only the current voter may call it (`NotVoter`).
    pub fn set_voter_and_distributor(
        &mut self,
        caller: AccountId,
        voter: AccountId,
        distributor: AccountId,
    ) -> Result<()> {
        if caller != self.voter {
            return Err(MathError::NotVoter);
        }
        self.voter = voter;
        self.distributor = distributor;
        Ok(())
    }

    /// `setAllowedManager(manager)`. Only the governor may call it (`NotGovernor`). The manager cannot be
    /// unchanged (`SameAddress`) or zero (`ZeroAddress`).
    pub fn set_allowed_manager(
        &mut self,
        caller: AccountId,
        governor: AccountId,
        manager: AccountId,
    ) -> Result<()> {
        if caller != governor {
            return Err(MathError::NotGovernor);
        }
        if manager == self.allowed_manager {
            return Err(MathError::SameAddress);
        }
        if manager == ZERO_ACCOUNT {
            return Err(MathError::ZeroAddress);
        }
        self.allowed_manager = manager;
        Ok(())
    }

    /// `createManagedLockFor(to)`. Callable by the allowed manager or the governor (`NotGovernorOrManager`).
    /// Mints the next id to `to` as a permanent lock with zero amount, marks it `Managed`, and creates its
    /// reward pair. Returns the managed token id. Reads `now` and `block`.
    pub fn create_managed_lock_for(
        &mut self,
        caller: AccountId,
        governor: AccountId,
        now: u64,
        block: u64,
        to: AccountId,
    ) -> Result<u64> {
        if caller != self.allowed_manager && caller != governor {
            return Err(MathError::NotGovernorOrManager);
        }
        self.token_id = self.token_id.checked_add(1).ok_or(MathError::Overflow)?;
        let m = self.token_id;
        self.mint(now, to, m)?;
        self.deposit_for_inner(
            now,
            block,
            m,
            U256::ZERO,
            0,
            LockedBalance {
                amount: 0,
                end: 0,
                is_permanent: true,
            },
        )?;
        self.escrow_type.insert(m, EscrowType::Managed);
        self.managed_rewards
            .insert(m, ManagedRewards::new(self.token.clone()));
        Ok(m)
    }

    /// `depositManaged(tokenId, mTokenId)`. Only the voter may call it (`NotVoter`). Moves the whole
    /// voting balance of normal token `token` into managed token `m`, which must be `Managed`
    /// (`NotManagedNFT`). `token` must be `Normal` (`NotNormalNFT`) with a non-zero balance at `now`
    /// (`ZeroBalance`). A permanent `token` dedelegates first. Both reward contracts are credited with the
    /// weight. Reads `now` and `block`.
    pub fn deposit_managed(
        &mut self,
        caller: AccountId,
        now: u64,
        block: u64,
        token: u64,
        m: u64,
    ) -> Result<()> {
        if caller != self.voter {
            return Err(MathError::NotVoter);
        }
        if self.escrow_type_of(m) != EscrowType::Managed {
            return Err(MathError::NotManagedNft);
        }
        if self.escrow_type_of(token) != EscrowType::Normal {
            return Err(MathError::NotNormalNft);
        }
        if self.balance_of_nft_at(token, now)?.is_zero() {
            return Err(MathError::ZeroBalance);
        }

        // Adjust the user's normal veNFT.
        let l = self.locked_of(token);
        if l.is_permanent {
            self.permanent_lock_balance =
                sub_u(self.permanent_lock_balance, to_uint256(l.amount)?)?;
            self.delegate_inner(now, block, token, 0)?;
        }
        self.checkpoint_inner(now, block, token, l, LockedBalance::default())?;
        self.locked.insert(token, LockedBalance::default());

        // Adjust the managed veNFT.
        let weight = to_uint256(l.amount)?;
        self.permanent_lock_balance = add_u(self.permanent_lock_balance, weight)?;
        let mut new_m = self.locked_of(m);
        new_m.amount = add_i(new_m.amount, l.amount)?;
        self.checkpoint_delegatee(now, self.delegate_of(m), weight, true)?;
        self.checkpoint_inner(now, block, m, self.locked_of(m), new_m)?;
        self.locked.insert(m, new_m);

        self.weights.insert((token, m), weight);
        self.id_to_managed.insert(token, m);
        self.escrow_type.insert(token, EscrowType::Locked);

        let rewards = self.managed_rewards_mut(m)?;
        rewards.locked.reward.deposit(weight, token, now, true)?;
        rewards.free.reward.deposit(weight, token, now, true)?;
        Ok(())
    }

    /// `withdrawManaged(tokenId)`. Only the voter may call it (`NotVoter`). The token must be in a managed
    /// veNFT (`InvalidManagedNFTId`) and `Locked` (`NotLockedNFT`).
    ///
    /// Claims the locked rewards (paid to the escrow), turns `token` back into a normal veNFT with amount
    /// `weight + reward` and unlock time `MAXTIME` from `now`, and removes the weight from the managed
    /// veNFT and both reward contracts. Returns `(total, reward)`: the new normal amount and the WIND the
    /// locked reward paid into the escrow. Reads `now` and `block`.
    pub fn withdraw_managed(
        &mut self,
        caller: AccountId,
        now: u64,
        block: u64,
        token: u64,
    ) -> Result<(U256, U256)> {
        let m = self.id_to_managed.get(&token).copied().unwrap_or(0);
        if caller != self.voter {
            return Err(MathError::NotVoter);
        }
        if m == 0 {
            return Err(MathError::InvalidManagedNftId);
        }
        if self.escrow_type_of(token) != EscrowType::Locked {
            return Err(MathError::NotLockedNft);
        }

        let weight = self.weights.get(&(token, m)).copied().unwrap_or_default();
        let escrow_token = self.token.clone();
        let tokens = [escrow_token.clone()];
        let rewards = self.managed_rewards_mut(m)?;
        let reward = rewards.locked.reward.earned(&escrow_token, token, now)?;
        let total = weight.checked_add(reward).ok_or(MathError::Overflow)?;
        let unlock = to_u64(round_to_week(now, MAXTIME))?;
        // Claim the locked rewards. They are paid to the escrow, which locks them in `token` below.
        rewards.locked.get_reward(true, token, &tokens, now)?;

        // Adjust the user's normal veNFT.
        let total_i = to_int128(total)?;
        let new_normal = LockedBalance {
            amount: total_i,
            end: unlock,
            is_permanent: false,
        };
        self.checkpoint_inner(now, block, token, self.locked_of(token), new_normal)?;
        self.locked.insert(token, new_normal);

        // Adjust the managed veNFT. `total` above the managed amount or the permanent balance is clamped.
        let mut new_m = self.locked_of(m);
        let take = if total_i < new_m.amount {
            total_i
        } else {
            new_m.amount
        };
        new_m.amount = sub_i(new_m.amount, take)?;
        let take_perm = if total < self.permanent_lock_balance {
            total
        } else {
            self.permanent_lock_balance
        };
        self.permanent_lock_balance = sub_u(self.permanent_lock_balance, take_perm)?;
        self.checkpoint_delegatee(now, self.delegate_of(m), total, false)?;
        self.checkpoint_inner(now, block, m, self.locked_of(m), new_m)?;
        self.locked.insert(m, new_m);

        let rewards = self.managed_rewards_mut(m)?;
        rewards.locked.reward.withdraw(weight, token, now, true)?;
        rewards.free.reward.withdraw(weight, token, now, true)?;

        self.id_to_managed.remove(&token);
        self.weights.remove(&(token, m));
        self.escrow_type.remove(&token);
        Ok((total, reward))
    }

    /// `setManagedState(mTokenId, state)`. Callable by the emergency council or the governor
    /// (`NotEmergencyCouncilOrGovernor`). `m` must be managed (`NotManagedNFT`) and not already in `state`
    /// (`SameState`).
    pub fn set_managed_state(
        &mut self,
        caller: AccountId,
        governor: AccountId,
        emergency_council: AccountId,
        m: u64,
        state: bool,
    ) -> Result<()> {
        if caller != emergency_council && caller != governor {
            return Err(MathError::NotEmergencyCouncilOrGovernor);
        }
        if self.escrow_type_of(m) != EscrowType::Managed {
            return Err(MathError::NotManagedNft);
        }
        if self.deactivated.get(&m).copied().unwrap_or(false) == state {
            return Err(MathError::SameState);
        }
        self.deactivated.insert(m, state);
        Ok(())
    }

    /// `deactivated(tokenId)`.
    pub fn is_deactivated(&self, m: u64) -> bool {
        self.deactivated.get(&m).copied().unwrap_or(false)
    }

    /// `FreeManagedReward(m).notifyRewardAmount(token, amount)`. Anyone may call it. `token_whitelisted` is
    /// `IVoter.isWhitelistedToken(token)` at call time. The caller has already moved `amount` in.
    pub fn notify_free_reward(
        &mut self,
        now: u64,
        m: u64,
        token: &str,
        amount: U256,
        token_whitelisted: bool,
    ) -> Result<()> {
        self.managed_rewards_mut(m)?.free.notify_reward_amount(
            token,
            amount,
            now,
            token_whitelisted,
        )
    }

    /// `FreeManagedReward(m).getReward(tokenId, tokens)`. The caller must be approved or the owner of
    /// `token` (`NotAuthorized`). Returns the amounts to pay to the owner, in `tokens` order.
    pub fn claim_free_reward(
        &mut self,
        caller: AccountId,
        now: u64,
        m: u64,
        token: u64,
        tokens: &[String],
    ) -> Result<Vec<(String, U256)>> {
        let approved_or_owner = self.is_approved_or_owner(caller, token);
        self.managed_rewards_mut(m)?
            .free
            .get_reward(approved_or_owner, token, tokens, now)
    }

    /// `managedToLocked(m)` / `managedToFree(m)` as a pair, or `None` when `m` is not managed.
    pub fn managed_rewards_of(&self, m: u64) -> Option<&ManagedRewards> {
        self.managed_rewards.get(&m)
    }
}
