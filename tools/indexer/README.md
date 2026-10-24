# Wind Swap v4 indexer

Reads the activity of the Wind Swap v4 NEAR contract (`windswapv4-btb.testnet` on testnet) and stores it in
Postgres, so the website can list pools, swaps, positions, and charts from a database instead of probing NEAR RPC.
Target: Supabase (standard Postgres), or any Postgres 14+.

The contract does not emit structured events yet, so the indexer decodes **function calls**: for every
action receipt executed on the contract it records the method, JSON args, return value, success or failure,
logs and error. Typed tables (pools, swaps, liquidity, deposits, withdrawals, locks, votes, ...) are filled from
successful calls. When the contract starts emitting NEP-297 `EVENT_JSON:` logs, they are already parsed and
stored in `chain_events`; handlers can be added in `src/decode/events.ts` (`EVENT_HANDLERS`).

The indexer is read-only towards NEAR: it sends no transactions.

## Data sources

| Use | Source | Why |
|---|---|---|
| Backlog and live following (default, `BACKFILL_MODE=account`) | FastNEAR tx API: `POST /v0/account` (transactions of the contract, by height range), then `POST /v0/transactions` (receipt trees; every receipt has its height) | A poll costs one head read plus a few requests, and only transactions that touched the contract are fetched. |
| Head | NEAR RPC `block` at `final` finality, `https://rpc.testnet.fastnear.com` | Only final heights are read, so there are no reorgs to handle. The last `FINALITY_MARGIN` (10) blocks are left for the account index to catch up. |
| Cross-check and fallback (`BACKFILL_MODE=blocks`) | `https://testnet.neardata.xyz/v0/block/{height}` and `/v0/last_block/final` | Every block; each receipt execution carries its `tx_hash`. |
| View state | NEAR RPC `query` / `call_function` (final) | `get_pool`, `get_fee`, `get_pool_reserves`, `get_lock`, `get_gauges`, `get_epoch`. |

Why not blocks for live following: the free neardata tier answers about 2 requests per second before it starts
returning HTTP 429, and testnet produces about 1.7 blocks per second. Block-by-block following falls behind. The
account path reads the same receipts with a handful of requests per poll. Block mode retries with backoff and
is kept for cross-checks over small ranges.

The cursor (`blocks_cursor`) only moves forward. Each batch of receipts commits its rows and the cursor in one
transaction, and an error backs off and resumes from the cursor.

Testnet first block of the contract: **274356280** (account created by `testnet`, 2026-10-23 05:43 UTC).
The deploy (`DeployContract` + `new`) is at 274356295. Without `START_HEIGHT` the indexer finds the first transaction
with the FastNEAR account API on first run.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | required | Postgres connection string (Supabase: see below). |
| `DATABASE_SSL` | unset | `true` enables TLS without certificate verification (Supabase poolers). |
| `NEAR_NETWORK` | `testnet` | `testnet` or `mainnet`; selects the default endpoints and contract id. |
| `CONTRACT_ID` | `windswapv4-btb.testnet` | Contract to index. |
| `START_HEIGHT` | first contract tx | First height to index. Only used when the cursor is empty. |
| `STOP_HEIGHT` | final head | Last height for `backfill` (partial runs, cross-checks). |
| `BACKFILL_MODE` | `account` | `account` (FastNEAR account history, default) or `blocks` (every neardata block, for cross-checks over small ranges). |
| `FINALITY_MARGIN` | `10` | Account mode: blocks behind the final head that are left for the account index to catch up. |
| `CONCURRENCY` | `16` | Blocks fetched in parallel from neardata (blocks mode). |
| `POLL_MS` | `2000` | Wait between head polls once caught up. |
| `LIVE_WINDOW` | `100` | Batches within this many blocks of the head refresh view-backed state. |
| `NEARDATA_URL`, `FASTNEAR_TX_URL`, `NEAR_RPC_URL` | per network | Endpoint overrides. |

Put them in `.env` (git-ignored); the npm scripts load it with `--env-file-if-exists`.

## Commands

```bash
npm install
npm run build              # tsc -> dist/
npm test                   # unit tests (decoder on captured fixtures)
npm run test:integration:local   # starts a throwaway Postgres in .local-pg/, runs the Postgres tests, stops it
npm run migrate            # apply db/schema.sql and db/views.sql (idempotent)
npm run backfill           # index the backlog up to the final head (minus FINALITY_MARGIN), then exit
npm start                  # index the backlog, then follow the head (run this in production)
npm run status             # cursor and row counts
```

`scripts/capture-fixtures.mjs <tx-hash>...` refreshes the test fixtures from the live APIs.

## Running against Supabase

1. Create the schema: in the Supabase SQL editor paste `db/schema.sql`, then `db/views.sql`. Or run
   `DATABASE_URL=... npm run migrate` from your machine.
2. Set `DATABASE_URL` to the **session pooler** (port 5432) or direct connection string of the project, with
   `DATABASE_SSL=true` if needed. The indexer takes a session-level advisory lock so only one instance writes;
   the transaction pooler (port 6543) does not keep session locks, so do not use it for the indexer.
3. The indexer connects as the database owner (`postgres`). Owners bypass RLS, so the indexer can write while
   the browser cannot. Keep this string on the server only. Never put the service-role key or this connection
   string in the frontend.
4. Enable public read access with the RLS block at the end of `db/views.sql` (read-only grants for `anon` and
   `authenticated`, no writes).

## Hosting the indexer

It is one long-running Node process with one Postgres connection pool. Run exactly one instance per database.
- **VPS / systemd**: `npm ci && npm run build`, then a unit running `node dist/cli.js run` with
  `EnvironmentFile=/etc/windswap-indexer.env`, `Restart=always`.
- **Fly.io / Railway**: a Node 24 image, start command `node dist/cli.js run`, no HTTP port needed,
  one machine (no autoscaling), `DATABASE_URL` as a secret.

On a fresh database the first `run` performs the backfill (about 40 seconds for the testnet history so far), then
keeps following the head. Restarts resume from the cursor. Re-running any range is safe: every event table has a
unique key on `(receipt_id, action_index, seq)`.

## What the frontend should query

Use the views for lists and charts; amounts in the public views are **text** (NUMERIC values above 2^53 lose
digits as JSON numbers). Format amounts with the token's `decimals` (`tokens` table; `null` for NEP-141 tokens
until metadata is added).

| Need | Query |
|---|---|
| Pool list with price, liquidity, reserves | `pools_public` |
| Recent swaps (newest first) | `swaps_public?order=block_ts.desc&limit=50` |
| Swaps of one pool | `swaps_public?pool_id=eq.4&order=block_ts.desc` |
| Daily volume / fees chart | `daily_pool_stats?pool_id=eq.4&order=day` |
| 24h volume per pool | `pool_volume_24h` |
| Pool reserves (TVL in token amounts) | `pool_tvl` |
| Headline numbers | `stats_overview` |
| A user's positions | `positions_public?owner=eq.<64-hex owner id>` |
| A user's LP and stake history | `liquidity_events`, `positions` |
| Deposits / withdrawals of a user | `deposits`, `withdrawals` (filter `owner`) |
| veNFT locks, votes, bribes, gauges | `locks`, `votes`, `bribes`, `gauges_public`, `epoch_state` |
| Raw contract calls (audit, errors) | `calls` (`ok`, `error`, `decoded_as`) |

Owner ids are 64-hex strings. For a wallet, `wallet_links` gives its NEAR account; `account` and `wallet` columns
on the event tables hold the readable names when known.

## Schema

`db/schema.sql` (tables), `db/views.sql` (views and RLS notes).

| Table | Filled by |
|---|---|
| `blocks_cursor` | last indexed height |
| `calls` | every function call on the contract, success or failure |
| `pools`, `pool_state` | `create_pool`; state from `get_pool`, `get_fee`, `get_pool_reserves` |
| `swaps` | `swap`, `swap_exact_input`, `swap_exact_output`, `execute` with a swap action |
| `liquidity_events`, `positions` | `mint_position`, `burn_position`, `collect` (and the signed forms) |
| `deposits` | `ft_on_transfer` (credited), `report_evm_deposit` (credited, not duplicates) |
| `withdrawals` | `withdraw_evm` / signed `WithdrawEvm`, `on_withdrawal_signed`, `mark_withdrawal_broadcast` |
| `tokens` | `add_token`, `remove_token`, `register_evm_asset` |
| `lock_events`, `locks` | `create_lock`, `increase_*`, `withdraw_lock`, `merge`, `split`, permanence, `delegate`; `get_lock` |
| `votes`, `bribes`, `claims` | `vote`, `reset`, `poke`; `bribe`; `claim_fees`, `claim_bribes`, `claim_rebase`, `claim_gauge_rewards` |
| `gauges`, `gauge_events` | `create_gauge`, `stake_position`, `unstake_position`; `get_gauges` |
| `epochs`, `epoch_state` | `distribute` / `on_distribute_minted`; `get_epoch` |
| `wallet_links`, `settings` | `link_wallet`, `unlink_wallet`, `set_wind_token` |
| `chain_events` | NEP-297 `EVENT_JSON` logs (none yet) |

Views: `pools_public`, `swaps_public`, `daily_pool_stats`, `pool_volume_24h`, `pool_tvl`, `stats_overview`,
`positions_public`, `gauges_public`.

## How calls become rows

- Direct methods and `execute` share the same handlers. A signed action (`execute`) runs as the wallet's owner:
  `via = signed`, and the owner is the wallet's linked account or its own id. Direct calls use `via = direct`
  (`route` for multi-hop `swap_exact_input`).
- Owner ids are computed the way the contract does (`accounts.rs`): `sha256(account)` for NEAR accounts,
  `sha256("evm:0x…")` for unlinked wallets. `evm:0x70997970c51812dc3a010c7d01b50e0d17dc79c8` resolves to
  `590247d8…0ab3f7`, the owner `docs/testnet.md` reports.
- Amounts from return values are the contract's own numbers (for example a swap's signed pool deltas, the
  amounts a mint paid, the withdrawal id). Nothing is re-computed except `swaps.amount_in/amount_out` and token
  sides, which come from those returns.
- A failed call is a `calls` row with `ok = false` and the error, and no typed rows.

## Known gaps

- **No USD prices.** TVL and volume are token amounts. Add a price table and a view when prices are available.
- **Token symbols and decimals** are only known for EVM assets (`register_evm_asset`). NEP-141 symbols need
  `ft_metadata` lookups.
- **Multi-hop routes**: the contract returns only the final output, so per-hop amounts are not known. A route is
  one `swaps` row with `route` (pool ids), `pool_id` null, and `amount_in/out` for the whole route. Daily stats and
  24h volume count single-pool swaps only.
- **Fees** in `daily_pool_stats` use the pool's current fee on the input side. Custom per-account fees are not
  reflected.
- **Lock created by a WIND deposit** (`ft_on_transfer` with a `lock` message) is recorded with `token_id` null,
  because the contract does not return the new veNFT id to the caller.
- **Views are current, not historical**: `pool_state`, `locks`, `gauges`, `epoch_state` hold the latest values
  read at final head. They are refreshed after each live batch and once after a backlog run. Their `updated_height`
  says when they were read. Historical state needs archival RPC.
- **Account-history reads depend on the FastNEAR index.** Each batch reads transactions up to a final height that is
  `FINALITY_MARGIN` blocks behind the head, and re-reads 50 blocks before its start to catch receipts that execute
  after their transaction. If the index lags further than the margin, a receipt can be missed. Check with
  `STOP_HEIGHT` and `BACKFILL_MODE=blocks` over the affected range, which reads every block directly.
- `ft_msg` and `intent` values of `swaps.via` are reserved: the contract does not accept swap messages yet.
- `link_wallet` is decoded with the calling account as the linked account (the contract requires the account to call it).
- Calls to `on_evm_signed` and `request_evm_payout` are recorded, but payouts have no typed table.
- Only final heights are read; the indexer lags the head by the chain's finality plus `FINALITY_MARGIN`.
