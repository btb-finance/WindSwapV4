import { keccak256, stringToHex } from "viem";
import type { BlockTx, DepositCandidate, EvmReader, Hex, Logger, RawLog, TransferEvent } from "./types.js";
import { mapLimit } from "./util.js";

export const TRANSFER_TOPIC = keccak256(stringToHex("Transfer(address,address,uint256)"));
export const NATIVE_ASSET = "native";

export interface ScanRange {
  from: bigint;
  to: bigint;
}

/**
 * Next inclusive range to scan, or null if already caught up to `head - confirmations`.
 * `toOverride` (from --to-block) caps the end.
 */
export function planScanRange(args: {
  from: bigint;
  head: bigint;
  confirmations: number;
  batchSize: number;
  toOverride?: bigint;
}): ScanRange | null {
  const safeHead = args.head - BigInt(args.confirmations);
  let limit = safeHead;
  if (args.toOverride !== undefined && args.toOverride < limit) limit = args.toOverride;
  if (args.from > limit) return null;
  const end = args.from + BigInt(args.batchSize) - 1n;
  return { from: args.from, to: end < limit ? end : limit };
}

/** Native deposits in one full block: plain transfers to the vault with value > 0. Receipt is checked separately. */
export function nativeDepositCandidates(txs: readonly BlockTx[], vault: Hex, chainId: number): DepositCandidate[] {
  const target = vault.toLowerCase();
  return txs
    .filter((tx) => tx.to !== null && tx.to.toLowerCase() === target && tx.value > 0n)
    .map((tx) => ({
      chainId,
      txHash: tx.hash,
      logIndex: 0,
      from: tx.from,
      asset: NATIVE_ASSET,
      amount: tx.value,
      blockNumber: tx.blockNumber,
      transactionIndex: tx.transactionIndex,
    }));
}

const ADDRESS_BYTES = 40;

/**
 * Decodes an ERC-20 Transfer log addressed to the vault. Returns null for anything else:
 * ERC-721 (4 topics), other events, transfers to other addresses, or malformed data.
 */
export function decodeTransferLog(log: RawLog, vault: Hex): TransferEvent | null {
  if (log.topics.length !== 3) return null;
  if (log.topics[0]?.toLowerCase() !== TRANSFER_TOPIC) return null;
  const toTopic = log.topics[2]!.toLowerCase();
  if (toTopic.slice(-ADDRESS_BYTES) !== vault.slice(2).toLowerCase()) return null;
  if (!/^0x[0-9a-fA-F]{64}$/.test(log.data)) return null;
  const fromTopic = log.topics[1]!.toLowerCase();
  return {
    token: log.address.toLowerCase() as Hex,
    from: ("0x" + fromTopic.slice(-ADDRESS_BYTES)) as Hex,
    to: vault.toLowerCase() as Hex,
    value: BigInt(log.data),
    txHash: log.transactionHash,
    logIndex: log.logIndex,
    blockNumber: log.blockNumber,
    transactionIndex: log.transactionIndex,
  };
}

export interface ScanResult {
  deposits: DepositCandidate[];
  /** Transfers to the vault from tokens that are not registered. Logged, never reported. */
  ignoredTransfers: TransferEvent[];
  /** Native candidates whose receipt was not successful. */
  revertedNative: number;
}

/**
 * Scans [range.from, range.to] for deposits to the vault. `registered` is the set of lowercase
 * asset keys ("native" plus ERC-20 addresses) for this chain. Unregistered assets are ignored.
 */
export async function scanRange(args: {
  reader: EvmReader;
  chainId: number;
  vault: Hex;
  range: { from: bigint; to: bigint };
  registered: ReadonlySet<string>;
  log: Logger;
  blockConcurrency?: number;
}): Promise<ScanResult> {
  const { reader, chainId, vault, range, registered, log } = args;
  const concurrency = args.blockConcurrency ?? 1;
  const deposits: DepositCandidate[] = [];
  let revertedNative = 0;

  if (registered.has(NATIVE_ASSET)) {
    const blockNumbers: bigint[] = [];
    for (let n = range.from; n <= range.to; n++) blockNumbers.push(n);
    const blocks = await mapLimit(blockNumbers, concurrency, (n) => reader.blockTransactions(n));
    const candidates = blocks.flatMap((txs) => nativeDepositCandidates(txs, vault, chainId));
    const statuses = await mapLimit(candidates, concurrency, (c) => reader.receiptStatus(c.txHash));
    candidates.forEach((candidate, i) => {
      if (statuses[i] === "success") {
        deposits.push(candidate);
      } else {
        revertedNative++;
        log.warn("skipping native transfer to vault with non-success receipt", {
          txHash: candidate.txHash,
          status: statuses[i],
        });
      }
    });
  } else {
    log.info("native asset not registered on this chain, skipping native scan", { chainId });
  }

  const ignoredTransfers: TransferEvent[] = [];
  const logs = await reader.transferLogs(range.from, range.to);
  for (const raw of logs) {
    const transfer = decodeTransferLog(raw, vault);
    if (!transfer) continue;
    if (!registered.has(transfer.token)) {
      ignoredTransfers.push(transfer);
      continue;
    }
    deposits.push({
      chainId,
      txHash: transfer.txHash,
      logIndex: transfer.logIndex,
      from: transfer.from,
      asset: transfer.token,
      amount: transfer.value,
      blockNumber: transfer.blockNumber,
      transactionIndex: transfer.transactionIndex,
    });
  }

  for (const t of ignoredTransfers) {
    log.info("ignoring transfer to vault from unregistered token", {
      token: t.token,
      txHash: t.txHash,
      amount: t.value,
    });
  }

  deposits.sort(
    (a, b) =>
      Number(a.blockNumber - b.blockNumber) || a.transactionIndex - b.transactionIndex || a.logIndex - b.logIndex,
  );
  return { deposits, ignoredTransfers, revertedNative };
}
