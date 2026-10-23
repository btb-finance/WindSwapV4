import { describe, expect, it } from "vitest";
import {
  decodeTransferLog,
  nativeDepositCandidates,
  planScanRange,
  scanRange,
  TRANSFER_TOPIC,
} from "../src/deposits.js";
import { addressTopic, createRateLimiter } from "../src/evm.js";
import { isContractRejection } from "../src/near.js";
import {
  block,
  CHAIN_ID,
  mockReader,
  OTHER,
  recordingLog,
  SENDER,
  transferLog,
  UNREGISTERED,
  USDC,
  VAULT,
} from "./helpers.js";

const H = (c: string) => ("0x" + c.repeat(64)) as `0x${string}`;

describe("planScanRange", () => {
  it("scans in batches up to head - confirmations", () => {
    expect(planScanRange({ from: 100n, head: 110n, confirmations: 2, batchSize: 5 })).toEqual({ from: 100n, to: 104n });
    expect(planScanRange({ from: 105n, head: 110n, confirmations: 2, batchSize: 5 })).toEqual({ from: 105n, to: 108n });
  });

  it("returns null when caught up", () => {
    expect(planScanRange({ from: 109n, head: 110n, confirmations: 2, batchSize: 5 })).toBeNull();
  });

  it("honours a toBlock override", () => {
    expect(planScanRange({ from: 100n, head: 1000n, confirmations: 0, batchSize: 50, toOverride: 120n })).toEqual({
      from: 100n,
      to: 120n,
    });
    expect(planScanRange({ from: 121n, head: 1000n, confirmations: 0, batchSize: 50, toOverride: 120n })).toBeNull();
  });
});

describe("native deposit detection", () => {
  // Fixture block: three transactions, only the first is a valid deposit.
  const fixture = block(49_074_792n, [
    { hash: H("a"), from: SENDER, to: VAULT, value: 100_000_000_000_000_000n, transactionIndex: 0 },
    { hash: H("b"), from: SENDER, to: OTHER, value: 5n, transactionIndex: 1 },
    { hash: H("c"), from: SENDER, to: VAULT, value: 0n, transactionIndex: 2 },
    { hash: H("d"), from: SENDER, to: null, value: 9n, transactionIndex: 3 },
  ]);

  it("selects transfers to the vault with value > 0", () => {
    const found = nativeDepositCandidates(fixture, VAULT, CHAIN_ID);
    expect(found).toHaveLength(1);
    expect(found[0]).toMatchObject({
      chainId: CHAIN_ID,
      txHash: H("a"),
      logIndex: 0,
      from: SENDER,
      asset: "native",
      amount: 100_000_000_000_000_000n,
      blockNumber: 49_074_792n,
    });
  });

  it("matches the vault address case-insensitively", () => {
    const upper = block(1n, [{ hash: H("e"), to: VAULT.toUpperCase().replace("0X", "0x") as `0x${string}`, value: 1n }]);
    expect(nativeDepositCandidates(upper, VAULT, CHAIN_ID)).toHaveLength(1);
  });

  it("drops reverted native transfers during scanRange", async () => {
    const reader = mockReader({
      head: 200n,
      blocks: { "101": fixture },
      receipts: { [H("a")]: "reverted" },
    });
    const res = await scanRange({
      reader,
      chainId: CHAIN_ID,
      vault: VAULT,
      range: { from: 101n, to: 101n },
      registered: new Set(["native"]),
      log: recordingLog(),
    });
    expect(res.deposits).toHaveLength(0);
    expect(res.revertedNative).toBe(1);
  });

  it("skips the block scan entirely when native is not registered", async () => {
    const reader = mockReader({ head: 200n, blocks: { "101": fixture } });
    await scanRange({
      reader,
      chainId: CHAIN_ID,
      vault: VAULT,
      range: { from: 101n, to: 101n },
      registered: new Set([USDC.toLowerCase()]),
      log: recordingLog(),
    });
    expect(reader.blocksFetched).toEqual([]);
  });
});

describe("ERC-20 Transfer decoding", () => {
  it("topic constant is keccak256 of the Transfer signature", () => {
    expect(TRANSFER_TOPIC).toBe("0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");
  });

  it("decodes a Transfer to the vault", () => {
    const log = transferLog({ token: USDC, value: 2_500_000n, blockNumber: 7n, txHash: H("f"), logIndex: 3 });
    expect(decodeTransferLog(log, VAULT)).toEqual({
      token: USDC,
      from: SENDER,
      to: VAULT,
      value: 2_500_000n,
      txHash: H("f"),
      logIndex: 3,
      blockNumber: 7n,
      transactionIndex: 0,
    });
  });

  it("rejects transfers to other addresses", () => {
    const log = transferLog({ token: USDC, to: OTHER, value: 1n, blockNumber: 7n, txHash: H("f"), logIndex: 0 });
    expect(decodeTransferLog(log, VAULT)).toBeNull();
  });

  it("rejects ERC-721 style logs (4 topics)", () => {
    const log = transferLog({ token: USDC, value: 1n, blockNumber: 7n, txHash: H("f"), logIndex: 0 });
    log.topics.push(addressTopic(OTHER));
    expect(decodeTransferLog(log, VAULT)).toBeNull();
  });

  it("rejects malformed data", () => {
    const log = transferLog({ token: USDC, value: 1n, blockNumber: 7n, txHash: H("f"), logIndex: 0 });
    log.data = "0x01";
    expect(decodeTransferLog(log, VAULT)).toBeNull();
  });

  it("reports registered tokens and ignores unregistered ones", async () => {
    const logs = [
      transferLog({ token: USDC, value: 5n, blockNumber: 102n, txHash: H("1"), logIndex: 0 }),
      transferLog({ token: UNREGISTERED, value: 9n, blockNumber: 102n, txHash: H("2"), logIndex: 1 }),
    ];
    const reader = mockReader({ head: 200n, logs });
    const log = recordingLog();
    const res = await scanRange({
      reader,
      chainId: CHAIN_ID,
      vault: VAULT,
      range: { from: 100n, to: 104n },
      registered: new Set([USDC.toLowerCase()]),
      log,
    });
    expect(res.deposits.map((d) => [d.asset, d.amount])).toEqual([[USDC.toLowerCase(), 5n]]);
    expect(res.ignoredTransfers).toHaveLength(1);
  });
});

describe("scanRange ordering", () => {
  it("returns deposits sorted by block, then transaction index", async () => {
    const reader = mockReader({
      head: 200n,
      blocks: {
        "101": block(101n, [{ hash: H("9"), value: 1n, transactionIndex: 4 }]),
        "100": block(100n, [{ hash: H("8"), value: 1n, transactionIndex: 2 }]),
      },
      logs: [transferLog({ token: USDC, value: 1n, blockNumber: 100n, txHash: H("7"), logIndex: 0 })],
    });
    const res = await scanRange({
      reader,
      chainId: CHAIN_ID,
      vault: VAULT,
      range: { from: 100n, to: 101n },
      registered: new Set(["native", USDC.toLowerCase()]),
      log: recordingLog(),
    });
    expect(res.deposits.map((d) => d.txHash)).toEqual([H("7"), H("8"), H("9")]);
  });
});

describe("helpers", () => {
  it("createRateLimiter spaces calls", async () => {
    const gate = createRateLimiter(50); // 20 ms interval
    const started = Date.now();
    await gate();
    await gate();
    await gate();
    expect(Date.now() - started).toBeGreaterThanOrEqual(35);
  });

  it("isContractRejection separates panics from transport errors", () => {
    expect(isContractRejection(new Error("Smart contract panicked: deposit exceeds cap"))).toBe(true);
    expect(isContractRejection(new Error("fetch failed"))).toBe(false);
  });
});
