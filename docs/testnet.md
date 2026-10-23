# NEAR testnet deployment

| | |
|---|---|
| Contract account | `windswapv4-btb.testnet` |
| Explorer | https://testnet.nearblocks.io/address/windswapv4-btb.testnet |
| Code | `crates/near-contract` (v0) |
| Wasm SHA-256 | `8a649b480df4eefc81d83a453f249d347d3f3ab2aff303d827f0f8175a44edc4` (348.9 KB) |

v0 is a demo: balances are internal and funded with the owner-only `test_credit` faucet.
There are no real token deposits/withdrawals and no Chain Signatures yet.

## Build

cargo-near needs rustc ≤ 1.86 for the NEAR protocol target:

```bash
rustup toolchain install 1.86.0 --target wasm32-unknown-unknown
cd crates/near-contract
RUSTUP_TOOLCHAIN=1.86.0 cargo near build non-reproducible-wasm --no-abi
# -> target/near/windswap_near/windswap_near.wasm
```

`--no-abi` is needed because the engine's `U256` has no `BorshSchema` implementation yet.

## Deploy

```bash
ACC=windswapv4-btb.testnet
near account create-account sponsor-by-faucet-service $ACC autogenerate-new-keypair \
  save-to-legacy-keychain network-config testnet create
near contract deploy $ACC use-file target/near/windswap_near/windswap_near.wasm \
  with-init-call new json-args "{\"owner\":\"$ACC\"}" \
  prepaid-gas '100.0 Tgas' attached-deposit '0 NEAR' \
  network-config testnet sign-with-legacy-keychain send
```

Redeploying to the same account keeps state (add a migration if the state layout changes).

## Smoke test (run on 2026-10-23)

```bash
call() { near contract call-function as-transaction $ACC "$1" json-args "$2" \
  prepaid-gas '300.0 Tgas' attached-deposit '0 NEAR' sign-as $ACC \
  network-config testnet sign-with-legacy-keychain send; }

call create_pool '{"token0":"tusdc","token1":"tweth","tick_spacing":3,"sqrt_price_x96":"79228162514264337593543950336"}'
call test_credit '{"account_id":"windswapv4-btb.testnet","token":"tusdc","amount":"1000000000000000000000"}'
call test_credit '{"account_id":"windswapv4-btb.testnet","token":"tweth","amount":"1000000000000000000000"}'
call mint_position '{"pool_id":0,"tick_lower":-300,"tick_upper":300,"liquidity":"100000000000000000000"}'
call swap '{"pool_id":0,"zero_for_one":true,"amount_specified":"1000000000000000000","sqrt_price_limit_x96":"4295128740"}'
call swap '{"pool_id":0,"zero_for_one":false,"amount_specified":"2000000000000000000","sqrt_price_limit_x96":"1461446703485210103287273052203988822378723970341"}'
call burn_position '{"pool_id":0,"tick_lower":-300,"tick_upper":300,"liquidity":"100000000000000000000"}'
call collect '{"pool_id":0,"tick_lower":-300,"tick_upper":300}'
```

Results:

| Step | Result |
|---|---|
| mint 1e20 liquidity, range ±300 at price 1 | paid 1.4887 tUSDC + 1.4887 tWETH |
| swap 1 tUSDC → tWETH (fee 0.03%) | received 0.98980 tWETH |
| swap 2 tWETH → tUSDC | received 1.99920 tUSDC |
| burn all | 0.48923 tUSDC + 2.49833 tWETH |
| collect | burn amounts + fees: +0.0003 tUSDC, +0.0006 tWETH (exactly 0.03% of each swap) |
| final balances | 1000 − 3 wei each: rounding dust stays in the pool, as in Uniswap v3 |
| swap with amount 0 | panics `ZeroAmount`, call rolled back |

## v1: real NEP-141 tokens (2026-10-23)

Upgraded in place with `with-init-call migrate` (state from v0 kept; pool 0 still at tick 200).

| Account | Role |
|---|---|
| `windswapv4-btb.testnet` | Wind Swap v4 contract (owner) |
| `windswapv4-tusdc.testnet` | Test token tUSDC (NEP-141, `crates/test-token`, owner `mint` faucet) |
| `windswapv4-tweth.testnet` | Test token tWETH |
| `windswapv4-user.testnet` | Test user |

Wasm SHA-256 `445a2eaf73dac29f130b708c708d2f5eecdf31983550b39b92614cf5fd85af70` (383.6 KB).

Flow tested as `windswapv4-user.testnet` on pool 1 (tUSDC/tWETH, tick spacing 3, price 1):

| Step | Call | Result |
|---|---|---|
| whitelist | owner `add_token` ×2 | ok |
| deposit | user `ft_transfer_call` 100 of each to the DEX (1 yocto) | DEX balance 100 / 100 |
| add liquidity | `mint_position` ±600, L = 5e20 | paid 14.7765 / 14.7765 |
| swap exact in | 5 tUSDC → tWETH | received 4.9490 tWETH |
| swap exact out | exactly 1 tUSDC out | paid 0.98254 tWETH |
| burn half | `burn_position` 2.5e20 | 9.3875 tUSDC + 5.4049 tWETH owed |
| collect | `collect` | burn amounts + accrued fees |
| withdraw | `withdraw` 10 tUSDC (1 yocto) | wallet 900 → 910 tUSDC, DEX balance −10 |

Note: the DEX account must be registered with each token contract (storage) before withdrawals;
the test-token faucet `mint` registers accounts automatically.

## v2: multi-hop routing, quotes, fee admin (2026-10-23)

Redeployed without migration (state layout unchanged). Wasm SHA-256
`da58fc1e9c9ce6075cf51df44c9bdd29a438058205cea96829c0087de8e0edfd` (448.6 KB).

| Step | Result |
|---|---|
| user balances after upgrade | unchanged (80.6125 tUSDC) |
| `create_pool` 2: tUSDC/tWETH, tick spacing 5 | pool id 2 |
| `get_fee` pool 1 / pool 2 | 300 (0.03%) / 2600 (0.26%) |
| `quote_exact_input` 1 tUSDC via pool 1 → pool 2 | 0.974290305953384360 tUSDC |
| `swap_exact_input` same route | 0.974290305953384360 tUSDC (identical to quote) |
| same swap with `min_amount_out` = 2 | panics `TooLittleReceived`, rolled back |

Path format: `[{"pool_id":1,"token_in":"windswapv4-tusdc.testnet"},{"pool_id":2,"token_in":"windswapv4-tweth.testnet"}]`.

## v3: contract-initiated Chain Signatures (2026-10-23)

Redeployed with `with-init-call migrate` (v1 layout → adds `mpc_contract`, default `v1.signer-prod.testnet`).
Wasm SHA-256 `03890a393061cc9a81ac4e2f8a4b23da5ea264292e37d15ad21f833157b818c5` (480.0 KB). User balances unchanged.

| | |
|---|---|
| Contract's derived EVM address (path `ethereum-1`) | `0xaa817ca355475d22d5a410f401dc877ec401259e` |
| Call | `request_evm_payout` chain 84532 (Base Sepolia), nonce 0, 0.000001 ETH to `0x…dEaD`, 21000 gas |
| Time | ~9 s end-to-end via CLI (contract → MPC sign → callback) |
| Result | signed EIP-1559 raw tx `0x02f872…` returned by `on_evm_signed` |
| Verification | `tools/chainsig-probe/payout-check.mjs`: signer recovers to the derived address, low-s, valid parity — PASS |

**Broadcast on Base Sepolia — first cross-chain payout signed by the NEAR contract:**
tx [`0x6aaac922d72ad4649e6bcf59be513a976a2f993f748374044a963cbba3c17277`](https://sepolia.basescan.org/tx/0x6aaac922d72ad4649e6bcf59be513a976a2f993f748374044a963cbba3c17277),
status success, block 0x2ecd268, from `0xaa817ca355475d22d5a410f401dc877ec401259e` (the contract's derived address, funded with 0.1 ETH) to `0x…dEaD`, 0.000001 ETH, 21000 gas.

## WIND token (2026-10-23)

| | |
|---|---|
| Token | `windswapv4-wind.testnet` (crate `crates/wind-token`) |
| Metadata | Wind / WIND / 18 decimals |
| Owner + minter | `windswapv4-btb.testnet` (the Wind Swap v4 contract) |
| Total supply | 0 (minted only by the contract's Minter at epoch flips, once wired) |
| Check | `mint` from any other account panics `Only the minter can mint` |
| Wasm SHA-256 | `a2036647ff5845639ddee4938cff372390196563df0dc74e40cfdc5e9566cb79` |

## v4: one account every chain — MetaMask-only user (2026-10-23)

Redeployed with `migrate` (v3 → owner-keyed balances + persisted wallet links); `migrate_balances` moved the 4 known
legacy balances (user tUSDC/tWETH, owner tusdc/tweth). Wasm SHA-256 `28f7ead8222ed04f657f0a2d7f196c0df54062de9e530cec3fd62098a969cca8` (580.3 KB).

Test with an EVM-only wallet `0x70997970c51812dc3a010c7d01b50e0d17dc79c8` (Hardhat test key #1, testnet only),
signing with viem exactly like MetaMask `personal_sign`; every NEAR transaction relayed by `windswapv4-user.testnet`:

| Step | Result |
|---|---|
| `resolve_wallet("evm:0x7099…79c8")` | owner `590247d8…0ab3f7` |
| deposit 20 tUSDC + 20 tWETH via `ft_transfer_call` msg `{"owner":"evm:0x7099…"}` | credited to the wallet's account |
| `execute` **MintPosition** pool 1, ±600, L = 1e20 (MetaMask-signed) | paid 4.1549 tUSDC + 1.7699 tWETH |
| `execute` **Swap** 1 tUSDC → tWETH (MetaMask-signed) | received 0.97339 tWETH |
| replay the same signed action (same nonce) | rejected: `action nonce was already used for this wallet` |
| final balances | 14.8451 tUSDC, 19.2035 tWETH — exactly 20 − LP − swap in (+ swap out) |

Signed message format: `Wind Swap v4 action on windswapv4-btb.testnet by evm:0x… nonce N expires T: {"Swap":{…}}` (EIP-191).

## v5: cross-chain — deposit on Base, trade on NEAR, withdraw to Base (2026-10-23)

Redeployed with `migrate` (v4 → bridge state); then `HopInput.token_in` fix redeployed without migration
(wasm SHA-256 `cfeed3c3bdad4b0f8ba812c2a3bbb58e11c1d0818e8329ce2683717ec25db74f`).
Config: relayer `windswapv4-relayer.testnet`; `set_evm_chain(84532, "ethereum-1", next_nonce 1, max fee 5 gwei)`;
`register_evm_asset(84532, "native", "base-sepolia:eth", 18, caps 0.05 ETH)`; pool 3 = base-sepolia:eth / tUSDC (spacing 4, price 3000).
Nothing is deployed on Base: the vault is the contract's MPC-derived address `0xaa817ca355475d22d5a410f401dc877ec401259e`.

Test user: a fresh EVM wallet `0x0d53794e20e0e4f96e07bcd8487ce557c6d9a4ac` (no NEAR account). All NEAR calls by the relayer.

| Step | Result |
|---|---|
| withdrawal #0 (NEAR user → fresh wallet, 0.01 ETH): `process_withdrawal` → MPC sign (~10 s) → broadcast | [0x0bdf…0d2d](https://sepolia.basescan.org/tx/0x0bdf6576a18e99a6c03f4f343e5301128fcfc35eb2bae06c0d71f6502ec10d2d), wallet = 0.01 ETH |
| fresh wallet sends 0.004 ETH on Base to the vault → `report_evm_deposit` | credited 0.004; replayed report returns `false` |
| signed `WithdrawEvm` 0.004 → withdrawal #1 → broadcast | [0x16e2…f56f](https://sepolia.basescan.org/tx/0x16e28d0f1886270053e1447104989373cafa4def3f0bab6b32665272898ff56f) |
| fresh wallet sends 0.003 ETH on Base → reported | [0xefa0…1a76](https://sepolia.basescan.org/tx/0xefa045cc9e47a02d76e25c40be0ca572ef467407cd05c4330356f0c3d4ca1a76), credited 0.003 |
| signed `SwapExactInput` 0.002 ETH → tUSDC (pool 3) | 5.8151 tUSDC |
| signed `SwapExactInput` all tUSDC → ETH | 0.0019980 ETH |
| signed `WithdrawEvm` 0.0029980 ETH → withdrawal #2 → broadcast | [0x8724…944d](https://sepolia.basescan.org/tx/0x872494075ad942e94b4cf9e84ee89ba4091f85f270135e3d8220b626c755944d) |
| fresh wallet on Base at the end | 0.0099978 ETH = 0.01 − swap fees/price impact − Base gas |

Bug found and fixed during this run: multi-hop paths typed `token_in` as a NEAR AccountId, so internal ids like
`base-sepolia:eth` were rejected (commit 9ca8033, regression test added).

## Base test token + ETH/USDC pool (2026-10-23)

Test ERC-20 `Usdctest` (name `usdctest`, symbol `test1`, 18 decimals), source `tools/base-test-token/src/Usdctest.sol`
(OpenZeppelin 5.6.1, owner-only `mint`). Deployed by the test wallet `0x0d53…a4ac`, which owns it.
Token address: [0x2d330FEe66912f3eB54086357c2eAB46b998bd4f](https://sepolia.basescan.org/address/0x2d330FEe66912f3eB54086357c2eAB46b998bd4f).
Internal token id `base-sepolia:test1`; the NEAR pool is `base-sepolia:eth` / `base-sepolia:test1`, pool 4, tick spacing 4, price 3000 test1 per ETH.

Base Sepolia transactions:

| Step | Tx | Result |
|---|---|---|
| deploy `Usdctest(0x0d53…a4ac)` | [0x0407…4653](https://sepolia.basescan.org/tx/0x0407524cc66a5bb13ea186b71fa2ac8de4f936aef9c2c867441b86e40ee14653) | contract at `0x2d33…bd4f` |
| `mint(0x0d53…a4ac, 100000e18)` | [0x1cd5…02da](https://sepolia.basescan.org/tx/0x1cd5d139a4d86032437434e1a555df09c7752c763d145b87e9df0315f66c02da) | totalSupply 100000e18, wallet balance 100000e18 |
| `transfer(vault, 9000e18)` | [0xaf5e…2bac](https://sepolia.basescan.org/tx/0xaf5e4613de5d7544c1d231855f0a24e61ea5a69436e64ee3ae3f025e8162bbac) | Transfer log index 57, reported as ERC-20 deposit |
| native 0.003 ETH to vault | [0x8345…8611](https://sepolia.basescan.org/tx/0x83450dfaedda78c739c8b3738314a252e4925bbef5d1bcaace68df78e4d38611) | reported as native deposit, log index 0 |

NEAR calls (contract `windswapv4-btb.testnet`; relayer calls signed by `windswapv4-relayer.testnet`):

| Step | Tx | Result |
|---|---|---|
| `register_evm_asset(84532, 0x2d33…bd4f, "base-sepolia:test1", 18, cap 100000e18, daily 100000e18)` | `6HVwEafzD2fSLNgrCVyk6oH9jv1WgktDwRHDR4PeFnvj` | `get_evm_assets` lists it |
| `report_evm_deposit` ERC-20 (log 57, 9000e18) | `FvuRMoYjcbGoCYAoX5pWep6g5zZ3mXPd7RyyUKYysfab` | `true` |
| `report_evm_deposit` native (log 0, 0.003 ETH) | `8EdUfBAQ9ccKECBgnRXyXWnFcCbkSWUW1uxU8ZePFmis` | `true` |
| owner balances after deposits | `resolve_wallet("evm:0x0d53…")` = `7d0cc58c…378e8f` | test1 9000e18, ETH 0.003e18 |
| `create_pool(base-sepolia:eth, base-sepolia:test1, 4, isqrt(3000·2^192))` | `9VF8LMPq9Qms3zLv53DsHefZ1jcprCr1QEJtd5HAvVC9` | pool id 4, tick 80067 |
| signed `MintPosition` pool 4, ticks 76000..84000, L = 767178844087728835 (nonce 101) via `execute` | `GHjybdjqqunjyK27CffDYcqiBrJMH64GLJ6fB7WSrzx2` | paid 0.0025 ETH + 7.732827575 test1 |
| signed `SwapExactInput` 0.0001 ETH → test1, min out 0.2 (nonce 102) via `execute` | `HHAHYDMdh5zopvTuyMLA3JFiGJvFqYVgsFx6Y1nABJoJ` | received 0.297725470343984462 test1 |

Final state: owner balances 8992.564897895165752477 test1 and 0.0004 ETH on NEAR (the rest is in the pool).
Pool 4: liquidity 767178844087728835, tick 79925 after the swap, sqrt price `4308758448352094782088345339054`.
The EVM wallet still holds 91000 test1 on Base and about 0.006993 ETH.

Signed messages use the format above with `{"MintPosition":{…}}` / `{"SwapExactInput":{…}}`. Liquidity was sized from the sqrt-price math
so that the ETH side was 0.0025 ETH (cap 0.0028) and the test1 side 7.73 (cap 8900).

## v6: ve(3,3) live on NEAR (2026-10-23)

Redeployed with `migrate` (v5 → adds VotingEscrow, Voter, Minter, RewardsDistributor, gauges; deposit dedupe keyed by asset).
Wasm 1.24 MB, SHA-256 `9dfc700c357492b135d3f688e458034fa20a502dbb8ff0c7735952d1b459bd02` (8 testnet NEAR moved from the
user/relayer accounts to cover storage). All earlier state (balances, pools 0–4, LP positions) survived the migration.

| Step | Result |
|---|---|
| `set_wind_token(windswapv4-wind.testnet)` | ok |
| `genesis_mint` 1,000,000 WIND — **real cross-contract mint** on the WIND token | token `ft_total_supply` = 1,000,000; DEX holds it; team credited internally; `wind_minted` matches |
| `create_gauge` pool 4 (ETH/test1), pool 1 (tUSDC/tWETH) | gauge ids 1, 2 |
| `create_lock` 100,000 WIND, 4 years | veNFT #1, voting power ≈ 99,631 |
| `vote` veNFT #1: 70% pool 4, 30% pool 1 | weights 69,742 / 29,889 |
| vote again in the same epoch | rejected `AlreadyVotedOrDeposited` |
| `whitelist_bribe_token` tUSDC; user `bribe` 5 tUSDC on pool 1 | ok |
| EVM-only wallet `0x0d53…a4ac` signs **StakePosition** (pool 4, 76000–84000), relayed | pool 4 `staked_liquidity` = full liquidity 767178844087728835 |

Epoch: `active_period` 1792627200; first flip at **1793232000 (Thu 2026-10-29 00:00 UTC)** — then anyone calls
`distribute` (≥ 230 TGas) to mint the week's WIND (10M) and send emissions to gauges by vote weight; voters can then
`claim_bribes` / `claim_fees`, veNFT holders `claim_rebase`, and the staked LP `claim_gauge_rewards`.

Known gaps before mainnet (from the ve(3,3) wiring review):
1. ~~Fee leak between stakers that share one tick range~~ — fixed in v7: the NFT manager keeps per-NFT owed amounts (`PositionManager`), and a collect pays only the NFT's share. The v6 NFT manager is converted once by `migrate_nft_positions`.
2. Each engine object is one Borsh blob (pools, ve, voter): gas grows with state — move to per-key storage.
3. No WIND conservation invariant check yet.

## v7: fee-leak fix, solvency accounting, owner views, partial-fill fix (2026-10-23)

One combined upgrade (`migrate`) + `migrate_nft_positions` (converted 1 staked NFT to per-NFT fee accounting).
Wasm 1.35 MB, SHA-256 `0440420b8ab8da78a937e5add4d4f84bc9bbabfb1766511807708d8bbbe4d9cd`. All v6 state kept
(gauges, votes, veNFT #1, epoch, balances, pools 0–4, the EVM wallet's staked position).

| Change | Live check |
|---|---|
| Per-NFT fee accounting for gauge-staked / shared-range LPs | `migrate_nft_positions` → 1 |
| Exact-input routes revert (`InsufficientLiquidity`) instead of charging unconsumed input | regression tests |
| Owner views + indexes (`get_positions_of_owner`, `get_balances_of_owner`, `get_stakes_of_owner`, `get_withdrawals_of_owner`, `get_gauges`, `get_bribe_tokens`) | backfilled 6 positions, 1 stake, 10 token keys, withdrawals 0–2; the EVM wallet's staked pool-4 position and balances are returned |
| Signed `Distribute`, `CreateLock.permanent` | relayer schema updated (129 tests) |
| Solvency accounting (`get_all_tokens_accounting`, `check_solvency`) | `seed_accounting` 11 keys. **WIND: 900,000 balances + 100,000 locked = 1,000,000 owed; `check_solvency` on-chain: held 1,000,000, solvent** |

Pending: `seed_pool_reserves` (pool-held amounts must be computed off-chain from pool state), so per-token
`pool_reserves` read 0 and `migrated: true` until seeded.

## v8: permissionless listing (not deployed)

Status: code and tests on branch `agent/permissionless`. Not deployed. The wasm size and hash are
recorded at deploy time.

Anyone can use the contract without owner registration. Owner-only calls that stay: `add_token`,
`set_evm_chain`, `enable_tick_spacing`, `set_default_caps`, `set_evm_asset_verified`,
`register_evm_asset`, the seeds and backfills.

| Change | What it does |
|---|---|
| Auto-registration of ERC-20s | `report_evm_deposit(..., decimals?, symbol?)`: an unregistered ERC-20 on an enabled chain is registered on its first report, with the owner's default caps and `verified: false`. Needs `decimals` (0–36) or the report returns `false` with a log. `native` is never auto-registered. A disabled chain still panics `is not registered`. |
| Token ids | Auto id `evm:{chain_id}:{address}`, derived from the address, so a look-alike token gets its own id. Existing ids (`base-sepolia:eth`) are unchanged. |
| Default caps | `set_default_caps(per_deposit_cap, daily_withdraw_cap)`, owner. Until set, no ERC-20 is auto-registered. `get_default_caps`. |
| Verified flag | `get_evm_assets` / `get_evm_asset` return `verified` and `symbol`. An asset with no listing entry (owner-registered) reads as verified. `set_evm_asset_verified(chain_id, asset, verified)`, owner. |
| Permissionless pools | `create_pool` callable by anyone when both tokens are known (whitelisted NEP-141 or registered EVM asset). A non-owner attaches at least `POOL_STORAGE_DEPOSIT` = 0.1 NEAR. It stays in the contract, and any excess is refunded (detached transfer). The owner needs no deposit. One pool per (sorted pair, tick spacing): `create_pool` panics with the existing id otherwise. `get_pool_id(token0, token1, tick_spacing)`. |
| Pool summaries | `get_pools(from_index, limit)` (limit capped at 100) and `get_pool_summary(pool_id)` read only a small `PoolSummary` per pool: tokens, tick spacing, fee, price, tick, liquidity, fee growth, `has_gauge`, `created_at`, `swap_count`, `volume0`, `volume1`. Every pool write refreshes it. |
| NEP-141 listing | `register_token(token)`, anyone, attached deposit `REGISTER_TOKEN_DEPOSIT` = 0.01 NEAR (kept on success). Calls `ft_metadata` and whitelists the token if it succeeds and `decimals <= 36`. Failure refunds. |

### Migration

1. Build: `cargo build -p windswap-near --release --target wasm32-unknown-unknown`.
2. Deploy with the init call: `near contract deploy <acct> use-file <wasm> with-init-call migrate json-args '{}'`.
   The state must be the v7 layout (the testnet state after the v7 upgrade). A v6 state needs the v7
   upgrade first.
3. `migrate` backfills the pool index and the pool summaries from the existing pools (lowest id first).
   `created_at` is the migration time for those pools, and swap counters start at zero.
4. Then the owner: `set_default_caps`, and `set_evm_asset_verified` for the major tokens.

### Frontend and relayer changes

- `get_pools` now returns `PoolSummary` (not `PoolView`). Use `get_pool` for the full state of one pool.
- `get_evm_assets` items have two new fields, `verified` and `symbol`. Show unverified tokens as such.
- `create_pool` from a non-owner needs the 0.1 NEAR deposit. Check `get_pool_id` first, since a
  duplicate panics.
- `report_evm_deposit` has two new optional arguments. The relayer sends them only for auto-registration.
- Relayer config: `evm[].autoRegister` (default `false`) and `evm[].denyTokens` (lower-case addresses).
  With auto-registration on, the watcher reads `decimals()` and `symbol()` through the token. A token
  whose decimals cannot be read is ignored with a warning, and a report is not sent.

### Trust note (read before enabling auto-registration)

The contract trusts what the relayer reports, and an ERC-20 or NEP-141 token is code that can say
anything. These are not guarded by this change:

- **EVM.** A token contract can emit `Transfer` logs to the vault without moving any tokens. The
  relayer reports those logs as real. The credit is then backed by nothing, and the caps only bound
  each report. Unverified tokens are credited the same way as verified ones.
- **NEP-141.** `ft_on_transfer` trusts its predecessor. Any contract that is whitelisted can call it
  with any amount. `register_token` lets anyone whitelist their own contract, so the credit can be
  unbacked.

Unbacked credit can be swapped out of any pool that holds that token and a real token, once a pool
exists for it. Permissionless pool creation makes that easy. Before enabling this on a network that
holds value, decide on a guard. Options: pools only for verified tokens, `register_token` tokens
not poolable until the owner verifies them, or a relayer-side check that is stronger than the log.
