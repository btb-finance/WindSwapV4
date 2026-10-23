//! One-step flows: a token transfer that swaps, or adds liquidity, in the same call.
//!
//! `ft_on_transfer` reads a `swap` or `add_liquidity` message (see [`OneStepMsg`]). Intents (see
//! `intents.rs`) run the same plans later, from the owner's internal balance.
//!
//! Every operation is planned before anything is written. A plan runs the route or the zap on
//! copies of the pools and makes every check. Only a successful plan is committed, so a plan that
//! fails writes nothing. The NEP-141 hook then returns the full amount, and the token contract
//! refunds the sender.
//!
//! Accounting: the input of a transferred swap enters the pool through `route_reserves`, so it is
//! counted as a pool reserve from the start. A transferred zap is first credited to the owner, and
//! then the parts it spends are debited, which keeps `liabilities` and `pool_reserves` in step (see
//! `accounting`).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::json_types::U128;
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::{env, ext_contract, near, AccountId, Gas, NearToken, Promise};
use windswap_engine::math::{full_math, tick_math, Q96};
use windswap_engine::pool::cl_pool::ClPool;
use windswap_engine::router::Hop;
use windswap_engine::U256;

use crate::accounting::{Flow, HopFlow};
use crate::accounts::{owner_hex, owner_of_account, parse_owner_hex, Owner, WalletId};
use crate::{evm, ext_ft, now, to_u128, Contract, ContractExt, HopInput, Route};

/// Bisection rounds of the zap search. Each round runs one swap on a copy of the pool, so the
/// split is found to within `amount / 2^SPLIT_ROUNDS` of the input.
const SPLIT_ROUNDS: u32 = 40;
/// Mint attempts when the liquidity rounds one unit over a budget.
const MINT_ATTEMPTS: u32 = 8;

/// Where a one-step operation delivers its output: `internal`, `near:<account>` or `evm:0x…`.
///
/// It is written as a string in JSON and in signed intents.
#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde", try_from = "String", into = "String")]
pub enum Receiver {
    /// Credited to the beneficiary's internal balance.
    Internal,
    /// Sent to this NEAR account with `ft_transfer`. Credited back to the beneficiary if the
    /// transfer fails.
    Near(AccountId),
    /// Withdrawn to this EVM address on the output asset's chain (a pending withdrawal).
    Evm([u8; 20]),
}

impl fmt::Display for Receiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Internal => f.write_str("internal"),
            Self::Near(account) => write!(f, "near:{account}"),
            Self::Evm(address) => write!(f, "evm:{}", evm::hex_0x(address)),
        }
    }
}

impl FromStr for Receiver {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "internal" {
            return Ok(Self::Internal);
        }
        match s.parse::<WalletId>() {
            Ok(WalletId::Near(account)) => Ok(Self::Near(account)),
            Ok(WalletId::Evm(address)) => Ok(Self::Evm(address)),
            Ok(WalletId::Ed25519(_)) => {
                Err("receiver must be internal, near:<account> or evm:0x...".into())
            }
            Err(e) => Err(format!("invalid receiver {s}: {e}")),
        }
    }
}

impl TryFrom<String> for Receiver {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Receiver> for String {
    fn from(receiver: Receiver) -> Self {
        receiver.to_string()
    }
}

/// The JSON `msg` of a one-step NEP-141 transfer. A message with a `swap` or `add_liquidity` key
/// is one-step. Any other message keeps the plain deposit behaviour of `ft_on_transfer`.
#[near(serializers = [json])]
pub struct OneStepMsg {
    /// Beneficiary wallet (`evm:…`, `ed25519:…`, `near:…`). Defaults to the sender's account.
    pub owner: Option<String>,
    pub swap: Option<SwapMsg>,
    pub add_liquidity: Option<AddLiquidityMsg>,
}

/// The `swap` part of a [`OneStepMsg`]. The amount is the transferred amount.
#[near(serializers = [json])]
pub struct SwapMsg {
    pub path: Vec<HopInput>,
    pub min_amount_out: U128,
    /// Defaults to the sender's NEAR account when the output is a NEP-141 token, else `internal`.
    pub to: Option<Receiver>,
}

/// The `add_liquidity` part of a [`OneStepMsg`]. The amount is the transferred amount.
#[near(serializers = [json])]
pub struct AddLiquidityMsg {
    pub pool_id: u64,
    pub tick_lower: i32,
    pub tick_upper: i32,
    /// `true` swaps part of the input into the pool's other token first. `false` deposits the
    /// input alone, which needs a range that lies entirely on the input token's side of the price.
    pub zap: bool,
    pub min_liquidity: Option<U128>,
    /// Where the leftovers go: `internal` (the default) or `near:<account>`. Position owner is
    /// always the beneficiary.
    pub to: Option<Receiver>,
}

/// Parses the one-step part of a deposit `msg`. Returns `None` for a message that is not one-step,
/// so the plain deposit path handles it. A one-step message that does not parse is an `Err`.
pub(crate) fn parse_one_step(msg: &str) -> Option<Result<OneStepMsg, String>> {
    let value: near_sdk::serde_json::Value = near_sdk::serde_json::from_str(msg).ok()?;
    let object = value.as_object()?;
    if !object.contains_key("swap") && !object.contains_key("add_liquidity") {
        return None;
    }
    Some(near_sdk::serde_json::from_value(value).map_err(|e| format!("invalid message: {e}")))
}

/// Where the input of a planned operation comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The tokens arrived with the transfer and are not in any balance yet.
    Transfer,
    /// The tokens are in the owner's internal balance.
    Balance,
}

/// A planned swap. `pools` are the working copies after the swap.
pub(crate) struct SwapPlan {
    pub route: Route,
    pub pools: BTreeMap<u64, ClPool>,
    pub flows: Vec<HopFlow>,
    pub amount_in: u128,
    pub amount_out: u128,
}

/// A planned single-token add-liquidity: an optional swap leg, then a mint, on a copy of the pool.
pub(crate) struct ZapPlan {
    pub pool_id: u64,
    pub token_in: String,
    pub token_out: String,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub amount_in: u128,
    /// Input swapped into the pool's other token, and the output received.
    pub spent_in: u128,
    pub received_out: u128,
    pub flows: Vec<HopFlow>,
    /// The pool after the swap leg and the mint.
    pub pool: ClPool,
    /// What the mint takes from each token.
    pub used_in: u128,
    pub used_out: u128,
    pub liquidity: u128,
}

impl ZapPlan {
    /// Input left over after the swap and the mint.
    pub fn left_in(&self) -> u128 {
        self.amount_in - self.spent_in - self.used_in
    }

    /// Output left over after the mint.
    pub fn left_out(&self) -> u128 {
        self.received_out - self.used_out
    }
}

/// The fixed inputs of a zap search.
struct ZapSearch {
    owner: Owner,
    pool_id: u64,
    route: Route,
    base: ClPool,
    in_is0: bool,
    amount_in: u128,
    sqrt_a: U256,
    sqrt_b: U256,
}

/// One candidate split: `spent` of the input is swapped, and the budgets are what the mint may take.
struct Split {
    spent: u128,
    received: u128,
    flows: Vec<HopFlow>,
    pool: ClPool,
    budget0: u128,
    budget1: u128,
    liquidity: u128,
    /// The token0 side limits the liquidity (`false` if token1 does, or both are equal).
    token0_binds: bool,
    balanced: bool,
}

#[allow(dead_code)]
#[ext_contract(ext_oneclick)]
trait OneStepCallbacks {
    fn on_payout(&mut self, owner: String, token: String, amount: U128);
}

/// Default receiver of a swap whose output is `token_out`.
fn default_receiver(sender: &AccountId, token_out: &str) -> Receiver {
    if token_out.parse::<AccountId>().is_ok() {
        Receiver::Near(sender.clone())
    } else {
        Receiver::Internal
    }
}

/// Liquidity that `amount` of token0 supports between `a` and `b` (`a < b`, Uniswap's
/// `getLiquidityForAmount0`). Returns 0 on any error, and clamps to `u128::MAX`.
fn liquidity_for_amount0(a: U256, b: U256, amount: u128) -> u128 {
    if b <= a {
        return 0;
    }
    let inter = match full_math::mul_div(a, b, Q96) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    match full_math::mul_div(U256::from(amount), inter, b - a) {
        Ok(v) => u128::try_from(v).unwrap_or(u128::MAX),
        Err(_) => 0,
    }
}

/// Liquidity that `amount` of token1 supports between `a` and `b` (`getLiquidityForAmount1`).
fn liquidity_for_amount1(a: U256, b: U256, amount: u128) -> u128 {
    if b <= a {
        return 0;
    }
    match full_math::mul_div(U256::from(amount), Q96, b - a) {
        Ok(v) => u128::try_from(v).unwrap_or(u128::MAX),
        Err(_) => 0,
    }
}

/// The liquidity the token0 and token1 budgets each support at `price`, over `[sqrt_a, sqrt_b]`.
/// A side the range does not need is unlimited (`u128::MAX`).
fn liquidity_caps(
    price: U256,
    sqrt_a: U256,
    sqrt_b: U256,
    budget0: u128,
    budget1: u128,
) -> (u128, u128) {
    if price <= sqrt_a {
        (liquidity_for_amount0(sqrt_a, sqrt_b, budget0), u128::MAX)
    } else if price >= sqrt_b {
        (u128::MAX, liquidity_for_amount1(sqrt_a, sqrt_b, budget1))
    } else {
        (
            liquidity_for_amount0(price, sqrt_b, budget0),
            liquidity_for_amount1(sqrt_a, price, budget1),
        )
    }
}

/// Mints `liquidity` on a copy of `pool`. If the rounded amounts go one unit over a budget, the
/// liquidity shrinks a little and the mint is tried again. Returns the pool copy, the liquidity
/// that fit, and the amounts used.
fn mint_within(
    pool: &ClPool,
    owner: Owner,
    tick_lower: i32,
    tick_upper: i32,
    mut liquidity: u128,
    budget0: u128,
    budget1: u128,
) -> Result<(ClPool, u128, u128, u128), String> {
    for _ in 0..MINT_ATTEMPTS {
        if liquidity == 0 {
            break;
        }
        let mut next = pool.clone();
        let (a0, a1) = next
            .mint(owner, tick_lower, tick_upper, liquidity, now())
            .map_err(|e| e.to_string())?;
        if a0 <= U256::from(budget0) && a1 <= U256::from(budget1) {
            return Ok((next, liquidity, to_u128(a0), to_u128(a1)));
        }
        liquidity -= (liquidity / 1_000_000).max(1);
    }
    Err("the liquidity does not fit the amounts".into())
}

impl Contract {
    /// Beneficiary of a one-step operation: the owner of the wallet in `owner`, or the sender's
    /// account when there is none.
    fn beneficiary(&self, sender: &AccountId, wallet: Option<&String>) -> Result<Owner, String> {
        match wallet {
            None => Ok(owner_of_account(sender)),
            Some(wallet) => {
                let wallet: WalletId = wallet.parse().map_err(|e| format!("{e}"))?;
                Ok(self.links.resolve(&wallet))
            }
        }
    }

    /// Checks that a receiver can take an output of `token`.
    pub(crate) fn check_receiver(&self, to: &Receiver, token: &str) -> Result<(), String> {
        match to {
            Receiver::Internal => Ok(()),
            Receiver::Near(_) => {
                if token.parse::<AccountId>().is_ok() {
                    Ok(())
                } else {
                    Err(format!(
                        "{token} is not a NEP-141 token and cannot be sent to a NEAR account"
                    ))
                }
            }
            Receiver::Evm(_) => {
                if self.evm_asset_of(token).is_some() {
                    Ok(())
                } else {
                    Err(format!("{token} is not a registered EVM asset"))
                }
            }
        }
    }

    /// Plans a swap of `amount_in` along `path` for `trader`. Writes nothing.
    pub(crate) fn plan_swap(
        &self,
        trader: Owner,
        path: &[HopInput],
        amount_in: u128,
        min_out: u128,
    ) -> Result<SwapPlan, String> {
        if amount_in == 0 {
            return Err("amount must be positive".into());
        }
        let route = self.try_resolve_path(path)?;
        let mut pools = self.working_pools(&route.hops);
        let flows = self
            .route_exact_input(&route, &mut pools, trader, amount_in, min_out)
            .map_err(|e| e.to_string())?;
        let amount_out = to_u128(flows.last().map(|f| f.received).unwrap_or_default());
        if amount_out == 0 {
            return Err("swap output is zero".into());
        }
        Ok(SwapPlan {
            route,
            pools,
            flows,
            amount_in,
            amount_out,
        })
    }

    /// Commits a planned swap. A `Balance` source is debited first. The output is delivered to
    /// `to`. Nothing in this function fails, since the plan made every check.
    pub(crate) fn commit_swap(
        &mut self,
        trader: Owner,
        source: Source,
        plan: SwapPlan,
        to: &Receiver,
    ) {
        let SwapPlan {
            route,
            pools,
            flows,
            amount_in,
            amount_out,
        } = plan;
        if source == Source::Balance {
            self.debit(&trader, &route.token_in, amount_in);
        }
        self.write_back(pools);
        self.route_reserves(&flows);
        self.deliver(trader, &route.token_out, amount_out, to);
    }

    /// Delivers `amount` of `token` that `owner` is owed to `to`. It is credited first, so a
    /// send or a withdrawal debits the same amount.
    fn deliver(&mut self, owner: Owner, token: &str, amount: u128, to: &Receiver) {
        self.credit(&owner, token, amount);
        match to {
            Receiver::Internal => {}
            Receiver::Near(account) => {
                let _ = self.send_out(owner, token, amount, account);
            }
            Receiver::Evm(address) => {
                self.withdraw_evm_for(owner, token, amount, &evm::hex_0x(address));
            }
        }
    }

    /// Debits `amount` of `token` from `owner` and sends it to `account` with `ft_transfer`. The
    /// callback `on_payout` credits it back if the transfer fails.
    fn send_out(
        &mut self,
        owner: Owner,
        token: &str,
        amount: u128,
        account: &AccountId,
    ) -> Promise {
        self.debit(&owner, token, amount);
        let token_id: AccountId = token
            .parse()
            .unwrap_or_else(|_| env::panic_str("token is not a NEP-141 account"));
        ext_ft::ext(token_id)
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(Gas::from_tgas(5))
            .ft_transfer(account.clone(), U128(amount), None)
            .then(
                ext_oneclick::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(10))
                    .on_payout(owner_hex(&owner), token.to_string(), U128(amount)),
            )
    }

    /// Plans a single-token add-liquidity of `amount_in` of `token_in` into `pool_id`. With `zap`,
    /// the input is split: part is swapped into the other token, and the mint takes the largest
    /// liquidity that the two remaining budgets support at the new price. Writes nothing.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn plan_zap(
        &self,
        owner: Owner,
        pool_id: u64,
        token_in: &str,
        amount_in: u128,
        tick_lower: i32,
        tick_upper: i32,
        zap: bool,
        min_liquidity: u128,
        to: &Receiver,
    ) -> Result<ZapPlan, String> {
        if amount_in == 0 {
            return Err("amount must be positive".into());
        }
        if tick_lower >= tick_upper {
            return Err("tick_lower must be below tick_upper".into());
        }
        let (token0, token1) = self
            .pool_tokens
            .get(&pool_id)
            .cloned()
            .ok_or_else(|| format!("pool {pool_id} does not exist"))?;
        let in_is0 = if token_in == token0 {
            true
        } else if token_in == token1 {
            false
        } else {
            return Err(format!("{token_in} is not a token of pool {pool_id}"));
        };
        let token_out = if in_is0 {
            token1.clone()
        } else {
            token0.clone()
        };
        match to {
            Receiver::Evm(_) => {
                return Err("add_liquidity leftovers go to internal or near:<account>".into())
            }
            Receiver::Near(_) => {
                self.check_receiver(to, &token0)?;
                self.check_receiver(to, &token1)?;
            }
            Receiver::Internal => {}
        }
        let sqrt_a = tick_math::get_sqrt_ratio_at_tick(tick_lower).map_err(|e| e.to_string())?;
        let sqrt_b = tick_math::get_sqrt_ratio_at_tick(tick_upper).map_err(|e| e.to_string())?;
        let base = self
            .pools
            .get(&pool_id)
            .cloned()
            .ok_or_else(|| format!("pool {pool_id} does not exist"))?;
        let search = ZapSearch {
            owner,
            pool_id,
            route: Route {
                hops: vec![Hop {
                    pool_id,
                    zero_for_one: in_is0,
                }],
                token_in: token_in.to_string(),
                token_out: token_out.clone(),
            },
            base,
            in_is0,
            amount_in,
            sqrt_a,
            sqrt_b,
        };

        let mut best = self
            .split_at(&search, 0)
            .ok_or("the pool cannot be read at this range")?;
        if zap {
            // Bisection on the amount swapped. More swapped in makes the input budget smaller and
            // the output budget larger. If the input budget limits the liquidity, swap less.
            let (mut lo, mut hi) = (0u128, amount_in);
            for _ in 0..SPLIT_ROUNDS {
                if lo >= hi {
                    break;
                }
                let mid = lo + (hi - lo).div_ceil(2);
                let Some(candidate) = self.split_at(&search, mid) else {
                    hi = mid - 1;
                    continue;
                };
                let balanced = candidate.balanced;
                let input_binds = !candidate.balanced && (candidate.token0_binds == in_is0);
                if candidate.liquidity > best.liquidity {
                    best = candidate;
                }
                if balanced {
                    break;
                }
                if input_binds {
                    hi = mid - 1;
                } else {
                    lo = mid;
                }
            }
        }
        if best.liquidity == 0 {
            return Err("no liquidity fits this range and amount".into());
        }

        let Split {
            spent,
            received,
            flows,
            pool,
            budget0,
            budget1,
            liquidity,
            ..
        } = best;
        let (pool, liquidity, used0, used1) = mint_within(
            &pool, owner, tick_lower, tick_upper, liquidity, budget0, budget1,
        )?;
        if liquidity < min_liquidity {
            return Err(format!(
                "liquidity {liquidity} is below min_liquidity {min_liquidity}"
            ));
        }
        let (used_in, used_out) = if in_is0 {
            (used0, used1)
        } else {
            (used1, used0)
        };
        Ok(ZapPlan {
            pool_id,
            token_in: token_in.to_string(),
            token_out,
            tick_lower,
            tick_upper,
            amount_in,
            spent_in: spent,
            received_out: received,
            flows,
            pool,
            used_in,
            used_out,
            liquidity,
        })
    }

    /// Evaluates one split of a zap on copies: swaps `spent` of the input, then reads the
    /// liquidity the budgets support at the new price. `None` if the swap fails.
    fn split_at(&self, search: &ZapSearch, spent: u128) -> Option<Split> {
        let mut pools = BTreeMap::from([(search.pool_id, search.base.clone())]);
        let (flows, received) = if spent == 0 {
            (Vec::new(), 0)
        } else {
            let flows = self
                .route_exact_input(&search.route, &mut pools, search.owner, spent, 0)
                .ok()?;
            let received = to_u128(flows.last()?.received);
            (flows, received)
        };
        let remaining = search.amount_in - spent;
        let (budget0, budget1) = if search.in_is0 {
            (remaining, received)
        } else {
            (received, remaining)
        };
        let pool = pools.remove(&search.pool_id)?;
        let (cap0, cap1) = liquidity_caps(
            pool.slot0.sqrt_price_x96,
            search.sqrt_a,
            search.sqrt_b,
            budget0,
            budget1,
        );
        Some(Split {
            spent,
            received,
            flows,
            pool,
            budget0,
            budget1,
            liquidity: cap0.min(cap1),
            token0_binds: cap0 < cap1,
            balanced: cap0 == cap1,
        })
    }

    /// Commits a planned zap. A `Transfer` source is credited first. Leftovers stay in the owner's
    /// balance, or go to `to` (the caller has checked that `to` is not an EVM receiver).
    pub(crate) fn commit_zap(
        &mut self,
        owner: Owner,
        source: Source,
        plan: ZapPlan,
        to: &Receiver,
    ) {
        if source == Source::Transfer {
            self.credit(&owner, &plan.token_in, plan.amount_in);
        }
        let left_in = plan.left_in();
        let left_out = plan.left_out();
        if plan.spent_in > 0 {
            self.debit(&owner, &plan.token_in, plan.spent_in);
            self.credit(&owner, &plan.token_out, plan.received_out);
            self.route_reserves(&plan.flows);
        }
        self.debit(&owner, &plan.token_in, plan.used_in);
        self.debit(&owner, &plan.token_out, plan.used_out);
        self.reserve_in(Flow::Mint, plan.pool_id, &plan.token_in, plan.used_in);
        self.reserve_in(Flow::Mint, plan.pool_id, &plan.token_out, plan.used_out);
        self.pools.insert(plan.pool_id, plan.pool);
        self.has_state.insert(owner);
        self.sync_position_index(owner, plan.pool_id, plan.tick_lower, plan.tick_upper);

        match to {
            Receiver::Internal | Receiver::Evm(_) => {}
            Receiver::Near(account) => {
                for (token, amount) in [(&plan.token_in, left_in), (&plan.token_out, left_out)] {
                    if amount > 0 {
                        let _ = self.send_out(owner, token, amount, account);
                    }
                }
            }
        }
    }

    /// Runs a one-step NEP-141 transfer of `amount` of `token`, from `sender`. `Err` means the
    /// transfer must be refunded, and nothing was written.
    pub(crate) fn one_step(
        &mut self,
        sender: &AccountId,
        token: &str,
        amount: u128,
        msg: OneStepMsg,
    ) -> Result<(), String> {
        let owner = self.beneficiary(sender, msg.owner.as_ref())?;
        match (msg.swap, msg.add_liquidity) {
            (Some(swap), None) => {
                match swap.path.first() {
                    Some(first) if first.token_in == token => {}
                    _ => return Err(format!("the path must start with {token}")),
                }
                let plan = self.plan_swap(owner, &swap.path, amount, swap.min_amount_out.0)?;
                let to = swap
                    .to
                    .unwrap_or_else(|| default_receiver(sender, &plan.route.token_out));
                self.check_receiver(&to, &plan.route.token_out)?;
                self.commit_swap(owner, Source::Transfer, plan, &to);
                Ok(())
            }
            (None, Some(add)) => {
                let to = add.to.unwrap_or(Receiver::Internal);
                let min_liquidity = add.min_liquidity.map(|m| m.0).unwrap_or(0);
                let plan = self.plan_zap(
                    owner,
                    add.pool_id,
                    token,
                    amount,
                    add.tick_lower,
                    add.tick_upper,
                    add.zap,
                    min_liquidity,
                    &to,
                )?;
                self.commit_zap(owner, Source::Transfer, plan, &to);
                Ok(())
            }
            _ => Err("the message needs exactly one of swap or add_liquidity".into()),
        }
    }
}

#[near]
impl Contract {
    /// Callback of a payout (`send_out`). Credits the owner back if the `ft_transfer` failed,
    /// so a failed payout loses nothing.
    #[private]
    pub fn on_payout(&mut self, owner: String, token: String, amount: U128) {
        if env::promise_result_checked(0, usize::MAX).is_err() {
            let owner = parse_owner_hex(&owner)
                .unwrap_or_else(|| env::panic_str("owner must be 64 hex digits"));
            self.credit(&owner, &token, amount.0);
            env::log_str(&format!(
                "payout of {} {token} failed: credited back",
                amount.0
            ));
        }
    }
}
