//! EVM intents: a wallet on another chain signs an intent once. Anyone may relay it, and it runs
//! from the owner's internal balance as soon as that balance holds the input.
//!
//! - `submit_intent` checks the wallet's signature over the intent (see `intent_message` in
//!   `accounts.rs`) and stores the intent `Open`. Its nonce is shared with `execute`.
//! - `report_evm_deposit` runs the owner's oldest open intent that spends the credited token, once
//!   the deposit is credited. At most one intent runs per report.
//! - A failed intent is marked `Failed` with its reason, and its deposit stays credited. The report
//!   never fails because of an intent.
//! - `execute_intent` retries an open intent once its balance is there. `cancel_intent` (signed by
//!   the wallet) and `cancel_intent_near` (called by the owning NEAR account) mark it `Failed`.
//!
//! An intent is planned with the same code as a one-step transfer (see `oneclick`), with the
//! `Balance` source.

use near_sdk::json_types::U128;
use near_sdk::store::LookupMap;
use near_sdk::{env, near};

use crate::accounts::{
    cancel_intent_message, intent_message, owner_hex, owner_of_account, parse_owner_hex,
    parse_signature, verify_wallet_signature, Owner, WalletId,
};
use crate::oneclick::{Receiver, Source};
use crate::{now_secs, ok, parse_wallet, Contract, ContractExt, HopInput};

/// Most open intents per owner. Expired ones do not count.
pub const MAX_OPEN_INTENTS: usize = 8;

/// A signed intent: what to do with the owner's balance of `token_in`.
#[derive(Clone)]
#[near(serializers = [json, borsh])]
pub enum Intent {
    /// As `swap` in a one-step message, from the balance. `path[0].token_in` is `token_in`.
    Swap {
        token_in: String,
        amount_in: U128,
        path: Vec<HopInput>,
        min_amount_out: U128,
        to: Receiver,
    },
    /// Zap `amount_in` of `token_in` into `pool_id` from the balance. The leftovers stay in the
    /// balance.
    AddLiquidity {
        token_in: String,
        amount_in: U128,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        min_liquidity: U128,
    },
}

impl Intent {
    /// The internal token the intent spends.
    pub fn token_in(&self) -> &str {
        match self {
            Self::Swap { token_in, .. } | Self::AddLiquidity { token_in, .. } => token_in,
        }
    }

    /// The amount the intent spends.
    pub fn amount_in(&self) -> u128 {
        match self {
            Self::Swap { amount_in, .. } | Self::AddLiquidity { amount_in, .. } => amount_in.0,
        }
    }
}

/// What a successful intent did.
#[derive(Clone)]
#[near(serializers = [json, borsh])]
pub enum IntentResult {
    /// The output (`amount_out`) was delivered to `to`.
    Swap { amount_out: U128, to: Receiver },
    /// A position was minted. `spent_in` was swapped into `received_out`, and `used_in` and
    /// `used_out` went into the mint.
    AddLiquidity {
        liquidity: U128,
        spent_in: U128,
        received_out: U128,
        used_in: U128,
        used_out: U128,
    },
}

#[derive(Clone)]
#[near(serializers = [borsh])]
enum IntentStatus {
    Open,
    Executed { result: IntentResult },
    Failed { reason: String },
    Expired,
}

#[derive(Clone)]
#[near(serializers = [borsh])]
pub(crate) struct StoredIntent {
    owner: Owner,
    /// The canonical wallet string that signed the intent.
    wallet: String,
    intent: Intent,
    status: IntentStatus,
    expires: u64,
}

/// JSON view of an intent. `status` is `Open`, `Executed`, `Failed` or `Expired`. An open intent
/// past its `expires` reads as `Expired` even before anyone marks it.
#[near(serializers = [json])]
pub struct IntentView {
    pub id: u64,
    /// Owner id, 64 hex digits.
    pub owner_hex: String,
    pub wallet: String,
    pub intent: Intent,
    pub status: String,
    pub result: Option<IntentResult>,
    pub reason: Option<String>,
    pub expires: u64,
}

fn intent_view(id: u64, rec: &StoredIntent, now: u64) -> IntentView {
    let (status, result, reason) = match &rec.status {
        IntentStatus::Open if now > rec.expires => ("Expired", None, None),
        IntentStatus::Open => ("Open", None, None),
        IntentStatus::Executed { result } => ("Executed", Some(result.clone()), None),
        IntentStatus::Failed { reason } => ("Failed", None, Some(reason.clone())),
        IntentStatus::Expired => ("Expired", None, None),
    };
    IntentView {
        id,
        owner_hex: owner_hex(&rec.owner),
        wallet: rec.wallet.clone(),
        intent: rec.intent.clone(),
        status: status.to_string(),
        result,
        reason,
        expires: rec.expires,
    }
}

fn push_id(map: &mut LookupMap<Owner, Vec<u64>>, owner: Owner, id: u64) {
    let mut ids = map.get(&owner).cloned().unwrap_or_default();
    ids.push(id);
    map.insert(owner, ids);
}

fn remove_id(map: &mut LookupMap<Owner, Vec<u64>>, owner: Owner, id: u64) {
    let mut ids = map.get(&owner).cloned().unwrap_or_default();
    ids.retain(|x| *x != id);
    if ids.is_empty() {
        map.remove(&owner);
    } else {
        map.insert(owner, ids);
    }
}

impl Contract {
    /// Checks an intent before it is stored. Nothing here depends on balances or on the pools'
    /// state after submission.
    fn check_intent(&self, intent: &Intent) -> Result<(), String> {
        match intent {
            Intent::Swap {
                token_in,
                amount_in,
                path,
                to,
                ..
            } => {
                if amount_in.0 == 0 {
                    return Err("amount_in must be positive".into());
                }
                match path.first() {
                    Some(first) if first.token_in == *token_in => {}
                    _ => return Err("path must start with token_in".into()),
                }
                let route = self.try_resolve_path(path)?;
                self.check_receiver(to, &route.token_out)
            }
            Intent::AddLiquidity {
                token_in,
                amount_in,
                pool_id,
                tick_lower,
                tick_upper,
                ..
            } => {
                if amount_in.0 == 0 {
                    return Err("amount_in must be positive".into());
                }
                let (token0, token1) = self
                    .pool_tokens
                    .get(pool_id)
                    .cloned()
                    .ok_or_else(|| format!("pool {pool_id} does not exist"))?;
                if *token_in != token0 && *token_in != token1 {
                    return Err(format!("{token_in} is not a token of pool {pool_id}"));
                }
                if tick_lower >= tick_upper {
                    return Err("tick_lower must be below tick_upper".into());
                }
                Ok(())
            }
        }
    }

    /// Verifies a wallet's signature over `msg` and consumes `nonce` for the wallet. The nonce
    /// set is shared with `execute`.
    fn check_wallet_signature(
        &mut self,
        wallet: &WalletId,
        nonce: u64,
        msg: &str,
        signature: &str,
    ) {
        let sig = ok(parse_signature(wallet, signature));
        if !verify_wallet_signature(msg, wallet, sig.as_wallet_signature()) {
            env::panic_str("wallet signature does not verify");
        }
        if !self.action_nonces.insert((wallet.clone(), nonce)) {
            env::panic_str("action nonce was already used for this wallet");
        }
    }

    /// Replaces the status of intent `id`. A status other than `Open` removes it from its owner's
    /// open list.
    fn set_status(&mut self, id: u64, status: IntentStatus) {
        let mut rec = self
            .intents
            .get(&id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(&format!("intent {id} does not exist")));
        let owner = rec.owner;
        let open = matches!(status, IntentStatus::Open);
        rec.status = status;
        self.intents.insert(id, rec);
        if !open {
            remove_id(&mut self.open_intents, owner, id);
        }
    }

    /// Marks the open intents of `owner` that are past their expiry as `Expired`.
    fn expire_open_intents(&mut self, owner: Owner) {
        let now = now_secs();
        let ids = self.open_intents.get(&owner).cloned().unwrap_or_default();
        for id in ids {
            if self.intents.get(&id).is_some_and(|rec| now > rec.expires) {
                self.set_status(id, IntentStatus::Expired);
            }
        }
    }

    /// Runs `intent` for `owner` from the owner's balance. `Err` means nothing was written.
    fn run_intent(&mut self, owner: Owner, intent: &Intent) -> Result<IntentResult, String> {
        let token_in = intent.token_in().to_string();
        let amount_in = intent.amount_in();
        if self.balance_of(&owner, &token_in) < amount_in {
            return Err(format!("insufficient {token_in} balance"));
        }
        match intent {
            Intent::Swap {
                path,
                min_amount_out,
                to,
                ..
            } => {
                let plan = self.plan_swap(owner, path, amount_in, min_amount_out.0)?;
                self.check_receiver(to, &plan.route.token_out)?;
                let amount_out = plan.amount_out;
                self.commit_swap(owner, Source::Balance, plan, to);
                Ok(IntentResult::Swap {
                    amount_out: U128(amount_out),
                    to: to.clone(),
                })
            }
            Intent::AddLiquidity {
                pool_id,
                tick_lower,
                tick_upper,
                min_liquidity,
                ..
            } => {
                let plan = self.plan_zap(
                    owner,
                    *pool_id,
                    &token_in,
                    amount_in,
                    *tick_lower,
                    *tick_upper,
                    true,
                    min_liquidity.0,
                    &Receiver::Internal,
                )?;
                let result = IntentResult::AddLiquidity {
                    liquidity: U128(plan.liquidity),
                    spent_in: U128(plan.spent_in),
                    received_out: U128(plan.received_out),
                    used_in: U128(plan.used_in),
                    used_out: U128(plan.used_out),
                };
                self.commit_zap(owner, Source::Balance, plan, &Receiver::Internal);
                Ok(result)
            }
        }
    }

    /// Records the outcome of an attempt on intent `id`. A failure is kept as its reason.
    fn settle_intent(&mut self, id: u64, outcome: Result<IntentResult, String>) {
        let status = match outcome {
            Ok(result) => IntentStatus::Executed { result },
            Err(reason) => {
                env::log_str(&format!("intent {id} failed: {reason}"));
                IntentStatus::Failed { reason }
            }
        };
        self.set_status(id, status);
    }

    /// Runs the oldest open intent of `owner` that spends `token` and can be paid from the balance.
    /// Expired intents met on the way are marked `Expired`. At most one intent is attempted.
    /// Called after a deposit is credited, and never fails.
    pub(crate) fn run_open_intent(&mut self, owner: Owner, token: &str) {
        let now = now_secs();
        let ids = self.open_intents.get(&owner).cloned().unwrap_or_default();
        for id in ids {
            let Some(rec) = self.intents.get(&id).cloned() else {
                continue;
            };
            if now > rec.expires {
                self.set_status(id, IntentStatus::Expired);
                continue;
            }
            if rec.intent.token_in() != token
                || self.balance_of(&owner, token) < rec.intent.amount_in()
            {
                continue;
            }
            let outcome = self.run_intent(owner, &rec.intent);
            self.settle_intent(id, outcome);
            return;
        }
    }

    fn cancel_open(&mut self, id: u64, reason: &str) {
        self.set_status(
            id,
            IntentStatus::Failed {
                reason: reason.to_string(),
            },
        );
    }

    fn stored(&self, id: u64) -> StoredIntent {
        self.intents
            .get(&id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(&format!("intent {id} does not exist")))
    }
}

#[near]
impl Contract {
    /// Stores an intent signed by `wallet` and returns its id. Anyone may relay it, and the caller
    /// pays the gas. `signature` is the wallet's signature over `intent_message` (see
    /// `accounts.rs`), where `intent_json` is `serde_json::to_string(&intent)`. The nonce is used
    /// up, as for `execute`. Panics unless the intent is well formed, and unless the owner has
    /// fewer than `MAX_OPEN_INTENTS` open intents.
    pub fn submit_intent(
        &mut self,
        wallet: String,
        intent: Intent,
        nonce: u64,
        expires: u64,
        signature: String,
    ) -> u64 {
        let wallet = parse_wallet(&wallet);
        if now_secs() > expires {
            env::panic_str("intent has expired");
        }
        ok(self.check_intent(&intent));
        let intent_json = ok(near_sdk::serde_json::to_string(&intent));
        let msg = intent_message(
            &env::current_account_id(),
            &wallet,
            nonce,
            expires,
            &intent_json,
        );
        self.check_wallet_signature(&wallet, nonce, &msg, &signature);

        let owner = self.links.resolve(&wallet);
        self.expire_open_intents(owner);
        let open = self.open_intents.get(&owner).map_or(0, Vec::len);
        if open >= MAX_OPEN_INTENTS {
            env::panic_str(&format!(
                "the owner already has {MAX_OPEN_INTENTS} open intents"
            ));
        }
        let id = self.next_intent_id;
        self.next_intent_id = id
            .checked_add(1)
            .unwrap_or_else(|| env::panic_str("intent id overflow"));
        self.intents.insert(
            id,
            StoredIntent {
                owner,
                wallet: wallet.to_string(),
                intent,
                status: IntentStatus::Open,
                expires,
            },
        );
        push_id(&mut self.intents_by_owner, owner, id);
        push_id(&mut self.open_intents, owner, id);
        env::log_str(&format!("intent {id} submitted by {wallet}"));
        id
    }

    /// Runs open intent `id` from its owner's balance, if the balance holds the input. Anyone may
    /// call it. Panics if the intent is not open, or if the balance is short (nothing changes then).
    /// Otherwise the intent becomes `Executed`, or `Failed` with its reason. Returns the intent.
    pub fn execute_intent(&mut self, id: u64) -> IntentView {
        let rec = self.stored(id);
        if !matches!(rec.status, IntentStatus::Open) {
            env::panic_str(&format!("intent {id} is not open"));
        }
        let now = now_secs();
        if now > rec.expires {
            self.set_status(id, IntentStatus::Expired);
        } else {
            let token_in = rec.intent.token_in();
            if self.balance_of(&rec.owner, token_in) < rec.intent.amount_in() {
                env::panic_str(&format!(
                    "insufficient {token_in} balance: fund the owner before retrying"
                ));
            }
            let outcome = self.run_intent(rec.owner, &rec.intent);
            self.settle_intent(id, outcome);
        }
        intent_view(id, &self.stored(id), now)
    }

    /// Cancels open intent `id`, signed by the wallet that submitted it. `signature` is the
    /// wallet's signature over `cancel_intent_message`. The nonce is used up. The intent becomes
    /// `Failed` with the reason `cancelled by owner`.
    pub fn cancel_intent(
        &mut self,
        wallet: String,
        id: u64,
        nonce: u64,
        expires: u64,
        signature: String,
    ) {
        let wallet = parse_wallet(&wallet);
        if now_secs() > expires {
            env::panic_str("cancel request has expired");
        }
        let msg = cancel_intent_message(&env::current_account_id(), &wallet, nonce, expires, id);
        self.check_wallet_signature(&wallet, nonce, &msg, &signature);
        let rec = self.stored(id);
        if rec.wallet != wallet.to_string() {
            env::panic_str("the wallet did not submit this intent");
        }
        self.cancel_checked(id, &rec);
    }

    /// Cancels open intent `id` for the calling NEAR account, which must own it.
    pub fn cancel_intent_near(&mut self, id: u64) {
        let rec = self.stored(id);
        if rec.owner != owner_of_account(&env::predecessor_account_id()) {
            env::panic_str("the caller does not own this intent");
        }
        self.cancel_checked(id, &rec);
    }

    /// One intent by id.
    pub fn get_intent(&self, id: u64) -> Option<IntentView> {
        self.intents
            .get(&id)
            .map(|rec| intent_view(id, rec, now_secs()))
    }

    /// Intents of an owner (64 hex digits), in submission order: `limit` from index `from_index`,
    /// of every status.
    pub fn get_intents_of_owner(
        &self,
        owner: String,
        from_index: u64,
        limit: u64,
    ) -> Vec<IntentView> {
        let owner = parse_owner_hex(&owner)
            .unwrap_or_else(|| env::panic_str("owner must be 64 hex digits"));
        let from = usize::try_from(from_index).unwrap_or(usize::MAX);
        let take = usize::try_from(limit).unwrap_or(usize::MAX);
        let now = now_secs();
        self.intents_by_owner
            .get(&owner)
            .map(|ids| {
                ids.iter()
                    .skip(from)
                    .take(take)
                    .filter_map(|id| self.intents.get(id).map(|rec| intent_view(*id, rec, now)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Contract {
    fn cancel_checked(&mut self, id: u64, rec: &StoredIntent) {
        if !matches!(rec.status, IntentStatus::Open) {
            env::panic_str(&format!("intent {id} is not open"));
        }
        self.cancel_open(id, "cancelled by owner");
    }
}
