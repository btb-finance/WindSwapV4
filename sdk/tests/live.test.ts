import { describe, expect, it } from "vitest";
import { WindSwapClient } from "../src/index.js";

// Read-only calls against the live testnet deployment. Runs only with WINDSWAP_LIVE=1.
const LIVE = process.env.WINDSWAP_LIVE === "1";

describe.skipIf(!LIVE)("live testnet (WINDSWAP_LIVE=1)", () => {
  const client = new WindSwapClient();
  const TUSDC = "windswapv4-tusdc.testnet";
  const TWETH = "windswapv4-tweth.testnet";

  it("getPool(1) returns the tUSDC/tWETH pool", async () => {
    const pool = await client.getPool(1);
    expect(pool.pool_id).toBe(1);
    expect([pool.token0, pool.token1].sort()).toEqual([TUSDC, TWETH].sort());
    expect(pool.initialized).toBe(true);
    expect(pool.tick_spacing).toBe(3);
    expect(BigInt(pool.slot0.sqrt_price_x96)).toBeGreaterThan(0n);
  });

  it("quoteExactInput returns a positive output for 1 token through pool 1", async () => {
    const out = await client.quoteExactInput([{ pool_id: 1, token_in: TUSDC }], 1_000_000_000_000_000_000n);
    expect(BigInt(out)).toBeGreaterThan(0n);
    console.log(`live quoteExactInput(pool 1, 1 tUSDC) = ${out}`);
  });
});
