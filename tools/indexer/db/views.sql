-- Read-friendly views for the frontend (PostgREST / supabase-js). Re-runnable: create or replace.
--
-- Amounts are NUMERIC(78,0). The public views return them as text: JSON numbers lose digits above 2^53.
-- Format with the token's decimals on the client. Read the tables directly only for small numbers.
-- USD prices are not stored yet (TODO: a price feed table); the views report token amounts only.

-- Human-readable pool list with its current state.
create or replace view pools_public as
select
  p.pool_id,
  p.token0,
  p.token1,
  t0.decimals as token0_decimals,
  t1.decimals as token1_decimals,
  p.tick_spacing,
  p.fee,
  p.created_at,
  s.sqrt_price_x96::text as sqrt_price_x96,
  s.tick,
  s.liquidity::text as liquidity,
  s.staked_liquidity::text as staked_liquidity,
  s.reserve0::text as reserve0,
  s.reserve1::text as reserve1,
  s.updated_height as state_height
from pools p
left join pool_state s on s.pool_id = p.pool_id
left join tokens t0 on t0.token_id = p.token0
left join tokens t1 on t1.token_id = p.token1;

-- Swap-level detail, newest first. route is null for single-pool swaps.
create or replace view swaps_public as
select
  s.id,
  s.block_ts,
  s.block_height,
  s.tx_hash,
  s.pool_id,
  s.route,
  s.token_in,
  s.token_out,
  s.exact,
  s.via,
  s.zero_for_one,
  s.amount_in::text as amount_in,
  s.amount_out::text as amount_out,
  s.amount0::text as amount0,
  s.amount1::text as amount1,
  s.trader_owner,
  s.trader_account,
  s.wallet
from swaps s;

-- Per pool and UTC day. Single-pool swaps only (multi-hop per-hop amounts are not stored).
-- volume0/volume1 are token amounts; fees are the pool fee applied to the input side.
create or replace view daily_pool_stats as
select
  s.pool_id,
  (date_trunc('day', s.block_ts at time zone 'utc'))::date as day,
  count(*)                                               as swaps,
  (coalesce(sum(abs(s.amount0)), 0))::text               as volume0,
  (coalesce(sum(abs(s.amount1)), 0))::text               as volume1,
  (coalesce(sum(case when s.zero_for_one then s.amount0 else 0 end), 0)
    * coalesce(max(p.fee), 0) / 1000000)::text           as fees0,
  (coalesce(sum(case when not s.zero_for_one then s.amount1 else 0 end), 0)
    * coalesce(max(p.fee), 0) / 1000000)::text           as fees1
from swaps s
join pools p on p.pool_id = s.pool_id
where s.pool_id is not null
group by s.pool_id, day;

-- Rolling 24 hours per pool, computed at query time (always fresh; index on swaps(pool_id, block_ts)).
create or replace view pool_volume_24h as
select
  s.pool_id,
  count(*)                              as swaps,
  (coalesce(sum(abs(s.amount0)), 0))::text as volume0,
  (coalesce(sum(abs(s.amount1)), 0))::text as volume1
from swaps s
where s.pool_id is not null
  and s.block_ts > now() - interval '24 hours'
group by s.pool_id;

-- Pool TVL in token amounts: the contract's own reserves (get_pool_reserves). USD: TODO (price feed).
create or replace view pool_tvl as
select
  s.pool_id,
  p.token0,
  p.token1,
  s.reserve0::text as reserve0,
  s.reserve1::text as reserve1,
  s.updated_height,
  s.updated_at
from pool_state s
join pools p on p.pool_id = s.pool_id;

-- One row of headline numbers for the dashboard.
create or replace view stats_overview as
select
  (select count(*) from pools)                                   as pools,
  (select count(*) from swaps)                                   as swaps,
  (select count(*) from swaps where block_ts > now() - interval '24 hours') as swaps_24h,
  (select count(*) from liquidity_events where kind = 'mint')    as mints,
  (select count(*) from deposits)                                as deposits,
  (select count(*) from withdrawals)                             as withdrawals,
  (select max(block_height) from calls)                          as last_call_height,
  (select last_height from blocks_cursor where id = 1)           as indexed_to_height;

-- Owner-level positions with the pool they belong to.
create or replace view positions_public as
select
  p.owner,
  p.account,
  p.wallet,
  p.pool_id,
  p.tick_lower,
  p.tick_upper,
  p.liquidity::text as liquidity,
  p.staked
from positions p
where p.liquidity > 0 or p.staked;

-- Gauge list with emission weight and the pool it belongs to.
create or replace view gauges_public as
select g.*, p.token0, p.token1
from gauges g
left join pools p on p.pool_id = g.pool_id;

-- ---------------------------------------------------------------------------
-- Row Level Security for Supabase (run once; the indexer connects as the table owner, which bypasses RLS).
--
-- The public schema is exposed through PostgREST with the `anon` and `authenticated` roles. Give them
-- read-only access to the public tables and views, and nothing else:
--
--   do $$
--   declare t text;
--   begin
--     foreach t in array array['blocks_cursor','settings','calls','chain_events','tokens','pools','pool_state',
--       'swaps','liquidity_events','positions','deposits','withdrawals','lock_events','locks','votes','bribes',
--       'claims','gauges','gauge_events','epochs','epoch_state','wallet_links','intents','one_step_transfers']
--     loop
--       execute format('alter table %I enable row level security', t);
--       execute format('drop policy if exists "public read" on %I', t);
--       execute format('create policy "public read" on %I for select to anon, authenticated using (true)', t);
--       execute format('revoke all on %I from anon, authenticated', t);
--       execute format('grant select on %I to anon, authenticated', t);
--     end loop;
--   end $$;
--
--   grant select on pools_public, swaps_public, daily_pool_stats, pool_volume_24h, pool_tvl,
--     stats_overview, positions_public, gauges_public to anon, authenticated;
--
-- Views run with the owner's rights, so they read the tables above; the grants above are what the browser
-- can use. Do NOT grant write access to anon or authenticated. Only the indexer (a server-side connection
-- string with the postgres / service role) writes. Never ship the service-role key to the browser.
