/**
 * Source-independent view of one executed action receipt. Both data sources (neardata blocks and the
 * FastNEAR transaction tree) are normalised into this shape before decoding.
 */
export interface FunctionCallAction {
  kind: "FunctionCall";
  methodName: string;
  /** Raw args bytes decoded from base64 (usually JSON). */
  argsText: string;
  deposit: string;
}

export type ReceiptAction = FunctionCallAction | { kind: "Other"; name: string };

export type ReceiptStatus =
  | { ok: true; value: string | null; chained: boolean }
  | { ok: false; error: string }
  | { ok: null };

export interface ReceiptExec {
  receiptId: string;
  /** Hash of the transaction that created this receipt tree (null for system receipts). */
  txHash: string | null;
  height: number;
  /** Block timestamp in milliseconds since the epoch. */
  timestampMs: number;
  predecessorId: string;
  receiverId: string;
  signerId: string | null;
  actions: ReceiptAction[];
  status: ReceiptStatus;
  logs: string[];
}

export interface BlockSnapshot {
  height: number;
  timestampMs: number;
  receipts: ReceiptExec[];
}
