//! Solvency accounting: what the contract owes, per token, so anyone can check it holds that much.
//!
//! # The claim
//! For every token `t`, the tokens the contract holds must be at least `total_owed(t)`:
//!
//! ```text
//! total_owed(t) = liabilities(t)        Σ internal balances of t (every owner, every system id)
//!               + pool_reserves(t)      tokens held for pools, their gauges and their NFT manager
//!               + locked(t)             WIND only: VotingEscrow.supply (locked veNFT amounts)
//!               + pending_withdrawals(t) EVM only: debited withdrawals not yet broadcast
//! ```
//!
//! `held(t)` is what the token contract reports for this account (NEP-141), or what the EVM vault
//! holds (EVM assets, checked off-chain with `get_evm_owed`). `unbacked(t)` is faucet credit
//! (`test_credit`) that no deposit paid for. It is included in `liabilities`, so a faucet credit
//! makes the token insolvent on purpose and the view shows it.
//!
//! # Where each total is updated
//! Every movement of a token goes through one of these, so the totals cannot drift:
//!
//! - `liabilities`: only `Contract::credit` and `Contract::debit` write balances, and they update
//!   `liabilities` by the same amount (see `liability_add` / `liability_sub`). Deposits,
//!   withdrawals, locks, rebases, rewards, bribes, fee claims and epoch mints all use them.
//! - `pool_reserves` (per `(pool, token)`), updated where the engine moves tokens across the pool
//!   boundary. The reserve of a pool is the net tokens that entered it minus the tokens that left
//!   it, so it covers liquidity, tokens owed to positions, gauge fees (held in the gauge, not
//!   yet booked) and rounding dust:
//!   - `mint`: the amounts the engine returns are paid in, so the reserve goes up by them.
//!   - `burn`: no tokens move. The amounts move from liquidity to tokens owed inside the pool,
//!     so no reserve changes.
//!   - `collect` (and the pool-side `collect` inside `stake` / `unstake` / the NFT manager's
//!     `collect`): the amounts the engine pays out leave the reserve.
//!   - `swap` (single pool, and each hop of a route): the engine's signed deltas. A positive
//!     delta is paid by the trader into the pool, so it goes up. A negative delta is paid out.
//!     Route hops are run one at a time, and each hop's paid and received amounts are recorded,
//!     so per-pool reserves follow each hop's own pool.
//!   - gauge emissions: the WIND the voter sends to a gauge (`notify_gauge`) enters the pool's
//!     reserve. The WIND a staker claims (`claim_gauge_for`, and the reward paid by `unstake`)
//!     leaves it.
//!   - gauge fees: the fees the gauge books for the voters (`notify_gauge`) leave the reserve,
//!     because `liabilities` takes them (credited to the fees owner).
//!   - `stake` and `unstake` are a pool burn, a pool collect and a pool mint, plus the gauge's
//!     collect of the NFT. Each collect is an outflow and each mint an inflow, recorded as above.
//!     The caller's balance is then settled by `net_settle`.
//! - `locked`: read from `VotingEscrow.supply` when the view runs. The engine adds and removes
//!   it with every lock change, so the contract does not keep a second copy.
//! - `pending_withdrawals`: `withdraw_evm` adds the amount, `mark_withdrawal_broadcast` removes
//!   it, and a withdrawal that fails and is credited back removes it too. A broadcast withdrawal
//!   is no longer counted: its tokens still sit in the vault until the transaction executes, so
//!   `vault >= owed` still holds in between.
//! - `unbacked`: `test_credit` only.
//!
//! # Migration (`seeding`)
//! The balances are a `LookupMap` and cannot be iterated, and the pools' reserves are not stored
//! anywhere, so a migrated contract cannot compute its totals on its own. On a migrated contract
//! (`needs_seed`):
//! - A balance is counted in `liabilities` the first time its key is touched after the migration
//!   (`seed_key` adds its current value, then the change). The untouched keys are added by
//!   `seed_accounting`, which takes the `(owner, token)` keys the owner has indexed. A key is
//!   counted once (the `accounted` set).
//! - Pool reserves are added by `seed_pool_reserves`, once per `(pool, token)`, with the amount
//!   the pool and its gauge hold before the migration. The owner computes them off-chain from the
//!   pool state. `check_solvency(token)` then tests the total against the token contract's balance,
//!   so a wrong split across pools shows up as a per-token total.
//! - `pending_withdrawals` are summed by `migrate` itself from the stored withdrawals.
//!
//! A fresh deployment (`Contract::new`) has `needs_seed = false`, counts every change from the
//! start, and never needs seeding.
//!
//! `seed_accounting` can run before or after the touches of a key. It adds only the keys not yet
//! in `accounted`, and a touch adds the balance it finds before it applies its change, so a key
//! is never counted twice.

use std::collections::BTreeSet;

use near_sdk::json_types::U128;
use near_sdk::{env, ext_contract, near, AccountId, Gas, Promise, PromiseError};
use windswap_engine::I256;

use crate::accounts::{parse_owner_hex, Owner};
use crate::{to_u128, Contract, ContractExt};

/// Where tokens crossed a pool boundary. Only used to label the reserve updates, and by the
/// test-only fault injection that proves the invariant checks catch a missed update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// `ClPool::mint` (also the mint inside a stake).
    Mint,
    /// `ClPool::collect` paid to a position.
    Collect,
    /// A swap, or one hop of a route.
    Swap,
    /// The collect and mint of `stake_position`.
    Stake,
    /// The collect and mint of `unstake_position`.
    Unstake,
    /// WIND the voter sends to a gauge.
    EmissionIn,
    /// WIND a staker claims from a gauge.
    EmissionOut,
    /// Gauge fees booked to the voters.
    FeesOut,
    /// A reserve set by `seed_pool_reserves`.
    Seed,
}

/// Test-only fault injection: a flow whose reserve update is skipped.
#[cfg(test)]
pub(crate) mod fault {
    use super::Flow;
    use std::cell::Cell;

    thread_local! {
        static SKIP: Cell<Option<Flow>> = const { Cell::new(None) };
    }

    /// Skips every reserve update of `flow` on this thread until `clear`.
    pub(crate) fn skip(flow: Flow) {
        SKIP.with(|s| s.set(Some(flow)));
    }

    pub(crate) fn skipped(flow: Flow) -> bool {
        SKIP.with(|s| s.get() == Some(flow))
    }
}

/// Per-token accounting, as a view.
#[near(serializers = [json])]
pub struct TokenAccountingView {
    pub token: String,
    /// Sum of internal balances of `token`.
    pub liabilities: U128,
    /// Tokens held for pools, gauges and the NFT manager.
    pub pool_reserves: U128,
    /// WIND locked in veNFTs (zero for other tokens).
    pub locked: U128,
    /// EVM withdrawals debited but not yet broadcast (zero for NEP-141 tokens).
    pub pending_withdrawals: U128,
    /// `liabilities + pool_reserves + locked + pending_withdrawals`.
    pub total_owed: U128,
    /// Faucet credit that no deposit backs. Already part of `liabilities`.
    pub unbacked: U128,
    /// True on a migrated contract. The totals are complete only after the owner has seeded
    /// every `(owner, token)` key that was not touched since the migration, and every pool.
    pub migrated: bool,
}

/// Result of `check_solvency`.
#[near(serializers = [json])]
pub struct SolvencyView {
    pub token: String,
    /// The token contract's balance of this account.
    pub held: U128,
    pub owed: U128,
    /// `held >= owed`.
    pub solvent: bool,
    pub unbacked: U128,
}

/// One engine hop of a route, with the amounts it paid in and received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HopFlow {
    pub pool_id: u64,
    pub zero_for_one: bool,
    pub paid: windswap_engine::U256,
    pub received: windswap_engine::U256,
}

#[allow(dead_code)]
#[ext_contract(ext_ft_balance)]
trait FtBalanceOf {
    fn ft_balance_of(&self, account_id: AccountId) -> U128;
}

#[allow(dead_code)]
#[ext_contract(ext_accounting)]
trait AccountingCallbacks {
    fn on_solvency_checked(&self, token: String) -> SolvencyView;
}

fn to_i128(x: u128) -> i128 {
    i128::try_from(x).unwrap_or_else(|_| env::panic_str("amount does not fit in i128"))
}

impl Contract {
    // ---- liabilities ----

    /// Counts `key` in `liabilities` on its first touch after a migration. `current` is its
    /// balance before the change. Does nothing on a fresh contract.
    fn seed_key(&mut self, key: &(Owner, String), current: u128) {
        if self.needs_seed && self.accounted.insert(key.clone()) {
            self.add_liabilities(&key.1, current);
        }
    }

    fn add_liabilities(&mut self, token: &str, amount: u128) {
        let total = self.liabilities.get(token).copied().unwrap_or(0);
        let next = total
            .checked_add(amount)
            .unwrap_or_else(|| env::panic_str("liabilities overflow"));
        self.liabilities.insert(token.to_string(), next);
    }

    /// Called by `credit` before the balance changes.
    pub(crate) fn liability_add(&mut self, key: &(Owner, String), current: u128, amount: u128) {
        self.seed_key(key, current);
        self.add_liabilities(&key.1, amount);
    }

    /// Called by `debit` before the balance changes.
    pub(crate) fn liability_sub(&mut self, key: &(Owner, String), current: u128, amount: u128) {
        self.seed_key(key, current);
        let total = self.liabilities.get(&key.1).copied().unwrap_or(0);
        let next = total
            .checked_sub(amount)
            .unwrap_or_else(|| env::panic_str("liabilities underflow"));
        self.liabilities.insert(key.1.clone(), next);
    }

    // ---- pool reserves ----

    /// Tokens entered `pool` (paid in by a trader, or emitted to it).
    pub(crate) fn reserve_in(&mut self, flow: Flow, pool: u64, token: &str, amount: u128) {
        if amount > 0 {
            self.reserve_add(flow, pool, token, to_i128(amount));
        }
    }

    /// Tokens left `pool` (paid out to a trader, or to a position or the fees owner).
    pub(crate) fn reserve_out(&mut self, flow: Flow, pool: u64, token: &str, amount: u128) {
        if amount > 0 {
            self.reserve_add(flow, pool, token, -to_i128(amount));
        }
    }

    /// Applies an engine delta as the pool sees it: positive is paid into the pool.
    pub(crate) fn reserve_flow(&mut self, flow: Flow, pool: u64, token: &str, delta: I256) {
        let magnitude = to_i128(to_u128(delta.unsigned_abs()));
        if delta.is_positive() {
            self.reserve_add(flow, pool, token, magnitude);
        } else if delta.is_negative() {
            self.reserve_add(flow, pool, token, -magnitude);
        }
    }

    fn reserve_add(&mut self, flow: Flow, pool: u64, token: &str, delta: i128) {
        #[cfg(test)]
        if fault::skipped(flow) {
            return;
        }
        let _ = flow;
        let key = (pool, token.to_string());
        let current = self.pool_reserves.get(&key).copied().unwrap_or(0);
        let next = current
            .checked_add(delta)
            .unwrap_or_else(|| env::panic_str("pool reserve overflow"));
        self.pool_reserves.insert(key, next);
        let total = self.reserve_totals.get(token).copied().unwrap_or(0);
        let total = total
            .checked_add(delta)
            .unwrap_or_else(|| env::panic_str("reserve total overflow"));
        self.reserve_totals.insert(token.to_string(), total);
    }

    // ---- EVM withdrawals and faucet ----

    /// A withdrawal of `amount` of `token` was recorded and debited.
    pub(crate) fn pending_add(&mut self, token: &str, amount: u128) {
        let total = self.pending_withdrawals.get(token).copied().unwrap_or(0);
        let next = total
            .checked_add(amount)
            .unwrap_or_else(|| env::panic_str("pending withdrawals overflow"));
        self.pending_withdrawals.insert(token.to_string(), next);
    }

    /// A pending withdrawal stopped being owed: broadcast, or failed and credited back.
    pub(crate) fn pending_sub(&mut self, token: &str, amount: u128) {
        let total = self.pending_withdrawals.get(token).copied().unwrap_or(0);
        let next = total
            .checked_sub(amount)
            .unwrap_or_else(|| env::panic_str("pending withdrawals underflow"));
        if next == 0 {
            self.pending_withdrawals.remove(token);
        } else {
            self.pending_withdrawals.insert(token.to_string(), next);
        }
    }

    /// Faucet credit of `amount` of `token`, not backed by a deposit.
    pub(crate) fn unbacked_add(&mut self, token: &str, amount: u128) {
        let total = self.unbacked.get(token).copied().unwrap_or(0);
        let next = total
            .checked_add(amount)
            .unwrap_or_else(|| env::panic_str("unbacked overflow"));
        self.unbacked.insert(token.to_string(), next);
    }

    // ---- views ----

    /// The accounting of `token`, as the totals stand now.
    pub(crate) fn accounting_of(&self, token: &str) -> TokenAccountingView {
        let liabilities = self.liabilities.get(token).copied().unwrap_or(0);
        let reserves = self.reserve_totals.get(token).copied().unwrap_or(0);
        if reserves < 0 {
            env::panic_str(&format!(
                "pool reserves of {token} are negative: seed_pool_reserves is incomplete"
            ));
        }
        let reserves = reserves as u128;
        let locked = if self.wind_token.as_ref().map(|w| w.as_str()) == Some(token) {
            self.ve
                .get()
                .as_ref()
                .map(|ve| to_u128(ve.supply))
                .unwrap_or(0)
        } else {
            0
        };
        let pending = self.pending_withdrawals.get(token).copied().unwrap_or(0);
        let total_owed = liabilities
            .checked_add(reserves)
            .and_then(|x| x.checked_add(locked))
            .and_then(|x| x.checked_add(pending))
            .unwrap_or_else(|| env::panic_str("total owed overflow"));
        TokenAccountingView {
            token: token.to_string(),
            liabilities: U128(liabilities),
            pool_reserves: U128(reserves),
            locked: U128(locked),
            pending_withdrawals: U128(pending),
            total_owed: U128(total_owed),
            unbacked: U128(self.unbacked.get(token).copied().unwrap_or(0)),
            migrated: self.needs_seed,
        }
    }

    /// Every token the contract knows: the NEP-141 whitelist, the registered EVM assets, and WIND.
    fn known_tokens(&self) -> BTreeSet<String> {
        let mut tokens: BTreeSet<String> =
            self.allowed_tokens.iter().map(|t| t.to_string()).collect();
        tokens.extend(self.evm_assets.values().map(|a| a.token_id.clone()));
        if let Some(wind) = &self.wind_token {
            tokens.insert(wind.to_string());
        }
        tokens
    }
}

#[near]
impl Contract {
    /// Accounting of one token: liabilities, pool reserves, locked WIND, pending EVM withdrawals,
    /// and `total_owed`, which the contract's holdings must cover.
    pub fn get_token_accounting(&self, token: String) -> TokenAccountingView {
        self.accounting_of(&token)
    }

    /// `get_token_accounting` for every known token (see `known_tokens`).
    pub fn get_all_tokens_accounting(&self) -> Vec<TokenAccountingView> {
        self.known_tokens()
            .iter()
            .map(|t| self.accounting_of(t))
            .collect()
    }

    /// Total owed of an EVM asset: what its vault on the asset's chain must hold. Compare it
    /// with the vault's on-chain balance (native balance, or `balanceOf(vault)` of the ERC-20).
    /// Panics if the asset is not registered.
    pub fn get_evm_owed(&self, chain_id: u64, asset: String) -> U128 {
        let key = crate::evm_asset_key(
            chain_id,
            &crate::normalize_asset(&asset).unwrap_or_else(|e| env::panic_str(&e)),
        );
        let info = self.evm_assets.get(&key).unwrap_or_else(|| {
            env::panic_str(&format!(
                "asset {asset} on chain {chain_id} is not registered"
            ))
        });
        self.accounting_of(&info.token_id.clone()).total_owed
    }

    /// Pool reserves of `pool_id`, per its two tokens: `[(token0, amount), (token1, amount)]`.
    pub fn get_pool_reserves(&self, pool_id: u64) -> Vec<(String, U128)> {
        let (token0, token1) = self.tokens_of(pool_id);
        [token0, token1]
            .into_iter()
            .map(|t| {
                let r = self
                    .pool_reserves
                    .get(&(pool_id, t.clone()))
                    .copied()
                    .unwrap_or(0);
                if r < 0 {
                    env::panic_str(&format!("reserve of {t} in pool {pool_id} is negative: seed_pool_reserves is incomplete"));
                }
                (t, U128(r as u128))
            })
            .collect()
    }

    /// Asks the NEP-141 token contract for this account's balance of `token`. The callback
    /// `on_solvency_checked` returns `{held, owed, solvent}`, with `owed` read at callback time.
    /// Panics for an EVM asset, which is checked with `get_evm_owed` against the vault.
    pub fn check_solvency(&self, token: String) -> Promise {
        if self.evm_asset_by_token.contains_key(&token) {
            env::panic_str("EVM assets are checked against the vault: use get_evm_owed");
        }
        let account: AccountId = token
            .parse()
            .unwrap_or_else(|_| env::panic_str("token is not a NEP-141 account"));
        let me = env::current_account_id();
        ext_ft_balance::ext(account)
            .with_static_gas(Gas::from_tgas(5))
            .ft_balance_of(me.clone())
            .then(
                ext_accounting::ext(me)
                    .with_static_gas(Gas::from_tgas(10))
                    .on_solvency_checked(token),
            )
    }

    /// Callback of `check_solvency`.
    #[private]
    pub fn on_solvency_checked(
        &self,
        token: String,
        #[callback_result] held: Result<U128, PromiseError>,
    ) -> SolvencyView {
        let held = held.unwrap_or_else(|e| env::panic_str(&format!("ft_balance_of failed: {e:?}")));
        let acc = self.accounting_of(&token);
        SolvencyView {
            token,
            held,
            owed: acc.total_owed,
            solvent: held.0 >= acc.total_owed.0,
            unbacked: acc.unbacked,
        }
    }

    /// Owner only, migrated contracts only. Counts the balances of the listed `(owner_hex,
    /// token)` keys in `liabilities`, once per key. Keys already counted are skipped, so the call
    /// can be repeated. Returns the number of keys newly counted.
    pub fn seed_accounting(&mut self, entries: Vec<(String, String)>) -> u64 {
        self.assert_owner();
        if !self.needs_seed {
            env::panic_str("the contract was not migrated: nothing to seed");
        }
        let mut counted = 0;
        for (owner_hex, token) in entries {
            let owner = parse_owner_hex(&owner_hex)
                .unwrap_or_else(|| env::panic_str("owner must be 64 hex digits"));
            let key = (owner, token.clone());
            let current = self.balances.get(&key).copied().unwrap_or(0);
            if !self.accounted.contains(&key) {
                self.seed_key(&key, current);
                counted += 1;
            }
        }
        counted
    }

    /// Owner only, migrated contracts only. Adds the tokens that `pool_id` held before the
    /// migration: `(token, amount)` for each of its two tokens, as the pool, its gauge and its NFT
    /// manager hold them. Each `(pool, token)` is seeded once. Returns the number seeded.
    pub fn seed_pool_reserves(&mut self, entries: Vec<(u64, String, U128)>) -> u64 {
        self.assert_owner();
        if !self.needs_seed {
            env::panic_str("the contract was not migrated: nothing to seed");
        }
        let mut seeded = 0;
        for (pool_id, token, amount) in entries {
            let (token0, token1) = self.tokens_of(pool_id);
            if token != token0 && token != token1 {
                env::panic_str(&format!("{token} is not a token of pool {pool_id}"));
            }
            if self.reserves_seeded.insert((pool_id, token.clone())) {
                self.reserve_in(Flow::Seed, pool_id, &token, amount.0);
                seeded += 1;
            }
        }
        seeded
    }
}
