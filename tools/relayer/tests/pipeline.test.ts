import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { runOnce } from "../src/runner.js";
import { runDepositPass } from "../src/watcher.js";
import { processPending, broadcastSigned } from "../src/withdrawals.js";
import { loadState, saveState, freshState, statePath } from "../src/state.js";
import { txHashOf } from "../src/evm.js";
import type { Hex } from "../src/types.js";
import {
  block,
  CHAIN_ID,
  chainCfg,
  mockNear,
  mockReader,
  mockWriter,
  NATIVE_ASSET_INFO,
  recordingLog,
  relayerCfg,
  SENDER,
  silentLog,
  transferLog,
  UNREGISTERED,
  USDC,
  USDC_ASSET_INFO,
  VAULT,
  withdrawal,
  type MockReader,
} from "./helpers.js";

const H = (c: string) => ("0x" + c.repeat(64)) as Hex;
const RAW = (n: number): Hex => ("0x02" + n.toString(16).padStart(2, "0") + "cd".repeat(16)) as Hex;

describe("deposit watcher", () => {
  let dir: string;

  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), "relayer-pipe-"));
  });

  afterEach(async () => {
    await rm(dir, { recursive: true, force: true });
  });

  function fixtureReader(): MockReader {
    return mockReader({
      head: 110n,
      blocks: {
        "103": block(103n, [{ hash: H("a"), from: SENDER, to: VAULT, value: 7n, transactionIndex: 0 }]),
      },
      logs: [
        transferLog({ token: USDC, value: 1_000_000n, blockNumber: 106n, txHash: H("b"), logIndex: 2 }),
        transferLog({ token: UNREGISTERED, value: 42n, blockNumber: 106n, txHash: H("c"), logIndex: 3 }),
      ],
    });
  }

  it("reports native and ERC-20 deposits, ignores unregistered tokens, advances the cursor", async () => {
    const cfg = chainCfg({ startBlock: 100, confirmations: 2, scanBatchSize: 5 });
    const config = relayerCfg(dir, cfg);
    const writer = mockWriter();
    const reader = fixtureReader();
    const stats = await runDepositPass(
      { config, near: mockNear(), writer, log: silentLog, shouldStop: () => false },
      cfg,
      reader,
      { dryRun: false },
    );

    expect(writer.reports).toEqual([
      { chain_id: CHAIN_ID, tx_hash: H("a"), log_index: 0, from: SENDER, asset: "native", amount: "7" },
      { chain_id: CHAIN_ID, tx_hash: H("b"), log_index: 2, from: SENDER, asset: USDC.toLowerCase(), amount: "1000000" },
    ]);
    expect(stats.ignoredTransfers).toBe(1);
    expect(stats.caughtUp).toBe(true);
    // head 110, confirmations 2 => safe head 108, cursor ends at 109
    const state = await loadState(dir, cfg.name);
    expect(state?.nextBlock).toBe(109);
  });

  it("is idempotent: a second pass over the same range reports nothing new", async () => {
    const cfg = chainCfg();
    const config = relayerCfg(dir, cfg);
    const writer = mockWriter();
    await runDepositPass({ config, near: mockNear(), writer, log: silentLog, shouldStop: () => false }, cfg, fixtureReader(), { dryRun: false });
    const second = await runDepositPass({ config, near: mockNear(), writer, log: silentLog, shouldStop: () => false }, cfg, fixtureReader(), { dryRun: false });
    expect(second.scannedFrom).toBeUndefined();
    expect(writer.reports).toHaveLength(2);
  });

  it("counts contract 'already reported' (false) as a duplicate, not an error", async () => {
    const cfg = chainCfg();
    const config = relayerCfg(dir, cfg);
    const writer = mockWriter({ reportResult: () => false });
    const stats = await runDepositPass({ config, near: mockNear(), writer, log: silentLog, shouldStop: () => false }, cfg, fixtureReader(), { dryRun: false });
    expect(stats.duplicates).toBe(2);
    expect(stats.reported).toBe(0);
    expect(stats.quarantined).toBe(0);
  });

  it("quarantines contract rejections and still advances the cursor", async () => {
    const cfg = chainCfg();
    const config = relayerCfg(dir, cfg);
    const writer = mockWriter({
      reportResult: (args) => (args.asset === "native" ? new Error("Smart contract panicked: deposit exceeds cap") : true),
    });
    const stats = await runDepositPass({ config, near: mockNear(), writer, log: silentLog, shouldStop: () => false }, cfg, fixtureReader(), { dryRun: false });
    expect(stats.quarantined).toBe(1);
    const state = await loadState(dir, cfg.name);
    expect(state?.quarantined[0]?.txHash).toBe(H("a"));
    expect(state?.nextBlock).toBe(109);
  });

  it("keeps the cursor on RPC failure so the batch is retried", async () => {
    const cfg = chainCfg({ startBlock: 100, scanBatchSize: 5 });
    const config = relayerCfg(dir, cfg);
    await saveState(dir, freshState(cfg.name, CHAIN_ID, 100));
    const reader = mockReader({ head: 200n, blockError: new Error("fetch failed") });
    const summary = await runOnce({
      config,
      near: mockNear(),
      writer: mockWriter(),
      readers: new Map([[CHAIN_ID, reader]]),
      dryRun: false,
      log: silentLog,
      shouldStop: () => false,
    });
    expect(summary.errors.some((e) => e.includes("fetch failed"))).toBe(true);
    expect((await loadState(dir, cfg.name))?.nextBlock).toBe(100);
  });

  it("dry-run reports nothing, writes no state, and uses fallbackAssets when get_evm_assets fails", async () => {
    const cfg = chainCfg();
    const config = relayerCfg(dir, cfg, [NATIVE_ASSET_INFO, USDC_ASSET_INFO]);
    const writer = mockWriter();
    const near = mockNear({ assets: new Error("method not found") });
    const stats = await runDepositPass({ config, near, writer: null, log: silentLog, shouldStop: () => false }, cfg, fixtureReader(), { dryRun: true });
    expect(writer.reports).toHaveLength(0);
    expect(stats.found.map((d) => d.txHash)).toEqual([H("a"), H("b")]);
    expect(await loadState(dir, cfg.name)).toBeNull();
  });

  it("dry-run fails closed in live mode when get_evm_assets is unavailable", async () => {
    const cfg = chainCfg();
    const config = relayerCfg(dir, cfg, [NATIVE_ASSET_INFO]);
    const near = mockNear({ assets: new Error("method not found") });
    await expect(
      runDepositPass({ config, near, writer: mockWriter(), log: silentLog, shouldStop: () => false }, cfg, fixtureReader(), { dryRun: false }),
    ).rejects.toThrow(/method not found/);
  });

  it("--from-block overrides the saved cursor", async () => {
    const cfg = chainCfg({ startBlock: 100 });
    const config = relayerCfg(dir, cfg);
    const reader = mockReader({ head: 110n });
    const stats = await runDepositPass({ config, near: mockNear(), writer: null, log: silentLog, shouldStop: () => false }, cfg, reader, { dryRun: true, fromBlock: 104n, toBlock: 106n });
    expect(stats.scannedFrom).toBe(104n);
    expect(stats.scannedTo).toBe(106n);
  });
});

describe("withdrawal pipeline", () => {
  const cfg = chainCfg();
  const config = relayerCfg("/unused", cfg);

  it("processes pending withdrawals in id order with computed fees", async () => {
    const writer = mockWriter();
    const pending = [withdrawal({ id: 9 }), withdrawal({ id: 4 })];
    const stats = await processPending(
      { near: mockNear({ pending }), writer, log: silentLog, limit: 20, dryRun: false },
      cfg,
      mockReader({ head: 0n, baseFee: 2_000_000_000n, priorityFee: 1_000_000_000n }),
      pending,
    );
    expect(writer.processed.map((p) => p.id)).toEqual([4, 9]);
    // 2*2 + 1 = 5 gwei, under the 100 gwei cap; priority 1 gwei
    expect(writer.processed[0]).toEqual({ id: 4, max_fee_per_gas: "5000000000", max_priority_fee_per_gas: "1000000000" });
    expect(stats.processed).toBe(2);
  });

  it("stops the chain on a transport error so later withdrawals do not jump the queue", async () => {
    const writer = mockWriter({ processError: (id) => (id === 4 ? new Error("fetch failed") : null) });
    const pending = [withdrawal({ id: 4 }), withdrawal({ id: 9 })];
    await processPending({ near: mockNear({ pending }), writer, log: silentLog, limit: 20, dryRun: false }, cfg, mockReader({ head: 0n }), pending);
    expect(writer.processed).toEqual([]);
  });

  it("skips a rejected withdrawal that is no longer Pending", async () => {
    const writer = mockWriter({ processError: (id) => (id === 4 ? new Error("Smart contract panicked: not pending") : null) });
    const pending = [withdrawal({ id: 4 }), withdrawal({ id: 9 })];
    const near = mockNear({ pending, statuses: { 4: "Signed" } });
    await processPending({ near, writer, log: silentLog, limit: 20, dryRun: false }, cfg, mockReader({ head: 0n }), pending);
    expect(writer.processed.map((p) => p.id)).toEqual([9]);
  });

  it("fails closed when the fee cap is below the base fee", async () => {
    const writer = mockWriter();
    const pending = [withdrawal({ id: 1 })];
    const near = mockNear({ pending, chainInfo: { chain_id: CHAIN_ID, path: "p", next_nonce: 0, max_fee_per_gas_cap: "1000" } });
    const stats = await processPending({ near, writer, log: silentLog, limit: 20, dryRun: false }, cfg, mockReader({ head: 0n, baseFee: 5_000n }), pending).catch((e: unknown) => e);
    expect(stats).toBeInstanceOf(Error);
    expect(writer.processed).toEqual([]);
  });

  it("broadcasts from the pending nonce in order and marks each with its hash", async () => {
    const signed = [
      withdrawal({ id: 1, status: "Signed", nonce: 5, raw_tx: RAW(1) }),
      withdrawal({ id: 2, status: "Signed", nonce: 6, raw_tx: RAW(2) }),
      withdrawal({ id: 3, status: "Signed", nonce: 8, raw_tx: RAW(3) }),
    ];
    const reader = mockReader({ head: 0n, pendingNonce: 5 });
    const writer = mockWriter();
    const stats = await broadcastSigned({ near: mockNear({ signed }), writer, log: silentLog, limit: 20, dryRun: false }, cfg, reader, signed);
    expect(reader.sent).toEqual([RAW(1), RAW(2)]);
    expect(writer.marked).toEqual([
      { id: 1, tx_hash: txHashOf(RAW(1)) },
      { id: 2, tx_hash: txHashOf(RAW(2)) },
    ]);
    expect(stats.broadcast).toBe(2);
  });

  it("treats 'already known' as broadcast and marks it without resending", async () => {
    const signed = [withdrawal({ id: 1, status: "Signed", nonce: 5, raw_tx: RAW(1) })];
    const reader = mockReader({
      head: 0n,
      pendingNonce: 5,
      sendError: () => new Error("already known"),
    });
    const writer = mockWriter();
    await broadcastSigned({ near: mockNear({ signed }), writer, log: silentLog, limit: 20, dryRun: false }, cfg, reader, signed);
    expect(writer.marked).toEqual([{ id: 1, tx_hash: txHashOf(RAW(1)) }]);
  });

  it("on 'nonce too low', marks only if the tx hash is known to the node", async () => {
    const signed = [withdrawal({ id: 1, status: "Signed", nonce: 5, raw_tx: RAW(1) })];
    const known = new Set<string>([txHashOf(RAW(1))]);
    const reader = mockReader({ head: 0n, pendingNonce: 5, knownHashes: known, sendError: () => new Error("nonce too low") });
    const writer = mockWriter();
    await broadcastSigned({ near: mockNear({ signed }), writer, log: silentLog, limit: 20, dryRun: false }, cfg, reader, signed);
    expect(writer.marked).toHaveLength(1);

    const unknownReader = mockReader({ head: 0n, pendingNonce: 5, sendError: () => new Error("nonce too low") });
    const writer2 = mockWriter();
    const log = recordingLog();
    await broadcastSigned({ near: mockNear({ signed }), writer: writer2, log, limit: 20, dryRun: false }, cfg, unknownReader, signed);
    expect(writer2.marked).toHaveLength(0);
    expect(log.errors.length).toBeGreaterThan(0);
  });

  it("marks stale signed withdrawals (nonce already used) only when their hash is on the node", async () => {
    const signed = [
      withdrawal({ id: 1, status: "Signed", nonce: 4, raw_tx: RAW(1) }),
      withdrawal({ id: 2, status: "Signed", nonce: 5, raw_tx: RAW(2) }),
    ];
    const reader = mockReader({ head: 0n, pendingNonce: 6, knownHashes: new Set([txHashOf(RAW(1))]) });
    const writer = mockWriter();
    const log = recordingLog();
    await broadcastSigned({ near: mockNear({ signed }), writer, log, limit: 20, dryRun: false }, cfg, reader, signed);
    expect(writer.marked.map((m) => m.id)).toEqual([1]);
    expect(log.errors).toHaveLength(1); // id 2: nonce 5 used by an unknown tx
    expect(reader.sent).toEqual([]);
  });

  it("dry-run never sends or writes", async () => {
    const signed = [withdrawal({ id: 1, status: "Signed", nonce: 5, raw_tx: RAW(1) })];
    const reader = mockReader({ head: 0n, pendingNonce: 5 });
    await broadcastSigned({ near: mockNear({ signed }), writer: null, log: silentLog, limit: 20, dryRun: true }, cfg, reader, signed);
    expect(reader.sent).toEqual([]);
  });

  it("runOnce in dry-run tolerates missing withdrawal views", async () => {
    const dir = await mkdtemp(join(tmpdir(), "relayer-dry-"));
    try {
      const near = mockNear({ assets: [NATIVE_ASSET_INFO], chainInfo: new Error("method not found") });
      near.getPendingWithdrawals = async () => {
        throw new Error("method not found");
      };
      const summary = await runOnce(
        { config: relayerCfg(dir, chainCfg({ maxFeePerGasCapFallback: "100000000000" }), [NATIVE_ASSET_INFO]), near, writer: null, readers: new Map([[CHAIN_ID, mockReader({ head: 110n })]]), dryRun: true, log: silentLog, shouldStop: () => false },
        { fromBlock: 100n, toBlock: 101n },
      );
      expect(summary.errors).toEqual([]);
      expect(summary.deposits[0]?.caughtUp).toBe(true);
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
});

describe("state file location", () => {
  it("uses <stateDir>/<chainName>.json", () => {
    expect(statePath("state", "base-sepolia")).toBe(join("state", "base-sepolia.json"));
  });
});

