/**
 * View-backed state: pool state, locks, gauges and epoch come from the contract's view methods, read at
 * final finality. They are fetched before a database transaction and written inside it.
 */
import type pg from "pg";
import type { NearRpc } from "../near/fastnear.js";

interface PoolView {
  pool_id: number;
  token0: string;
  token1: string;
  tick_spacing: number;
  slot0: { sqrt_price_x96: string; tick: number };
  liquidity: string;
  staked_liquidity: string;
  fee_growth_global0_x128: string;
  fee_growth_global1_x128: string;
}

interface LockView {
  token_id: number;
  owner: string;
  amount: string;
  end: number;
  permanent: boolean;
  voted: boolean;
}

interface GaugeView {
  pool_id: number;
  gauge_id: number;
  alive: boolean;
  weight: string;
  reward_balance: string;
}

interface EpochView {
  active_period: number;
  epoch_count: number;
  weekly: string;
  next_epoch_start: number;
  wind_minted: string;
  distribution_pending: boolean;
}

/** Entities whose view-backed state a batch touched. */
export interface Touched {
  pools: Set<number>;
  locks: Set<number>;
  gauges: Set<number>;
  epoch: boolean;
}

export interface ViewSnapshot {
  pools: Map<number, { view: PoolView; fee: number | null; reserves: [string, string] | null }>;
  locks: Map<number, LockView | null>;
  gauges: GaugeView[] | null;
  epoch: EpochView | null;
}

/** Pool ids, lock ids and gauges to refresh: explicit sets from a batch, or everything when `all` is set. */
export async function fetchViews(
  rpc: NearRpc,
  contractId: string,
  touched: Touched,
  all: { pools: number[]; locks: number[] } | null,
): Promise<ViewSnapshot> {
  const pools = all ? all.pools : [...touched.pools];
  const locks = all ? all.locks : [...touched.locks];
  const snap: ViewSnapshot = { pools: new Map(), locks: new Map(), gauges: null, epoch: null };
  for (const poolId of pools) {
    const view = await rpc.view<PoolView>(contractId, "get_pool", { pool_id: poolId });
    const fee = await rpc.view<number>(contractId, "get_fee", { pool_id: poolId, account_id: contractId }).catch(() => null);
    const reserves = await rpc
      .view<Array<[string, string]>>(contractId, "get_pool_reserves", { pool_id: poolId })
      .then((r) => (r.length === 2 ? ([r[0]?.[1] ?? "0", r[1]?.[1] ?? "0"] as [string, string]) : null))
      .catch(() => null);
    snap.pools.set(poolId, { view, fee, reserves });
  }
  for (const tokenId of locks) {
    snap.locks.set(tokenId, await rpc.view<LockView | null>(contractId, "get_lock", { token_id: tokenId }));
  }
  if (all || touched.gauges.size > 0) {
    snap.gauges = await rpc.view<GaugeView[]>(contractId, "get_gauges", {});
  }
  if (all || touched.epoch) {
    snap.epoch = await rpc.view<EpochView>(contractId, "get_epoch", {});
  }
  return snap;
}

/** Writes a snapshot. `height` is the height the batch was indexed to (views are read at final head). */
export async function writeViews(client: pg.PoolClient | pg.Client, snap: ViewSnapshot, height: number): Promise<void> {
  for (const [poolId, p] of snap.pools) {
    await client.query(
      `insert into pool_state (pool_id, sqrt_price_x96, tick, liquidity, staked_liquidity, fee_growth_global0_x128,
          fee_growth_global1_x128, reserve0, reserve1, updated_height, updated_at)
       values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,now())
       on conflict (pool_id) do update set sqrt_price_x96 = excluded.sqrt_price_x96, tick = excluded.tick,
         liquidity = excluded.liquidity, staked_liquidity = excluded.staked_liquidity,
         fee_growth_global0_x128 = excluded.fee_growth_global0_x128,
         fee_growth_global1_x128 = excluded.fee_growth_global1_x128, reserve0 = excluded.reserve0,
         reserve1 = excluded.reserve1, updated_height = excluded.updated_height, updated_at = now()`,
      [poolId, p.view.slot0.sqrt_price_x96, p.view.slot0.tick, p.view.liquidity, p.view.staked_liquidity,
        p.view.fee_growth_global0_x128, p.view.fee_growth_global1_x128, p.reserves?.[0] ?? null,
        p.reserves?.[1] ?? null, height],
    );
    if (p.fee !== null) {
      await client.query("update pools set fee = $2 where pool_id = $1", [poolId, p.fee]);
    }
  }
  for (const [tokenId, l] of snap.locks) {
    if (l === null) continue;
    await client.query(
      `insert into locks (token_id, owner, amount, end_ts, permanent, voted, updated_height)
       values ($1,$2,$3,$4,$5,$6,$7)
       on conflict (token_id) do update set owner = excluded.owner, amount = excluded.amount, end_ts = excluded.end_ts,
         permanent = excluded.permanent, voted = excluded.voted, updated_height = excluded.updated_height`,
      [tokenId, l.owner, l.amount, l.end, l.permanent, l.voted, height],
    );
  }
  for (const g of snap.gauges ?? []) {
    await client.query(
      `insert into gauges (pool_id, gauge_id, alive, weight, reward_balance, updated_height)
       values ($1,$2,$3,$4,$5,$6)
       on conflict (pool_id) do update set gauge_id = excluded.gauge_id, alive = excluded.alive,
         weight = excluded.weight, reward_balance = excluded.reward_balance, updated_height = excluded.updated_height`,
      [g.pool_id, g.gauge_id, g.alive, g.weight, g.reward_balance, height],
    );
  }
  if (snap.epoch) {
    const e = snap.epoch;
    await client.query(
      `insert into epoch_state (id, active_period, epoch_count, weekly, next_epoch_start, wind_minted,
          distribution_pending, updated_height)
       values (1,$1,$2,$3,$4,$5,$6,$7)
       on conflict (id) do update set active_period = excluded.active_period, epoch_count = excluded.epoch_count,
         weekly = excluded.weekly, next_epoch_start = excluded.next_epoch_start, wind_minted = excluded.wind_minted,
         distribution_pending = excluded.distribution_pending, updated_height = excluded.updated_height`,
      [e.active_period, e.epoch_count, e.weekly, e.next_epoch_start, e.wind_minted, e.distribution_pending, height],
    );
  }
}
