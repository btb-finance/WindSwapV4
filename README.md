# Wind Swap v4

Wind Swap v4 rebuilds the Wind Swap protocol (concentrated-liquidity pools, WIND token, veNFT locks,
voting, gauges, emissions, bribes) as an **off-chain engine in Rust** that serves **any chain without
deploying contracts on it**. All pools are concentrated liquidity (Uniswap v3 style).

- One shared liquidity pool per pair across every chain
- Cross-chain swaps built in
- Free voting, claiming and staking from any chain's wallet
- New chains added by configuration

See [`docs/plan.md`](docs/plan.md) for the full design and roadmap.

## Repository layout

```
crates/
  engine/        Pure, deterministic protocol logic (no I/O, no clock). Compiles to native and wasm (NEAR).
diff-tests/      Foundry project that runs the original Solidity libraries and writes test vectors
docs/plan.md     Architecture, decisions and roadmap
```

## Status

Phase 0 — engine port:

- [x] Concentrated-liquidity math: `FullMath`, `UnsafeMath`, `BitMath`, `LiquidityMath`, `TickMath`, `SqrtPriceMath`, `SwapMath`
- [ ] Ticks, tick bitmap, positions, oracle
- [ ] CL pool (swap, mint, burn, collect) + fee modules
- [ ] Router + quoter

## Correctness: differential testing

Every math function is checked against the original Solidity code from windv3swap
(copied into `diff-tests/src/libraries`) on thousands of random and edge-case inputs. Results must match
bit-for-bit, including when the Solidity code reverts.

```bash
# Run all Rust tests (unit + differential)
cargo test

# Regenerate vectors from Solidity (needs Foundry)
cd diff-tests && forge test --match-test testGenerate
```

## Build

```bash
cargo build --release
cargo build --release --target wasm32-unknown-unknown -p windswap-engine   # NEAR-compatible
```

## License

GPL-3.0-or-later. Ported from Wind Swap (windv3swap, windv2), which are GPL-2.0-or-later and derived
from Uniswap v3 and Velodrome/Aerodrome.
