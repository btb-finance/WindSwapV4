import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { freshState, loadState, saveState, statePath } from "../src/state.js";

describe("cursor persistence", () => {
  let dir: string;

  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), "relayer-state-"));
  });

  afterEach(async () => {
    await rm(dir, { recursive: true, force: true });
  });

  it("returns null when no state file exists", async () => {
    expect(await loadState(dir, "base-sepolia")).toBeNull();
  });

  it("round-trips the cursor and quarantine list", async () => {
    const state = { ...freshState("base-sepolia", 84532, 100), nextBlock: 250 };
    state.quarantined.push({
      chainId: 84532,
      txHash: ("0x" + "a".repeat(64)) as `0x${string}`,
      logIndex: 0,
      asset: "native",
      amount: "5",
      from: ("0x" + "2".repeat(40)) as `0x${string}`,
      blockNumber: "120",
      error: "panicked",
    });
    await saveState(dir, state);
    const loaded = await loadState(dir, "base-sepolia");
    expect(loaded?.nextBlock).toBe(250);
    expect(loaded?.quarantined).toHaveLength(1);
    expect(loaded?.updatedAt).not.toBe(state.updatedAt);
  });

  it("writes atomically (no temp file left behind)", async () => {
    await saveState(dir, freshState("base-sepolia", 84532, 100));
    const raw = await readFile(statePath(dir, "base-sepolia"), "utf8");
    expect(JSON.parse(raw).nextBlock).toBe(100);
    await expect(readFile(`${statePath(dir, "base-sepolia")}.tmp`, "utf8")).rejects.toThrow();
  });

  it("refuses a corrupt state file instead of silently restarting", async () => {
    await writeFile(statePath(dir, "base-sepolia"), '{"nextBlock":"x"}', "utf8");
    await expect(loadState(dir, "base-sepolia")).rejects.toThrow(/corrupt/);
  });
});
