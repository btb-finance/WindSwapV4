import { describe, expect, it } from "vitest";
import { planBroadcast, type SignedWithdrawal } from "../src/broadcast.js";
import { txHashOf } from "../src/evm.js";
import type { Hex } from "../src/types.js";

function entry(id: number, nonce: number): SignedWithdrawal {
  const rawTx = ("0x02" + id.toString(16).padStart(4, "0") + "ab".repeat(8)) as Hex;
  return { id, nonce, rawTx, txHash: txHashOf(rawTx) };
}

describe("planBroadcast", () => {
  it("returns a contiguous run starting at the pending nonce", () => {
    const plan = planBroadcast([entry(1, 5), entry(2, 6), entry(3, 7)], 5);
    expect(plan.ready.map((w) => w.nonce)).toEqual([5, 6, 7]);
    expect(plan.stale).toEqual([]);
    expect(plan.blocked).toEqual([]);
  });

  it("sorts input by nonce regardless of order", () => {
    const plan = planBroadcast([entry(3, 7), entry(1, 5), entry(2, 6)], 5);
    expect(plan.ready.map((w) => w.id)).toEqual([1, 2, 3]);
  });

  it("blocks everything behind a nonce gap", () => {
    const plan = planBroadcast([entry(1, 5), entry(2, 6), entry(3, 8), entry(4, 9)], 5);
    expect(plan.ready.map((w) => w.nonce)).toEqual([5, 6]);
    expect(plan.blocked.map((w) => w.nonce)).toEqual([8, 9]);
  });

  it("does not send anything when the pending nonce is ahead of the first signed nonce", () => {
    // pending = 7 means nonces 5 and 6 are already used (mined or in mempool)
    const plan = planBroadcast([entry(1, 5), entry(2, 6), entry(3, 7)], 7);
    expect(plan.stale.map((w) => w.nonce)).toEqual([5, 6]);
    expect(plan.ready.map((w) => w.nonce)).toEqual([7]);
  });

  it("flags duplicate nonces and sends nothing", () => {
    const plan = planBroadcast([entry(1, 5), entry(2, 5)], 5);
    expect(plan.problem).toMatch(/duplicate nonce 5/);
    expect(plan.ready).toEqual([]);
  });

  it("returns nothing for an empty set", () => {
    expect(planBroadcast([], 3)).toEqual({ stale: [], ready: [], blocked: [] });
  });
});
