import { describe, expect, it } from "vitest";
import { DailyGasBudget, TokenBucket } from "../src/relay/limits.js";

describe("TokenBucket", () => {
  it("allows a burst up to capacity, then refuses with a retry delay", () => {
    const b = new TokenBucket({ capacity: 3, refillPerSecond: 1 }, 100);
    const t = 1_000_000;
    expect([b.take("ip", t), b.take("ip", t), b.take("ip", t)]).toEqual([0, 0, 0]);
    expect(b.take("ip", t)).toBe(1);
  });

  it("refills over time and keys are independent", () => {
    const b = new TokenBucket({ capacity: 1, refillPerSecond: 0.5 }, 100);
    expect(b.take("a", 0)).toBe(0);
    expect(b.take("a", 0)).toBe(2);
    expect(b.take("a", 1_999)).toBeGreaterThan(0);
    expect(b.take("a", 2_000)).toBe(0);
    expect(b.take("b", 0)).toBe(0);
  });

  it("evicts the least recently used key when full", () => {
    const b = new TokenBucket({ capacity: 1, refillPerSecond: 0 }, 2);
    b.take("a", 0);
    b.take("b", 0);
    b.take("c", 0);
    // "a" was evicted, so it gets a fresh bucket.
    expect(b.take("a", 0)).toBe(0);
  });
});

describe("DailyGasBudget", () => {
  it("reserves until the daily limit, then resets at the next UTC day", () => {
    const budget = new DailyGasBudget(600);
    const day1 = Date.UTC(2026, 9, 23, 12);
    expect(budget.reserve(300, day1)).toBe(true);
    expect(budget.reserve(300, day1)).toBe(true);
    expect(budget.reserve(1, day1)).toBe(false);
    expect(budget.usedToday(day1)).toBe(600);
    expect(budget.reserve(300, Date.UTC(2026, 9, 24, 0, 0, 1))).toBe(true);
  });
});
