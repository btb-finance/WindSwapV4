import { describe, expect, it } from "vitest";
import { RelayError } from "../src/relay/errors.js";
import { checkAction, SIGNED_ACTION_FIELDS } from "../src/relay/schema.js";

const ALL = "*" as const;

/** One valid example per variant, in declaration order. */
const EXAMPLES: Record<string, Record<string, unknown>> = {
  Swap: { Swap: { pool_id: 1, zero_for_one: true, amount_specified: "-1000", sqrt_price_limit_x96: "79228162514264337593543950336" } },
  SwapExactInput: { SwapExactInput: { path: [{ pool_id: 1, token_in: "windswapv4-tusdc.testnet" }], amount_in: "100", min_amount_out: "0" } },
  MintPosition: { MintPosition: { pool_id: 1, tick_lower: -60, tick_upper: 60, liquidity: "5" } },
  BurnPosition: { BurnPosition: { pool_id: 1, tick_lower: -60, tick_upper: 60, liquidity: "5" } },
  Collect: { Collect: { pool_id: 1, tick_lower: -60, tick_upper: 60 } },
  WithdrawEvm: { WithdrawEvm: { token: "base-sepolia:eth", amount: "1000", to_chain_id: 84532, to_address: "0x" + "11".repeat(20) } },
  CreateLock: { CreateLock: { amount: "1000", duration: 604800 } },
  IncreaseAmount: { IncreaseAmount: { token_id: 1, amount: "1000" } },
  IncreaseUnlockTime: { IncreaseUnlockTime: { token_id: 1, duration: 604800 } },
  WithdrawLock: { WithdrawLock: { token_id: 1 } },
  Merge: { Merge: { from: 1, to: 2 } },
  Split: { Split: { token_id: 1, amount: "10" } },
  LockPermanent: { LockPermanent: { token_id: 1 } },
  UnlockPermanent: { UnlockPermanent: { token_id: 1 } },
  Delegate: { Delegate: { delegator: 1, delegatee: 2 } },
  Vote: { Vote: { token_id: 1, pools: [4, 1], weights: ["70", "30"] } },
  Reset: { Reset: { token_id: 1 } },
  Poke: { Poke: { token_id: 1 } },
  StakePosition: { StakePosition: { pool_id: 1, tick_lower: -60, tick_upper: 60 } },
  UnstakePosition: { UnstakePosition: { pool_id: 1, tick_lower: -60, tick_upper: 60 } },
  ClaimGaugeRewards: { ClaimGaugeRewards: { pool_id: 1, tick_lower: -60, tick_upper: 60 } },
  ClaimFees: { ClaimFees: { token_id: 1, gauges: [1, 2] } },
  ClaimBribes: { ClaimBribes: { token_id: 1, gauges: [1], tokens: ["windswapv4-wind.testnet"] } },
  Bribe: { Bribe: { pool_id: 1, token: "windswapv4-wind.testnet", amount: "500" } },
  ClaimRebase: { ClaimRebase: { token_id: 1 } },
};

describe("checkAction", () => {
  it("covers every variant of SignedAction and accepts a valid example of each", () => {
    expect(Object.keys(EXAMPLES).sort()).toEqual(Object.keys(SIGNED_ACTION_FIELDS).sort());
    for (const [variant, action] of Object.entries(EXAMPLES)) {
      expect(checkAction(action, ALL)).toBe(variant);
    }
  });

  it("rejects an object whose fields are in another order", () => {
    const action = { Vote: { token_id: 1, weights: ["70", "30"], pools: [4, 1] } };
    expect(() => checkAction(action, ALL)).toThrow(/in that order/);
  });

  it("rejects missing and extra fields", () => {
    expect(() => checkAction({ Vote: { token_id: 1, pools: [4] } }, ALL)).toThrow(RelayError);
    expect(() => checkAction({ Vote: { token_id: 1, pools: [4], weights: ["1"], extra: 1 } }, ALL)).toThrow(RelayError);
  });

  it("rejects hop objects whose keys are out of order", () => {
    const action = { SwapExactInput: { path: [{ token_in: "a.testnet", pool_id: 1 }], amount_in: "1", min_amount_out: "0" } };
    expect(() => checkAction(action, ALL)).toThrow(/in that order/);
  });

  it("requires amounts as strings, not numbers", () => {
    expect(() => checkAction({ CreateLock: { amount: 1000, duration: 604800 } }, ALL)).toThrow(/decimal string/);
  });

  it("rejects non-canonical and out-of-range big numbers", () => {
    expect(() => checkAction({ CreateLock: { amount: "01", duration: 1 } }, ALL)).toThrow(/decimal string/);
    expect(() => checkAction({ CreateLock: { amount: "-1", duration: 1 } }, ALL)).toThrow(/decimal string/);
    const tooBig = ((1n << 128n)).toString();
    expect(() => checkAction({ CreateLock: { amount: tooBig, duration: 1 } }, ALL)).toThrow(/out of range/);
  });

  it("rejects integers outside the contract's types and unsafe JSON numbers", () => {
    expect(() => checkAction({ WithdrawLock: { token_id: -1 } }, ALL)).toThrow(/non-negative/);
    expect(() => checkAction({ WithdrawLock: { token_id: 2 ** 60 } }, ALL)).toThrow(/non-negative/);
    expect(() => checkAction({ CollectX: {} }, ALL)).toThrow(/unknown action variant/);
    expect(() => checkAction({ Collect: { pool_id: 1, tick_lower: 2 ** 31, tick_upper: 0 } }, ALL)).toThrow(/32-bit/);
  });

  it("rejects vectors longer than the limit", () => {
    const pools = Array.from({ length: 33 }, (_, i) => i);
    const weights = pools.map(() => "1");
    expect(() => checkAction({ Vote: { token_id: 1, pools, weights } }, ALL)).toThrow(/at most 32/);
  });

  it("rejects actions that are not a single-variant object", () => {
    expect(() => checkAction({ Reset: { token_id: 1 }, Poke: { token_id: 1 } }, ALL)).toThrow(/exactly one variant/);
    expect(() => checkAction([{ Reset: { token_id: 1 } }], ALL)).toThrow(/exactly one variant/);
    expect(() => checkAction("Reset", ALL)).toThrow(RelayError);
  });

  it("rejects unknown prototype keys such as toString", () => {
    expect(() => checkAction({ toString: {} }, ALL)).toThrow(/unknown action variant/);
  });

  it("enforces the allow-list with a 403", () => {
    expect(checkAction({ Vote: { token_id: 1, pools: [], weights: [] } }, ["Vote"])).toBe("Vote");
    try {
      checkAction({ Reset: { token_id: 1 } }, ["Vote"]);
      expect.unreachable();
    } catch (err) {
      expect(err).toMatchObject({ status: 403 });
    }
  });
});

describe("Distribute and permanent CreateLock (contract v7)", () => {
  it("accepts the unit variant Distribute as a bare string", () => {
    expect(checkAction("Distribute", "*")).toBe("Distribute");
    // serde serialises a unit variant as a JSON string, so the signed text is "\"Distribute\"".
    expect(JSON.stringify("Distribute")).toBe('"Distribute"');
  });

  it("rejects unknown strings and the object form of Distribute", () => {
    expect(() => checkAction("Nope", "*")).toThrow(RelayError);
    expect(() => checkAction({ Distribute: {} }, "*")).toThrow(RelayError);
  });

  it("respects the allow-list for Distribute", () => {
    try {
      checkAction("Distribute", ["Swap"]);
      throw new Error("expected 403");
    } catch (e) {
      expect((e as RelayError).status).toBe(403);
    }
  });

  it("accepts CreateLock with permanent omitted or true, rejects false and wrong order", () => {
    expect(checkAction({ CreateLock: { amount: "1", duration: 604800 } }, "*")).toBe("CreateLock");
    expect(checkAction({ CreateLock: { amount: "1", duration: 604800, permanent: true } }, "*")).toBe("CreateLock");
    expect(() => checkAction({ CreateLock: { amount: "1", duration: 604800, permanent: false } }, "*")).toThrow(RelayError);
    expect(() => checkAction({ CreateLock: { permanent: true, amount: "1", duration: 604800 } }, "*")).toThrow(RelayError);
  });
});
