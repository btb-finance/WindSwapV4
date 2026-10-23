# seed-reserves

Computes what each Wind Swap v4 pool holds, per token, on NEAR testnet, and the arguments for
`seed_pool_reserves` (and `seed_accounting`) so the contract's solvency totals match the chain.
Read-only: NEAR views, NEP-141 `ft_balance_of`/`ft_metadata`, Base Sepolia `eth_getBalance` and
`eth_call`. It never sends a transaction.

## Run

Node 22.18+ (type stripping; developed on Node 24, no dependencies, no install step).

```bash
cd tools/seed-reserves
npm run compute          # prints the tables, writes out/seed-entries.json and out/accounting-entries.json
```

Exit code 0 means every cross-check passed and the two JSON files were written. Exit code 1 means
an ERROR line was printed and nothing was written. Public RPCs rate-limit (HTTP 429): the tool
rotates between `rpc.testnet.near.org`, `archival-rpc.testnet.near.org` and `test.rpc.fastnear.com`.
Override with `NEAR_RPC=...`, Base with `BASE_RPC=...`, the vault with `VAULT=...`.

## What is counted

For each pool `p` and each of its two tokens `t`:

| Term | Source | Rounding |
|---|---|---|
| positions | every owner position in `p` (`get_positions_of_owner`), valued at the current tick and `sqrtPriceX96` (`get_pool`) with `ClPool::modify_position`'s branches | up (what mint pays in) |
| staked positions | the gauge's pool position for each staked range (`get_position_of_owner(gauge account, p, lo, hi)`); the gauge holds staked NFTs (`holder_of` in `gauge.rs`) | up |
| NFT manager position | `get_position_of_owner(nft-manager, p, lo, hi)`, unstaked NFT liquidity (none expected) | up |
| owed | `tokens_owed0/1` of all of the above | exact |
| gauge fees | `ClPool.gauge_fees` and the gauge's `fee_balance`: **not exposed by any view** | n/a |
| unpoked LP fees | fees accrued to a position but not yet written into `tokens_owed` (needs tick fee-growth-outside, not exposed) | n/a |
| residual | derived, see below | |

`holdings = positions + owed + residual share`, and the seed amount is `holdings - recorded`,
where `recorded` is what the contract already put in `pool_reserves` since the v7 migration
(`get_pool_reserves`). Today `recorded` is 0 everywhere, so seed = holdings.

### Rounding and dust

Mint rounds amounts up (`get_amount0_delta(..., round_up = true)` in `sqrt_price_math.rs`), so the
pool keeps up to 1 wei per mint that burn would not pay out. The tool values each held position
rounding up, so it can overstate by at most 1 wei per term. The dust bound used below is
`2 * terms + 2` wei per (pool, token) group. Burn and collect round down, and any wei they leave
behind are inside the residual.

### Cross-check 1: liquidity (per pool)

`pool.liquidity` is all in-range liquidity (staked included, pool 4 is all staked). It must equal
the sum over direct positions and gauge/NFT positions whose range contains the current tick.
`pool.staked_liquidity` must equal the gauge and NFT-manager liquidity in range, and the owners'
staked views must agree. All five pools pass.

### Cross-check 2: tokens (NEP-141 and EVM vault)

```
residual(t) = held(t) - liabilities(t) - locked(t) - pending(t) - Σ_pools (positions + owed)(p, t)
```

- `held`: `ft_balance_of(windswapv4-btb.testnet)` for NEP-141 tokens. The vault balance
  (`eth_getBalance` / `balanceOf`) for EVM assets.
- `liabilities`: Σ internal balances, from `get_balance_of_owner` for every known owner and token.
  Not `get_balances_of_owner`: its token list is an index that starts empty at v7, so balances
  credited earlier (the bribe pool's tUSDC, for example) are missing from it.
- `locked`, `pending`: `get_all_tokens_accounting`.

The residual is what the cross-check cannot see directly: gauge fees, unpoked LP fees, rounding
dust, and anything the owner list does not name.

- **NEP-141 tokens** (windswapv4-tusdc, windswapv4-tweth): the residual is split across the pools
  holding the token, in proportion to their pool-side amounts, and added to their holdings. The
  split is exact in total; the per-pool share is an estimate. A shortfall beyond the dust bound is
  an ERROR and nothing is written.
- **WIND**: no pool holds WIND as a token. WIND reaches a pool only as gauge reward balance, and
  `seed_pool_reserves` rejects a token that is not one of the pool's tokens, so it cannot be
  seeded. The residual is checked to equal the gauge reward balances (currently 0).
- **EVM assets** (base-sepolia:eth, base-sepolia:test1): the residual is reported and **not
  allocated**. Adding a surplus to pool reserves, or subtracting a shortfall, would hide an
  untracked balance inside pool accounting. See the results below.
- **Pool 0** (`tusdc`, `tweth`): these ids are not NEAR accounts and have no token contract, so
  there is nothing to check. Their entries are the positions only. Their balances are faucet
  credit (`test_credit`), which `unbacked` already reports.

### Owner list

Users: `windswapv4-user.testnet`, `windswapv4-btb.testnet` (also the team owner),
`windswapv4-relayer.testnet`, the hardhat #1 EVM wallet and the fresh EVM wallet. Protocol owners
(derived with the engine's `system_id`): voter, minter, distributor, NFT manager, each gauge's
account, and each gauge's fee and bribe owner. Balances of unnamed owners would fall into the
residual, so the owner list must be complete. The on-chain `liabilities` after `seed_accounting`
can be compared with the sum printed here.

## Seeding (owner: windswapv4-btb.testnet)

Run `npm run compute` right before seeding and confirm it exits 0. Then seed both sets. Both calls
are one-shot per key (`seed_pool_reserves` skips a `(pool, token)` pair it has already seeded), so
send both, then verify.

```bash
cd tools/seed-reserves
ACCT=$(node -p 'JSON.stringify({entries: require("./out/accounting-entries.json")})')
RES=$(node -p 'JSON.stringify({entries: require("./out/seed-entries.json")})')

# 1. liabilities for keys not yet counted (idempotent)
near contract call-function as-transaction windswapv4-btb.testnet seed_accounting json-args "$ACCT" \
  prepaid-gas '300.0 Tgas' attached-deposit '0 NEAR' sign-as windswapv4-btb.testnet \
  network-config testnet sign-with-legacy-keychain send

# 2. pool-held amounts: one entry per (pool, token), including zeros
near contract call-function as-transaction windswapv4-btb.testnet seed_pool_reserves json-args "$RES" \
  prepaid-gas '300.0 Tgas' attached-deposit '0 NEAR' sign-as windswapv4-btb.testnet \
  network-config testnet sign-with-legacy-keychain send
```

## Verifying afterwards

```bash
near contract call-function as-read-only windswapv4-btb.testnet get_all_tokens_accounting json-args '{}' network-config testnet now
near contract call-function as-read-only windswapv4-btb.testnet get_pool_reserves json-args '{"pool_id":1}' network-config testnet now
# ... pools 2, 3, 4 (pool 0 too: its entries are zero)

# check_solvency returns a Promise, so run it as a transaction (no state change) and read the result
near contract call-function as-transaction windswapv4-btb.testnet check_solvency json-args '{"token":"windswapv4-tusdc.testnet"}' \
  prepaid-gas '100.0 Tgas' attached-deposit '0 NEAR' sign-as windswapv4-btb.testnet network-config testnet sign-with-legacy-keychain send
# same for windswapv4-tweth.testnet and windswapv4-wind.testnet

# EVM: owed (contract) vs held (vault), on Base Sepolia
near contract call-function as-read-only windswapv4-btb.testnet get_evm_owed json-args '{"chain_id":84532,"asset":"native"}' network-config testnet now
near contract call-function as-read-only windswapv4-btb.testnet get_evm_owed json-args '{"chain_id":84532,"asset":"0x2d330fee66912f3eb54086357c2eab46b998bd4f"}' network-config testnet now
```

Expected: `pool_reserves` read the seeded `seed` amounts; for tUSDC and tWETH, `held` equals
`total_owed` (dust 0 by construction of the residual split). The real test is the pre-seed residual
shown by `npm run compute`: 0.0035 tUSDC and 0.0025 tWETH, of which most is unpoked LP and gauge
fees. Check that these look plausible for the volume traded. For test1 the residual is 1 wei.
For ETH `get_evm_owed` should be at most the vault balance.

## Known limits

- `gauge_fees` and unpoked LP fees are not exposed by any view. They are inside the residual,
  which is allocated to pools for NEP-141 tokens and not allocated for EVM assets.
- Unknown owners and out-of-range positions of unknown owners cannot be seen from views. The
  residual absorbs them. Keep the owner list complete.
- Pool-level WIND (gauge reward balances) cannot be seeded by the current contract.
- The two JSON files are a snapshot of one block. Re-run `npm run compute` before seeding.
