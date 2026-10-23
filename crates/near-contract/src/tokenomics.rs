//! ve(3,3) tokenomics on NEAR: veNFT locks, voting, CL gauges, fee and bribe rewards, rebase, and
//! the weekly WIND emission. The logic is the engine's (`ve`, `voter`, `minter`, `rebase`,
//! `gauge`); this module only moves WIND between internal balances and calls the engine.
//!
//! # Accounts and balances
//! - A NEAR account and a wallet-only user are both an [`Owner`]. veNFTs, positions and balances
//!   are keyed by it, so an EVM user can lock and vote with no NEAR account.
//! - WIND is `wind_token`, a NEP-141 token minted only by this contract. Internal WIND is the
//!   owner-keyed balance map under the WIND account id.
//! - The protocol's own accounts are system owners, derived from a label with `system_id`:
//!   the voter, the minter, the rewards distributor, and per gauge the fee and bribe pools.
//!   Their WIND (or fee and bribe) is a normal internal balance, so every token the contract
//!   holds is in exactly one place. A veNFT lock holds its WIND in `VotingEscrow` (`supply`).
//! - The team share goes to the governance owner, `owner_of_account(owner)`.
//!
//! # The epoch flip (`distribute`)
//! `distribute` is callable by anyone, once a week.
//! 1. It runs `Minter::update_period` on a **copy** of the minter, with the WIND supply the
//!    contract tracks (`wind_minted`, the sum of everything it minted) and the minter's internal
//!    WIND balance.
//! 2. If nothing must be minted, the copy is committed and the gauges are distributed in the same
//!    call. This is the path when the minter already holds the required WIND.
//! 3. Otherwise the copy is stored in `pending_epoch` and `wind.mint(self, mint_required)` is
//!    called. The callback `on_distribute_minted` commits the copy (credits the mint, then the
//!    team, distributor and voter shares, then distributes the gauges) only if the mint succeeded.
//!    If the mint failed, the callback clears `pending_epoch` and changes nothing else, so the
//!    epoch is retried by the next `distribute` call. While `pending_epoch` is set, `distribute`
//!    panics. The owner can clear it with `clear_pending_epoch` if the callback ever fails.
//!
//! # Gauges and staked positions
//! - One `ClGauge` per pool, created by the owner (`create_gauge`). Its account is `system_id`.
//! - One `PositionManager` for the contract (`nft_manager`), as the pools' `nft` account. A staked
//!   position is an NFT of that manager, and its liquidity sits in the manager's pool position.
//!   The engine's `ClPool::stake` debits the pool's `nft` account, so all stakes must use it.
//! - `stake_position(pool, tick_lower, tick_upper)` takes the caller's whole engine position in
//!   that range (its liquidity is burned and collected to the caller), mints it as an NFT of the
//!   manager owned by the caller, and deposits the NFT into the gauge. The key of a staked
//!   position is `(owner, pool, tick_lower, tick_upper)`, and the value is the NFT id.
//! - `unstake_position` withdraws the NFT, burns its liquidity back to the owner's range, and pays
//!   the collected fees and any gauge emissions.
//! - KNOWN LIMIT: the engine's `PositionManager` pays the owed amounts of the whole pool position
//!   it shares, not per NFT (see its module docs). Two stakers in one range can therefore collect
//!   each other's accrued fees. This must be fixed in the engine (per-NFT owed amounts) before
//!   mainnet. It is not fixed here because the engine logic is out of scope for this change.
//!
//! # Rounding
//! Burn and re-mint round differently, so stake and unstake can move a few wei either way.
//! Rounding is applied to the caller's balance as a net amount.

use std::collections::BTreeMap;

use near_sdk::json_types::U128;
use near_sdk::{env, ext_contract, near, AccountId, Gas, Promise, PromiseError, PromiseOrValue};
use windswap_engine::gauge::{ClGauge, GaugeConfig};
use windswap_engine::minter::{EpochEmission, Minter, WEEK};
use windswap_engine::rebase::{Payout, RewardsDistributor};
use windswap_engine::ve::VotingEscrow;
use windswap_engine::voter::{GaugeId, PoolId, Voter, DURATION as VOTE_DURATION};
use windswap_engine::U256;

use crate::accounts::{owner_hex, owner_of_account, parse_owner_hex, Owner};
use crate::{now, now_secs, ok, to_u128, Contract, ContractExt};

/// Unwraps an engine result whose error type has no `Display` (the gauge's `GaugeError`).
fn ok_dbg<T, E: std::fmt::Debug>(result: Result<T, E>) -> T {
    result.unwrap_or_else(|e| env::panic_str(&format!("{e:?}")))
}

/// `(owner, pool id, tick_lower, tick_upper)` of a staked position. The value is the NFT id.
pub type StakeKey = (Owner, u64, i32, i32);

/// An epoch whose WIND mint is in flight. Committed by `on_distribute_minted`.
#[near(serializers = [borsh])]
pub struct PendingEpoch {
    pub minter: Minter,
    pub emission: EpochEmission,
}

#[allow(dead_code)]
#[ext_contract(ext_wind)]
pub trait WindToken {
    fn mint(&mut self, account_id: AccountId, amount: U128);
}

#[allow(dead_code)]
#[ext_contract(ext_tokenomics)]
pub trait TokenomicsCallbacks {
    fn on_distribute_minted(&mut self, amount: U128);
    fn on_genesis_minted(&mut self, amount: U128);
}

/// View of a veNFT lock.
#[near(serializers = [json])]
pub struct LockView {
    pub token_id: u64,
    pub owner: String,
    pub amount: U128,
    pub end: u64,
    pub permanent: bool,
    pub voted: bool,
}

/// View of the epoch.
#[near(serializers = [json])]
pub struct EpochView {
    pub active_period: u64,
    pub epoch_count: u64,
    pub weekly: U128,
    pub next_epoch_start: u64,
    pub wind_minted: U128,
    pub distribution_pending: bool,
}

/// View of a pool's gauge.
#[near(serializers = [json])]
pub struct GaugeView {
    pub pool_id: u64,
    pub gauge_id: u64,
    pub alive: bool,
    pub weight: U128,
    pub gauge_account: String,
    pub reward_balance: U128,
}

const MINT_GAS_TGAS: u64 = 15;
const DISTRIBUTE_CALLBACK_GAS_TGAS: u64 = 180;

// ---- system accounts ----

/// Engine account of a protocol-owned pool, derived from a label so it cannot collide with a
/// user's owner id (those are hashes of an account id or of a wallet string).
fn system_id(parts: &[&str]) -> Owner {
    env::sha256_array(format!("windswap-system:{}", parts.join(":")).as_bytes())
}

pub(crate) fn voter_id() -> Owner {
    system_id(&["voter"])
}

pub(crate) fn minter_id() -> Owner {
    system_id(&["minter"])
}

pub(crate) fn distributor_id() -> Owner {
    system_id(&["distributor"])
}

fn fees_id(gauge: GaugeId) -> Owner {
    system_id(&["fees", &gauge.to_string()])
}

fn bribe_id(gauge: GaugeId) -> Owner {
    system_id(&["bribe", &gauge.to_string()])
}

fn gauge_account(pool: PoolId) -> Owner {
    system_id(&["gauge", &pool.to_string()])
}

/// The engine's id of a token. Gauge and NFT positions name tokens by this, not by string.
fn token_key(token: &str) -> Owner {
    env::sha256_array(token.as_bytes())
}

/// The contract's NFT manager (the pools' `nft` account).
pub(crate) fn nft_manager_account() -> Owner {
    system_id(&["nft-manager"])
}

/// Builds the fresh engine state for a contract owned by `owner`.
pub(crate) fn fresh_state(
    owner: &AccountId,
    now: u64,
    block: u64,
) -> (VotingEscrow, Voter, Minter, RewardsDistributor) {
    let gov = owner_of_account(owner);
    let mut ve = VotingEscrow::new(gov, now, block);
    ok(ve.set_voter_and_distributor(gov, voter_id(), distributor_id()));
    let mut voter = Voter::new(voter_id(), gov);
    voter.minter = minter_id();
    (
        ve,
        voter,
        Minter::new(now),
        RewardsDistributor::new(now, minter_id()),
    )
}

/// Net effect of receiving `received` and paying `paid` of one token, applied to `owner`.
fn net_settle(contract: &mut Contract, owner: &Owner, token: &str, received: u128, paid: u128) {
    if received >= paid {
        contract.credit(owner, token, received - paid);
    } else {
        contract.debit(owner, token, paid - received);
    }
}

impl Contract {
    pub(crate) fn wind(&self) -> String {
        self.wind_token
            .as_ref()
            .map(|a| a.to_string())
            .unwrap_or_else(|| env::panic_str("wind token is not set"))
    }

    pub(crate) fn team_id(&self) -> Owner {
        owner_of_account(&self.owner)
    }

    fn ve_ref(&self) -> &VotingEscrow {
        self.ve
            .get()
            .as_ref()
            .unwrap_or_else(|| env::panic_str("no ve state"))
    }

    pub(crate) fn ve_mut(&mut self) -> &mut VotingEscrow {
        self.ve
            .get_mut()
            .as_mut()
            .unwrap_or_else(|| env::panic_str("no ve state"))
    }

    fn voter_ref(&self) -> &Voter {
        self.voter
            .get()
            .as_ref()
            .unwrap_or_else(|| env::panic_str("no voter state"))
    }

    pub(crate) fn voter_mut(&mut self) -> &mut Voter {
        self.voter
            .get_mut()
            .as_mut()
            .unwrap_or_else(|| env::panic_str("no voter state"))
    }

    fn minter_ref(&self) -> &Minter {
        self.minter
            .get()
            .as_ref()
            .unwrap_or_else(|| env::panic_str("no minter state"))
    }

    fn distributor_mut(&mut self) -> &mut RewardsDistributor {
        self.distributor
            .get_mut()
            .as_mut()
            .unwrap_or_else(|| env::panic_str("no distributor state"))
    }

    /// Total WIND supply the contract tracks: the sum of everything it has minted.
    fn wind_total_supply(&self) -> U256 {
        U256::from(self.wind_minted)
    }

    // ---- veNFT actions (keyed by owner) ----

    /// Locks `amount` of the owner's WIND for `duration` seconds. Returns the token id.
    pub(crate) fn lock_create(&mut self, owner: Owner, amount: u128, duration: u64) -> u64 {
        let wind = self.wind();
        let (now, block) = (now_secs(), env::block_height());
        let (token, pulled) =
            ok(self
                .ve_mut()
                .create_lock(owner, now, block, U256::from(amount), duration));
        self.debit(&owner, &wind, to_u128(pulled));
        token
    }

    /// Adds `amount` of the owner's WIND to a lock.
    pub(crate) fn lock_increase_amount(&mut self, owner: Owner, token: u64, amount: u128) {
        let wind = self.wind();
        let (now, block) = (now_secs(), env::block_height());
        let pulled =
            ok(self
                .ve_mut()
                .increase_amount(owner, now, block, token, U256::from(amount)));
        self.debit(&owner, &wind, to_u128(pulled));
    }

    pub(crate) fn lock_increase_unlock_time(&mut self, owner: Owner, token: u64, duration: u64) {
        let (now, block) = (now_secs(), env::block_height());
        ok(self
            .ve_mut()
            .increase_unlock_time(owner, now, block, token, duration));
    }

    /// Withdraws an expired lock. The WIND goes back to the owner's internal balance.
    pub(crate) fn lock_withdraw(&mut self, owner: Owner, token: u64) -> u128 {
        let wind = self.wind();
        let (now, block) = (now_secs(), env::block_height());
        let amount = to_u128(ok(self.ve_mut().withdraw(owner, now, block, token)));
        self.credit(&owner, &wind, amount);
        amount
    }

    pub(crate) fn lock_merge(&mut self, owner: Owner, from: u64, to: u64) {
        let (now, block) = (now_secs(), env::block_height());
        ok(self.ve_mut().merge(owner, now, block, from, to));
    }

    pub(crate) fn lock_split(&mut self, owner: Owner, from: u64, amount: u128) -> (u64, u64) {
        let (now, block) = (now_secs(), env::block_height());
        ok(self
            .ve_mut()
            .split(owner, now, block, from, U256::from(amount)))
    }

    pub(crate) fn lock_set_permanent(&mut self, owner: Owner, token: u64, permanent: bool) {
        let (now, block) = (now_secs(), env::block_height());
        if permanent {
            ok(self.ve_mut().lock_permanent(owner, now, block, token));
        } else {
            ok(self.ve_mut().unlock_permanent(owner, now, block, token));
        }
    }

    pub(crate) fn lock_delegate(&mut self, owner: Owner, delegator: u64, delegatee: u64) {
        let (now, block) = (now_secs(), env::block_height());
        ok(self
            .ve_mut()
            .delegate(owner, now, block, delegator, delegatee));
    }

    // ---- voting ----

    pub(crate) fn vote_for(
        &mut self,
        owner: Owner,
        token: u64,
        pools: &[PoolId],
        weights: &[U256],
    ) {
        let (now, block) = (now_secs(), env::block_height());
        let ve = self
            .ve
            .get_mut()
            .as_mut()
            .unwrap_or_else(|| env::panic_str("no ve state"));
        let voter = self
            .voter
            .get_mut()
            .as_mut()
            .unwrap_or_else(|| env::panic_str("no voter state"));
        ok(voter.vote(ve, owner, token, pools, weights, now, block));
    }

    pub(crate) fn reset_for(&mut self, owner: Owner, token: u64) {
        let now = now_secs();
        let ve = self
            .ve
            .get_mut()
            .as_mut()
            .unwrap_or_else(|| env::panic_str("no ve state"));
        let voter = self
            .voter
            .get_mut()
            .as_mut()
            .unwrap_or_else(|| env::panic_str("no voter state"));
        ok(voter.reset(ve, owner, token, now));
    }

    pub(crate) fn poke_token(&mut self, token: u64) {
        let (now, block) = (now_secs(), env::block_height());
        let ve = self
            .ve
            .get_mut()
            .as_mut()
            .unwrap_or_else(|| env::panic_str("no ve state"));
        let voter = self
            .voter
            .get_mut()
            .as_mut()
            .unwrap_or_else(|| env::panic_str("no voter state"));
        ok(voter.poke(ve, token, now, block));
    }

    // ---- gauges ----

    /// Owner only. Creates the gauge of `pool_id`: the voter entry, the pool wiring, and the
    /// `ClGauge`. The governor sends the voter call, so the pool's tokens need no whitelist.
    pub(crate) fn gauge_create(&mut self, pool_id: u64) -> GaugeId {
        let (token0, token1) = self.tokens_of(pool_id);
        let spacing = self.pool_ref(pool_id).tick_spacing;
        let gov = self.team_id();
        let gauge = ok(self.voter_mut().create_gauge(
            gov,
            true,
            pool_id,
            Some((token0.clone(), token1.clone())),
        ));
        let account = gauge_account(pool_id);
        ok(self
            .pool_mut(pool_id)
            .set_gauge_and_position_manager(account, nft_manager_account()));
        let config = GaugeConfig {
            account,
            voter: voter_id(),
            notify_admin: gov,
            token0: token_key(&token0),
            token1: token_key(&token1),
            tick_spacing: spacing,
            is_pool: true,
        };
        self.gauges.insert(pool_id, ClGauge::new(config));
        gauge
    }

    /// Owner only, one time: mints `amount` WIND to the contract and credits it to the team. This is
    /// the genesis supply that the first epoch's growth formula divides by (the WIND token starts
    /// at zero). Only the first call counts.
    pub(crate) fn genesis_mint_start(&mut self, amount: u128) -> Promise {
        if self.wind_genesis_done || self.wind_genesis_pending {
            env::panic_str("the genesis mint was already run");
        }
        if amount == 0 {
            env::panic_str("the genesis amount must be positive");
        }
        let wind: AccountId = self
            .wind()
            .parse()
            .unwrap_or_else(|_| env::panic_str("wind token is not an account id"));
        self.wind_genesis_pending = true;
        let me = env::current_account_id();
        ext_wind::ext(wind)
            .with_static_gas(Gas::from_tgas(MINT_GAS_TGAS))
            .mint(me.clone(), U128(amount))
            .then(
                ext_tokenomics::ext(me)
                    .with_static_gas(Gas::from_tgas(DISTRIBUTE_CALLBACK_GAS_TGAS))
                    .on_genesis_minted(U128(amount)),
            )
    }

    pub(crate) fn genesis_mint_done(&mut self, amount: u128, minted: Result<(), PromiseError>) {
        self.wind_genesis_pending = false;
        if minted.is_err() {
            env::log_str("genesis mint failed; it can be run again");
            return;
        }
        self.wind_genesis_done = true;
        self.wind_minted = self
            .wind_minted
            .checked_add(amount)
            .unwrap_or_else(|| env::panic_str("wind supply overflow"));
        let wind = self.wind();
        self.credit(&self.team_id(), &wind, amount);
    }

    /// Stakes the caller's whole position in `[tick_lower, tick_upper)` of `pool_id`.
    pub(crate) fn stake_for(
        &mut self,
        owner: Owner,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    ) {
        let key: StakeKey = (owner, pool_id, tick_lower, tick_upper);
        if self.stakes.contains_key(&key) {
            env::panic_str("position is already staked");
        }
        if !self.gauges.contains_key(&pool_id) {
            env::panic_str(&format!("pool {pool_id} has no gauge"));
        }
        let (token0, token1) = self.tokens_of(pool_id);
        let liquidity = self
            .pool_ref(pool_id)
            .position(&owner, tick_lower, tick_upper)
            .liquidity;
        if liquidity == 0 {
            env::panic_str("no liquidity in that range");
        }
        let time = now();
        // Move the caller's liquidity out of its own position and into the stake's manager.
        let (collected0, collected1) = {
            let pool = self.pool_mut(pool_id);
            ok(pool.burn(owner, tick_lower, tick_upper, liquidity, time));
            let (c0, c1) = ok(pool.collect(owner, tick_lower, tick_upper, u128::MAX, u128::MAX));
            (c0, c1)
        };
        let spacing = self.pool_ref(pool_id).tick_spacing;
        let (token_id, minted0, minted1) = {
            let pool = self
                .pools
                .get_mut(&pool_id)
                .unwrap_or_else(|| env::panic_str("pool does not exist"));
            let nft = self
                .nft_manager
                .get_mut()
                .as_mut()
                .unwrap_or_else(|| env::panic_str("no nft manager"));
            let (id, a0, a1) = ok_dbg(nft.mint(
                pool,
                owner,
                token_key(&token0),
                token_key(&token1),
                spacing,
                tick_lower,
                tick_upper,
                liquidity,
                time,
            ));
            (id, to_u128(a0), to_u128(a1))
        };
        net_settle(self, &owner, &token0, collected0, minted0);
        net_settle(self, &owner, &token1, collected1, minted1);
        // The gauge pulls the NFT from its owner, so the owner approves the gauge for this call only.
        let gauge_acc = gauge_account(pool_id);
        self.set_nft_approval(owner, gauge_acc, true);
        let (deposit0, deposit1) = {
            let pool = self
                .pools
                .get_mut(&pool_id)
                .unwrap_or_else(|| env::panic_str("pool does not exist"));
            let gauge = self
                .gauges
                .get_mut(&pool_id)
                .unwrap_or_else(|| env::panic_str("gauge does not exist"));
            let nft = self
                .nft_manager
                .get_mut()
                .as_mut()
                .unwrap_or_else(|| env::panic_str("no nft manager"));
            ok_dbg(gauge.deposit(pool, nft, owner, token_id, time))
        };
        self.set_nft_approval(owner, gauge_acc, false);
        self.credit(&owner, &token0, deposit0);
        self.credit(&owner, &token1, deposit1);
        self.stakes.insert(key, token_id);
    }

    fn set_nft_approval(&mut self, owner: Owner, operator: Owner, approved: bool) {
        self.nft_manager
            .get_mut()
            .as_mut()
            .unwrap_or_else(|| env::panic_str("no nft manager"))
            .set_approval_for_all(owner, operator, approved);
    }

    /// Unstakes the caller's position in the range, and returns it to the caller's range.
    pub(crate) fn unstake_for(
        &mut self,
        owner: Owner,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    ) {
        let key: StakeKey = (owner, pool_id, tick_lower, tick_upper);
        let token_id = self
            .stakes
            .remove(&key)
            .unwrap_or_else(|| env::panic_str("position is not staked"));
        let (token0, token1) = self.tokens_of(pool_id);
        let wind = self.wind();
        let time = now();
        let (w0, w1, reward) = {
            let pool = self
                .pools
                .get_mut(&pool_id)
                .unwrap_or_else(|| env::panic_str("pool does not exist"));
            let gauge = self
                .gauges
                .get_mut(&pool_id)
                .unwrap_or_else(|| env::panic_str("gauge does not exist"));
            let nft = self
                .nft_manager
                .get_mut()
                .as_mut()
                .unwrap_or_else(|| env::panic_str("no nft manager"));
            let (w0, w1, reward) = ok_dbg(gauge.withdraw(pool, nft, owner, token_id, time));
            (w0, w1, to_u128(reward))
        };
        self.credit(&owner, &wind, reward);
        let (liquidity, stake_acc) = {
            let nft = self
                .nft_manager
                .get()
                .as_ref()
                .unwrap_or_else(|| env::panic_str("no nft manager"));
            (ok_dbg(nft.position(token_id)).liquidity, nft.account)
        };
        let (p0, p1, m0, m1) = {
            let pool = self.pool_mut(pool_id);
            ok(pool.burn(stake_acc, tick_lower, tick_upper, liquidity, time));
            let (p0, p1) =
                ok(pool.collect(stake_acc, tick_lower, tick_upper, u128::MAX, u128::MAX));
            let (m0, m1) = ok(pool.mint(owner, tick_lower, tick_upper, liquidity, time));
            (p0, p1, to_u128(m0), to_u128(m1))
        };
        net_settle(self, &owner, &token0, w0 + p0, m0);
        net_settle(self, &owner, &token1, w1 + p1, m1);
    }

    /// Pays the caller's gauge emissions for one staked position. Returns the amount.
    pub(crate) fn claim_gauge_for(
        &mut self,
        owner: Owner,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    ) -> u128 {
        let key: StakeKey = (owner, pool_id, tick_lower, tick_upper);
        let token_id = *self
            .stakes
            .get(&key)
            .unwrap_or_else(|| env::panic_str("position is not staked"));
        let wind = self.wind();
        let time = now();
        let reward = {
            let pool = self
                .pools
                .get_mut(&pool_id)
                .unwrap_or_else(|| env::panic_str("pool does not exist"));
            let gauge = self
                .gauges
                .get_mut(&pool_id)
                .unwrap_or_else(|| env::panic_str("gauge does not exist"));
            let nft = self
                .nft_manager
                .get()
                .as_ref()
                .unwrap_or_else(|| env::panic_str("no nft manager"));
            to_u128(ok_dbg(
                gauge.get_reward_token(pool, nft, owner, token_id, time),
            ))
        };
        self.credit(&owner, &wind, reward);
        reward
    }

    // ---- rewards ----

    /// Claims the caller's fee share of `gauges` for `token`. Returns the amounts per token.
    pub(crate) fn claim_fees_for(
        &mut self,
        owner: Owner,
        token: u64,
        gauges: &[GaugeId],
    ) -> BTreeMap<String, u128> {
        self.claim_rewards_for(owner, token, gauges, None, true)
    }

    /// Claims the caller's bribe share of `gauges` for `token`, for the listed `tokens`.
    pub(crate) fn claim_bribes_for(
        &mut self,
        owner: Owner,
        token: u64,
        gauges: &[GaugeId],
        tokens: &[String],
    ) -> BTreeMap<String, u128> {
        self.claim_rewards_for(owner, token, gauges, Some(tokens), false)
    }

    fn claim_rewards_for(
        &mut self,
        owner: Owner,
        token: u64,
        gauges: &[GaugeId],
        bribe_tokens: Option<&[String]>,
        is_fees: bool,
    ) -> BTreeMap<String, u128> {
        let now = now_secs();
        let mut out: BTreeMap<String, u128> = BTreeMap::new();
        for &gauge in gauges {
            let pool = *self
                .voter_ref()
                .pool_for_gauge
                .get(&gauge)
                .unwrap_or_else(|| env::panic_str(&format!("gauge {gauge} does not exist")));
            let tokens: Vec<String> = match bribe_tokens {
                Some(list) => list.to_vec(),
                None => {
                    let (t0, t1) = self.tokens_of(pool);
                    vec![t0, t1]
                }
            };
            let entries = vec![(gauge, tokens)];
            let paid = {
                let ve = self
                    .ve
                    .get()
                    .as_ref()
                    .unwrap_or_else(|| env::panic_str("no ve state"));
                let voter = self
                    .voter
                    .get_mut()
                    .as_mut()
                    .unwrap_or_else(|| env::panic_str("no voter state"));
                ok(voter.claim(ve, owner, token, now, is_fees, &entries))
            };
            let pool_owner = if is_fees {
                fees_id(gauge)
            } else {
                bribe_id(gauge)
            };
            for (name, amount) in paid {
                let amount = to_u128(amount);
                self.debit(&pool_owner, &name, amount);
                self.credit(&owner, &name, amount);
                *out.entry(name).or_default() += amount;
            }
        }
        out
    }

    /// Adds `amount` of `token` from the caller's balance as a bribe for the gauge of `pool_id`.
    pub(crate) fn bribe_for(&mut self, owner: Owner, pool_id: u64, token: &str, amount: u128) {
        let gauge = self.voter_ref().gauge_of(pool_id);
        if gauge == 0 {
            env::panic_str(&format!("pool {pool_id} has no gauge"));
        }
        let whitelisted = self.voter_ref().whitelisted_token(token);
        self.debit(&owner, token, amount);
        let now = now_secs();
        {
            let reward = self
                .voter_mut()
                .gauge_rewards
                .get_mut(&gauge)
                .unwrap_or_else(|| env::panic_str("gauge rewards missing"));
            ok(reward
                .bribe
                .notify_bribe_reward(token, U256::from(amount), now, whitelisted));
        }
        self.credit(&bribe_id(gauge), token, amount);
    }

    /// Pays the rebase owed to `token`. It goes to the lock if the lock is active, otherwise to
    /// the lock's owner. Anyone may call it, as in `RewardsDistributor.claim`.
    pub(crate) fn claim_rebase_for(&mut self, token: u64) -> u128 {
        let wind = self.wind();
        let (now, block) = (now_secs(), env::block_height());
        let period = self.minter_ref().active_period;
        let payout = {
            let ve = self
                .ve
                .get_mut()
                .as_mut()
                .unwrap_or_else(|| env::panic_str("no ve state"));
            let dist = self
                .distributor
                .get_mut()
                .as_mut()
                .unwrap_or_else(|| env::panic_str("no distributor state"));
            ok(dist.claim(now, block, period, token, ve))
        };
        let amount = to_u128(payout.amount());
        self.debit(&distributor_id(), &wind, amount);
        if let Payout::Transferred { to, amount } = &payout {
            self.credit(to, &wind, to_u128(*amount));
        }
        amount
    }

    // ---- epoch ----

    /// Commits a minted epoch: spends the minter's WIND on the team, distributor and voter
    /// shares, then distributes the gauges.
    fn commit_epoch(&mut self, minter: Minter, em: EpochEmission, now: u64) {
        let wind = self.wind();
        *self.minter.get_mut() = Some(minter);
        let required = to_u128(em.growth + em.emission + em.team);
        self.debit(&minter_id(), &wind, required);
        self.credit(&self.team_id(), &wind, to_u128(em.team));
        self.credit(&distributor_id(), &wind, to_u128(em.growth));
        let distributor_balance = U256::from(self.balance_of(&distributor_id(), &wind));
        ok(self
            .distributor_mut()
            .checkpoint_token(minter_id(), now, distributor_balance));
        self.credit(&voter_id(), &wind, to_u128(em.emission));
        ok(self
            .voter_mut()
            .notify_reward_amount(minter_id(), em.emission));
        self.distribute_gauges(now);
    }

    /// `Voter.distribute` over all gauges. The dead-gauge share returns to the minter. A gauge
    /// whose claimable is above its left and above one week is funded, if its notify succeeds.
    fn distribute_gauges(&mut self, now: u64) {
        let mut lefts: BTreeMap<GaugeId, U256> = BTreeMap::new();
        let pools: Vec<PoolId> = self.voter_ref().pools.clone();
        for pool in &pools {
            if let Some(gauge) = self.gauges.get(pool) {
                let gauge_id = self.voter_ref().gauge_of(*pool);
                lefts.insert(gauge_id, gauge.left(now as u32));
            }
        }
        let gauge_ids: Vec<GaugeId> = pools
            .iter()
            .map(|p| self.voter_ref().gauge_of(*p))
            .collect();
        let (to_minter, candidates) = {
            let voter = self.voter_mut();
            let to_minter = ok(voter.distribute_gauges(&gauge_ids, |g| {
                (lefts.get(&g).copied().unwrap_or(U256::ZERO), false)
            }));
            let mut candidates = Vec::new();
            for gauge in &gauge_ids {
                let amount = voter.claimable_of(*gauge);
                let left = lefts.get(gauge).copied().unwrap_or(U256::ZERO);
                if amount > left && amount > U256::from(VOTE_DURATION) {
                    let pool = voter.pool_for_gauge.get(gauge).copied().unwrap_or(0);
                    candidates.push((*gauge, pool, to_u128(amount)));
                }
            }
            (to_u128(to_minter), candidates)
        };
        let wind = self.wind();
        if to_minter > 0 {
            self.debit(&voter_id(), &wind, to_minter);
            self.credit(&minter_id(), &wind, to_minter);
        }
        for (gauge, pool, amount) in candidates {
            self.notify_gauge(gauge, pool, amount, now);
        }
    }

    /// Sends `amount` of WIND from the voter to a gauge's reward, and books its fees to the voters.
    /// Works on copies, so a failed notify leaves the claimable in place.
    fn notify_gauge(&mut self, gauge: GaugeId, pool: PoolId, amount: u128, now: u64) {
        let wind = self.wind();
        let (token0, token1) = self.tokens_of(pool);
        let (Some(mut gauge_state), Some(mut pool_state)) = (
            self.gauges.get(&pool).cloned(),
            self.pools.get(&pool).cloned(),
        ) else {
            return;
        };
        gauge_state.donate(U256::from(amount));
        let result = gauge_state.notify_reward_amount(
            &mut pool_state,
            voter_id(),
            U256::from(amount),
            now as u32,
        );
        let Ok((fees0, fees1)) = result else {
            return;
        };
        self.gauges.insert(pool, gauge_state);
        self.pools.insert(pool, pool_state);
        self.debit(&voter_id(), &wind, amount);
        let (fees0, fees1) = (to_u128(fees0), to_u128(fees1));
        {
            let voter = self.voter_mut();
            voter.claimable.remove(&gauge);
            let rewards = voter
                .gauge_rewards
                .get_mut(&gauge)
                .unwrap_or_else(|| env::panic_str("gauge rewards missing"));
            if fees0 > 0 {
                ok(rewards
                    .fees
                    .notify_fees_reward(&token0, U256::from(fees0), now, true));
            }
            if fees1 > 0 {
                ok(rewards
                    .fees
                    .notify_fees_reward(&token1, U256::from(fees1), now, true));
            }
        }
        self.credit(&fees_id(gauge), &token0, fees0);
        self.credit(&fees_id(gauge), &token1, fees1);
    }

    /// The `distribute` flow. See the module docs.
    pub(crate) fn distribute_epoch(&mut self) -> PromiseOrValue<U128> {
        if self.pending_epoch.is_some() {
            env::panic_str("a distribution is already in progress");
        }
        let now = now_secs();
        let mut minter = self.minter_ref().clone();
        let period = now / WEEK * WEEK;
        let ve_supply = ok(self.ve_ref().total_supply_at(period.saturating_sub(1)));
        let wind = self.wind();
        let balance = U256::from(self.balance_of(&minter_id(), &wind));
        let total = self.wind_total_supply();
        match ok(minter.update_period(now, total, balance, ve_supply)) {
            None => {
                self.distribute_gauges(now);
                PromiseOrValue::Value(U128(0))
            }
            Some(em) if em.mint_required.is_zero() => {
                self.commit_epoch(minter, em, now);
                PromiseOrValue::Value(U128(0))
            }
            Some(em) => {
                let amount = to_u128(em.mint_required);
                self.pending_epoch = Some(PendingEpoch {
                    minter,
                    emission: em,
                });
                let me = env::current_account_id();
                let wind_account: AccountId = wind
                    .parse()
                    .unwrap_or_else(|_| env::panic_str("wind token is not an account id"));
                let promise = ext_wind::ext(wind_account)
                    .with_static_gas(Gas::from_tgas(MINT_GAS_TGAS))
                    .mint(me.clone(), U128(amount))
                    .then(
                        ext_tokenomics::ext(me)
                            .with_static_gas(Gas::from_tgas(DISTRIBUTE_CALLBACK_GAS_TGAS))
                            .on_distribute_minted(U128(amount)),
                    );
                PromiseOrValue::Promise(promise)
            }
        }
    }

    /// Callback of the mint in `distribute_epoch`. If the mint failed, the epoch stays uncommitted.
    pub(crate) fn on_distribute_minted_epoch(
        &mut self,
        amount: u128,
        minted: Result<(), PromiseError>,
    ) -> u128 {
        let pending = self
            .pending_epoch
            .take()
            .unwrap_or_else(|| env::panic_str("no distribution is pending"));
        if minted.is_err() {
            env::log_str("wind mint failed; the epoch is not committed and can be retried");
            return 0;
        }
        self.wind_minted = self
            .wind_minted
            .checked_add(amount)
            .unwrap_or_else(|| env::panic_str("wind supply overflow"));
        let wind = self.wind();
        self.credit(&minter_id(), &wind, amount);
        self.commit_epoch(pending.minter, pending.emission, now_secs());
        amount
    }
}

#[near]
impl Contract {
    /// Owner only. Sets the WIND token (a NEP-141 account this contract mints). It can be set
    /// once: the lock rows and the minter's balance are keyed by this id.
    pub fn set_wind_token(&mut self, account_id: AccountId) {
        self.assert_owner();
        if self.wind_token.is_some() {
            env::panic_str("wind token is already set");
        }
        self.ve_mut().token = account_id.to_string();
        self.wind_token = Some(account_id);
    }

    /// Owner only. Lets `bribe` accept `token`.
    pub fn whitelist_bribe_token(&mut self, token: String, flag: bool) {
        self.assert_owner();
        let gov = self.team_id();
        ok(self.voter_mut().whitelist_token(gov, &token, flag));
    }

    /// Owner only. Creates the gauge of `pool_id`. Returns the gauge id.
    pub fn create_gauge(&mut self, pool_id: u64) -> u64 {
        self.assert_owner();
        self.gauge_create(pool_id)
    }

    /// Owner only. Lets the team split locks.
    pub fn toggle_split(&mut self, account_id: AccountId, flag: bool) {
        self.assert_owner();
        let team = self.team_id();
        ok(self
            .ve_mut()
            .toggle_split(team, owner_of_account(&account_id), flag));
    }

    /// Owner only. Clears a distribution whose mint callback never committed. Use only if
    /// `distribute` is stuck, since the WIND of that mint then stays unallocated.
    pub fn clear_pending_epoch(&mut self) {
        self.assert_owner();
        self.pending_epoch = None;
    }

    /// Locks `amount` of the caller's WIND for `duration` seconds.
    pub fn create_lock(&mut self, amount: U128, duration: u64) -> u64 {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.lock_create(owner, amount.0, duration)
    }

    pub fn increase_amount(&mut self, token_id: u64, amount: U128) {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.lock_increase_amount(owner, token_id, amount.0);
    }

    pub fn increase_unlock_time(&mut self, token_id: u64, duration: u64) {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.lock_increase_unlock_time(owner, token_id, duration);
    }

    /// Withdraws an expired lock to the caller's internal WIND balance. Returns the amount.
    pub fn withdraw_lock(&mut self, token_id: u64) -> U128 {
        let owner = owner_of_account(&env::predecessor_account_id());
        U128(self.lock_withdraw(owner, token_id))
    }

    pub fn merge(&mut self, from: u64, to: u64) {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.lock_merge(owner, from, to);
    }

    /// Splits `amount` off `token_id`. Returns the two new token ids.
    pub fn split(&mut self, token_id: u64, amount: U128) -> (u64, u64) {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.lock_split(owner, token_id, amount.0)
    }

    pub fn lock_permanent(&mut self, token_id: u64) {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.lock_set_permanent(owner, token_id, true);
    }

    pub fn unlock_permanent(&mut self, token_id: u64) {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.lock_set_permanent(owner, token_id, false);
    }

    pub fn delegate(&mut self, delegator: u64, delegatee: u64) {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.lock_delegate(owner, delegator, delegatee);
    }

    /// Votes `weights` (relative) over `pools` with the veNFT's power. Once per epoch.
    pub fn vote(&mut self, token_id: u64, pools: Vec<u64>, weights: Vec<U128>) {
        let owner = owner_of_account(&env::predecessor_account_id());
        let weights: Vec<U256> = weights.iter().map(|w| U256::from(w.0)).collect();
        self.vote_for(owner, token_id, &pools, &weights);
    }

    pub fn reset(&mut self, token_id: u64) {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.reset_for(owner, token_id);
    }

    /// Re-votes a veNFT's pools with its current power. Anyone may call it.
    pub fn poke(&mut self, token_id: u64) {
        self.poke_token(token_id);
    }

    /// Stakes the caller's CL position in `[tick_lower, tick_upper)` of `pool_id` into its gauge.
    pub fn stake_position(&mut self, pool_id: u64, tick_lower: i32, tick_upper: i32) {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.stake_for(owner, pool_id, tick_lower, tick_upper);
    }

    pub fn unstake_position(&mut self, pool_id: u64, tick_lower: i32, tick_upper: i32) {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.unstake_for(owner, pool_id, tick_lower, tick_upper);
    }

    /// Pays the caller's gauge emissions for one staked position. Returns the amount.
    pub fn claim_gauge_rewards(&mut self, pool_id: u64, tick_lower: i32, tick_upper: i32) -> U128 {
        let owner = owner_of_account(&env::predecessor_account_id());
        U128(self.claim_gauge_for(owner, pool_id, tick_lower, tick_upper))
    }

    /// Claims fee rewards of a veNFT for `gauges`. Returns the amounts per token.
    pub fn claim_fees(&mut self, token_id: u64, gauges: Vec<u64>) -> Vec<(String, U128)> {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.claim_fees_for(owner, token_id, &gauges)
            .into_iter()
            .map(|(t, a)| (t, U128(a)))
            .collect()
    }

    /// Claims bribe rewards of a veNFT for `gauges`, for `tokens`. Returns the amounts per token.
    pub fn claim_bribes(
        &mut self,
        token_id: u64,
        gauges: Vec<u64>,
        tokens: Vec<String>,
    ) -> Vec<(String, U128)> {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.claim_bribes_for(owner, token_id, &gauges, &tokens)
            .into_iter()
            .map(|(t, a)| (t, U128(a)))
            .collect()
    }

    /// Adds `amount` of `token` as a bribe for the gauge of `pool_id`, paid from the caller's balance.
    pub fn bribe(&mut self, pool_id: u64, token: String, amount: U128) {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.bribe_for(owner, pool_id, &token, amount.0);
    }

    /// Pays the rebase owed to a veNFT. Returns the amount.
    pub fn claim_rebase(&mut self, token_id: u64) -> U128 {
        U128(self.claim_rebase_for(token_id))
    }

    /// Runs the weekly epoch flip, if due, and distributes the gauges. Callable by anyone.
    /// Attach at least 230 Tgas: the mint, then a callback that distributes every gauge.
    pub fn distribute(&mut self) -> PromiseOrValue<U128> {
        self.distribute_epoch()
    }

    /// Owner only, one time. Mints the genesis WIND supply to the team. See `genesis_mint_start`.
    pub fn genesis_mint(&mut self, amount: U128) -> Promise {
        self.assert_owner();
        self.genesis_mint_start(amount.0)
    }

    #[private]
    pub fn on_genesis_minted(
        &mut self,
        amount: U128,
        #[callback_result] minted: Result<(), PromiseError>,
    ) {
        self.genesis_mint_done(amount.0, minted);
    }

    #[private]
    pub fn on_distribute_minted(
        &mut self,
        amount: U128,
        #[callback_result] minted: Result<(), PromiseError>,
    ) -> U128 {
        U128(self.on_distribute_minted_epoch(amount.0, minted))
    }

    // ---- views ----

    pub fn get_lock(&self, token_id: u64) -> Option<LockView> {
        let ve = self.ve_ref();
        let owner = ve.owner_of.get(&token_id)?;
        let locked = ve.locked_of(token_id);
        Some(LockView {
            token_id,
            owner: owner_hex(owner),
            amount: U128(locked.amount.max(0) as u128),
            end: locked.end,
            permanent: locked.is_permanent,
            voted: ve.is_voted(token_id),
        })
    }

    /// Every veNFT owned by `owner` (64 hex digits, optional `0x`).
    pub fn get_locks_of(&self, owner: String) -> Vec<u64> {
        let Some(owner) = parse_owner_hex(&owner) else {
            return Vec::new();
        };
        self.ve_ref()
            .owner_of
            .iter()
            .filter(|(_, o)| **o == owner)
            .map(|(t, _)| *t)
            .collect()
    }

    /// Current voting power of a veNFT.
    pub fn balance_of_nft(&self, token_id: u64) -> U128 {
        let v = ok(self
            .ve_ref()
            .balance_of_nft(now_secs(), env::block_height(), token_id));
        U128(to_u128(v))
    }

    /// Total voting power, the sum of all locks now.
    pub fn total_voting_power(&self) -> U128 {
        U128(to_u128(ok(self.ve_ref().total_supply(now_secs()))))
    }

    /// The pools a veNFT voted for, with the weight on each.
    pub fn get_votes(&self, token_id: u64) -> Vec<(u64, U128)> {
        let voter = self.voter_ref();
        voter
            .pool_vote
            .get(&token_id)
            .map(|pools| {
                pools
                    .iter()
                    .map(|p| (*p, U128(to_u128(voter.votes_of(token_id, *p)))))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_gauge(&self, pool_id: u64) -> Option<GaugeView> {
        let voter = self.voter_ref();
        let gauge_id = voter.gauge_of(pool_id);
        if gauge_id == 0 {
            return None;
        }
        let g = self.gauges.get(&pool_id)?;
        Some(GaugeView {
            pool_id,
            gauge_id,
            alive: voter.alive(gauge_id),
            weight: U128(to_u128(voter.weight_of(pool_id))),
            gauge_account: owner_hex(&g.account),
            reward_balance: U128(to_u128(g.reward_balance)),
        })
    }

    pub fn get_epoch(&self) -> EpochView {
        let m = self.minter_ref();
        EpochView {
            active_period: m.active_period,
            epoch_count: m.epoch_count,
            weekly: U128(to_u128(m.weekly)),
            next_epoch_start: m.active_period + WEEK,
            wind_minted: U128(self.wind_minted),
            distribution_pending: self.pending_epoch.is_some(),
        }
    }

    /// Unclaimed fees of a veNFT for `gauges`, per token.
    pub fn claimable_fees(&self, token_id: u64, gauges: Vec<u64>) -> Vec<(String, U128)> {
        self.claimable_rewards(token_id, &gauges, None, true)
    }

    /// Unclaimed bribes of a veNFT for `gauges`, for `tokens`.
    pub fn claimable_bribes(
        &self,
        token_id: u64,
        gauges: Vec<u64>,
        tokens: Vec<String>,
    ) -> Vec<(String, U128)> {
        self.claimable_rewards(token_id, &gauges, Some(&tokens), false)
    }

    fn claimable_rewards(
        &self,
        token_id: u64,
        gauges: &[u64],
        tokens: Option<&Vec<String>>,
        is_fees: bool,
    ) -> Vec<(String, U128)> {
        let voter = self.voter_ref();
        let now = now_secs();
        let mut out: BTreeMap<String, u128> = BTreeMap::new();
        for gauge in gauges {
            let Some(pool) = voter.pool_for_gauge.get(gauge).copied() else {
                continue;
            };
            let names: Vec<String> = match tokens {
                Some(list) => list.clone(),
                None => {
                    let (t0, t1) = self.tokens_of(pool);
                    vec![t0, t1]
                }
            };
            let Some(rewards) = voter.gauge_rewards.get(gauge) else {
                continue;
            };
            let reward = if is_fees {
                &rewards.fees
            } else {
                &rewards.bribe
            };
            for name in names {
                let earned = ok(reward.earned(&name, token_id, now));
                *out.entry(name).or_default() += to_u128(earned);
            }
        }
        out.into_iter().map(|(t, a)| (t, U128(a))).collect()
    }

    /// Unclaimed rebase of a veNFT.
    pub fn claimable_rebase(&self, token_id: u64) -> U128 {
        let dist = self
            .distributor
            .get()
            .as_ref()
            .unwrap_or_else(|| env::panic_str("no distributor state"));
        U128(to_u128(ok(dist.claimable(token_id, self.ve_ref()))))
    }

    /// Unclaimed gauge emissions of a staked position.
    pub fn claimable_gauge_rewards(
        &self,
        owner: String,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    ) -> U128 {
        let Some(owner) = parse_owner_hex(&owner) else {
            return U128(0);
        };
        let Some(token_id) = self.stakes.get(&(owner, pool_id, tick_lower, tick_upper)) else {
            return U128(0);
        };
        let (Some(gauge), Some(pool), Some(nft)) = (
            self.gauges.get(&pool_id),
            self.pools.get(&pool_id),
            self.nft_manager.get().as_ref(),
        ) else {
            return U128(0);
        };
        U128(to_u128(ok_dbg(gauge.earned(
            pool,
            nft,
            owner,
            *token_id,
            now(),
        ))))
    }
}
