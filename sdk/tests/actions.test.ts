import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { buildActions, DEFAULT_GAS, ONE_YOCTO } from "../src/index.js";

const CONTRACT = "windswapv4-btb.testnet";
const TOKEN = "windswapv4-tusdc.testnet";
const LIB_RS = fileURLToPath(new URL("../../crates/near-contract/src/lib.rs", import.meta.url));

/** Parameter names (without self) of every `pub fn` in the contract source, by method name. */
function contractSignatures(): Map<string, string[]> {
  const src = readFileSync(LIB_RS, "utf8");
  const sigs = new Map<string, string[]>();
  const re = /pub fn (\w+)\s*\(([\s\S]*?)\)\s*(?:->|\{|$)/gm;
  for (const m of src.matchAll(re)) {
    const params = m[2]!
      .split(",")
      .map((p) => p.trim())
      .filter((p) => p !== "" && !p.startsWith("&"))
      .map((p) => p.split(":")[0]!.trim());
    sigs.set(m[1]!, params);
  }
  return sigs;
}

const actions = buildActions(CONTRACT);
const sigs = contractSignatures();

describe("action builders match the contract ABI", () => {
  it("every change method exists in lib.rs with exactly the argument names the builder sends", () => {
    const cases: [string, Record<string, unknown>][] = [
      ["create_pool", actions.createPool({ token0: TOKEN, token1: "windswapv4-tweth.testnet", tickSpacing: 3, sqrtPriceX96: "79228162514264337593543950336" }).args],
      ["add_token", actions.addToken({ token: TOKEN }).args],
      ["remove_token", actions.removeToken({ token: TOKEN }).args],
      ["withdraw", actions.withdraw({ token: TOKEN, amount: 1n }).args],
      ["mint_position", actions.mintPosition({ poolId: 1, tickLower: -600, tickUpper: 600, liquidity: 1n }).args],
      ["burn_position", actions.burnPosition({ poolId: 1, tickLower: -600, tickUpper: 600, liquidity: 1n }).args],
      ["collect", actions.collect({ poolId: 1, tickLower: -600, tickUpper: 600 }).args],
      ["swap", actions.swap({ poolId: 1, zeroForOne: true, amountSpecified: 1n, sqrtPriceLimitX96: 1n }).args],
      ["swap_exact_input", actions.swapExactInput({ path: [{ poolId: 1, tokenIn: TOKEN }], amountIn: 1n, minAmountOut: 0n }).args],
      ["swap_exact_output", actions.swapExactOutput({ path: [{ poolId: 1, tokenIn: TOKEN }], amountOut: 1n, maxAmountIn: 2n }).args],
      ["enable_tick_spacing", actions.enableTickSpacing({ tickSpacing: 7, fee: 500 }).args],
      ["set_custom_swap_fee", actions.setCustomSwapFee({ poolId: 1, fee: 0 }).args],
      ["set_custom_unstaked_fee", actions.setCustomUnstakedFee({ poolId: 1, fee: 0 }).args],
      ["set_fee_whitelist", actions.setFeeWhitelist({ accountId: "a.testnet", whitelisted: true }).args],
      ["test_credit", actions.testCredit({ accountId: "a.testnet", token: TOKEN, amount: 1n }).args],
    ];
    for (const [method, args] of cases) {
      expect(sigs.has(method), `${method} is not a pub fn in lib.rs`).toBe(true);
      expect(Object.keys(args).sort(), method).toEqual([...sigs.get(method)!].sort());
    }
  });

  it("the view methods used by the client exist in lib.rs", () => {
    for (const m of ["get_pool", "get_position", "get_balance", "get_tokens", "get_fee", "quote_exact_input", "quote_exact_output"]) {
      expect(sigs.has(m), m).toBe(true);
    }
  });
});

describe("action builder JSON", () => {
  it("swapExactInput sends the path in the contract's HopInput shape with decimal-string amounts", () => {
    const a = actions.swapExactInput({
      path: [
        { poolId: 1, tokenIn: TOKEN },
        { poolId: 2, tokenIn: "windswapv4-tweth.testnet" },
      ],
      amountIn: 1_000_000_000_000_000_000n,
      minAmountOut: "974000000000000000",
    });
    expect(a).toEqual({
      receiverId: CONTRACT,
      methodName: "swap_exact_input",
      args: {
        path: [
          { pool_id: 1, token_in: TOKEN },
          { pool_id: 2, token_in: "windswapv4-tweth.testnet" },
        ],
        amount_in: "1000000000000000000",
        min_amount_out: "974000000000000000",
      },
      gas: DEFAULT_GAS,
      deposit: "0",
    });
    // The wire format is JSON: amounts must be strings, not numbers.
    expect(JSON.parse(JSON.stringify(a.args))).toEqual(a.args);
    expect(typeof JSON.parse(JSON.stringify(a.args)).amount_in).toBe("string");
  });

  it("swapExactOutput uses amount_out and max_amount_in", () => {
    const a = actions.swapExactOutput({ path: [{ poolId: 1, tokenIn: TOKEN }], amountOut: 1n, maxAmountIn: "2" });
    expect(a.methodName).toBe("swap_exact_output");
    expect(a.args).toEqual({ path: [{ pool_id: 1, token_in: TOKEN }], amount_out: "1", max_amount_in: "2" });
    expect(a.gas).toBe("300000000000000");
    expect(a.deposit).toBe("0");
  });

  it("deposit is ft_transfer_call on the token contract, to the DEX, with 1 yocto", () => {
    const a = actions.deposit({ token: TOKEN, amount: 100_000_000_000_000_000_000n });
    expect(a).toEqual({
      receiverId: TOKEN,
      methodName: "ft_transfer_call",
      args: { receiver_id: CONTRACT, amount: "100000000000000000000", msg: "" },
      gas: DEFAULT_GAS,
      deposit: ONE_YOCTO,
    });
    expect(ONE_YOCTO).toBe("1");
  });

  it("deposit passes a custom msg through", () => {
    expect(actions.deposit({ token: TOKEN, amount: 1n, msg: "hi" }).args.msg).toBe("hi");
  });

  it("withdraw is a 1-yocto call to the DEX", () => {
    const a = actions.withdraw({ token: TOKEN, amount: 10n });
    expect(a).toEqual({
      receiverId: CONTRACT,
      methodName: "withdraw",
      args: { token: TOKEN, amount: "10" },
      gas: DEFAULT_GAS,
      deposit: "1",
    });
  });

  it("zero-deposit change methods use deposit 0 and 300 TGas by default", () => {
    const calls = [
      actions.mintPosition({ poolId: 0, tickLower: -300, tickUpper: 300, liquidity: "100000000000000000000" }),
      actions.burnPosition({ poolId: 0, tickLower: -300, tickUpper: 300, liquidity: 1 }),
      actions.collect({ poolId: 0, tickLower: -300, tickUpper: 300 }),
      actions.swap({ poolId: 0, zeroForOne: false, amountSpecified: "-5", sqrtPriceLimitX96: "1461446703485210103287273052203988822378723970341" }),
      actions.createPool({ token0: TOKEN, token1: TOKEN, tickSpacing: 3, sqrtPriceX96: 1n }),
    ];
    for (const c of calls) {
      expect(c.receiverId).toBe(CONTRACT);
      expect(c.deposit).toBe("0");
      expect(c.gas).toBe("300000000000000");
    }
  });

  it("mintPosition and swap args are exact", () => {
    expect(actions.mintPosition({ poolId: 0, tickLower: -300, tickUpper: 300, liquidity: "100000000000000000000" }).args).toEqual({
      pool_id: 0,
      tick_lower: -300,
      tick_upper: 300,
      liquidity: "100000000000000000000",
    });
    expect(
      actions.swap({ poolId: 0, zeroForOne: false, amountSpecified: "-5", sqrtPriceLimitX96: "4295128740" }).args,
    ).toEqual({ pool_id: 0, zero_for_one: false, amount_specified: "-5", sqrt_price_limit_x96: "4295128740" });
  });

  it("a custom gas limit overrides the default", () => {
    expect(actions.collect({ poolId: 0, tickLower: 0, tickUpper: 60 }, { gas: 100n }).gas).toBe("100");
  });

  it("rejects out-of-range inputs before building an action", () => {
    expect(() => actions.withdraw({ token: TOKEN, amount: -1n })).toThrow(RangeError);
    expect(() => actions.withdraw({ token: TOKEN, amount: 1n << 128n })).toThrow(RangeError);
    expect(() => actions.swapExactInput({ path: [], amountIn: 1n, minAmountOut: 0n })).toThrow(RangeError);
    expect(() => actions.mintPosition({ poolId: 0, tickLower: 2 ** 31, tickUpper: 0, liquidity: 1n })).toThrow(RangeError);
    expect(() => actions.swap({ poolId: 0, zeroForOne: true, amountSpecified: 1n << 255n, sqrtPriceLimitX96: 1n })).toThrow(RangeError);
  });
});
