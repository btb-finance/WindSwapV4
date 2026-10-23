# @windswap/sdk

TypeScript SDK for the Wind Swap v4 NEAR contract (`crates/near-contract`).

- **Math**: exact BigInt ports of Uniswap v3 `TickMath` and `LiquidityAmounts`, price and tick conversions, slippage helpers. The TickMath port is checked against `diff-tests/vectors`.
- **Views**: read-only JSON-RPC calls (`get_pool`, `get_position`, `get_balance`, `get_tokens`, `get_fee`, quotes). Plain `fetch`, no runtime dependencies.
- **Actions**: builders for every change method. They return FunctionCall descriptions that any NEAR wallet library can sign. The SDK never signs or sends transactions.

Runtime dependencies: none. Requires Node 18+ (or any runtime with `fetch`, `TextEncoder` and `btoa`).

## Install and build

```bash
cd sdk
npm install
npm run build   # tsc -> dist/ (ESM + .d.ts)
npm test        # vitest
```

Live testnet check (read-only, skipped by default):

```bash
WINDSWAP_LIVE=1 npx vitest run tests/live.test.ts
```

## Conventions

- Values wider than 2^53 (U128, U256, I256) are **decimal strings** on the wire. Pass `bigint` or string into the SDK. Views return strings. Convert with `BigInt(x)`.
- Pool ids, ticks and fees are numbers.
- Prices: `price` means token1 per token0 in whole-token units. Pass `decimals0` and `decimals1` for both tokens.
- Fees are in hundredths of a bip (`300` = 0.03%). `420` means a 0% fee.
- Testnet tokens use 18 decimals in the current deployment.

## Math

```ts
import {
  priceToSqrtPriceX96, sqrtPriceX96ToPrice, tickToPrice, priceToTick,
  nearestUsableTick, getSqrtRatioAtTick, getTickAtSqrtRatio,
  getLiquidityForAmounts, getAmountsForLiquidity,
  minAmountOut, maxAmountIn, defaultSqrtPriceLimit,
} from "@windswap/sdk";

// Initial price 2000 tWETH per tUSDC (6 and 18 decimals respectively)
const sqrtPriceX96 = priceToSqrtPriceX96("2000", 6, 18);

// Tick range around the current price, aligned to tick spacing 3
const tick = priceToTick("2000", 6, 18);
const lower = nearestUsableTick(tick - 600, 3);
const upper = nearestUsableTick(tick + 600, 3);

// How much liquidity 100 tUSDC + 0.05 tWETH buys in that range
const liquidity = getLiquidityForAmounts(
  sqrtPriceX96,
  getSqrtRatioAtTick(lower),
  getSqrtRatioAtTick(upper),
  100_000_000n,                    // amount0 in base units (6 decimals)
  50_000_000_000_000_000n,         // amount1 in base units (18 decimals)
);

// Amounts a given liquidity needs (rounded down, as on chain)
const { amount0, amount1 } = getAmountsForLiquidity(
  sqrtPriceX96, getSqrtRatioAtTick(lower), getSqrtRatioAtTick(upper), liquidity,
);

minAmountOut(amount1, 50);   // 0.5% slippage floor for an exact-input swap output
maxAmountIn(amount1, 50);    // 0.5% ceiling for an exact-output swap input
defaultSqrtPriceLimit(true); // sqrt price limit that never stops a swap
```

## Views

```ts
import { WindSwapClient } from "@windswap/sdk";

const client = new WindSwapClient({
  contractId: "windswapv4-btb.testnet",     // default
  rpcUrl: "https://rpc.testnet.near.org",   // default
});

const pool = await client.getPool(1);       // PoolView
const fee = await client.getFee(1, "alice.testnet");  // 300
const tokens = await client.getTokens();    // ["windswapv4-tusdc.testnet", ...]
const balance = await client.getBalance("alice.testnet", "windswapv4-tusdc.testnet");

const path = [
  { pool_id: 1, token_in: "windswapv4-tusdc.testnet" },
  { pool_id: 2, token_in: "windswapv4-tweth.testnet" },
];
const out = await client.quoteExactInput(path, 10n ** 18n);   // decimal string
const input = await client.quoteExactOutput(path, 10n ** 18n);
```

Views throw `WindSwapError` on an RPC error or a contract panic (for example `pool 9 does not exist`).

## Change methods (actions)

`client.actions` (or `buildActions(contractId)`) returns descriptions like this:

```ts
{
  receiverId: "windswapv4-btb.testnet",
  methodName: "swap_exact_input",
  args: {
    path: [{ pool_id: 1, token_in: "windswapv4-tusdc.testnet" }, { pool_id: 2, token_in: "windswapv4-tweth.testnet" }],
    amount_in: "1000000000000000000",
    min_amount_out: "974000000000000000",
  },
  gas: "300000000000000",   // 300 TGas
  deposit: "0",
}
```

Defaults: gas 300 TGas (unused gas is refunded), deposit `"0"`. `withdraw` and `deposit` use `"1"` (1 yocto).

Builders: `createPool`, `addToken`, `removeToken`, `deposit`, `withdraw`, `mintPosition`, `burnPosition`, `collect`, `swap`, `swapExactInput`, `swapExactOutput`, `enableTickSpacing`, `setCustomSwapFee`, `setCustomUnstakedFee`, `setFeeWhitelist`, `testCredit`.

Owner-only methods (`createPool`, `addToken`, `removeToken`, `enableTickSpacing`, `setCustomSwapFee`, `setCustomUnstakedFee`, `setFeeWhitelist`, `testCredit`) only succeed when called by the contract owner. `testCredit` exists only in testnet builds.

`deposit` is `ft_transfer_call` on the token contract. Its `receiverId` is the token, and `args.receiver_id` is the DEX. The DEX credits the internal balance in `ft_on_transfer`.

### Signing with your wallet library

Each action maps directly onto a function call. With near-api-js, for example:

```ts
import { WindSwapClient } from "@windswap/sdk";

const client = new WindSwapClient();
const a = client.actions.deposit({ token: "windswapv4-tusdc.testnet", amount: 100n * 10n ** 18n });
// account.functionCall({
//   contractId: a.receiverId, methodName: a.methodName, args: a.args,
//   gas: a.gas, attachedDeposit: a.deposit,
// });
```

Check your wallet library's exact parameter names. The SDK only produces the call description.

## Notes

- Quotes use the fee of a non-whitelisted account. A whitelisted account pays a 0 swap fee, so its real swap can return more than the quote.
- `swapExactInput` and `swapExactOutput` are the multi-hop entry points. `swap` is the single-pool engine call with a sqrt price limit.
- The contract has no gauges yet, so unstaked fees do not change swap outputs.
