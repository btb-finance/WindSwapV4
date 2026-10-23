//! Multi-hop swap router and quoter over [`ClPool`]s.
//!
//! Port of the exact-input and exact-output paths of `SwapRouter.sol` (`exactInput`,
//! `exactOutput`) and of `QuoterV2.sol`, without callbacks: the engine has no token
//! transfers, so each hop is a plain [`ClPool::swap`] whose result feeds the next hop.
//!
//! Path convention: `path[0]` is the first hop in trading order (token in to token out).
//! Exact input runs `path` front to back. Exact output runs it back to front, as
//! `SwapRouter` does: the last pool produces the requested output, and its required input
//! becomes the requested output of the previous pool.
//!
//! Atomicity: every hop runs on a working copy of the pools the route touches. The store is
//! written only after all hops and slippage checks pass, so an `Err` leaves every pool
//! unchanged.
//!
//! Price limits are `MIN_SQRT_RATIO + 1` / `MAX_SQRT_RATIO - 1`, the defaults `SwapRouter`
//! uses when `sqrtPriceLimitX96 == 0`.

use std::collections::BTreeMap;

use crate::error::{MathError, Result};
use crate::fees::{AccountId, FeeConfig, PoolId};
use crate::math::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};
use crate::pool::cl_pool::{ClPool, SwapResult};
use crate::{I256, U256};

/// One hop of a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hop {
    /// Pool to trade through.
    pub pool_id: PoolId,
    /// Direction of the trade in this pool. `true` pays token0 and receives token1.
    pub zero_for_one: bool,
}

/// Access to the pools and fee inputs the router needs.
pub trait PoolStore {
    /// Mutable access to a pool, or `None` if it does not exist.
    fn pool_mut(&mut self, id: PoolId) -> Option<&mut ClPool>;
    /// Whether the pool has a live gauge (`voter.isAlive(voter.gauges(pool))`).
    fn gauge_alive(&self, id: PoolId) -> bool;
}

/// Plain map store. `gauge_alive` is always `false`, so no unstaked fee applies.
impl PoolStore for BTreeMap<PoolId, ClPool> {
    fn pool_mut(&mut self, id: PoolId) -> Option<&mut ClPool> {
        self.get_mut(&id)
    }

    fn gauge_alive(&self, _id: PoolId) -> bool {
        false
    }
}

/// Executes multi-hop swaps against a [`PoolStore`] for one trader at one time.
pub struct Router<'a, S: PoolStore> {
    pub store: &'a mut S,
    pub fees: &'a FeeConfig,
    pub trader: AccountId,
    pub time: u32,
}

/// Working copy of a pool used by one route, plus its gauge flag read at load time.
struct Working {
    pool: ClPool,
    gauge_alive: bool,
}

type WorkingSet = BTreeMap<PoolId, Working>;

impl<S: PoolStore> Router<'_, S> {
    /// `SwapRouter.exactInput`. Spends `amount_in` of the first hop's input token and returns
    /// the output of the last hop. Fails with [`MathError::TooLittleReceived`] if the output
    /// is below `amount_out_minimum`. Pools are unchanged on any error.
    pub fn exact_input(
        &mut self,
        path: &[Hop],
        amount_in: U256,
        amount_out_minimum: U256,
    ) -> Result<U256> {
        let mut work = self.load(path)?;
        let amount_out = self.run_exact_input(&mut work, path, amount_in)?;
        if amount_out < amount_out_minimum {
            return Err(MathError::TooLittleReceived);
        }
        self.commit(work);
        Ok(amount_out)
    }

    /// `SwapRouter.exactOutput`. Receives exactly `amount_out` of the last hop's output token
    /// and returns the input the trader must pay for the first hop. Fails with
    /// [`MathError::TooMuchRequested`] if that input is above `amount_in_maximum`. Pools are
    /// unchanged on any error.
    pub fn exact_output(
        &mut self,
        path: &[Hop],
        amount_out: U256,
        amount_in_maximum: U256,
    ) -> Result<U256> {
        let mut work = self.load(path)?;
        let amount_in = self.run_exact_output(&mut work, path, amount_out)?;
        if amount_in > amount_in_maximum {
            return Err(MathError::TooMuchRequested);
        }
        self.commit(work);
        Ok(amount_in)
    }

    /// Validates the path and takes a working copy of every pool it touches. No pool is
    /// modified here.
    fn load(&mut self, path: &[Hop]) -> Result<WorkingSet> {
        if path.is_empty() {
            return Err(MathError::InvalidInput);
        }
        let mut work = WorkingSet::new();
        for hop in path {
            if work.contains_key(&hop.pool_id) {
                continue;
            }
            let gauge_alive = self.store.gauge_alive(hop.pool_id);
            let pool = self
                .store
                .pool_mut(hop.pool_id)
                .ok_or(MathError::InvalidInput)?;
            work.insert(
                hop.pool_id,
                Working {
                    pool: pool.clone(),
                    gauge_alive,
                },
            );
        }
        Ok(work)
    }

    /// Writes the working copies back to the store. Called only after every check has passed.
    fn commit(&mut self, work: WorkingSet) {
        for (id, working) in work {
            if let Some(pool) = self.store.pool_mut(id) {
                *pool = working.pool;
            }
        }
    }

    /// Chains exact-input swaps front to back. Returns the last hop's output.
    fn run_exact_input(
        &self,
        work: &mut WorkingSet,
        path: &[Hop],
        amount_in: U256,
    ) -> Result<U256> {
        let mut amount = amount_in;
        for hop in path {
            let result = self.swap_hop(work, *hop, to_i256(amount)?)?;
            let (_, received) = hop_amounts(*hop, result);
            amount = non_negative(received)?;
        }
        Ok(amount)
    }

    /// Chains exact-output swaps back to front, as `SwapRouter` does. Each hop must deliver
    /// exactly the amount the next hop needs. Returns the first hop's required input.
    fn run_exact_output(
        &self,
        work: &mut WorkingSet,
        path: &[Hop],
        amount_out: U256,
    ) -> Result<U256> {
        let mut needed = amount_out;
        for hop in path.iter().rev() {
            let wanted = to_i256(needed)?;
            let result = self.swap_hop(work, *hop, -wanted)?;
            let (paid, received) = hop_amounts(*hop, result);
            // The price limit can stop a swap early. `SwapRouter` requires the full amount, so
            // a short fill is an error. Here that means the price bound was reached.
            if received != wanted {
                return Err(MathError::PriceOutOfBounds);
            }
            needed = non_negative(paid)?;
        }
        Ok(needed)
    }

    /// Runs one swap on the working copy of the hop's pool, with the fees for this trader.
    fn swap_hop(
        &self,
        work: &mut WorkingSet,
        hop: Hop,
        amount_specified: I256,
    ) -> Result<SwapResult> {
        let working = work.get_mut(&hop.pool_id).ok_or(MathError::InvalidInput)?;
        let fee = self
            .fees
            .swap_fee(hop.pool_id, working.pool.tick_spacing, &self.trader);
        let unstaked_fee = self.fees.unstaked_fee(hop.pool_id, working.gauge_alive);
        let sqrt_price_limit = if hop.zero_for_one {
            MIN_SQRT_RATIO + U256::from(1)
        } else {
            MAX_SQRT_RATIO - U256::from(1)
        };
        working.pool.swap(
            hop.zero_for_one,
            amount_specified,
            sqrt_price_limit,
            fee,
            unstaked_fee,
            self.time,
        )
    }
}

/// Quotes an exact-input route without changing `store`. Runs the route on a clone and
/// returns the output it would produce. Equal to the result of [`Router::exact_input`] on
/// the same state.
///
/// The whole store is cloned, since [`PoolStore::pool_mut`] needs `&mut`.
pub fn quote_exact_input<S: PoolStore + Clone>(
    store: &S,
    fees: &FeeConfig,
    trader: AccountId,
    time: u32,
    path: &[Hop],
    amount_in: U256,
) -> Result<U256> {
    let mut scratch = store.clone();
    Router {
        store: &mut scratch,
        fees,
        trader,
        time,
    }
    .exact_input(path, amount_in, U256::ZERO)
}

/// Quotes an exact-output route without changing `store`. Returns the input the trader would
/// have to pay. Equal to the result of [`Router::exact_output`] on the same state.
pub fn quote_exact_output<S: PoolStore + Clone>(
    store: &S,
    fees: &FeeConfig,
    trader: AccountId,
    time: u32,
    path: &[Hop],
    amount_out: U256,
) -> Result<U256> {
    let mut scratch = store.clone();
    Router {
        store: &mut scratch,
        fees,
        trader,
        time,
    }
    .exact_output(path, amount_out, U256::MAX)
}

/// Trader-side amounts for one hop, as `(paid, received)`. `paid` is what the trader gives
/// in the hop's input token and `received` is what it gets in the output token. Both are
/// positive for a normal fill.
///
/// `ClPool::swap` reports signed amounts from the pool's view (positive is owed to the pool).
/// This flips the output side, as `SwapRouter` does with `-(zeroForOne ? amount1 : amount0)`.
fn hop_amounts(hop: Hop, result: SwapResult) -> (I256, I256) {
    if hop.zero_for_one {
        (result.amount0, result.amount1.wrapping_neg())
    } else {
        (result.amount1, result.amount0.wrapping_neg())
    }
}

/// Converts an unsigned amount to the signed form `ClPool::swap` takes. Errors if it does not
/// fit in `I256`, as `SafeCast.toInt256` would revert.
fn to_i256(x: U256) -> Result<I256> {
    if x > I256::MAX.into_raw() {
        return Err(MathError::Overflow);
    }
    Ok(I256::from_raw(x))
}

/// Converts a trader-side signed amount to unsigned. Errors on a negative value, which a
/// valid swap never produces for the input or output side.
fn non_negative(x: I256) -> Result<U256> {
    if x.is_negative() {
        return Err(MathError::Underflow);
    }
    Ok(x.into_raw())
}
