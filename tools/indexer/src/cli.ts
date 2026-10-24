#!/usr/bin/env node
/**
 * windswap-indexer
 *   migrate   apply db/schema.sql and db/views.sql (idempotent)
 *   run       index the backlog, then follow the chain head (default)
 *   backfill  index the backlog up to the current final head, then exit
 *   status    print the cursor and row counts
 */
import { loadConfig } from "./config.js";
import { createPool } from "./db/connect.js";
import { migrate } from "./db/migrate.js";
import { FastnearTxSource, NearRpc } from "./near/fastnear.js";
import { NeardataSource } from "./near/neardata.js";
import { Indexer } from "./indexer.js";
import { consoleLogger } from "./log.js";
import { getCursor } from "./db/writer.js";

const TABLES = [
  "calls", "pools", "pool_state", "swaps", "liquidity_events", "positions", "deposits", "withdrawals",
  "tokens", "lock_events", "locks", "votes", "bribes", "claims", "gauges", "gauge_events", "epochs",
  "wallet_links", "chain_events",
] as const;

async function withLock<T>(pool: import("pg").Pool, fn: () => Promise<T>): Promise<T> {
  const client = await pool.connect();
  try {
    const { rows } = await client.query<{ locked: boolean }>(
      "select pg_try_advisory_lock(hashtext('windswap-indexer')) as locked",
    );
    if (!rows[0]?.locked) throw new Error("another windswap indexer is running against this database");
    return await fn();
  } finally {
    await client.query("select pg_advisory_unlock(hashtext('windswap-indexer'))").catch(() => undefined);
    client.release();
  }
}

async function main(): Promise<void> {
  const command = process.argv[2] ?? "run";
  const config = loadConfig(process.env, true);
  const pool = createPool(config.databaseUrl);
  try {
    if (command === "migrate") {
      const client = await pool.connect();
      try {
        await migrate(client);
      } finally {
        client.release();
      }
      consoleLogger.info("schema and views applied");
      return;
    }

    if (command === "status") {
      const client = await pool.connect();
      try {
        const cursor = await getCursor(client);
        console.log(`contract ${config.contractId} (${config.network})`);
        console.log(`indexed to height: ${cursor ?? "not started"}`);
        for (const t of TABLES) {
          const { rows } = await client.query<{ n: string }>(`select count(*)::text as n from ${t}`);
          console.log(`${t.padEnd(18)} ${rows[0]?.n ?? "0"}`);
        }
      } finally {
        client.release();
      }
      return;
    }

    if (command !== "run" && command !== "backfill") {
      throw new Error(`unknown command ${command}`);
    }

    const indexer = new Indexer({
      config,
      pool,
      neardata: new NeardataSource(config.neardataUrl, new Set([config.contractId])),
      fastnear: new FastnearTxSource(config.fastnearTxUrl),
      rpc: new NearRpc(config.nearRpcUrl),
      log: consoleLogger,
    });
    await withLock(pool, async () => {
      await indexer.run({ follow: command === "run", stopAt: command === "backfill" ? config.stopHeight ?? undefined : undefined });
    });
    consoleLogger.info(
      `done: blocks ${indexer.stats.blocks}, receipts ${indexer.stats.receipts}, calls ${indexer.stats.calls}, rows ${indexer.stats.rows}, decode errors ${indexer.stats.decodeErrors}`,
    );
  } finally {
    await pool.end();
  }
}

main().catch((err) => {
  console.error(err instanceof Error ? err.stack ?? err.message : err);
  process.exitCode = 1;
});
