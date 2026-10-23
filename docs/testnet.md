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
