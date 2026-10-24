-- Wind Swap v4 indexer schema (PostgreSQL 14+, Supabase compatible).
-- Idempotent: every statement uses IF NOT EXISTS, so the file can be re-run on each deploy.
--
-- Conventions
--   * Big integers (u128 / u256 amounts, sqrt prices, fee growth) are NUMERIC(78,0), stored as decimal text
--     by the indexer. Never convert them to float on the client.
--   * Owners are the contract's owner ids: 64 lower-case hex characters (sha256 of the NEAR account id, or of
--     the canonical wallet string for unlinked wallets). `account` and `wallet` are the readable names when known.
--   * Timestamps are block timestamps (timestamptz). Heights are NEAR block heights.
--   * Every event table is keyed by (receipt_id, action_index, seq), so re-indexing a block is a no-op.

create table if not exists blocks_cursor (
  id          smallint primary key default 1 check (id = 1),
  last_height bigint not null,
  updated_at  timestamptz not null default now()
);

create table if not exists settings (
  key        text primary key,
  value      text not null,
  updated_at timestamptz not null default now()
);

-- Every function call the indexer sees on the contract, successful or not. Typed tables hold effects only.
create table if not exists calls (
  receipt_id   text not null,
  action_index int  not null,
  tx_hash      text,
  block_height bigint not null,
  block_ts     timestamptz not null,
  method       text not null,
  decoded_as   text,
  predecessor  text not null,
  signer       text,
  args         jsonb,
  ok           boolean,
  return_value jsonb,
  error        text,
  logs         jsonb not null default '[]'::jsonb,
  deposit_yocto numeric(78,0) not null default 0,
  decode_error text,
  primary key (receipt_id, action_index)
);
create index if not exists calls_height on calls (block_height);
create index if not exists calls_tx on calls (tx_hash);
create index if not exists calls_method on calls (method, block_height);

-- NEP-297 events emitted by the contract (none yet; the decoder stores them as soon as they appear).
create table if not exists chain_events (
  receipt_id   text not null,
  log_index    int  not null,
  tx_hash      text,
  block_height bigint not null,
  block_ts     timestamptz not null,
  standard     text not null,
  version      text not null,
  event        text not null,
  data         jsonb,
  primary key (receipt_id, log_index)
);
create index if not exists chain_events_event on chain_events (standard, event, block_height);

-- Tokens known to the contract: NEP-141 accounts (add_token) and EVM assets (register_evm_asset).
create table if not exists tokens (
  token_id      text primary key,
  kind          text not null check (kind in ('nep141', 'evm')),
  symbol        text,
  decimals      int,
  chain_id      bigint,
  asset         text,
  listed        boolean not null default true,
  first_height  bigint,
  updated_height bigint
);
create unique index if not exists tokens_evm_asset on tokens (chain_id, asset) where kind = 'evm';

-- Pools. fee is in pips of the input amount (300 = 0.03%), read from the contract's get_fee view.
create table if not exists pools (
  pool_id          bigint primary key,
  token0           text not null,
  token1           text not null,
  tick_spacing     int  not null,
  fee              int,
  sqrt_price_x96_init numeric(78,0),
  created_at       timestamptz,
  created_tx       text,
  created_height   bigint
);

-- Current state of each pool, refreshed by the get_pool / get_pool_reserves views after activity on it.
create table if not exists pool_state (
  pool_id               bigint primary key references pools (pool_id),
  sqrt_price_x96        numeric(78,0),
  tick                  int,
  liquidity             numeric(78,0),
  staked_liquidity      numeric(78,0),
  fee_growth_global0_x128 numeric(78,0),
  fee_growth_global1_x128 numeric(78,0),
  reserve0              numeric(78,0),
  reserve1              numeric(78,0),
  updated_height        bigint,
  updated_at            timestamptz not null default now()
);

-- One row per swap call. Single-pool swaps set pool_id and the signed pool deltas (amount0/amount1).
-- Multi-hop routes set route (pool ids in order) and leave pool_id null: per-hop amounts are not returned
-- by the contract, so they are not invented here.
create table if not exists swaps (
  id            bigserial primary key,
  receipt_id    text not null,
  action_index  int  not null,
  tx_hash       text,
  block_height  bigint not null,
  block_ts      timestamptz not null,
  pool_id       bigint,
  route         bigint[],
  token_in      text,
  token_out     text,
  exact         text not null check (exact in ('input', 'output')),
  via           text not null check (via in ('direct', 'route', 'signed', 'ft_msg', 'intent')),
  zero_for_one  boolean,
  amount0       numeric(78,0),
  amount1       numeric(78,0),
  amount_in     numeric(78,0) not null,
  amount_out    numeric(78,0) not null,
  trader_owner  text not null,
  trader_account text,
  wallet        text,
  unique (receipt_id, action_index)
);
create index if not exists swaps_pool_ts on swaps (pool_id, block_ts);
create index if not exists swaps_ts on swaps (block_ts desc);
create index if not exists swaps_trader on swaps (trader_owner, block_ts desc);

-- Concentrated-liquidity mint / burn / collect. amount0/amount1 are the amounts the contract returned.
create table if not exists liquidity_events (
  id           bigserial primary key,
  receipt_id   text not null,
  action_index int  not null,
  seq          int  not null,
  kind         text not null check (kind in ('mint', 'burn', 'collect')),
  via          text not null,
  owner        text not null,
  account      text,
  wallet       text,
  pool_id      bigint not null,
  tick_lower   int not null,
  tick_upper   int not null,
  liquidity    numeric(78,0),
  amount0      numeric(78,0) not null,
  amount1      numeric(78,0) not null,
  tx_hash      text,
  block_height bigint not null,
  block_ts     timestamptz not null,
  unique (receipt_id, action_index, seq)
);
create index if not exists liquidity_events_owner on liquidity_events (owner, pool_id);
create index if not exists liquidity_events_pool on liquidity_events (pool_id, block_ts);

-- Live position ranges: liquidity is the running sum of mints minus burns (applied once per event row).
create table if not exists positions (
  owner          text   not null,
  pool_id        bigint not null,
  tick_lower     int    not null,
  tick_upper     int    not null,
  account        text,
  wallet         text,
  liquidity      numeric(78,0) not null default 0,
  staked         boolean not null default false,
  updated_height bigint,
  primary key (owner, pool_id, tick_lower, tick_upper)
);

-- Deposits credited to an owner: NEP-141 ft_on_transfer (kind ft) and EVM deposits reported by a relayer (kind evm).
create table if not exists deposits (
  id           bigserial primary key,
  receipt_id   text not null,
  action_index int  not null,
  seq          int  not null,
  kind         text not null check (kind in ('ft', 'evm')),
  token_id     text,
  chain_id     bigint,
  asset        text,
  amount       numeric(78,0) not null,
  owner        text not null,
  account      text,
  wallet       text,
  sender       text,
  tx_hash_evm  text,
  log_index    int,
  from_address text,
  tx_hash      text,
  block_height bigint not null,
  block_ts     timestamptz not null,
  unique (receipt_id, action_index, seq)
);
create unique index if not exists deposits_evm_log on deposits (chain_id, tx_hash_evm, log_index, asset) where kind = 'evm';
create index if not exists deposits_owner on deposits (owner, block_ts desc);

-- EVM withdrawals. id is the contract's withdrawal id. status: pending -> signed -> broadcast.
create table if not exists withdrawals (
  id                  bigint primary key,
  owner               text not null,
  account             text,
  wallet              text,
  token_id            text not null,
  amount              numeric(78,0) not null,
  to_chain_id         bigint,
  to_address          text not null,
  status              text not null default 'pending' check (status in ('pending', 'signed', 'broadcast')),
  nonce               bigint,
  evm_tx_hash         text,
  requested_tx_hash   text,
  requested_height    bigint,
  requested_ts        timestamptz,
  updated_height      bigint,
  updated_ts          timestamptz,
  via                 text
);
create index if not exists withdrawals_owner on withdrawals (owner, requested_ts desc);

-- Every ve(3,3) lock action (create, increase, withdraw, merge, split, permanence, delegate).
create table if not exists lock_events (
  id           bigserial primary key,
  receipt_id   text not null,
  action_index int  not null,
  seq          int  not null,
  kind         text not null,
  via          text not null,
  owner        text not null,
  account      text,
  wallet       text,
  token_id     bigint,
  amount       numeric(78,0),
  duration     bigint,
  permanent    boolean,
  extra        jsonb not null default '{}'::jsonb,
  tx_hash      text,
  block_height bigint not null,
  block_ts     timestamptz not null,
  unique (receipt_id, action_index, seq)
);
create index if not exists lock_events_token on lock_events (token_id, block_height);

-- Current veNFT state, refreshed from get_lock after activity on the token.
create table if not exists locks (
  token_id       bigint primary key,
  owner          text,
  amount         numeric(78,0),
  end_ts         bigint,
  permanent      boolean,
  voted          boolean,
  updated_height bigint
);

-- Gauge votes (vote / reset / poke) per veNFT and pool.
create table if not exists votes (
  id           bigserial primary key,
  receipt_id   text not null,
  action_index int  not null,
  seq          int  not null,
  kind         text not null check (kind in ('vote', 'reset', 'poke')),
  via          text not null,
  owner        text not null,
  account      text,
  wallet       text,
  token_id     bigint not null,
  pool_id      bigint,
  weight       numeric(78,0),
  tx_hash      text,
  block_height bigint not null,
  block_ts     timestamptz not null,
  unique (receipt_id, action_index, seq)
);
create index if not exists votes_token on votes (token_id, block_height);

-- Bribes deposited on a pool's gauge.
create table if not exists bribes (
  id           bigserial primary key,
  receipt_id   text not null,
  action_index int  not null,
  seq          int  not null,
  via          text not null,
  owner        text not null,
  account      text,
  wallet       text,
  pool_id      bigint not null,
  token        text not null,
  amount       numeric(78,0) not null,
  tx_hash      text,
  block_height bigint not null,
  block_ts     timestamptz not null,
  unique (receipt_id, action_index, seq)
);

-- Claims: fees and bribes (per veNFT), rebase (per veNFT) and staked-LP gauge rewards (per position).
create table if not exists claims (
  id           bigserial primary key,
  receipt_id   text not null,
  action_index int  not null,
  seq          int  not null,
  kind         text not null check (kind in ('fees', 'bribes', 'rebase', 'gauge_rewards')),
  via          text not null,
  owner        text not null,
  account      text,
  wallet       text,
  token_id     bigint,
  pool_id      bigint,
  token        text,
  amount       numeric(78,0) not null,
  extra        jsonb not null default '{}'::jsonb,
  tx_hash      text,
  block_height bigint not null,
  block_ts     timestamptz not null,
  unique (receipt_id, action_index, seq)
);

-- Gauges (one per pool), refreshed from get_gauges.
create table if not exists gauges (
  pool_id        bigint primary key,
  gauge_id       bigint,
  alive          boolean,
  weight         numeric(78,0),
  reward_balance numeric(78,0),
  created_height bigint,
  created_ts     timestamptz,
  updated_height bigint
);

-- Gauge events: create, stake (a CL position joins its gauge), unstake.
create table if not exists gauge_events (
  id           bigserial primary key,
  receipt_id   text not null,
  action_index int  not null,
  seq          int  not null,
  kind         text not null check (kind in ('create', 'stake', 'unstake')),
  via          text not null,
  owner        text,
  account      text,
  wallet       text,
  pool_id      bigint not null,
  gauge_id     bigint,
  tick_lower   int,
  tick_upper   int,
  tx_hash      text,
  block_height bigint not null,
  block_ts     timestamptz not null,
  unique (receipt_id, action_index, seq)
);

-- Weekly emissions minted by the epoch flip (distribute / on_distribute_minted).
create table if not exists epochs (
  receipt_id   text primary key,
  amount       numeric(78,0) not null,
  block_height bigint not null,
  block_ts     timestamptz not null
);

-- Current epoch state, refreshed from get_epoch.
create table if not exists epoch_state (
  id                  smallint primary key default 1 check (id = 1),
  active_period       bigint,
  epoch_count         bigint,
  weekly              numeric(78,0),
  next_epoch_start    bigint,
  wind_minted         numeric(78,0),
  distribution_pending boolean,
  updated_height      bigint
);

-- Wallet links: a wallet (evm:0x.., ed25519:.., near:..) belongs to a NEAR account.
create table if not exists wallet_links (
  wallet        text primary key,
  account       text not null,
  owner         text not null,
  linked_height bigint,
  linked_ts     timestamptz
);
create index if not exists wallet_links_account on wallet_links (account);

-- Intents (submit_intent / execute_intent / cancel_intent). status is the contract's own status text.
create table if not exists intents (
  id              bigint primary key,
  owner           text,
  account         text,
  wallet          text,
  kind            text check (kind in ('swap', 'add_liquidity')),
  token_in        text,
  amount_in       numeric(78,0),
  spec            jsonb,
  nonce           bigint,
  expires         bigint,
  status          text not null default 'open',
  result          jsonb,
  reason          text,
  submitted_tx_hash text,
  submitted_height bigint,
  submitted_ts    timestamptz,
  updated_height  bigint,
  updated_ts      timestamptz
);
create index if not exists intents_owner on intents (owner, submitted_ts desc);

-- Successful one-step NEP-141 transfers (ft_on_transfer with a swap or add_liquidity message).
create table if not exists one_step_transfers (
  receipt_id   text primary key,
  kind         text not null check (kind in ('swap', 'add_liquidity')),
  owner        text not null,
  account      text,
  wallet       text,
  sender       text not null,
  token_id     text not null,
  amount       numeric(78,0) not null,
  spec         jsonb not null,
  tx_hash      text,
  block_height bigint not null,
  block_ts     timestamptz not null
);
