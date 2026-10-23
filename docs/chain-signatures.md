# NEAR Chain Signatures: testnet probe (Phase 0b)

Status: **probe complete, all 6 MPC signatures verified locally.** Source of the plan: `docs/plan.md`, section "Candidate: Host the Engine on NEAR".

Account: `windswapv4-user.testnet` (signer and predecessor). MPC contract: `v1.signer-prod.testnet` (testnet).
Probe code and raw outputs: `tools/chainsig-probe/` (scripts, `out/derived.json`, `out/sign_*.txt`, `out/verify.json`, `out/chain_latency.txt`, `out/txcost_summary.txt`).

Date of run: 2026-10-23 (testnet).

---

## 1. Interface (what we found, and where)

The public docs (`https://docs.near.org/chain-abstraction/chain-signatures` and `.../chain-signatures/implementation`) do not spell out the request JSON, the deposit, or the view names. Their code samples are "See code on GitHub" links. The details below come from the MPC contract source at commit `01f33ed0a2a2c4c24ef49a2f36df3b20aa400816`:

- `https://github.com/near/mpc/blob/01f33ed0a2a2c4c24ef49a2f36df3b20aa400816/libs/chain-signatures/contract/src/lib.rs` (`sign`, `derived_public_key`, `public_key`, `latest_key_version`, `return_signature_and_clean_state_on_success`)
- `.../src/primitives/signature.rs` (`SignRequestArgs`, `Payload`)
- `.../src/primitives/domain.rs` (`DomainId`, `SignatureScheme`)
- `.../src/crypto_shared/types.rs` (`SignatureResponse`)

The on-chain behaviour matched this source (the logged `SignRequestArgs` and the returned signature shape). The deployed testnet build differs from the source in one place: see "Discrepancies" below.

### sign (request)

Method: `sign`, single argument `request`, `#[payable]`.

```json
{
  "request": {
    "path": "ethereum-1",
    "payload_v2": { "Ecdsa": "<64 hex chars, 32 bytes>" },
    "domain_id": 0
  }
}
```

For Ed25519 (domain 1):

```json
{
  "request": {
    "path": "solana-1",
    "payload_v2": { "Eddsa": "<hex, up to 1232 bytes; we used 32>" },
    "domain_id": 1
  }
}
```

- `payload_v2` is a tagged enum: `Ecdsa` (exactly 32 bytes, must be a valid secp256k1 scalar) or `Eddsa`. The legacy `payload` / `key_version` fields are deprecated.
- `domain_id`: 0 = Secp256k1, 1 = Ed25519 (the testnet deployment calls this scheme `Edwards25519`, see below).
- `prepaid-gas`: the contract requires at least `GAS_FOR_SIGN_CALL` = 10 Tgas. We attached 100 Tgas.
- **Deposit: exactly 1 yoctoNEAR required** (`env::attached_deposit()` minus 1 yocto is refunded to the predecessor). Less than 1 yocto panics.

Example CLI (what `sign.sh` runs):

```
near contract call-function as-transaction v1.signer-prod.testnet sign \
  json-args '{"request":{"path":"ethereum-1","payload_v2":{"Ecdsa":"<hex>"},"domain_id":0}}' \
  prepaid-gas '100.0 Tgas' attached-deposit '1 yoctoNEAR' \
  sign-as windswapv4-user.testnet network-config testnet sign-with-legacy-keychain send
```

### sign (response)

`sign` is a yield: the call returns a promise that is resolved when the MPC nodes call back, so the signature arrives as the function's return value. From a contract, it arrives in the `.then` callback of the cross-contract call (`env::promise_return(promise_index)` in the source).

ECDSA (`domain_id` 0), observed:

```json
{
  "big_r": { "affine_point": "02889b36b14a14132b5c3294674a7f6b2acf99419c43a17a3b8417ef275dc3faf4" },
  "recovery_id": 1,
  "s": { "scalar": "7513ff534708652e2ee6bb3106ca050184be08a14e2e82caefeb39befd15b892" },
  "scheme": "Secp256k1"
}
```

- `big_r.affine_point`: compressed SEC1 point (33 bytes, `02`/`03` prefix). `r` = x-coordinate of R mod n.
- `s`: 32-byte scalar. Not normalised to low-s by the contract (we normalise in verification).
- `recovery_id`: 0 or 1.

Ed25519 (`domain_id` 1), observed:

```json
{ "scheme": "Ed25519", "signature": [134, 78, 247, "...64 bytes total..."] }
```

The 64 bytes are the standard Ed25519 signature (R || S).

### Derived public key (view)

```
near contract call-function as-read-only v1.signer-prod.testnet derived_public_key \
  json-args '{"path":"ethereum-1","predecessor":"windswapv4-user.testnet","domain_id":0}' network-config testnet now
```

Returns a string:
- secp256k1: `"secp256k1:<base58 of 64 bytes>"`. The 64 bytes are the uncompressed point **without** the `04` prefix (verified by the EVM address check).
- ed25519: `"ed25519:<base58 of 32 bytes>"`.

Other views used: `public_key({"domain_id":N})` returns the root key per domain. `latest_key_version({"signature_scheme":"Edwards25519"})` returned `1` (see below for the `Secp256k1` default, which returned `3`).

### Discrepancies

- The pinned source names the scheme `Ed25519`. The deployed testnet contract rejects that name and expects `Edwards25519` (error: `unknown variant 'Ed25519', expected one of 'Secp256k1', 'Edwards25519', 'Bls12381'`). The `sign` request itself uses only `domain_id`, so this did not block signing. Use `Edwards25519` if calling the view.
- The docs describe `domain_id` 0 and 1 as the convention. Confirmed on testnet: domain 0 = Secp256k1, domain 1 = Edwards25519.

---

## 2. Derived addresses (predecessor `windswapv4-user.testnet`)

| Target | Path | Domain | Derived key (NEAR form) | Address |
|---|---|---|---|---|
| EVM (secp256k1) | `ethereum-1` | 0 | `secp256k1:45BBRRyeF5G1tGqRaKJbXo2GZCS3HAC5WDsHypN6HhHQxghdBdpKjmFA3ApTREx6a4sAyV5a5WHiiLedWpyWP1nk` | `0x68378500c10e3fc0b46b9f00f8d9839b5973d845` |
| Solana (ed25519) | `solana-1` | 1 | `ed25519:6wptuEHgPoXhrCYoa4sWowe9R8BiPZfDRd1foHS2caor` | `6wptuEHgPoXhrCYoa4sWowe9R8BiPZfDRd1foHS2caor` |

- EVM: `keccak256(64-byte uncompressed pubkey)[12:]`. Compressed pubkey: `0399a6dc3a46dbb15bb0c20c38f5c58ec391a60c876b5ae6a7851d73c79606001e`.
- Solana: base58 of the 32-byte Ed25519 key.
- These addresses are derived, not funded. Nothing was broadcast to EVM or Solana.

Reproduce: `cd tools/chainsig-probe && node derive.mjs` (uses the public RPC `https://rpc.testnet.near.org`, no signer).

---

## 3. Signing results

Six signature requests in total: 3 for ECDSA (`ethereum-1`, domain 0), 3 for Ed25519 (`solana-1`, domain 1). All succeeded on the first attempt. Payload for both: `keccak256("windswap v4 chain signatures probe")` = `9b75a30e8184ea6c7a97b8fd6aced2722c0eb0e0e303047d0ced1be70ab20eea` (for ECDSA, a valid scalar; for Ed25519, the same 32 bytes are used as the raw message).

### Latency

Two measurements. The on-chain number is the one to use for design.

- **Wall clock (CLI):** from starting `near ... send` to CLI exit. Includes CLI startup and RPC polling.
- **On-chain:** block timestamp of the request transaction to the block that executed the last receipt (where the MPC response resumes the call). Read from the RPC (`tx`/`EXPERIMENTAL_tx_status` plus `block`).

| Domain | Run | CLI wall (s) | On-chain (s) |
|---|---|---|---|
| ECDSA (domain 0) | 1 | 7.970 | 4.545 |
| ECDSA (domain 0) | 2 | 8.155 | 4.655 |
| ECDSA (domain 0) | 3 | 7.945 | 4.242 |
| Ed25519 (domain 1) | 1 | 8.788 | 4.768 |
| Ed25519 (domain 1) | 2 | 8.276 | 4.771 |
| Ed25519 (domain 1) | 3 | 7.402 | 4.256 |

Summary (n = 3 per domain):

| Domain | CLI wall min / avg / max (s) | On-chain min / avg / max (s) |
|---|---|---|
| ECDSA | 7.945 / 8.023 / 8.155 | 4.242 / 4.481 / 4.655 |
| Ed25519 | 7.402 / 8.155 / 8.788 | 4.256 / 4.598 / 4.771 |

Reproduce: `./tools/chainsig-probe/sign.sh evm 1` (or `sol`), then `node chainlatency.mjs <tx hash>`.

Caveat: n = 3 on testnet, one time of day. No measurement under load, and nothing on mainnet.

### Cost

Per signature request, from the RPC receipt tree (`node txcost.mjs <tx hash> windswapv4-user.testnet`):

- Attached deposit: 1 yoctoNEAR (refunded excess: 0).
- Gas burnt on the signer's transaction: 0.315 Tgas (CLI "Gas burned"). Transaction fee shown by CLI: about 0.0000315 NEAR.
- Whole receipt tree (sign, yield, MPC resume, callback): about 8.55 to 8.59 Tgas burnt across 4 receipts.
- Total NEAR burnt across the tree: 0.000810 to 0.000815 NEAR per signature.

Reconciliation: the six probe transactions total 0.0048732449 NEAR (sum of the per-tx tree costs). The account balance went from 9.996724376935 to 9.991851132034 NEAR, a drop of 0.0048732449 NEAR. The balance check matches exactly, so the signer pays the whole receipt tree, not just the CLI-reported fee.

Mainnet price and gas are not measured.

---

## 4. Local verification

`node tools/chainsig-probe/verify.mjs`. No broadcast. Uses `@noble/curves` and `@noble/hashes`.

ECDSA (per signature, 3 of 3 pass):
1. Recompute `r` from `big_r` (x mod n) and check it matches.
2. Normalise `s` to low-s (flip recovery bit if needed). The contract does not normalise.
3. Recover the public key from `(r, s, recovery_id)` over the 32-byte payload. It equals the derived key `99a6dc…dd` (64-byte X||Y).
4. The recovered EVM address equals `0x68378500c10e3fc0b46b9f00f8d9839b5973d845`.
5. Standard ECDSA verify with the derived key passes.
6. Negative control: a different message does not recover the derived key.

Ed25519 (per signature, 3 of 3 pass):
1. Signature length is 64 bytes.
2. `ed25519.verify(sig, payload, derivedPub)` passes.
3. Negative controls: tampered payload rejected, unrelated key rejected.

Result: **6 of 6 signatures verified** (`out/verify.json`).

What this does not show: that the signature is valid for a real Solana transaction or EVM transaction. Verification here is over a raw 32-byte payload. A real Solana message is the serialised transaction message, and a real EVM transaction is an RLP-encoded hash. Those are not built or broadcast here, by the probe's rule.

---

## 5. Implications for Wind Swap v4 payouts

- **Latency:** about 4.2 to 4.8 s on-chain per signature, about 7.4 to 8.8 s in the CLI. Add the target chain's broadcast and finality time on top. Compare the plan's "~2 to 3 s" for Stage 0. A payout on NEAR costs roughly the MPC round trip plus target-chain confirmation.
- **One signature per payout:** `sign` takes one payload, so there is no batching. Each payout is one `sign` call, and cost scales linearly with payouts.
- **Calling from a contract (source-based, not tested):** `sign` yields and returns a promise. The calling contract receives the `SignatureResponse` in a `.then` callback. It must attach at least 1 yoctoNEAR and at least 10 Tgas for the call.
- **Gas to reserve (observed):** 8.6 Tgas burnt across the whole receipt tree per signature, with 100 Tgas attached. A contract could attach less (30 Tgas or more, with margin) but this was not tested.
- **Callback timeout (not verified):** the yield has a timeout and the source has a `fail_on_timeout` path. Its length was not found in the consts we read (only `DEFAULT_KEY_EVENT_TIMEOUT_BLOCKS` = 30, which is for key events, not signing). Design a payout retry or failure path, and measure the timeout before relying on it.
- **Cost per payout:** about 0.00081 NEAR on testnet, plus the target-chain fee. Mainnet cost is not measured.
- **Deposit handling:** Chain Signatures is outbound only. It does not verify inbound deposits (per the plan). Deposit proofs still need Omni, Intents, or our own relayer.

---

## 6. Not verified

- Calling `sign` from a deployed contract and receiving the result in a callback. Source-based only (no contract deployed, as per the rules).
- Yield timeout length and the `fail_on_timeout` path.
- Mainnet latency and cost.
- Latency under load or across different times of day. n = 3.
- Signatures over real Solana transaction messages or real EVM transaction hashes. Only a 32-byte probe payload was signed.
- Whether the testnet deployment matches source commit `01f33ed0` exactly. The observed behaviour and the `Edwards25519` naming suggest it is at least close, but this was not confirmed.
- Who pays the MPC-side resume gas: inferred from the receipt tree and the balance reconciliation. Consistent, not confirmed by NEAR documentation.

---

## 7. Reproduce

```
cd /Users/abc/WindSwapV4/tools/chainsig-probe
npm install
node derive.mjs                     # derived keys and addresses (no signer)
./sign.sh evm 1                     # one ECDSA sign (repeat for runs 2, 3)
./sign.sh sol 1                     # one Ed25519 sign
node verify.mjs                     # local verification of all out/sign_*.txt
node txcost.mjs <tx hash> windswapv4-user.testnet
node chainlatency.mjs <tx hash>
```

Requires `near` (near-cli-rs 0.30.1 in `~/.cargo/bin`) and the legacy keychain entry for `windswapv4-user.testnet`. The first ECDSA run was sent inline with the same command as `sign.sh` (its output is `out/sign_evm_1.txt`).

Files:
- `tools/chainsig-probe/common.mjs`: RPC helper, payload, constants
- `tools/chainsig-probe/derive.mjs`: derived keys and addresses to `out/derived.json`
- `tools/chainsig-probe/sign.sh`: one sign call with timing
- `tools/chainsig-probe/verify.mjs`: local verification to `out/verify.json`
- `tools/chainsig-probe/txcost.mjs`: receipt tree cost
- `tools/chainsig-probe/chainlatency.mjs`: on-chain latency
- `tools/chainsig-probe/out/`: raw CLI output, derived keys, verification and cost results
