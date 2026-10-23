import type { Hex } from "./types.js";

export interface SignedWithdrawal {
  id: number;
  nonce: number;
  rawTx: Hex;
  /** keccak256 of the signed raw transaction (the transaction hash for typed txs). */
  txHash: Hex;
}

export interface BroadcastPlan {
  /** Signed, nonce below the vault's pending nonce. Already on chain or in mempool, or stranded. */
  stale: SignedWithdrawal[];
  /** Contiguous run starting at the pending nonce. Safe to send in this order. */
  ready: SignedWithdrawal[];
  /** Behind a nonce gap. Wait for the gap to be filled. */
  blocked: SignedWithdrawal[];
  /** Set when the signed set is inconsistent (e.g. two withdrawals share a nonce). Nothing is sent. */
  problem?: string;
}

/**
 * Decides which signed withdrawals can be broadcast now.
 * `pendingNonce` must be eth_getTransactionCount(vault, "pending"): the next nonce the vault
 * may use, including transactions already in the mempool.
 */
export function planBroadcast(signed: readonly SignedWithdrawal[], pendingNonce: number): BroadcastPlan {
  const sorted = [...signed].sort((a, b) => a.nonce - b.nonce);

  for (let i = 1; i < sorted.length; i++) {
    const prev = sorted[i - 1]!;
    const cur = sorted[i]!;
    if (prev.nonce === cur.nonce) {
      return {
        stale: [],
        ready: [],
        blocked: sorted,
        problem: `duplicate nonce ${cur.nonce} for withdrawals ${prev.id} and ${cur.id}`,
      };
    }
  }

  const stale = sorted.filter((w) => w.nonce < pendingNonce);
  const upcoming = sorted.filter((w) => w.nonce >= pendingNonce);
  const ready: SignedWithdrawal[] = [];
  let expected = pendingNonce;
  let idx = 0;
  for (; idx < upcoming.length; idx++) {
    const w = upcoming[idx]!;
    if (w.nonce !== expected) break;
    ready.push(w);
    expected++;
  }
  const blocked = upcoming.slice(idx);
  return { stale, ready, blocked };
}
