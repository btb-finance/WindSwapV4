import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { getSqrtRatioAtTick, getTickAtSqrtRatio, MAX_SQRT_RATIO, MAX_TICK, MIN_SQRT_RATIO, MIN_TICK } from "../src/index.js";

// Repo root is two levels above this package: <root>/sdk/tests -> <root>/diff-tests/vectors
const VECTORS = fileURLToPath(new URL("../../diff-tests/vectors/", import.meta.url));

function readLines(name: string): string[][] {
  return readFileSync(`${VECTORS}${name}`, "utf8")
    .split("\n")
    .filter((line) => line.trim() !== "")
    .map((line) => line.split("|"));
}

describe("TickMath vs diff-tests vectors (exact)", () => {
  it("getSqrtRatioAtTick matches every ok line and throws on every err line", () => {
    const rows = readLines("sqrt_ratio_at_tick.txt");
    const failures: string[] = [];
    let ok = 0;
    let err = 0;
    for (const [input, status, output] of rows) {
      const tick = Number(input);
      if (status === "ok") {
        ok++;
        const got = getSqrtRatioAtTick(tick);
        if (got !== BigInt(output!)) failures.push(`tick ${input}: got ${got}, want ${output}`);
      } else if (status === "err") {
        err++;
        let threw = false;
        try {
          getSqrtRatioAtTick(tick);
        } catch {
          threw = true;
        }
        if (!threw) failures.push(`tick ${input}: expected error`);
      } else {
        failures.push(`unknown status line: ${input}|${status}`);
      }
    }
    expect(failures).toEqual([]);
    expect(ok).toBeGreaterThan(1000);
    expect(err).toBeGreaterThan(0);
  });

  it("getTickAtSqrtRatio matches every ok line and throws on every err line", () => {
    const rows = readLines("tick_at_sqrt_ratio.txt");
    const failures: string[] = [];
    let ok = 0;
    let err = 0;
    for (const [input, status, output] of rows) {
      if (status === "ok") {
        ok++;
        const got = getTickAtSqrtRatio(BigInt(input));
        if (got !== Number(output)) failures.push(`sqrt ${input}: got ${got}, want ${output}`);
      } else if (status === "err") {
        err++;
        let threw = false;
        try {
          getTickAtSqrtRatio(BigInt(input));
        } catch {
          threw = true;
        }
        if (!threw) failures.push(`sqrt ${input}: expected error`);
      } else {
        failures.push(`unknown status line: ${input}|${status}`);
      }
    }
    expect(failures).toEqual([]);
    expect(ok).toBeGreaterThan(1000);
    expect(err + ok).toBe(rows.length);
  });

  it("boundary values", () => {
    expect(getSqrtRatioAtTick(MIN_TICK)).toBe(MIN_SQRT_RATIO);
    expect(getSqrtRatioAtTick(MAX_TICK)).toBe(MAX_SQRT_RATIO);
    expect(getSqrtRatioAtTick(0)).toBe(1n << 96n);
    expect(() => getSqrtRatioAtTick(MAX_TICK + 1)).toThrow(RangeError);
    expect(() => getSqrtRatioAtTick(1.5)).toThrow(RangeError);
    expect(getTickAtSqrtRatio(MIN_SQRT_RATIO)).toBe(MIN_TICK);
    expect(getTickAtSqrtRatio(MAX_SQRT_RATIO - 1n)).toBe(MAX_TICK - 1);
    expect(() => getTickAtSqrtRatio(MAX_SQRT_RATIO)).toThrow(RangeError);
    expect(() => getTickAtSqrtRatio(MIN_SQRT_RATIO - 1n)).toThrow(RangeError);
  });
});
