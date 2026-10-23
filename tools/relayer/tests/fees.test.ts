import { describe, expect, it } from "vitest";
import { computeFees, FeeCapError } from "../src/fees.js";

const GWEI = 1_000_000_000n;

describe("computeFees", () => {
  it("uses 2 * base + priority when under the cap", () => {
    expect(computeFees({ baseFeePerGas: 10n * GWEI, priorityFeePerGas: GWEI, capPerGas: 100n * GWEI })).toEqual({
      maxFeePerGas: 21n * GWEI,
      maxPriorityFeePerGas: GWEI,
    });
  });

  it("clamps max fee to the contract cap", () => {
    expect(computeFees({ baseFeePerGas: 10n * GWEI, priorityFeePerGas: GWEI, capPerGas: 15n * GWEI })).toEqual({
      maxFeePerGas: 15n * GWEI,
      maxPriorityFeePerGas: GWEI,
    });
  });

  it("clamps priority fee to max fee", () => {
    expect(computeFees({ baseFeePerGas: 0n, priorityFeePerGas: GWEI, capPerGas: GWEI / 2n })).toEqual({
      maxFeePerGas: GWEI / 2n,
      maxPriorityFeePerGas: GWEI / 2n,
    });
  });

  it("throws when the cap is below the base fee", () => {
    expect(() => computeFees({ baseFeePerGas: 10n * GWEI, priorityFeePerGas: GWEI, capPerGas: 5n * GWEI })).toThrow(
      FeeCapError,
    );
  });

  it("rejects non-positive caps and negative inputs", () => {
    expect(() => computeFees({ baseFeePerGas: 1n, priorityFeePerGas: 1n, capPerGas: 0n })).toThrow(FeeCapError);
    expect(() => computeFees({ baseFeePerGas: -1n, priorityFeePerGas: 1n, capPerGas: 5n })).toThrow(FeeCapError);
  });
});
