import { postJson } from "../http.js";
import { normalizeTxTree, type RawTxTree } from "./normalize.js";
import type { ReceiptExec } from "./types.js";

/**
 * FastNEAR transaction API (account history and transaction trees). Used for backfill: one account
 * query lists every transaction that touched the contract, and one batched lookup returns each
 * transaction's full receipt tree with per-receipt block heights.
 */
export class FastnearTxSource {
  constructor(private readonly baseUrl: string) {}

  /**
   * Hashes of all transactions involving `account` with a block height in [from, to], oldest first.
   * Pages through `resume_token` until exhausted.
   */
  async accountTxHashes(account: string, from: number, to: number): Promise<string[]> {
    const hashes: string[] = [];
    let resume: string | undefined;
    do {
      const body: Record<string, unknown> = {
        account_id: account,
        desc: false,
        limit: 200,
        from_tx_block_height: from,
        to_tx_block_height: to,
      };
      if (resume) body.resume_token = resume;
      const page = await postJson<{
        account_txs: Array<{ transaction_hash: string }>;
        resume_token?: string;
      }>(`${this.baseUrl}/v0/account`, body, { label: "fastnear account" });
      for (const t of page.account_txs) hashes.push(t.transaction_hash);
      resume = page.resume_token;
    } while (resume);
    return [...new Set(hashes)];
  }

  /** Earliest transaction height that involves `account`, or null when it has no history. */
  async firstTxHeight(account: string): Promise<number | null> {
    const page = await postJson<{ account_txs: Array<{ tx_block_height: number }> }>(
      `${this.baseUrl}/v0/account`,
      { account_id: account, desc: false, limit: 1 },
      { label: "fastnear first tx" },
    );
    return page.account_txs[0]?.tx_block_height ?? null;
  }

  /** Receipt executions with receiver in `receivers` for the given transactions (max 20 per request). */
  async receiptsOf(
    txHashes: string[],
    receivers: ReadonlySet<string>,
    maxHeight?: number,
  ): Promise<ReceiptExec[]> {
    const out: ReceiptExec[] = [];
    for (let i = 0; i < txHashes.length; i += 20) {
      const chunk = txHashes.slice(i, i + 20);
      const res = await postJson<{ transactions: RawTxTree[] }>(
        `${this.baseUrl}/v0/transactions`,
        { tx_hashes: chunk },
        { label: "fastnear transactions" },
      );
      for (const tree of res.transactions) out.push(...normalizeTxTree(tree, receivers, maxHeight));
    }
    return out;
  }
}

/** NEAR RPC: read-only view calls at final finality. */
export class NearRpc {
  constructor(private readonly url: string) {}

  async view<T>(contractId: string, method: string, args: Record<string, unknown> = {}): Promise<T> {
    const res = await postJson<{
      result?: { result: number[] };
      error?: { message: string; cause?: { name: string; info?: unknown } };
    }>(
      this.url,
      {
        jsonrpc: "2.0",
        id: "windswap",
        method: "query",
        params: {
          request_type: "call_function",
          finality: "final",
          account_id: contractId,
          method_name: method,
          args_base64: Buffer.from(JSON.stringify(args)).toString("base64"),
        },
      },
      { label: `rpc ${method}` },
    );
    if (res.error || !res.result) {
      throw new Error(`view ${method} failed: ${res.error?.message ?? "no result"}`);
    }
    return JSON.parse(Buffer.from(res.result.result).toString("utf8")) as T;
  }

  /** Latest final block height (the head the indexer may read up to). */
  async finalHeight(): Promise<number> {
    const res = await postJson<{ result?: { header: { height: number } }; error?: { message: string } }>(
      this.url,
      { jsonrpc: "2.0", id: "windswap", method: "block", params: { finality: "final" } },
      { label: "rpc block final" },
    );
    if (!res.result) throw new Error(`rpc block final: ${res.error?.message ?? "no result"}`);
    return res.result.header.height;
  }
}

