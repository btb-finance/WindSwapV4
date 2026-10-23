# Wind Swap v4 — Plan

## Vision

**Wind Swap v4** rebuilds the **entire Wind Swap protocol** as an **off-chain engine in Rust** that works on **hundreds of chains with zero smart-contract deployments** on those chains.

Sources being ported:
- **windv3swap** (`../contracts`): concentrated-liquidity (CL) pools, custom fee modules, CL gauges, WindMEV hook
- **windv2** (github.com/btb-finance/windv2): WIND token, VotingEscrow (veNFT), Voter, Minter, RewardsDistributor, fee/bribe rewards, managed veNFTs, governance. **Its V2 volatile/stable pools are not ported.**

**All pools are concentrated liquidity (Uniswap v3 style).** There are no V2 constant-product or stable-curve pools. Stable pairs (e.g. USDC/USDT) are CL pools with a small tick spacing and low fee.

- The same pool math, fee modules, staking, voting, emissions and rebase logic as the Solidity contracts, ported to Rust.
- Pools live in **one shared off-chain state**, so a single `ETH/SOL` pool serves every chain instead of separate, fragmented pools on each chain.
- Funds sit in **threshold-signature (TSS/MPC) vaults**: on each chain a vault is just a normal address, and a quorum of independent nodes (e.g. 2/3) must cooperate to sign any withdrawal.
- Nothing is deployed on any chain, because every chain already verifies signatures natively.

Prior art for the vault/node model: THORChain, Chainflip, NEAR Chain Signatures.

---

## Why Full Off-Chain (decision)

Considered: (a) keep on-chain only, (b) hybrid = deploy the V3 fork on every EVM chain + off-chain voting, (c) full off-chain. **Chosen: (c).**

- **New chains launch constantly.** Adding an EVM chain is a config entry and is live the same day. A new VM needs one adapter (days), not a contract rewrite + audit (months).
- **Day-one liquidity on every new chain.** Pools are shared, so a new chain gets full depth immediately. This makes BTB attractive to chains' launch/grant programs.
- **Cross-chain swaps built in.** No bridge needed.
- **Free voting, claiming, staking from any chain's wallet.**
- **Non-EVM chains** (Solana, Sui, Aptos, Bitcoin, TON…) are supported the same way.

Accepted trade-off: Stage 0/1 is custodial (team-held keys). This is mitigated with TVL caps, hot/cold split, proof of reserves, daily Merkle root of balances posted on-chain, auto-pause, and decentralizing to Safe → bot signers → TSS nodes, funded by revenue.

The existing on-chain Wind Swap (Sei, Base) keeps running alongside.

---

## Stage 0 (current decision): Single-Wallet MVP

To launch with **zero budget**, start with **one wallet per chain family** controlled by **one engine server**. No TSS, no consensus, no node network yet.

```
Users (any chain)
   │ sign intent (free) + transfer          ▲ payout (instant)
   ▼                                        │
┌──────────── One wallet per chain family ──┴─────────────┐
│ EVM: one EOA (same address on Base, ETH, BSC, Sei…)     │
│ Solana: one keypair (also WIND mint authority)          │
└───────────────┬─────────────────────────▲───────────────┘
                │ websocket watchers       │ sign + send
                ▼                          │
┌──────────── Engine server (Rust) ────────┴──────────────┐
│ Oracle Cloud free tier · pools · ve · voting · API      │
└───────────────┬─────────────────────────────────────────┘
                ▼
      Supabase (free) → Frontend on Vercel (free)
```

### Swap flow
1. User requests a quote → engine locks the price for ~30s
2. User signs the intent (free): from, amount, min out, destination, expiry
3. User transfers tokens to the wallet
4. Watcher sees the transfer (websocket) → matches the signed intent
5. Engine executes the swap → wallet sends the payout immediately
6. If the quote expired, re-price; if below min out, refund. A transfer with no matching intent is credited to the internal balance

### Minimum safety rules (cheap but important)
| Rule | Why |
|---|---|
| Private keys only in env vars / secret manager, never in git or Supabase | Key leak = total loss |
| Separate keys: EVM wallet, Solana wallet, deployer/admin | Limits the damage from one leak |
| Per-swap and per-day payout caps in the engine | Limits loss from a bug or exploit |
| TVL cap (e.g. $50k at launch), raised gradually | Keeps the loss from a key leak small |
| Supply/balance invariant check every block → auto-pause | Catches drains early |
| Pause switch (env flag / admin endpoint) | Stop everything instantly |
| Telegram alerts: large swaps, low balance, invariant mismatch | Know immediately |
| Server: firewall, SSH key only, no other software | Smaller attack surface |
| Keep extra funds in a cold wallet, top up manually | Hot wallet never holds everything |
| Clearly tell users it is a custodial beta | Honesty and trust |

### Upgrade path
- **Stage 1:** cold Safe/Squads (3-of-5 humans) + hot wallet
- **Stage 2:** hot Safe signed by 2-of-3 bots on separate servers
- **Stage 3:** either **NEAR Chain Signatures** (see below) or our own TSS node network run by bonded operators (the rest of this document)

The engine code stays the same in every stage. Only the signer module changes.

---

## Candidate: Host the Engine on NEAR

NEAR offers most of what Stage 3 needs, without running our own validators. **To be confirmed by a testnet prototype (Phase 0b).**

### What NEAR provides (researched Oct 2026)
- **Chain Signatures:** a NEAR smart contract can request signatures for other chains through `v1.signer` (`sign(payload, path, domain_id)`). `domain_id 0` = secp256k1 (BTC, EVM, Cosmos, XRP…), `1` = Ed25519 (Solana, Sui, Aptos, TON, Stellar…). Signing is done by an independent MPC node set (threshold; live set at mpc.nearone.org). Docs say signing on NEAR is free; you pay target-chain gas. **Latency is not documented.**
- **Chain Signatures is outbound only.** It cannot see deposits on other chains.
- **Omni Bridge** (deposit verification): light clients for Ethereum, Bitcoin, Zcash; Wormhole for Solana, BNB and EVM L2s; MPC read-RPC for others.
- **NEAR Intents:** cross-chain swap system; balances live in `intents.near` (NEP-245); deposits via PoA, Omni and HOT bridges; 30+ chains. Had a ~$3.8M deposit-side exploit around Sep 30 – Oct 1 (reports differ on whether the bug was in the Intents contract or the HOT Omni Bridge; fully recovered). **Deposit handling is the riskiest part of any design.**

### How Wind Swap v4 would map onto NEAR
```
┌──────────────────────── NEAR ─────────────────────────┐
│ Wind v4 engine = NEAR contract (Rust, same engine lib) │
│ pools · ve · voting · rewards · accounts (public state)│
│                 │ sign payout                           │
│                 ▼                                       │
│ Chain Signatures (MPC) → vault addresses on every chain│
└───────────────▲─────────────────────────┬─────────────┘
                │ deposit proofs           │ signed tx
        Relayer / Omni Bridge          Broadcaster bot
                ▲                          ▼
           Users on ETH, Base, Solana, BTC, …
```

### Options
| | A. NEAR contract + existing bridges (Omni / Intents) | B. NEAR contract + own Chain Signatures vaults + own relayer | C. Own server + single wallet (Stage 0) |
|---|---|---|---|
| Who holds keys | NEAR MPC / bridge | NEAR MPC | **BTB team** |
| Deposit verification | Bridge (light client / Wormhole / MPC) | **Our relayer** (trust point) | Our watcher |
| New chain same day | Only if NEAR bridges support it | ✅ any ECDSA/Ed25519 chain | ✅ |
| Our code | Least | Medium | Medium |
| Server cost | $0 | ~$0 (small relayer bot) | $0–4/mo |
| Payout speed | Bridge speed | Chain Signatures latency (TBD) | ~2–3 s |
| Custodial | No | No | **Yes** |

**Direction:** build the engine as a pure library first (needed in every option), prototype A/B on NEAR testnet, then choose between NEAR and Stage 0 based on measured latency and cost.

### Rule: the engine is a pure Rust library
`crates/engine` has **no I/O, no async, no networking, no clock**. Time and inputs are passed in. Then the same code runs:
- inside our own server (Stage 0)
- as a NEAR contract (`near-sdk`, compiled to wasm)
- inside our own chain/node network (Stage 3)

---

## How a Swap Works

```
1. User sends 1 ETH to the ETH vault address
   (memo: "SWAP:SOL.SOL:<destination address>:<min out>")
2. Nodes observe the deposit on Ethereum (after finality)
3. Nodes reach BFT consensus on the deposit
4. Engine runs the swap through the CL pools (same math as CLPool.swap)
5. A threshold of nodes jointly signs a payout tx from the SOL vault
6. User receives SOL; if min out can't be met, the deposit is refunded
```

Adding liquidity works the same way: the LP deposits both assets with an `ADD:<pool>:<tickLower>:<tickUpper>` memo, and the engine creates the position in state.

---

## User Actions: Deposits vs Signed Messages

Actions that **move funds in** go through a vault deposit (with a memo). Everything else is a **signed off-chain message**: the user signs with the same wallet key they already have (EIP-712 on EVM, ed25519 on Solana/Sui, BIP-322 on Bitcoin…) and submits it straight to the node API. These actions cost **no gas**.

| Action | How |
|---|---|
| Swap, add liquidity, create lock, `depositFor`, bribe | Vault deposit + memo |
| Remove liquidity, withdraw unlocked WIND, claim rewards → wallet | Signed message → engine → vault payout (user picks destination chain) |
| Stake/unstake LP in gauge, vote, reset, poke | Signed message (state-only) |
| increaseAmount (from internal balance), increaseUnlockTime, merge, split, lockPermanent/unlockPermanent | Signed message (state-only) |
| Delegate, depositManaged/withdrawManaged | Signed message (state-only) |
| Transfer LP position / veNFT to another user | Signed message (state-only) |
| Governance proposals & votes | Signed message (state-only) |

Each user has an **internal account** in engine state, keyed by their wallet address on whichever chain they use. They can link several addresses (e.g. an EVM and a Solana address) to one account by signing a link message from each.

Rewards can stay in the internal balance and be re-locked, re-staked or swapped without ever touching a chain. A user only pays chain gas when they actually withdraw.

---

## The WIND Token: SPL Token on Solana

WIND is an **SPL token on Solana**. Creating it is a single `create mint` instruction on the standard SPL Token program, so **no custom program is deployed**.

| Setting | Value |
|---|---|
| Program | SPL Token (or Token-2022), standard Solana program |
| Mint authority | **TSS Ed25519 group key** (FROST): only a 2/3 quorum of nodes can mint |
| Freeze authority | None, so nobody can freeze holders |
| Metadata | Metaplex token metadata (name, symbol, logo) |
| Decimals | 9 (Solana standard) or 18 → engine scales to 18 internally to keep Solidity math identical |

### On-chain vs off-chain

```
           Solana (on-chain)                       Engine (off-chain)
  ┌─────────────────────────────┐       ┌───────────────────────────────────────┐
  │ WIND SPL mint (TSS authority)│       │ WIND balances (internal accounts)     │
  │ Vault WIND account           │◀────▶│ veNFT locks · decay · delegation       │
  │ User wallets                 │       │ Voter · gauges · rewards · rebase     │
  └─────────────────────────────┘       │ Minter (emission schedule)             │
                                         └───────────────────────────────────────┘
```

**Everything about locking happens off-chain.** Solana only holds the token itself.

### Flows

| Action | What happens |
|---|---|
| **Lock WIND** | User sends SPL WIND to the Solana vault with memo `LOCK:<duration>` → engine creates a veNFT in state. Tokens stay in the vault. |
| **Lock from any chain** | User on Base sends ETH with memo `SWAP_AND_LOCK:WIND:<duration>` → engine swaps in the WIND pool and creates the veNFT directly, without touching Solana |
| **Increase / extend / merge / split / permanent / delegate** | Signed message, state-only |
| **Unlock (after expiry)** | Signed `WITHDRAW` → threshold signers send SPL WIND from the vault to the user's Solana wallet |
| **Claim emissions / rebase / bribes** | Credited to the internal balance. User can re-lock, swap, or withdraw to Solana |
| **Weekly emissions** | At epoch flip, the engine runs `Minter.updatePeriod` and the threshold signers sign **one `mint_to` tx per week** into the vault, so on-chain supply always matches engine supply |

### Supply invariant
```
SPL total supply  =  WIND in user wallets (on-chain)  +  WIND held by vault
WIND held by vault =  Σ internal balances  +  Σ locked amounts  +  WIND in pools  +  undistributed emissions
```
Nodes check this every block. If it breaks, they halt withdrawals.

### Why minting weekly instead of lazily on withdraw
`Minter`'s tail emissions are a percentage of `totalSupply`. Minting each epoch keeps the on-chain supply, which explorers and CoinGecko show, equal to the engine's supply, and it costs one Solana tx a week.

### Existing WIND on Sei / Base
Holders bridge 1:1 into Solana WIND: send old WIND to the Sei or Base vault, and the engine credits the new WIND in their internal balance. They can then lock it or withdraw it to Solana. Old WIND stays in the vault permanently, which removes it from circulation.

---

## Wind Swap → Off-Chain Mapping

### Ported from windv2: tokenomics, staking, voting

| Solidity (windv2 `contracts/`) | Rust module | Notes |
|---|---|---|
| `Wind.sol` | SPL mint on Solana + `engine::token` | On-chain SPL token whose mint authority is the TSS key; engine tracks internal balances; Minter triggers a weekly `mint_to` |
| `VotingEscrow.sol` + `BalanceLogicLibrary`, `DelegationLogicLibrary` | `engine::ve` | veNFT locks (max 4 years), linear decay (slope/bias points), permanent locks, merge, split, delegation, managed veNFTs, user/global checkpoints |
| `Voter.sol` | `engine::voter` | Weekly epochs, vote/reset/poke, gauge weights, create/kill/revive gauge, token whitelist, `distribute` emissions to gauges |
| `Minter.sol` | `engine::minter` | Weekly emissions: 10M start, +3% for 14 epochs, then −1%/week, tail emissions at `TAIL_START`, 5% team rate, `nudge` via EpochGovernor |
| `RewardsDistributor.sol` | `engine::rebase` | Rebase (anti-dilution) for veNFT holders, `claim`/`claimMany` |
| `rewards/Reward.sol`, `VotingReward`, `FeesVotingReward`, `BribeVotingReward` | `engine::rewards` | Per-epoch fee & bribe rewards for voters (balance/supply checkpoints) |
| `rewards/ManagedReward`, `LockedManagedReward`, `FreeManagedReward` | `engine::rewards::managed` | Managed veNFT (relay / auto-compounder) rewards |
| `factories/VotingRewardsFactory`, `ManagedRewardsFactory`, `FactoryRegistry` | `engine::factory` | Gauge/reward creation become plain state ops |
| `ProtocolGovernor.sol`, `governance/*` (VetoGovernor) | `engine::governance` | ve-weighted proposals, veto council, executable actions = engine parameter changes |
| `EpochGovernor.sol` | `engine::governance::epoch` | Weekly tail-emission nudge vote |
| `AirdropDistributor.sol` | `engine::airdrop` | Airdrop as locked veNFTs in state |
| `libraries/ProtocolTimeLibrary.sol` | `engine::time` | Epoch math on consensus timestamps |
| `VeArtProxy.sol`, `art/*` | `api` (optional) | veNFT image rendering for the frontend |

### Ported from windv3swap: concentrated liquidity

| Solidity (`../contracts`) | Rust module | Notes |
|---|---|---|
| `core/libraries/TickMath.sol` | `engine::math::tick_math` | 1:1 port, U256 math |
| `core/libraries/SqrtPriceMath.sol` | `engine::math::sqrt_price_math` | 1:1 port |
| `core/libraries/SwapMath.sol` | `engine::math::swap_math` | 1:1 port |
| `core/libraries/FullMath.sol`, `UnsafeMath`, `BitMath`, `LiquidityMath`, `FixedPoint96/128` | `engine::math::*` | Use `ruint`/`alloy-primitives` U256 |
| `core/libraries/Tick.sol`, `TickBitmap.sol` | `engine::pool::ticks` | BTreeMap instead of storage mappings |
| `core/libraries/Position.sol` | `engine::pool::position` | |
| `core/libraries/Oracle.sol` | `engine::pool::oracle` | TWAP observations; useful for an API price feed |
| `core/CLPool.sol` — `swap`, `mint`, `burn`, `collect` | `engine::pool::ClPool` | Same algorithm without callbacks: tokens are already in the vault |
| `CLPool` staked/unstaked fee split (`splitFees`, `applyUnstakedFees`, `calculateFees`) | `engine::pool::fees` | Keep the exact behaviour |
| `core/CLFactory.sol` | `engine::factory` | Pool creation, tick spacings → default fees |
| `core/fees/CustomSwapFeeModule.sol` | `engine::fees::swap_fee` | Per-pool fee, `ZERO_FEE_INDICATOR`, whitelist |
| `core/fees/CustomUnstakedFeeModule.sol` | `engine::fees::unstaked_fee` | Default 10%, max 50% |
| `gauge/CLGauge.sol` | `engine::gauge::cl` | Stake CL position → earn WIND; staked/unstaked fee split |
| `periphery/lens/Quoter*.sol`, `SugarHelper.sol` | `api::quote` | Read-only queries against engine state |
| `periphery/SwapRouter.sol` / `Path.sol` | `engine::router` | Multi-hop routing over CL pools |

CL gauges plug into `engine::voter`, as they do on-chain today.

### Not needed off-chain

| Solidity | Why |
|---|---|
| windv2 `migration/WindMigrationSender/Receiver` (Hyperlane veNFT migration) | All chains share one state, so no cross-chain migration is needed. Existing on-chain veNFTs migrate in once (see Migration below) |
| windv2 `WindAggregatorProxy.sol` | Becomes an optional solver that routes to external DEXs |
| windv2 `Pool.sol`, `PoolFees.sol`, `Router.sol`, `PoolFactory`, `gauges/Gauge.sol` (V2 volatile/stable pools) | Decision: all pools are CL (v3 style); stable pairs use small tick spacing |
| ERC-721 machinery in `VotingEscrow` (approve, safeTransfer, `onERC721Received`) | veNFTs are state records; transfers are signed messages |
| Forwarder / meta-tx (ERC-2771) | Every non-deposit action is already a gasless signed message |
| Swap/mint/flash callbacks (`ICL*Callback`, `CallbackValidation`) | No on-chain atomicity; funds are already in vaults |
| `flash()` / `PairFlash` | No same-transaction borrowing in an off-chain engine |
| `NonfungiblePositionManager`, NFT SVG/descriptor | Positions are records in state (an optional NFT can be minted later on a chain of choice) |
| `SelfPermit`, `Multicall`, `PeripheryPayments`, `ERC721Permit` | EVM UX helpers |
| `WindMEV.sol` (current form) | All pools share one state, so the router splits every swap optimally and **internal arbitrage disappears**. That value goes straight to LPs/voters. Arbitrage against *external* DEXs becomes an optional solver bot. |

### Equivalent of WindMEV's bribe flow
WindMEV currently sends arbitrage profit to voters as bribes. Off-chain, the engine can route a configurable share of protocol revenue straight into `engine::rewards` fee rewards each epoch. This keeps the same incentive without needing a bot.

### Weekly epoch flow (same as on-chain, run automatically by the state machine)

```
Epoch flip (Thursday 00:00 UTC, consensus time)
  1. Minter.updatePeriod   → mint weekly emissions + rebase + team share
  2. RewardsDistributor    → checkpoint rebase for veNFT holders
  3. Voter.distribute      → split emissions to CL gauges by vote weight
  4. Gauges claim pool fees → FeesVotingReward for that pool's voters
  5. Bribes deposited last epoch become claimable
```

On-chain this needs someone to call `distribute()` with gas. Off-chain it runs automatically every epoch at zero cost.

### Migration from the existing Sei/Base deployments
- **WIND:** users send old WIND to the Sei/Base vault and receive the same amount of Solana WIND in the engine. Old WIND stays in the vault permanently.
- **veNFTs:** users transfer their veNFT to the vault address (ERC-721 transfers to an EOA work). The engine reads `locked(tokenId)` and recreates the lock with the same amount and end time, or as permanent, backed by Solana WIND. The on-chain veNFT stays held by the vault.
- **LP positions:** users withdraw from the on-chain pool and deposit the tokens into engine pools. An optional incentive period can encourage this.

---

## Why It Scales to 100s of Chains

**Engine = written once.** Pools, fees, gauges and ve are all chain-agnostic. An asset is just an ID like `ETH.ETH` or `SOL.USDC`.

Per-chain code is only the **I/O edge** (reading deposits and sending payouts), and it is written **per chain family**, not per chain:

| Dimension | Variants | Per-chain work |
|---|---|---|
| Signature curve | ~3: ECDSA secp256k1 (BTC, EVM, Cosmos, Tron), Ed25519 (Solana, Sui, Aptos, NEAR, TON), Schnorr/Taproot (BTC) | None once the curve is supported |
| Transaction format | One per family | Tx builder (a few hundred lines) |
| Reading deposits | One per family | Watcher (RPC / light client) + finality rules |

- `chain-evm` covers Ethereum, Base, Sei EVM, Arbitrum, BSC, Polygon… and adding one is a config line.
- `chain-cosmos` covers ~50 Cosmos chains.
- ~8–10 adapters → hundreds of chains.

```
           ┌──────────── written ONCE (Wind Swap engine) ───────────┐
deposit →  │ CL pools · fee modules · router · gauges · ve(3,3)     │ → payout
           └────────────────────────────────────────────────────────┘
   ▲                                                                  │
   │            thin adapter per chain FAMILY (read + write)           ▼
   EVM · Solana · Bitcoin · Cosmos · Sui/Aptos · TON · Tron ...
```

---

## Architecture (100% Rust)

```
┌──────────────────── Node (run by N independent operators) ────────────────────┐
│                                                                                │
│  Chain Watchers ──▶ Consensus (CometBFT / Malachite) ──▶ Wind Engine          │
│  (per family)        agree on deposits & ordering        (CL pools, fees,     │
│                                                            gauges, ve)        │
│                                        │                                       │
│                                        ▼                                       │
│              TSS Signer: FROST (Ed25519, Schnorr) · CGGMP21 (ECDSA)           │
│                                        │                                       │
│                                        ▼                                       │
│              Tx Builders (per family) ──▶ broadcast to chain                   │
└────────────────────────────────────────────────────────────────────────────────┘
```

### Workspace layout

```
windswap-v4/
├── crates/
│   ├── engine/          # Wind Swap port (pure library, no I/O; also compiles to wasm for NEAR):
│   │                    #   math, pool (CL), factory, fees, router,
│   │                    #   token, ve, voter, minter, rebase, gauge,
│   │                    #   rewards, governance, airdrop, time
│   ├── accounts/        # internal accounts, multi-chain address linking, signed-message verification
│   ├── state/           # vaults, balances, positions, pending outbounds, storage
│   ├── consensus/       # BFT integration, deterministic state machine
│   ├── tss/             # keygen, signing, resharing (FROST + CGGMP21)
│   ├── chain-core/      # ChainAdapter trait, AssetId, common types
│   ├── chain-evm/       # all EVM chains
│   ├── chain-solana/
│   ├── chain-bitcoin/
│   ├── chain-cosmos/
│   ├── chain-sui/ ...
│   ├── near-contract/   # engine wrapped as a NEAR contract + Chain Signatures calls
│   └── node/            # binary: wires everything together
├── api/                 # quotes (QuoterV2 equivalent), positions, swap status
├── sdk-ts/              # TypeScript SDK for the frontend
└── diff-tests/          # Foundry vs Rust differential tests against ../contracts and windv2
```

### Core trait

```rust
#[async_trait]
pub trait ChainAdapter {
    fn chain_id(&self) -> ChainId;
    fn curve(&self) -> Curve;                       // Secp256k1 | Ed25519 | Schnorr
    fn vault_address(&self, pubkey: &PublicKey) -> Address;
    async fn scan_deposits(&self, from: Height) -> Result<Vec<Deposit>>;
    async fn finalized_height(&self) -> Result<Height>;
    async fn build_tx(&self, out: &Outbound) -> Result<UnsignedTx>;   // returns sighash(es)
    fn attach_signature(&self, tx: UnsignedTx, sig: Signature) -> Result<SignedTx>;
    async fn broadcast(&self, tx: SignedTx) -> Result<TxHash>;
}
```

### Key crates

| Purpose | Crate |
|---|---|
| U256 math | `alloy-primitives` / `ruint` |
| Reference V3 math port (to cross-check) | `uniswap-v3-math` |
| Threshold Ed25519 / Schnorr | `frost-ed25519`, `frost-secp256k1-tr` (ZF FROST) |
| Threshold ECDSA | `cggmp21` (Dfns) |
| Consensus | `malachite` or `tendermint-rs` + CometBFT |
| P2P | `libp2p` |
| EVM | `alloy` |
| Solana | `solana-sdk` |
| Bitcoin | `bitcoin`, `bdk` |
| Storage | `rocksdb` / `redb` |
| NEAR contract | `near-sdk` (Rust → wasm), Chain Signatures via `v1.signer` |

### Determinism rules for the engine
- No floats; all math in U256/I256 exactly as in Solidity.
- No `HashMap` iteration in state transitions (use `BTreeMap`).
- Time comes from consensus block timestamps (replaces `block.timestamp`, `ProtocolTimeLibrary` epochs, ve decay).
- **Differential testing:** run the same action sequences through the Solidity contracts (Foundry) and the Rust engine. Results must match bit-for-bit: swaps, mints, burns, ve locks and decay, votes, emissions, rewards.

---

## Security Model & Risks

| Risk | Mitigation |
|---|---|
| **Node collusion** (t nodes drain vaults; Multichain lost ~$126M) | Nodes post bonds; total bond ≥ 1.5× vault value; slashing on misbehavior |
| **Fake deposits** via malicious RPC | Multiple independent RPCs per node, light clients where available, confirmations, reorg handling |
| **Math divergence** from Solidity | Differential + fuzz tests against `../contracts` and windv2 (reuse the existing echidna properties and the `VOTINGESCROW.md` state-transition table as test cases) |
| **Forged / replayed signed messages** | Domain-separated messages with account nonce, chain-family tag, expiry |
| **Key churn** | Periodic resharing / vault migration as node set changes |
| **Bugs in TSS code** | Use audited libraries only, independent audits, fuzzing |
| **Chain halts / exploits** | Per-chain exposure caps, per-chain circuit breakers, ability to halt a chain |
| **Front-running / MEV** | Ordering decided by consensus; batch swaps per block |
| **Stuck outbounds** | Retries with gas bumping, outbound reassignment to new signer set |

The bond asset could be **$BTB / the Wind ve token**, which ties node security to the existing tokenomics.

---

## Roadmap

> **Order:** Phase 0 (engine) → Phase 0b (NEAR prototype → decide NEAR vs single wallet) → Phase 2 (two chains, with the chosen signer) → Phase 4 (tokenomics) → Phase 5 (product). Phases 1, 3 and 7 (own TSS, consensus, decentralization) are only needed if NEAR is not chosen, and come later, paid for by revenue.

### Phase 0: Pool Engine Port (4–6 weeks), no chains needed
- [ ] Rust workspace scaffold
- [ ] Port CL math libraries (TickMath, SqrtPriceMath, SwapMath, FullMath, LiquidityMath, BitMath)
- [ ] Port `ClPool`: initialize, mint, burn, collect, swap, ticks, positions, oracle
- [ ] Port CL fee modules + staked/unstaked fee split
- [ ] Port factory (tick spacings → fees)
- [ ] Differential tests vs Foundry on `../contracts` (bit-for-bit)
- [ ] Multi-hop router over CL pools + quoter

### Phase 0b: NEAR Prototype (2–3 weeks)
- [ ] Wrap the engine library in a NEAR contract (`near-sdk`), deploy to NEAR testnet
- [ ] Derive vault addresses via Chain Signatures for Base Sepolia (secp256k1) and Solana devnet (Ed25519)
- [ ] Contract requests a payout signature → broadcaster bot sends it → measure **latency** and **cost**
- [ ] Try deposit flows: Omni Bridge / NEAR Intents (option A) vs own relayer (option B)
- [ ] Check contract limits: gas for multi-tick CL swaps, storage cost of pool/ve state
- [ ] **Decision:** NEAR (A or B) vs Stage 0 single wallet

### Phase 1: TSS Prototype (3–4 weeks), only if not using NEAR
- [ ] FROST Ed25519 keygen + signing across 3 local parties
- [ ] CGGMP21 ECDSA keygen + signing across 3 local parties
- [ ] Derive vault addresses for EVM + Solana from group pubkeys
- [ ] Sign and broadcast a devnet tx on Sepolia + Solana devnet

### Phase 2: Two Chains End-to-End (1–2 months)
- [ ] `ChainAdapter` trait + `chain-evm` + `chain-solana`
- [ ] Deposit watchers with finality + memo parsing
- [ ] Internal accounts + signed-message API (EIP-712, ed25519) + address linking
- [ ] Swap, add/remove liquidity, collect fees via memos and signed messages
- [ ] Payout + refund flow
- [ ] Single-operator mode (still uses TSS between local parties)

### Phase 3: Consensus & Multi-Node (1–2 months)
- [ ] Integrate Malachite / CometBFT
- [ ] Nodes vote on observed deposits (witness threshold)
- [ ] Deterministic state machine for engine + outbound scheduling
- [ ] libp2p networking for TSS rounds
- [ ] Testnet with 5–7 nodes

### Phase 4: Tokenomics, Staking & Voting (2–3 months)
- [ ] Create WIND SPL mint on Solana devnet with the TSS key as mint authority + Metaplex metadata
- [ ] Lock / unlock flows via the Solana vault (memo `LOCK`, signed `WITHDRAW`)
- [ ] Minter (emission schedule, tail, team rate) → weekly TSS-signed `mint_to`
- [ ] Supply-invariant check every block + halt on mismatch
- [ ] VotingEscrow: create/increase/extend lock, withdraw, decay checkpoints
- [ ] VotingEscrow: permanent locks, merge, split, delegation
- [ ] Managed veNFTs + Locked/Free managed rewards
- [ ] RewardsDistributor (rebase) + claims
- [ ] Voter: vote/reset/poke, whitelist, create/kill/revive gauge, distribute
- [ ] CL Gauge staking and emissions
- [ ] FeesVotingReward + BribeVotingReward (any whitelisted asset from any chain as a bribe)
- [ ] Automatic weekly epoch flip
- [ ] Protocol share → voter fee rewards (replaces the WindMEV bribe flow)
- [ ] Differential tests vs windv2 Foundry (use the `VOTINGESCROW.md` state table)

### Phase 4b: Governance & Migration (1 month)
- [ ] ProtocolGovernor (veto) + EpochGovernor (tail-emission nudge)
- [ ] AirdropDistributor equivalent
- [ ] WIND migration from Sei/Base (deposit → ledger, vault keeps reserve)
- [ ] veNFT migration (transfer to vault → recreate lock)

### Phase 5: Product (1–2 months)
- [ ] Quote / positions / analytics API (SugarHelper equivalent)
- [ ] TypeScript SDK (deposit building + message signing for EVM, Solana, Sui, BTC wallets)
- [ ] Frontend: swap, CL liquidity (range picker), gauges/staking, lock/veNFT management, voting, bribes, rewards, governance

### Phase 6: Chain Expansion (ongoing)
- [ ] Config-driven EVM onboarding (Sei, Base, Ethereum, Arbitrum, BSC, …)
- [ ] Bitcoin (Taproot / FROST Schnorr)
- [ ] Cosmos family
- [ ] Sui / Aptos
- [ ] TON, Tron, NEAR

### Phase 7: Decentralization & Security (2–3 months)
- [ ] Bonding, rewards, slashing
- [ ] Key resharing & vault churn
- [ ] Exposure caps & circuit breakers
- [ ] External audits (engine port, TSS, consensus, chain adapters)
- [ ] Permissionless node onboarding

### Mainnet
- [ ] Guarded launch with low caps
- [ ] Gradually raise caps as bond grows

---

## Open Questions
- NEAR vs own infrastructure: decided after Phase 0b (Chain Signatures latency, cost, deposit-verification trust)
- If NEAR: option A (existing bridges) or B (own vaults + relayer)? Does the WIND SPL mint authority become a Chain Signatures-derived Solana key?
- Also act as a solver on NEAR Intents for extra volume?
- Bond asset: $BTB / WIND / veNFT, or another asset?
- Launch with the full ve(3,3) model, or pools first and tokenomics in a second release?
- Continue the current Sei/Base emission schedule (carry over `weekly`, `epochCount`, `tailEmissionRate`), or restart it?
- Should on-chain Sei/Base pools keep running alongside, or wind down after migration?
- WIND decimals on Solana: 9 (native standard) or 18 (matches EVM WIND)?
- SPL Token vs Token-2022 (Token-2022 adds transfer fees/hooks; classic SPL has wider wallet/DEX support)
- Later: also allow withdrawing WIND to other chains (e.g. a mintable ERC-20 on EVM with the vault as minter)?
- Node set size and threshold (e.g. 2/3 of 20 → 100)?
- Fee split between LPs, voters, nodes, and the treasury
- Position representation: state-only, or an optional NFT on a chosen chain?
- Migration path for existing Sei/Base on-chain liquidity
- Legal/compliance posture of running a cross-chain swap network

## License
`../contracts` is GPL-2.0-or-later (Uniswap v3-derived). windv2 is GPL-2.0-or-later, with some files (e.g. `Minter.sol`) marked GPL-3.0-or-later. Licensing the Rust port as **GPL-3.0-or-later** is compatible with both.
