# Wind Swap v4 relayer

Off-chain service for the Wind Swap v4 NEAR contract. It does two things:

1. **Deposit watcher.** Scans an EVM chain for deposits into the contract's vault address and
   reports each one to the NEAR contract with `report_evm_deposit`.
2. **Withdrawal pipeline.** Calls `process_withdrawal` for pending withdrawals, then broadcasts the
   MPC-signed raw transactions in nonce order and calls `mark_withdrawal_broadcast`.

The relayer is trusted for deposits in this version. See [Security notes](#security-notes).

## Setup

Requires Node 24+.

```bash
cd tools/relayer
npm install
npm run build
npm test
cp config/relayer.example.json config/relayer.json   # then edit (gitignored)
```

Credentials: the relayer account's NEAR key lives in the legacy keychain format,
`~/.near-credentials/testnet/<account>.json`, with `account_id`, `public_key` and `private_key`.
The file's `account_id` must match `near.relayerAccountId`. Only `run` and `once` without
`--dry-run` read the key.

## Commands

| Command | What it does |
| --- | --- |
| `npm run start` | Loop forever: deposits, then withdrawals, every `pollIntervalMs`. `SIGINT` stops after the current batch. |
| `npm run once` | One pass, then exit. Exit code 1 if any stage reported an error. |
| `npm run once -- --dry-run` | No NEAR or EVM writes, no state writes. Prints what it would report, process and broadcast. |
| `npm run serve` | HTTP relay only. `POST /v1/execute` submits a wallet-signed action to `execute` on NEAR, and `POST /v1/intent` submits a signed swap or add-liquidity intent to `submit_intent`, paying the gas from the relayer account. Listens on `relay.host`:`relay.port` (default `127.0.0.1:8787`). |
| `npm run start -- --http` | Watcher loop and the HTTP relay in one process. |

Flags (all optional):

- `--config <path>`: default `config/relayer.json`.
- `--from-block <n>` / `--to-block <n>`: override the scan range for this run. Works with `--dry-run`.
- `--http`: with `run`, also serve the HTTP relay in the same process. Not allowed with `--dry-run` (the relay signs real transactions).

Example, read-only scan of the testnet vault:

```bash
npm run once -- --config config/relayer.example.json --dry-run \
  --from-block 49070000 --to-block 49114828
```

## Testnet (windswapv4-btb.testnet, Base Sepolia)

`config/testnet.json` holds the live testnet values. It has no secrets: the key is read from
`credentialsPath` only when running without `--dry-run`.

- NEAR: `https://rpc.testnet.near.org`, contract `windswapv4-btb.testnet`, relayer account
  `windswapv4-relayer.testnet` (registered with `add_relayer`).
- EVM: Base Sepolia (chain 84532), `https://sepolia.base.org`, vault
  `0xaa817ca355475d22d5a410f401dc877ec401259e`, `confirmations` 3, `startBlock` 49116400.
- Assets come from the contract (`get_evm_assets`). The registry has one entry:
  `chain_id 84532`, `asset "native"`, `token_id "base-sepolia:eth"`, 18 decimals,
  `per_deposit_cap` 0.05 ETH. `fallbackAssets` is empty, so a failing `get_evm_assets` stops the
  deposit stage.
- Live views checked read-only: `get_evm_assets` and `get_evm_chain` return the fields the relayer
  parses. `get_pending_withdrawals` and `get_signed_withdrawals` were both empty.
  The `raw_tx` format is still unverified, since no signed withdrawal existed yet.

Dry-run scan of blocks 49116400 to 49117920 (read-only):

```bash
npm run once -- --config config/testnet.json --dry-run --from-block 49116400 --to-block 49117920
```

It finds two native deposits from `0x0d53794e20e0e4f96e07bcd8487ce557c6d9a4ac`:

- `0xaffaed4cd7f9ba2afb44e615ce3554ceb1102fb20a43fb3126143a070228e665`, block 49116593, 0.004 ETH
- `0xefa045cc9e47a02d76e25c40be0ca572ef467407cd05c4330356f0c3d4ca1a76`, block 49117347, 0.003 ETH

Live run (sends `report_evm_deposit`; both should return `false` if already reported):

```bash
npm run once -- --config config/testnet.json --from-block 49116400 --to-block <latest-3>
```

Use `--from-block` only for the first run. After that the cursor in `state/base-sepolia.json`
takes over. Delete the state file to rescan.

## Configuration

Config is JSON (see `config/relayer.example.json`). Env overrides for the NEAR section:
`RELAYER_NEAR_RPC`, `RELAYER_CONTRACT_ID`, `RELAYER_ACCOUNT_ID`, `RELAYER_CREDENTIALS`.

| Field | Meaning |
| --- | --- |
| `near.rpcUrl` | NEAR JSON-RPC endpoint. |
| `near.contractId` | Wind Swap v4 contract, e.g. `windswapv4-btb.testnet`. |
| `near.relayerAccountId` | Account that signs change calls. Must be registered with `add_relayer`. |
| `near.credentialsPath` | Keychain JSON for that account. `~` is expanded. |
| `near.gasTgas` | Gas per change call in TGas: `report` 50, `process` 200, `mark` 30, `execute` 300, `submitIntent` 300, `cancelIntent` 300 by default. `process_withdrawal` needs at least 150. |
| `evm[]` | One entry per chain: `name` (state file name), `chainId`, `rpcUrl`, `vault`, `confirmations`, `startBlock`, `scanBatchSize`, `requestsPerSecond`, `blockConcurrency`. |
| `evm[].maxFeePerGasCapFallback` | Dry-run only. Used if the contract view `get_evm_chain` is unavailable. |
| `pollIntervalMs` | Sleep between passes in `run` mode. |
| `stateDir` | Directory for `<name>.json` cursor files. Default `state`. |
| `withdrawalLimit` | Page size for the withdrawal views. |
| `relay` | Optional HTTP relay section. See **HTTP relay** below. Every key has a default. |
| `fallbackAssets` | Dry-run only. Used if `get_evm_assets` is unavailable. In live mode a failing `get_evm_assets` stops the deposit stage, so nothing is reported against a stale registry. |

State file `state/<name>.json`: `nextBlock` (first block not yet fully processed) and `quarantined`
(deposits the contract rejected). It is written atomically after every scanned batch.

## How it works

### Deposits

For each chain, from the saved cursor (or `startBlock`) up to `head - confirmations`, in batches
of `scanBatchSize` blocks:

- **Native:** every block is fetched with full transactions. A transaction counts when `to == vault`,
  `value > 0`, and its receipt status is `success`. It is reported with `asset: "native"` and `log_index: 0`.
  Only done when `native` is registered for the chain.
- **ERC-20:** one `eth_getLogs` per batch on `Transfer(address,address,uint256)` with `to == vault`
  (no address filter). Only logs with exactly three topics (ERC-20 shape) are decoded. Registered tokens
  are reported. Unregistered tokens are logged and ignored.

Each batch's deposits are reported in block order. The cursor moves only after the whole batch
is reported. Then:

- `true` from the contract: reported. The watcher then reads the sender's owner
  (`resolve_wallet("evm:<from>")`) and the first page of its intents (`get_intents_of_owner`, 50 rows)
  and logs `intent status` for each intent whose status changed since this process last saw it. The
  contract executes an open intent inside `report_evm_deposit`, so the log shows the result of that
  call. A failed read is logged as a warning and does not change the deposit outcome.
- `false`: already reported, counted as a duplicate. This is what makes re-runs safe.
- Contract panic (for example, over the per-deposit cap): the deposit is written to `quarantined` in
  the state file and the cursor still advances. It is not retried automatically. An operator must
  look at it.
- RPC or network error: the pass stops with the cursor where it was, and the batch is retried next time.

### Withdrawals

Per pass:

1. `get_pending_withdrawals` is processed in id order, one at a time per chain. Fees:
   `maxFee = min(2 * baseFee + priority, cap)`, `maxPriority = min(priority, maxFee)`,
   where `cap` comes from `get_evm_chain.max_fee_per_gas_cap`. If the cap is below the base fee,
   the chain is skipped with an error. The first failure stops that chain, so later withdrawals
   never jump ahead. A contract rejection for a withdrawal that is no longer `Pending` is skipped.
2. `get_signed_withdrawals` is grouped by chain and sorted by nonce. Let `pending` be
   `eth_getTransactionCount(vault, "pending")`:
   - nonce `< pending`: stale. If `keccak256(raw_tx)` is known to the node, the withdrawal is marked
     broadcast with that hash. Otherwise it is logged as an error for manual review.
   - nonce `== pending`, then `pending + 1`, and so on, with no gap: broadcast with
     `eth_sendRawTransaction`, then `mark_withdrawal_broadcast`.
   - nonce after a gap: waits.
   - "already known" on send: treated as broadcast, marked with the known hash. "nonce too low" on
     send: marked only if the hash is known to the node, otherwise the chain pass stops.

   A failed `mark_withdrawal_broadcast` is not fatal. The next pass finds the transaction by hash
   and marks it then.

Withdrawals whose `to_chain_id` is not configured are logged and skipped.

### Reorgs

Reorg safety comes only from `confirmations`. Blocks are read only up to `head - confirmations`.
A reorg deeper than that could leave a reported deposit that no longer exists on chain. Contract
caps limit the damage. Choose `confirmations` for the chain (3 on Base Sepolia in the example).

Known limitations:

- Assets are read at the start of each pass. A deposit in an asset that was not registered when its
  block was scanned is ignored, and the cursor moves past it. Registering the asset later does not
  re-scan old blocks. To pick those up, lower `nextBlock` in the state file (or pass `--from-block`)
  and run again. The contract's idempotency makes this safe.
- Deposits made through internal transactions (contract-to-vault ETH transfers) are not detected.
  Only top-level transactions are.
- A native deposit and an ERC-20 deposit in the same transaction may share `(tx_hash, log_index)`
  if the ERC-20 log has index 0 in its block. The contract should key on the asset as well.
- `get_signed_withdrawals` and `get_pending_withdrawals` are read from `from_id = 0` each pass.
  This assumes the contract drops entries once they are processed or marked broadcast. Entries that
  never resolve (a stale nonce no one can find) sit at the front of the page and must be cleared by
  the operator.
- `eth_getTransactionCount(vault, "pending")` also counts any other pending transaction from the
  vault address, so the vault must only send through the relayer.

## Owner setup

1. Deploy the contract and set the owner.
2. Register the relayer account with the owner-only call `add_relayer` (argument shape per the
   contract ABI). Use the same account as `near.relayerAccountId`.
3. Register each asset per chain (owner-only, contract ABI). Then `get_evm_assets` returns it. The
   relayer only reads this view. Set `per_deposit_cap` and `daily_withdraw_cap` there.
4. Check with `npm run once -- --dry-run` that the registry and the vault scan look right.
5. Start the relayer with `npm run start`, or run `npm run once` from cron.

## HTTP relay

Wallet users (EVM or Ed25519) sign an action off-chain. The relay checks the signature and submits
`execute` for them, so they need no NEAR account or NEAR balance.

Endpoints (all JSON; the frontend calls these from `NEXT_PUBLIC_V4_RELAY_URL`):

| Route | Request | Response |
| --- | --- | --- |
| `POST /v1/execute` | `{"contractId":"windswapv4-btb.testnet","method":"execute","args":{"wallet","action","nonce","expires","signature"}}` | `200 {"txHash","result"}`; `result` is the contract's `ActionResult`. Errors: `4xx/5xx {"error":"..."}`. |
| `POST /v1/intent` | `{"wallet","intent","nonce","expires","signature"}` (or `{"contractId","method":"submit_intent","args":{...}}`) | `200 {"txHash","intentId"}`; `intentId` is the `u64` that `submit_intent` returns. |
| `POST /v1/intent/cancel` | `{"wallet","id","nonce","expires","signature"}` (or `{"contractId","method":"cancel_intent","args":{...}}`) | `200 {"txHash","result"}`. |
| `GET /v1/intent/:id` | | The `get_intent` view record as returned by the contract. `404 {"error":"intent not found"}` when absent. No gas. Rate-limited per IP like `nonce-hint`. |
| `GET /v1/health` | | `{"ok":true,"contractId","relayerAccountId"}` |
| `GET /v1/nonce-hint?wallet=<optional>` | | `{"nonce"}`: a fresh suggestion. Any value unused for that wallet works. |

The bare `args` object (without the envelope) is also accepted on every POST route.

Intent fields:

- `intent`: externally tagged, one of two variants. **Keys must be in exactly the order below**
  (checked in `src/relay/schema.ts`, `INTENT_FIELDS`). Amounts are decimal strings.
  - `{"Swap":{"token_in","amount_in","path":[{"pool_id","token_in"}],"min_amount_out","to"}}`
  - `{"AddLiquidity":{"token_in","amount_in","pool_id","tick_lower","tick_upper","min_liquidity"}}`
  - `to` is `"internal"`, `"near:<account>"` (lowercase NEAR account id, 2 to 64 characters), or
    `"evm:0x<40 lowercase hex>"`. Anything else is a 400.
- Signed message for `POST /v1/intent` (EIP-191 `personal_sign` for `evm:0x...`, raw Ed25519 bytes for `ed25519:...`):
  `Wind Swap v4 intent on <contract> by <wallet> nonce <n> expires <t>: <JSON.stringify(intent)>`
- Signed message for `POST /v1/intent/cancel`:
  `Wind Swap v4 cancel intent <id> on <contract> by <wallet> nonce <n> expires <t>`
- Nonces are shared with `execute` per wallet. An intent and an action with the same nonce conflict (409).
- The same checks as `/v1/execute` apply: expiry (at most `relay.maxExpirySeconds` ahead), canonical wallet,
  signature over the message, rate limits, idempotency (an identical retry returns the stored result),
  and the daily gas budget (`near.gasTgas.submitIntent` / `cancelIntent` TGas each). `allowedActions`
  does not apply to intents.

Errors on `/v1/intent` and `/v1/intent/cancel` follow the same mapping as `/v1/execute`: 400 for
validation and contract panics, 409 for a reused nonce with a different body, 429, 503 for the gas budget,
502 for a NEAR RPC failure (retrying with the same nonce is safe).

Request fields, matching the contract's `execute`:

- `wallet`: canonical, `evm:0x` plus 40 lowercase hex, or `ed25519:` plus base58 of a 32-byte key.
- `action`: one `SignedAction` variant, externally tagged, e.g. `{"Vote":{"token_id":1,"pools":[4,1],"weights":["70","30"]}}`.
- `nonce`: a JSON number (safe integer), unique per wallet.
- `expires`: unix seconds, in the future and at most `relay.maxExpirySeconds` ahead.
- `signature`: EVM `personal_sign` (65 bytes, hex) or Ed25519 over the message bytes (64 bytes, hex or base58).

The signed message is exactly
`Wind Swap v4 action on <contract> by <wallet> nonce <n> expires <t>: <JSON.stringify(action)>`.
**Clients must send the action's fields in the Rust declaration order** (listed in
`src/relay/schema.ts`, from `crates/near-contract/src/lib.rs`). The relay rejects any other order,
because the contract re-serialises the parsed action and the signature covers that byte string.
Amounts are decimal strings (U128/U256/I256), not JSON numbers.

Checks run in this order, and nothing reaches NEAR until all of them pass (intents follow the same order,
with `checkIntent` in place of the action check):

1. Per-IP rate limit (429 with `Retry-After`).
2. Body size (413), JSON, envelope and field shape, and the contract id and method.
3. `expires`: not in the past, not more than `maxExpirySeconds` ahead (400).
4. Action schema: variant in `allowedActions` (403 otherwise), exact fields in order, types and ranges.
5. Wallet canonical form, and the signature over the recomputed message (400).
6. Idempotency: the same (wallet, nonce) with the same body returns the stored result. A different body is 409.
7. Per-wallet rate limit (429), then the daily gas budget (503).

Submissions for one wallet are queued one at a time. Each `execute` call runs with 300 TGas and no
deposit (`near.gasTgas.execute`). A contract panic returns 400 with the panic message. A NEAR RPC
failure returns 502. A 502 may or may not have executed, but retrying with the same nonce is safe,
because the contract refuses a nonce that was already used.

Relay config keys (`relay` in the config file; env overrides in brackets):

| Key | Default | Meaning |
| --- | --- | --- |
| `host` | `127.0.0.1` | Bind address [`RELAYER_HTTP_HOST`]. |
| `port` | `8787` | Port [`RELAYER_HTTP_PORT`]. |
| `corsOrigins` | `[]` | Exact origins allowed to call from a browser, e.g. `["https://your-frontend.example"]` [`RELAYER_CORS_ORIGINS`, comma-separated]. Empty means no CORS headers. |
| `trustProxy` | `false` | Read the client IP from the first `X-Forwarded-For` entry. Enable only behind a proxy you control. |
| `maxBodyBytes` | `16384` | Largest accepted request body. |
| `maxExpirySeconds` | `86400` | Largest `expires` window. |
| `allowedActions` | `"*"` | `"*"` or an array of variant names, e.g. `["Vote","Poke","Reset"]`. |
| `rateLimit.perIp` | `{capacity: 20, refillPerSecond: 0.5}` | Token bucket per client IP. |
| `rateLimit.perWallet` | `{capacity: 5, refillPerSecond: 0.05}` | Token bucket per wallet, charged only after the signature verifies. |
| `dailyGasBudgetTgas` | `30000` | Gas the relayer may reserve per UTC day (100 submissions at 300 TGas). Reservations are not refunded. |
| `idempotencyTtlSeconds` | `86400` | How long a successful (wallet, nonce) result is replayed. |

The frontend points at the relay with `NEXT_PUBLIC_V4_RELAY_URL=https://<host>/v1/execute`.
Set `relay.corsOrigins` to the frontend origin.

## Security notes

- **Trust.** In this version the relayer decides which deposits happened. A compromised relayer
  key or RPC can report fake deposits. Contract caps (`per_deposit_cap`, `daily_withdraw_cap`) and
  the relayer allowlist bound the damage. Plan to move this to a light client or an
  intents/bridge proof.
- **Key.** The relayer key can only call the contract's relayer methods. Keep it out of the repo.
  `credentialsPath` points outside the repo by default.
- **RPC.** Public RPC endpoints can lie or rate-limit. Two RPCs compared on
  deposits would catch most of that. Rate limiting is per process.
- **Dry-run is read-only.** It makes no NEAR change calls, sends no EVM transactions, and writes no state.
- **Quarantine.** Rejected deposits need a human decision. Nothing reports them automatically.
- **Relay pays gas, cannot forge actions.** The relayer account signs only the transaction. It cannot
  produce a wallet signature, so it can only submit actions users signed. The contract checks the
  signature, the expiry and the nonce itself, so a compromised relay can delay or drop actions but
  not alter them. It can still spend the gas budget, so the limits below matter.
- **Gas budget.** The per-IP and per-wallet limits and `dailyGasBudgetTgas` cap what a caller can
  cost the relayer. Local checks reject bad signatures, malformed bodies and expired actions before
  NEAR is called. Schema checks cover types, not business rules: a request that passes but
  panics on chain (for example, an unknown pool) still spends gas.
- **Rate limits are per process.** Behind several instances, put the limit in the proxy or a shared store.
- **Serve in production behind TLS.** The relay speaks plain HTTP. Put it behind a reverse proxy
  that terminates TLS, and set `trustProxy` only for that proxy.

## Layout

```
src/config.ts      config parsing and env overrides
src/deposits.ts    block/log decoding, cursor range planning, scanRange
src/watcher.ts     deposit pass: report, quarantine, cursor
src/withdrawals.ts process pending, broadcast signed (nonce ordering)
src/broadcast.ts   pure nonce planner
src/fees.ts        EIP-1559 fee policy with cap
src/state.ts       cursor/quarantine persistence (atomic writes)
src/evm.ts         viem reader (rate-limited) and tx hash helper
src/near.ts        NEAR views (JSON-RPC) and change calls, incl. execute (near-js)
src/runner.ts      one pass across chains, plus the loop
src/http.ts        HTTP relay server: routes, body limit, CORS, error mapping
src/relay/         execute and intent paths: service (checks, queue, idempotency), schema
                   (SignedAction and Intent field tables, `to` format), wallet (canonical ids,
                   EVM and Ed25519 verify), limits (token buckets, daily gas budget), errors
src/cli.ts         CLI entry point
tests/             vitest, mocked RPC and NEAR, no network, no real transactions
```

## Intent flow end to end

Swap and add-liquidity intents are executed by the contract when the deposit that funds them arrives.
The relayer submits the intent, watches the vault, and then relays the payout.

1. **Sign intent.** The wallet (EVM or Ed25519) signs the intent message above. The nonce is unused for
   the wallet, and `expires` is within `maxExpirySeconds`.
2. **POST /v1/intent.** The relay checks the body, the schema and the signature, then calls `submit_intent`
   (300 TGas, paid by the relayer). The response carries the `intentId`. The intent stays open until a
   matching deposit is credited.
3. **Send tokens on Base to the vault.** The deposit comes from the address the watcher resolves to the
   intent owner (`resolve_wallet("evm:<from>")`), so for an EVM wallet send from that same address.
   Deposits are scanned after `confirmations`.
4. **Watcher reports.** The deposit watcher calls `report_evm_deposit`. The contract credits the owner and
   executes open intents for `token_in`. The watcher logs `intent status` for the owner's intents.
5. **Contract executes.** The contract runs the intent and records the result. A payout that leaves the
   contract is created as a `PendingWithdrawal`, which the relayer processes in step 6. An intent that
   did not execute stays open, and `execute_intent({id})` retries it.
6. **Payout broadcast.** The withdrawal pipeline in this README (`process_withdrawal`, then the MPC-signed
   broadcast) sends the payout on the destination chain.

Cancel a still-open intent with `POST /v1/intent/cancel`. Follow its status with `GET /v1/intent/:id`.

