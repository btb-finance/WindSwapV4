//! Port of the managed veNFT reward contracts: `ManagedReward.sol` (base), `LockedManagedReward.sol`
//! and `FreeManagedReward.sol`. Each one wraps the base `Reward` accounting from `rewards.rs`.
//!
//! Differences from the Solidity contracts (same conventions as `rewards.rs`):
//! - Authorization is a boolean parameter. `sender == ve` (the escrow) is `sender_is_escrow`,
//!   `isApprovedOrOwner(sender, tokenId)` is `sender_is_approved_or_owner`, and the voter whitelist is a
//!   boolean. Token transfers are the caller's job. `notify` takes the amount already moved in, and
//!   `get_reward` returns the amounts to pay.
//! - `ve` and `voter` are not stored. The escrow token string is kept as `token`, which the locked
//!   reward checks against `IVotingEscrow.token()`.
//! - `nonReentrant` and the forwarder are not modeled.

use crate::error::{MathError, Result};
use crate::rewards::{Reward, RewardToken};
use crate::U256;

/// `LockedManagedReward`: rebases and compounded rewards, paid to the escrow on `withdrawManaged`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "borsh",
    derive(borsh::BorshSerialize, borsh::BorshDeserialize)
)]
pub struct LockedManagedReward {
    /// The base accounting. Its reward list starts with the escrow token (`ManagedReward` constructor).
    pub reward: Reward,
    /// `IVotingEscrow(ve).token()`, the only token this contract accepts.
    pub token: RewardToken,
}

impl LockedManagedReward {
    /// Constructor: `ManagedReward` pushes the escrow token into `rewards` and `isReward`.
    pub fn new(token: RewardToken) -> Self {
        Self {
            reward: Reward::new(std::slice::from_ref(&token)),
            token,
        }
    }

    /// `getReward(tokenId, tokens)`, called by the escrow with `msg.sender == ve`. Returns the amounts paid
    /// to the escrow.
    pub fn get_reward(
        &mut self,
        sender_is_escrow: bool,
        token_id: u64,
        tokens: &[RewardToken],
        now: u64,
    ) -> Result<Vec<(RewardToken, U256)>> {
        if !sender_is_escrow {
            return Err(MathError::NotVotingEscrow);
        }
        if tokens.len() != 1 {
            return Err(MathError::NotSingleToken);
        }
        if tokens[0] != self.token {
            return Err(MathError::NotEscrowToken);
        }
        self.reward.claim(token_id, tokens, now)
    }

    /// `notifyRewardAmount(token, amount)`, called by the escrow with `msg.sender == ve`. The escrow has
    /// already moved `amount` into this contract.
    pub fn notify_reward_amount(
        &mut self,
        sender_is_escrow: bool,
        token: &str,
        amount: U256,
        now: u64,
    ) -> Result<()> {
        if !sender_is_escrow {
            return Err(MathError::NotVotingEscrow);
        }
        if token != self.token {
            return Err(MathError::NotEscrowToken);
        }
        self.reward.add_epoch_reward(token, amount, now)
    }
}

/// `FreeManagedReward`: rewards the owner of each deposited veNFT can claim directly.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "borsh",
    derive(borsh::BorshSerialize, borsh::BorshDeserialize)
)]
pub struct FreeManagedReward {
    /// The base accounting. Its reward list starts with the escrow token (`ManagedReward` constructor).
    pub reward: Reward,
}

impl FreeManagedReward {
    /// Constructor. The escrow token is the first reward token, as in `ManagedReward`.
    pub fn new(token: &str) -> Self {
        Self {
            reward: Reward::new(&[token.to_owned()]),
        }
    }

    /// `getReward(tokenId, tokens)`. The caller must be approved or the owner (`NotAuthorized`). Returns
    /// the amounts to pay to the owner.
    pub fn get_reward(
        &mut self,
        sender_is_approved_or_owner: bool,
        token_id: u64,
        tokens: &[RewardToken],
        now: u64,
    ) -> Result<Vec<(RewardToken, U256)>> {
        if !sender_is_approved_or_owner {
            return Err(MathError::NotAuthorized);
        }
        self.reward.claim(token_id, tokens, now)
    }

    /// `notifyRewardAmount(token, amount)`. Anyone may notify. A token that is not yet a reward must be
    /// whitelisted by the voter (`NotWhitelisted`).
    pub fn notify_reward_amount(
        &mut self,
        token: &str,
        amount: U256,
        now: u64,
        token_whitelisted: bool,
    ) -> Result<()> {
        self.reward
            .notify_bribe_reward(token, amount, now, token_whitelisted)
    }
}

/// The pair `VotingEscrow` creates for each managed veNFT (`managedToLocked`, `managedToFree`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "borsh",
    derive(borsh::BorshSerialize, borsh::BorshDeserialize)
)]
pub struct ManagedRewards {
    pub locked: LockedManagedReward,
    pub free: FreeManagedReward,
}

impl ManagedRewards {
    /// `ManagedRewardsFactory.createRewards`: a locked and a free reward for the escrow token.
    pub fn new(token: RewardToken) -> Self {
        Self {
            free: FreeManagedReward::new(&token),
            locked: LockedManagedReward::new(token),
        }
    }
}
