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

Flags (all optional):

- `--config <path>`: default `config/relayer.json`.
- `--from-block <n>` / `--to-block <n>`: override the scan range for this run. Works with `--dry-run`.

Example, read-only scan of the testnet vault:

```bash
npm run once -- --config config/relayer.example.json --dry-run \
  --from-block 49070000 --to-block 49114828
```

## Configuration

Config is JSON (see `config/relayer.example.json`). Env overrides for the NEAR section:
`RELAYER_NEAR_RPC`, `RELAYER_CONTRACT_ID`, `RELAYER_ACCOUNT_ID`, `RELAYER_CREDENTIALS`.

| Field | Meaning |
| --- | --- |
| `near.rpcUrl` | NEAR JSON-RPC endpoint. |
| `near.contractId` | Wind Swap v4 contract, e.g. `windswapv4-btb.testnet`. |
| `near.relayerAccountId` | Account that signs change calls. Must be registered with `add_relayer`. |
| `near.credentialsPath` | Keychain JSON for that account. `~` is expanded. |
| `near.gasTgas` | Gas per change call in TGas: `report` 50, `process` 200, `mark` 30 by default. `process_withdrawal` needs at least 150. |
| `evm[]` | One entry per chain: `name` (state file name), `chainId`, `rpcUrl`, `vault`, `confirmations`, `startBlock`, `scanBatchSize`, `requestsPerSecond`, `blockConcurrency`. |
| `evm[].maxFeePerGasCapFallback` | Dry-run only. Used if the contract view `get_evm_chain` is unavailable. |
| `pollIntervalMs` | Sleep between passes in `run` mode. |
| `stateDir` | Directory for `<name>.json` cursor files. Default `state`. |
| `withdrawalLimit` | Page size for the withdrawal views. |
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

- `true` from the contract: reported.
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
src/near.ts        NEAR views (JSON-RPC) and change calls (near-js)
src/runner.ts      one pass across chains, plus the loop
src/cli.ts         CLI entry point
tests/             vitest, mocked RPC and NEAR, no network
```
