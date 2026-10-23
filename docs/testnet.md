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
