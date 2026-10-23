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

Engine (Rust, `crates/engine`) — every module is differentially tested against the original Solidity:

- [x] CL math (`FullMath`, `TickMath`, `SqrtPriceMath`, `SwapMath`, …), ticks, tick bitmap, positions, oracle
- [x] CL pool (mint, burn, collect, swap, stake, reward growth, gauge fees) — 5,508 random ops vs `CLPool.sol`
- [x] Fee config, multi-hop router + quoter
- [x] Minter (weekly emissions, tail, team rate, nudge)
- [x] VotingEscrow (locks, decay, merge, split, permanent, delegation, managed veNFTs)
- [x] Voting rewards (fees, bribes, locked/free managed rewards)
- [x] CL gauge (LP staking emissions)
- [x] Voter (votes, gauge weights, distribute, claims)
- [x] RewardsDistributor (rebase)

NEAR contract (`crates/near-contract`), live on testnet `windswapv4-btb.testnet` (see [docs/testnet.md](docs/testnet.md)):

- [x] Pools, swaps, multi-hop routing, quotes, liquidity, fee admin
- [x] NEP-141 deposits and withdrawals
- [x] One account, every chain: EVM / Ed25519 wallet linking, signed actions relayed by anyone (MetaMask-only user swapped and added liquidity on testnet)
- [x] Chain Signatures: contract-signed payout confirmed on Base Sepolia
- [ ] EVM deposits + withdrawals via relayer (in progress)
- [ ] ve(3,3) in the contract: locks, voting, epoch flip, WIND emissions
- [ ] Per-tick storage (pools are one blob each today)

Other:

- [x] WIND token (`crates/wind-token`), testnet `windswapv4-wind.testnet`, minter = the contract
- [x] TypeScript SDK (`sdk/`)
- [x] Frontend: v4 pages on the `v4-near` branch of `windswapfrontend-mainnet`

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
