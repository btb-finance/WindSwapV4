import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { encodeAbiParameters, stringToHex } from "viem";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { parseConfig } from "../src/config.js";
import { decodeDecimalsResult, decodeSymbolResult } from "../src/evm.js";
import { scanRange } from "../src/deposits.js";
import { runDepositPass } from "../src/watcher.js";
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
} from "./helpers.js";

const H = (c: string) => ("0x" + c.repeat(64)) as Hex;
const NEW_TOKEN: Hex = "0x4444444444444444444444444444444444444444";
const DENIED: Hex = "0x5555555555555555555555555555555555555555";
const CHAIN_CFG = { autoRegister: true };

describe("token metadata decoding", () => {
  it("decodes a string symbol", () => {
    expect(decodeSymbolResult(encodeAbiParameters([{ type: "string" }], ["USDT"]))).toBe("USDT");
  });

  it("decodes a bytes32 symbol (MKR-style)", () => {
    const data = ("0x" + stringToHex("MKR").slice(2).padEnd(64, "0")) as Hex;
    expect(decodeSymbolResult(data)).toBe("MKR");
  });

  it("returns an empty symbol for data that is neither", () => {
    expect(decodeSymbolResult("0x1234")).toBe("");
  });

  it("decodes decimals and refuses data that is not a uint8", () => {
    expect(decodeDecimalsResult(encodeAbiParameters([{ type: "uint8" }], [18]))).toBe(18);
    expect(decodeDecimalsResult("0x")).toBeNull();
  });
});

describe("scanRange auto-registration", () => {
  const meta = { [NEW_TOKEN]: { decimals: 6, symbol: "NEW" } };
  const logs = () => [
    transferLog({ token: NEW_TOKEN, value: 500n, blockNumber: 101n, txHash: H("a"), logIndex: 1 }),
    transferLog({ token: DENIED, value: 9n, blockNumber: 101n, txHash: H("b"), logIndex: 2 }),
    transferLog({ token: UNREGISTERED, value: 1n, blockNumber: 101n, txHash: H("c"), logIndex: 3 }),
  ];
  const scan = (opts: Parameters<typeof mockReader>[0], auto?: { deny: Set<Hex> }) =>
    scanRange({
      reader: mockReader({ head: 200n, ...opts }),
      chainId: CHAIN_ID,
      vault: VAULT,
      range: { from: 100n, to: 110n },
      registered: new Set(["native", USDC.toLowerCase()]),
      log: recordingLog(),
      autoRegister: auto
        ? { deny: new Set([DENIED]), readMeta: async (t) => (await mockReader({ head: 1n, tokenMeta: meta }).tokenMeta(t)) }
        : undefined,
    });

  it("reports an unknown token with its decimals and symbol, and honours the deny list", async () => {
    const res = await scan({ logs: logs(), tokenMeta: meta }, { deny: new Set([DENIED]) });
    expect(res.deposits).toEqual([
      expect.objectContaining({ asset: NEW_TOKEN, amount: 500n, decimals: 6, symbol: "NEW", logIndex: 1 }),
    ]);
    expect(res.ignoredTransfers.map((t) => t.token).sort()).toEqual([DENIED, UNREGISTERED].sort());
  });

  it("ignores unknown tokens when auto-registration is off", async () => {
    const res = await scan({ logs: logs(), tokenMeta: meta });
    expect(res.deposits).toEqual([]);
    expect(res.ignoredTransfers).toHaveLength(3);
  });

  it("ignores a token whose decimals cannot be read", async () => {
    const log = recordingLog();
    const res = await scanRange({
      reader: mockReader({ head: 200n, logs: logs() }),
      chainId: CHAIN_ID,
      vault: VAULT,
      range: { from: 100n, to: 110n },
      registered: new Set(["native"]),
      log,
      autoRegister: { deny: new Set([DENIED]), readMeta: async () => null },
    });
    expect(res.deposits).toEqual([]);
    expect(log.warns.some((w) => w.includes("decimals unreadable"))).toBe(true);
  });

  it("reads each token's metadata once per scan", async () => {
    let calls = 0;
    await scanRange({
      reader: mockReader({
        head: 200n,
        logs: [
          transferLog({ token: NEW_TOKEN, value: 1n, blockNumber: 101n, txHash: H("a"), logIndex: 1 }),
          transferLog({ token: NEW_TOKEN, value: 2n, blockNumber: 102n, txHash: H("b"), logIndex: 1 }),
        ],
      }),
      chainId: CHAIN_ID,
      vault: VAULT,
      range: { from: 100n, to: 110n },
      registered: new Set(["native"]),
      log: silentLog,
      autoRegister: {
        deny: new Set(),
        readMeta: async () => {
          calls++;
          return { decimals: 6, symbol: "NEW" };
        },
      },
    });
    expect(calls).toBe(1);
  });
});

describe("config auto-registration options", () => {
  const base = {
    near: { rpcUrl: "u", contractId: "c.testnet", relayerAccountId: "r.testnet", credentialsPath: "/x" },
    evm: [
      {
        name: "base-sepolia",
        chainId: CHAIN_ID,
        rpcUrl: "u",
        vault: VAULT,
        confirmations: 1,
        startBlock: 1,
        scanBatchSize: 10,
      },
    ],
  };

  it("is off by default, with an empty deny list", () => {
    const cfg = parseConfig(base, {});
    expect(cfg.evm[0]?.autoRegister).toBe(false);
    expect(cfg.evm[0]?.denyTokens).toEqual([]);
  });

  it("reads autoRegister and lower-cases the deny list", () => {
    const raw = structuredClone(base);
    (raw.evm[0] as Record<string, unknown>).autoRegister = true;
    (raw.evm[0] as Record<string, unknown>).denyTokens = [DENIED.toUpperCase().replace("0X", "0x")];
    const cfg = parseConfig(raw, {});
    expect(cfg.evm[0]?.autoRegister).toBe(true);
    expect(cfg.evm[0]?.denyTokens).toEqual([DENIED]);
  });

  it("rejects a non-boolean autoRegister and a malformed deny address", () => {
    const a = structuredClone(base);
    (a.evm[0] as Record<string, unknown>).autoRegister = "yes";
    expect(() => parseConfig(a, {})).toThrow(/autoRegister/);
    const b = structuredClone(base);
    (b.evm[0] as Record<string, unknown>).denyTokens = ["0x12"];
    expect(() => parseConfig(b, {})).toThrow(/denyTokens/);
  });
});

describe("watcher auto-registration", () => {
  let dir: string;
  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), "relayer-auto-"));
  });
  afterEach(async () => {
    await rm(dir, { recursive: true, force: true });
  });

  const reader = () =>
    mockReader({
      head: 110n,
      logs: [transferLog({ token: NEW_TOKEN, value: 500n, blockNumber: 106n, txHash: H("b"), logIndex: 2 })],
      tokenMeta: { [NEW_TOKEN]: { decimals: 6, symbol: "NEW" } },
    });

  it("reports the unknown token with decimals and symbol when the chain is enabled", async () => {
    const cfg = chainCfg({ ...CHAIN_CFG, startBlock: 100 });
    const config = relayerCfg(dir, cfg, [NATIVE_ASSET_INFO, USDC_ASSET_INFO]);
    const writer = mockWriter();
    await runDepositPass(
      { config, near: mockNear(), writer, log: silentLog, shouldStop: () => false },
      cfg,
      reader(),
      { dryRun: false },
    );
    expect(writer.reports).toEqual([
      {
        chain_id: CHAIN_ID,
        tx_hash: H("b"),
        log_index: 2,
        from: SENDER,
        asset: NEW_TOKEN,
        amount: "500",
        decimals: 6,
        symbol: "NEW",
      },
    ]);
  });

  it("does not auto-register on a chain the contract has not enabled", async () => {
    const cfg = chainCfg({ ...CHAIN_CFG, startBlock: 100 });
    const config = relayerCfg(dir, cfg, [NATIVE_ASSET_INFO, USDC_ASSET_INFO]);
    const writer = mockWriter();
    const stats = await runDepositPass(
      { config, near: mockNear({ chainInfo: null }), writer, log: silentLog, shouldStop: () => false },
      cfg,
      reader(),
      { dryRun: false },
    );
    expect(writer.reports).toHaveLength(0);
    expect(stats.ignoredTransfers).toBe(1);
  });

  it("does not auto-register when the option is off", async () => {
    const cfg = chainCfg({ startBlock: 100 });
    const config = relayerCfg(dir, cfg, [NATIVE_ASSET_INFO, USDC_ASSET_INFO]);
    const writer = mockWriter();
    await runDepositPass(
      { config, near: mockNear(), writer, log: silentLog, shouldStop: () => false },
      cfg,
      reader(),
      { dryRun: false },
    );
    expect(writer.reports).toHaveLength(0);
  });
});
