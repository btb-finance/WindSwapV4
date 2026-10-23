# WIND token (NEP-141 on NEAR)

Crate: `crates/wind-token`. Governance token for Wind Swap v4 on NEAR. Decision and rationale: `docs/plan.md`, section "The WIND Token".

## Properties

- NEP-141 core, NEP-145 storage management, NEP-148 metadata (`near-contract-standards` 5.27.0).
- Name `Wind`, symbol `WIND`, 18 decimals, spec `ft-1.0.0`, no icon.
- Mintable only by `minter_id`. The owner cannot mint.
- No pause, no blacklist, no freeze. This is by design: transfers can never be stopped.

## API

| Method | Caller | Behaviour |
|---|---|---|
| `new(owner_id, minter_id)` | deployer (init) | Zero supply. Metadata is fixed as above. |
| `mint(account_id, amount)` | minter only | Registers `account_id` if needed, then credits `amount` and emits NEP-141 `ft_mint`. Amount must be > 0. |
| `burn(amount)` | any holder | Burns the caller's own tokens, reduces total supply, emits `ft_burn`. |
| `set_minter(new_minter)` | owner only | Replaces the minter. |
| `set_owner(new_owner)` | owner only | Replaces the owner. |
| `owner()`, `minter()` | view | Current roles. |
| `ft_transfer`, `ft_transfer_call`, `ft_balance_of`, `ft_total_supply`, `ft_metadata`, `storage_*` | NEP-141 / 145 / 148 | Standard. |

### Storage on mint

`mint` registers an unregistered recipient. The storage cost is paid from the WIND contract's NEAR balance, not by the minter. Fund the contract account for storage. Holders who want to register themselves can call `storage_deposit`.

### Trust model

- The minter must be the Wind Swap v4 contract account. No human key can mint.
- The owner can rotate the minter with `set_minter`. Set `owner_id` to a community-controlled governance account before mainnet. Do not use a single EOA.

## Build

```
cd crates/wind-token
RUSTUP_TOOLCHAIN=1.86.0 cargo near build non-reproducible-wasm --no-abi
# output: target/near/wind_token/wind_token.wasm
```

## Testnet deployment (not yet done)

The account `wind.windswapv4-btb.testnet` does not exist yet. Command shapes with placeholders, using near-cli-rs:

```
# 1. Create the account (placeholders: <ACCOUNT>, <SPONSOR>)
near account create-account fund-myself wind.windswapv4-btb.testnet '<AMOUNT> NEAR' \
  autogenerate-new-keypair save-to-keychain sign-as <SPONSOR> network-config testnet sign-with-keychain send

# 2. Deploy and initialise (placeholders: <OWNER>, <MINTER>)
near contract deploy wind.windswapv4-btb.testnet use-file target/near/wind_token/wind_token.wasm \
  with-init-call new json-args '{"owner_id":"<OWNER>","minter_id":"<MINTER>"}' \
  prepaid-gas '100.0 Tgas' attached-deposit '0 NEAR' \
  network-config testnet sign-with-keychain send
```

`<MINTER>` must be the Wind Swap v4 contract account on the same network. Do not deploy with a placeholder minter.

## Tests

```
cargo test -q -p wind-token
```

Covers init and metadata, minter-only mint (owner and random accounts rejected), mint registration and `ft_mint` event, burn with `ft_burn` event, owner-only admin and minter rotation, and transfers.
