/**
 * The indexing loop.
 *
 *  - Backlog, account mode (default): FastNEAR account history lists the transactions that touched the
 *    contract; their receipt trees give every execution with its height. Only those heights are written.
 *  - Blocks: neardata blocks are fetched in parallel and processed strictly in height order.
 *  - Following: once caught up, the loop polls the final head and processes new blocks as they finalize.
 *
 * Only final blocks are read (neardata `last_block/final`), so there are no reorgs to handle.
 * Each block with activity is one transaction: its rows and the cursor commit together.
 */
import type pg from "pg";
import type { Config } from "./config.js";
import { decodeReceipt } from "./decode/decoder.js";
import type { Decoded, Row } from "./decode/rows.js";
import { getCursor, setCursor, writeDecoded } from "./db/writer.js";
import { fetchViews, writeViews, type Touched, type ViewSnapshot } from "./db/state.js";
import type { FastnearTxSource, NearRpc } from "./near/fastnear.js";
import type { NeardataSource } from "./near/neardata.js";
import type { ReceiptExec } from "./near/types.js";
import { sleep } from "./http.js";

export interface Logger {
  info(msg: string): void;
  warn(msg: string): void;
}

export interface IndexerDeps {
  config: Config;
  pool: pg.Pool;
  neardata: NeardataSource;
  fastnear: FastnearTxSource;
  rpc: NearRpc;
  log: Logger;
}

export interface RunOptions {
  /** Keep polling for new blocks after catching up. False exits once the head has been reached. */
  follow: boolean;
  /** Stop after this many blocks have been indexed (tests and partial runs). */
  stopAt?: number;
}

export interface Stats {
  blocks: number;
  receipts: number;
  calls: number;
  rows: number;
  decodeErrors: number;
}

/** Entities whose view-backed state changes with the rows of a batch. */
function touchedOf(rows: Row[]): Touched {
  const pools = new Set<number>();
  const locks = new Set<number>();
  const gauges = new Set<number>();
  let epoch = false;
  for (const r of rows) {
    switch (r.kind) {
      case "pool":
        pools.add(r.poolId);
        break;
      case "swap":
        r.path.forEach((h) => pools.add(h.poolId));
        break;
      case "liquidity":
        pools.add(r.poolId);
        break;
      case "bribe":
        pools.add(r.poolId);
        break;
      case "gauge":
        gauges.add(r.poolId);
        if (r.subkind !== "create") pools.add(r.poolId);
        break;
      case "lock_event":
        if (r.tokenId !== null) locks.add(r.tokenId);
        break;
      case "vote":
        locks.add(r.tokenId);
        break;
      case "claim":
        if (r.tokenId !== null) locks.add(r.tokenId);
        break;
      case "epoch":
        epoch = true;
        break;
      default:
        break;
    }
  }
  return { pools, locks, gauges, epoch };
}

function mergeDecoded(parts: Decoded[]): Decoded {
  return {
    calls: parts.flatMap((p) => p.calls),
    rows: parts.flatMap((p) => p.rows),
  };
}

export class Indexer {
  readonly stats: Stats = { blocks: 0, receipts: 0, calls: 0, rows: 0, decodeErrors: 0 };
  private readonly receivers: ReadonlySet<string>;

  constructor(private readonly deps: IndexerDeps) {
    this.receivers = new Set([deps.config.contractId]);
  }

  /** Cursor, initialised on first run to START_HEIGHT or the height of the contract's first transaction. */
  async ensureCursor(): Promise<number> {
    const client = await this.deps.pool.connect();
    try {
      const existing = await getCursor(client);
      if (existing !== null) return existing;
      const start =
        this.deps.config.startHeight ??
        (await this.deps.fastnear.firstTxHeight(this.deps.config.contractId));
      if (start === null) throw new Error(`no transactions found for ${this.deps.config.contractId}`);
      const cursor = Math.max(0, start - 1);
      await client.query("begin");
      await setCursor(client, cursor);
      await client.query("commit");
      this.deps.log.info(`cursor initialised: start height ${start} (${this.deps.config.startHeight === null ? "first contract transaction" : "START_HEIGHT"})`);
      return cursor;
    } finally {
      client.release();
    }
  }

  /** Decodes the receipts of one batch and writes them, refreshing view state when `live`. */
  private async writeBatch(height: number, receipts: ReceiptExec[], live: boolean): Promise<void> {
    const parts = receipts.map((r) => decodeReceipt(r, this.deps.config.contractId));
    const decoded = mergeDecoded(parts);
    for (const c of decoded.calls) {
      if (c.decodeError) {
        this.stats.decodeErrors++;
        this.deps.log.warn(`decode error ${c.method} receipt ${c.receiptId} height ${c.height}: ${c.decodeError}`);
      }
    }
    const touched = touchedOf(decoded.rows);
    const changed = touched.pools.size + touched.locks.size + touched.gauges.size > 0 || touched.epoch;
    // Views are read before the transaction (network), written inside it. Only live batches refresh them;
    // backlog runs refresh everything once at the end (refreshAll).
    const views: ViewSnapshot | null = live && changed
      ? await fetchViews(this.deps.rpc, this.deps.config.contractId, touched, null)
      : null;

    const client = await this.deps.pool.connect();
    try {
      await client.query("begin");
      await writeDecoded(client, decoded);
      if (views) await writeViews(client, views, height);
      await setCursor(client, height);
      await client.query("commit");
    } catch (err) {
      await client.query("rollback").catch(() => undefined);
      throw err;
    } finally {
      client.release();
    }
    this.stats.blocks++;
    this.stats.receipts += receipts.length;
    this.stats.calls += decoded.calls.length;
    this.stats.rows += decoded.rows.length;
  }

  /**
   * Indexes the contract's receipts in (from, target] from FastNEAR account history. Receipts of
   * transactions that started before `from` are included when they execute after it (lookback).
   * Batches within `liveWindow` of the head refresh view-backed state; a larger gap refreshes everything once.
   */
  async catchUpFromAccount(from: number, target: number): Promise<void> {
    const cfg = this.deps.config;
    const lookback = 50;
    const hashes = await this.deps.fastnear.accountTxHashes(cfg.contractId, Math.max(0, from - lookback), target);
    const receipts = (await this.deps.fastnear.receiptsOf(hashes, this.receivers, target)).filter(
      (r) => r.height >= from,
    );
    const byHeight = new Map<number, ReceiptExec[]>();
    for (const r of receipts) {
      const list = byHeight.get(r.height) ?? [];
      list.push(r);
      byHeight.set(r.height, list);
    }
    const heights = [...byHeight.keys()].sort((a, b) => a - b);
    if (heights.length > 0 || target - from > cfg.liveWindow) {
      this.deps.log.info(
        `account history [${from}, ${target}]: ${hashes.length} transactions, ${receipts.length} contract receipts in ${heights.length} blocks`,
      );
    }
    for (const h of heights) {
      await this.writeBatch(h, byHeight.get(h) ?? [], target - h < cfg.liveWindow);
    }
    if (target - from > cfg.liveWindow) await this.refreshAll(target);
    const client = await this.deps.pool.connect();
    try {
      await client.query("begin");
      await setCursor(client, target);
      await client.query("commit");
    } catch (err) {
      await client.query("rollback").catch(() => undefined);
      throw err;
    } finally {
      client.release();
    }
  }

  /** Refreshes every pool, lock, gauge and the epoch from the views (used after a backlog run). */
  async refreshAll(height: number): Promise<void> {
    const client = await this.deps.pool.connect();
    let pools: number[] = [];
    let locks: number[] = [];
    try {
      pools = (await client.query<{ pool_id: string }>("select pool_id from pools order by pool_id")).rows.map((r) => Number(r.pool_id));
      locks = (await client.query<{ token_id: string }>("select distinct token_id from lock_events where token_id is not null")).rows.map((r) => Number(r.token_id));
    } finally {
      client.release();
    }
    const views = await fetchViews(
      this.deps.rpc,
      this.deps.config.contractId,
      { pools: new Set(), locks: new Set(), gauges: new Set(), epoch: true },
      { pools, locks },
    );
    const w = await this.deps.pool.connect();
    try {
      await w.query("begin");
      await writeViews(w, views, height);
      await w.query("commit");
    } finally {
      w.release();
    }
    this.deps.log.info(`views refreshed: ${pools.length} pools, ${locks.length} locks, gauges, epoch`);
  }

  /** Blocks [from, to] fetched in parallel, written in height order. */
  private async blockBatch(from: number, to: number, live: boolean): Promise<number> {
    const heights: number[] = [];
    for (let h = from; h <= to; h++) heights.push(h);
    const snaps = await Promise.all(heights.map((h) => this.deps.neardata.block(h)));
    let last = from - 1;
    for (const snap of snaps) {
      last = snap.height;
      if (snap.receipts.length > 0) await this.writeBatch(snap.height, snap.receipts, live);
    }
    // Trailing empty blocks still move the cursor, in one small transaction.
    const cursorNow = await this.currentCursor();
    if (last > cursorNow) {
      const client = await this.deps.pool.connect();
      try {
        await client.query("begin");
        await setCursor(client, last);
        await client.query("commit");
      } finally {
        client.release();
      }
    }
    return last;
  }

  private async currentCursor(): Promise<number> {
    const client = await this.deps.pool.connect();
    try {
      return (await getCursor(client)) ?? -1;
    } finally {
      client.release();
    }
  }

  /**
   * Runs until the head is reached (or forever with `follow`). Errors (rate limits, network) back off and retry;
   * the cursor only moves after a batch has committed, so a retry resumes exactly where the failure happened.
   */
  async run(opts: RunOptions): Promise<void> {
    const cfg = this.deps.config;
    let cursor = await this.ensureCursor();
    let failures = 0;
    let lastHeartbeat = 0;
    for (;;) {
      if (opts.stopAt !== undefined && cursor >= opts.stopAt) return;
      try {
        // The head is read at final finality. In account mode the last `finalityMargin` blocks are left for
        // the account index to catch up (not applied when an explicit stopAt bounds the run).
        const head = cfg.backfillMode === "account"
          ? await this.deps.rpc.finalHeight()
          : await this.deps.neardata.finalHeight();
        const bound = opts.stopAt !== undefined
          ? Math.min(head, opts.stopAt)
          : head - (cfg.backfillMode === "account" ? cfg.finalityMargin : 0);
        if (cursor >= bound) {
          if (!opts.follow) return;
          if (Date.now() - lastHeartbeat >= 60_000) {
            lastHeartbeat = Date.now();
            this.deps.log.info(`caught up at ${cursor} (head ${head}); blocks with activity ${this.stats.blocks}`);
          }
          failures = 0;
          await sleep(cfg.pollMs);
          cursor = await this.currentCursor();
          continue;
        }
        if (cfg.backfillMode === "account") {
          await this.catchUpFromAccount(cursor + 1, bound);
          if (Date.now() - lastHeartbeat >= 60_000) {
            lastHeartbeat = Date.now();
            this.deps.log.info(`indexed to ${bound} (head ${head}); blocks with activity ${this.stats.blocks}, calls ${this.stats.calls}, rows ${this.stats.rows}`);
          }
        } else {
          const to = Math.min(bound, cursor + cfg.concurrency);
          await this.blockBatch(cursor + 1, to, head - to < cfg.liveWindow);
          if (Date.now() - lastHeartbeat >= 10_000 || to === bound) {
            lastHeartbeat = Date.now();
            this.deps.log.info(`indexed to ${to} (head ${head}); blocks with activity ${this.stats.blocks}, calls ${this.stats.calls}, rows ${this.stats.rows}`);
          }
        }
        failures = 0;
        cursor = await this.currentCursor();
      } catch (err) {
        if (opts.stopAt !== undefined && failures >= 20) throw err;
        failures++;
        const delay = Math.min(60_000, 1000 * 2 ** Math.min(failures, 6));
        this.deps.log.warn(`${(err as Error).message}; retrying in ${delay / 1000}s`);
        await sleep(delay);
        cursor = await this.currentCursor();
      }
    }
  }
}
