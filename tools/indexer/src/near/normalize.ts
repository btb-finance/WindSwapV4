import type { BlockSnapshot, ReceiptAction, ReceiptExec, ReceiptStatus } from "./types.js";

/** Raw JSON shapes, as served by neardata.xyz and the FastNEAR transaction API. */
export interface RawExecutionOutcome {
  id?: string;
  block_hash?: string;
  outcome: {
    logs: string[];
    status: unknown;
    executor_id?: string;
  };
}

export interface RawReceipt {
  receipt_id: string;
  predecessor_id: string;
  receiver_id: string;
  receipt: { Action?: { signer_id?: string; actions: unknown[] }; Data?: unknown };
}

function decodeB64(value: string): string {
  return Buffer.from(value, "base64").toString("utf8");
}

export function normalizeStatus(status: unknown): ReceiptStatus {
  if (status === null || typeof status !== "object") return { ok: null };
  const s = status as Record<string, unknown>;
  if ("SuccessValue" in s) {
    const raw = s.SuccessValue as string;
    return { ok: true, value: raw === "" ? null : decodeB64(raw), chained: false };
  }
  if ("SuccessReceiptId" in s) return { ok: true, value: null, chained: true };
  if ("Failure" in s) return { ok: false, error: JSON.stringify(s.Failure) };
  return { ok: null };
}

function normalizeActions(actions: unknown[]): ReceiptAction[] {
  return actions.map((a): ReceiptAction => {
    // Unit variants (CreateAccount, DeleteAccount, ...) serialize as bare strings.
    if (typeof a === "string") return { kind: "Other", name: a };
    const action = a as Record<string, unknown>;
    if ("FunctionCall" in action) {
      const fc = action.FunctionCall as { method_name: string; args: string; deposit?: string };
      return {
        kind: "FunctionCall",
        methodName: fc.method_name,
        argsText: fc.args ? decodeB64(fc.args) : "",
        deposit: fc.deposit ?? "0",
      };
    }
    return { kind: "Other", name: Object.keys(action)[0] ?? "unknown" };
  });
}

/**
 * Converts one receipt execution (outcome + receipt) into a ReceiptExec. Returns null for receipts
 * that are not action receipts (data receipts, or transaction-conversion records without receipt).
 */
export function normalizeReceiptExec(
  outcome: RawExecutionOutcome,
  receipt: RawReceipt | null,
  ctx: { height: number; timestampMs: number; txHash: string | null },
): ReceiptExec | null {
  if (!receipt || !receipt.receipt || !receipt.receipt.Action) return null;
  const action = receipt.receipt.Action;
  return {
    receiptId: receipt.receipt_id,
    txHash: ctx.txHash,
    height: ctx.height,
    timestampMs: ctx.timestampMs,
    predecessorId: receipt.predecessor_id,
    receiverId: receipt.receiver_id,
    signerId: action.signer_id ?? null,
    actions: normalizeActions(action.actions),
    status: normalizeStatus(outcome.outcome.status),
    logs: outcome.outcome.logs ?? [],
  };
}

interface NeardataBlock {
  block: { header: { height: number; timestamp: number } };
  shards: Array<{
    receipt_execution_outcomes: Array<{
      execution_outcome: RawExecutionOutcome;
      receipt: RawReceipt | null;
      tx_hash?: string | null;
    }>;
  }>;
}

/** Normalises a neardata block. Only receipts whose receiver is one of `receivers` are kept. */
export function normalizeNeardataBlock(json: unknown, receivers: ReadonlySet<string>): BlockSnapshot {
  const b = json as NeardataBlock;
  const height = b.block.header.height;
  const timestampMs = Math.floor(Number(b.block.header.timestamp) / 1e6);
  const receipts: ReceiptExec[] = [];
  for (const shard of b.shards) {
    for (const entry of shard.receipt_execution_outcomes) {
      if (!entry.receipt || !receivers.has(entry.receipt.receiver_id)) continue;
      const r = normalizeReceiptExec(entry.execution_outcome, entry.receipt, {
        height,
        timestampMs,
        txHash: entry.tx_hash ?? null,
      });
      if (r) receipts.push(r);
    }
  }
  return { height, timestampMs, receipts };
}

export interface RawTxTree {
  transaction: { hash: string };
  execution_outcome: { block_height: number; block_timestamp: number | string };
  receipts: Array<{ execution_outcome: RawExecutionOutcome & { block_height: number; block_timestamp?: number | string }; receipt: RawReceipt & { block_height: number; block_timestamp?: number | string } }>;
}

/**
 * Normalises the receipts of a FastNEAR transaction tree whose receiver is in `receivers` and whose
 * execution height is at most `maxHeight` (when given). Every receipt of the tree carries its own block
 * height and timestamp, so the same tree can contribute to several blocks.
 */
export function normalizeTxTree(tree: RawTxTree, receivers: ReadonlySet<string>, maxHeight?: number): ReceiptExec[] {
  const out: ReceiptExec[] = [];
  for (const entry of tree.receipts) {
    const r = entry.receipt;
    if (!r || !receivers.has(r.receiver_id)) continue;
    const height = entry.execution_outcome.block_height;
    if (maxHeight !== undefined && height > maxHeight) continue;
    const ts = Number(entry.execution_outcome.block_timestamp ?? tree.execution_outcome.block_timestamp);
    const norm = normalizeReceiptExec(entry.execution_outcome, r, {
      height,
      timestampMs: Math.floor(ts / 1e6),
      txHash: tree.transaction.hash,
    });
    if (norm) out.push(norm);
  }
  return out;
}
