import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { runDepositPass } from "../src/watcher.js";
import type { Hex } from "../src/types.js";
import {
  block,
  chainCfg,
  mockNear,
  mockReader,
  mockWriter,
  OWNER_HEX,
  recordingLog,
  relayerCfg,
  SENDER,
  VAULT,
} from "./helpers.js";

const H = (c: string) => ("0x" + c.repeat(64)) as Hex;

describe("deposit watcher: intent status after a reported deposit", () => {
  let dir: string;

  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), "relayer-intent-watch-"));
  });

  afterEach(async () => {
    await rm(dir, { recursive: true, force: true });
  });

  /** One native deposit from SENDER in block 103, scanned with confirmations 2. */
  function depositReader() {
    return mockReader({
      head: 110n,
      blocks: { "103": block(103n, [{ hash: H("a"), from: SENDER, to: VAULT, value: 7n, transactionIndex: 0 }]) },
      logs: [],
    });
  }

  it("resolves the depositor's owner, reads its intents and logs status changes once", async () => {
    const chain = chainCfg({ startBlock: 100, confirmations: 2, scanBatchSize: 5 });
    const near = mockNear({
      intents: [{ id: 7, status: "Executed", owner_hex: OWNER_HEX }],
    });
    const writer = mockWriter();
    const logs: Array<{ msg: string; fields?: Record<string, unknown> }> = [];
    const log = { ...recordingLog(), info: (msg: string, fields?: Record<string, unknown>) => logs.push({ msg, fields }) };
    const intentStatus = new Map<number, string>();

    await runDepositPass(
      { config: relayerCfg(dir, chain), near, writer, log, shouldStop: () => false, intentStatus },
      chain,
      depositReader(),
      { dryRun: false },
    );

    expect(writer.reports).toHaveLength(1);
    expect(near.resolvedWallets).toEqual([`evm:${SENDER.toLowerCase()}`]);
    const statusLogs = logs.filter((l) => l.msg === "intent status");
    expect(statusLogs).toEqual([
      { msg: "intent status", fields: { intent_id: 7, status: "Executed", previous: null, owner: OWNER_HEX } },
    ]);
    expect(intentStatus.get(7)).toBe("Executed");
  });

  it("does not change the deposit outcome when reading intents fails", async () => {
    const chain = chainCfg({ startBlock: 100, confirmations: 2, scanBatchSize: 5 });
    const near = mockNear();
    near.getIntentsOfOwner = async () => {
      throw new Error("NEAR RPC HTTP 500 for get_intents_of_owner");
    };
    const writer = mockWriter();
    const log = recordingLog();

    const stats = await runDepositPass(
      { config: relayerCfg(dir, chain), near, writer, log, shouldStop: () => false },
      chain,
      depositReader(),
      { dryRun: false },
    );

    expect(stats.reported).toBe(1);
    expect(log.warns).toEqual(["could not read intents after deposit"]);
    expect(log.errors).toHaveLength(0);
  });
});
