/**
 * Integration tests against a real Postgres (TEST_DATABASE_URL). Run with `npm run test:integration:local`,
 * which starts a throwaway cluster. These tests TRUNCATE the schema of the database they are given.
 *
 * The chain is replayed from fixtures: neardata blocks come from test/fixtures/blocks (a height without a
 * fixture is an empty block), transaction trees from test/fixtures/txs, and view calls return fixed values.
 */
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import pg from "pg";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { migrate } from "../src/db/migrate.js";
import { Indexer, type Logger } from "../src/indexer.js";
import type { Config } from "../src/config.js";
import { normalizeNeardataBlock, normalizeTxTree, type RawTxTree } from "../src/near/normalize.js";
import type { BlockSnapshot, ReceiptExec } from "../src/near/types.js";
import type { FastnearTxSource, NearRpc } from "../src/near/fastnear.js";
import type { NeardataSource } from "../src/near/neardata.js";
import { CONTRACT, RECEIVERS, blockJson } from "./helpers.js";

const URL = process.env.TEST_DATABASE_URL;
const FIX = join(import.meta.dirname, "fixtures");
const silent: Logger = { info: () => undefined, warn: () => undefined };

const TABLES = [
  "calls", "pools", "pool_state", "swaps", "liquidity_events", "positions", "deposits", "withdrawals", "tokens",
  "lock_events", "locks", "votes", "bribes", "claims", "gauges", "gauge_events", "epochs", "wallet_links",
  "chain_events", "settings", "epoch_state",
] as const;

/** Tables whose content does not depend on how the chain was read (view-backed tables are refreshed differently). */
const EVENT_TABLES = TABLES.filter((t) => !["pool_state", "locks", "gauges", "epoch_state"].includes(t));

const config: Config = {
  databaseUrl: URL ?? "",
  network: "testnet",
  contractId: CONTRACT,
  neardataUrl: "fixture",
  fastnearTxUrl: "fixture",
  nearRpcUrl: "fixture",
  startHeight: null,
  stopHeight: null,
  backfillMode: "blocks",
  concurrency: 8,
  pollMs: 10,
  finalityMargin: 0,
  liveWindow: 100,
};

const fixtureBlockHeights = readdirSync(join(FIX, "blocks"))
  .map((f) => Number(f.replace(/\.json$/, "")))
  .sort((a, b) => a - b);

/** Serves fixture blocks; a height without a fixture is an empty block. */
class FixtureNeardata {
  head = 0;
  async finalHeight(): Promise<number> {
    return this.head;
  }
  async block(height: number): Promise<BlockSnapshot> {
    const have = fixtureBlockHeights.includes(height);
    if (!have) return { height, timestampMs: 0, receipts: [] };
    return normalizeNeardataBlock(blockJson(height), RECEIVERS);
  }
}

/** Serves the receipt trees of the fixture transactions, as the FastNEAR account API would. */
class FixtureFastnear {
  trees: RawTxTree[] = readdirSync(join(FIX, "txs")).map(
    (f) => JSON.parse(readFileSync(join(FIX, "txs", f), "utf8")) as RawTxTree,
  );
  async accountTxHashes(_account: string, from: number, to: number): Promise<string[]> {
    return this.trees
      .filter((t) => t.execution_outcome.block_height >= from && t.execution_outcome.block_height <= to)
      .map((t) => t.transaction.hash);
  }
  async receiptsOf(hashes: string[], receivers: ReadonlySet<string>, maxHeight?: number): Promise<ReceiptExec[]> {
    const want = new Set(hashes);
    return this.trees
      .filter((t) => want.has(t.transaction.hash))
      .flatMap((t) => normalizeTxTree(t, receivers, maxHeight));
  }
}

/** Fixed view answers. Real values come from the chain; these only exercise the write path. */
class FixtureRpc {
  async view<T>(_contract: string, method: string, args: Record<string, unknown>): Promise<T> {
    if (method === "get_pool") {
      return {
        pool_id: args.pool_id, token0: "a", token1: "b", initialized: true, tick_spacing: 4,
        slot0: { sqrt_price_x96: "79228162514264337593543950336", tick: 0, observation_index: 0,
          observation_cardinality: 1, observation_cardinality_next: 1 },
        liquidity: "1", staked_liquidity: "0", fee_growth_global0_x128: "0", fee_growth_global1_x128: "0",
      } as T;
    }
    if (method === "get_fee") return 300 as T;
    if (method === "get_pool_reserves") return [["a", "10"], ["b", "20"]] as T;
    if (method === "get_gauges") return [] as T;
    if (method === "get_lock") return null as T;
    if (method === "get_epoch") {
      return { active_period: 1, epoch_count: 0, weekly: "0", next_epoch_start: 2, wind_minted: "0", distribution_pending: false } as T;
    }
    throw new Error(`unexpected view ${method}`);
  }
  /** The fixture chain's head. */
  async finalHeight(): Promise<number> {
    return CHAIN_TO;
  }
}

async function resetSchema(client: pg.Client): Promise<void> {
  const { rows } = await client.query<{ tablename: string }>(
    "select tablename from pg_tables where schemaname = 'public'",
  );
  const views = await client.query<{ viewname: string }>("select viewname from pg_views where schemaname = 'public'");
  for (const v of views.rows) await client.query(`drop view if exists ${v.viewname} cascade`);
  for (const t of rows) await client.query(`drop table if exists ${t.tablename} cascade`);
}

/** Canonical dump of the event tables: rows as sorted JSON, without surrogate ids. */
async function dump(client: pg.Client): Promise<Record<string, string[]>> {
  const out: Record<string, string[]> = {};
  for (const t of EVENT_TABLES) {
    // pools.fee is a view-backed column: the block path does not refresh views, so it is left out.
    const drop = t === "pools" ? "- 'fee'" : "";
    const { rows } = await client.query<{ j: unknown }>(`select to_jsonb(x) - 'id' ${drop} as j from ${t} x`);
    out[t] = rows.map((r) => JSON.stringify(r.j)).sort();
  }
  return out;
}

/** The replayed range: from the first fixture transaction (the contract's deployment) to the last fixture block. */
const CHAIN_FROM = 274356295;
const CHAIN_TO = 274378017;

const describeDb = URL ? describe : describe.skip;

describeDb("indexer against Postgres (fixture chain)", () => {
  let client: pg.Client;
  let pool: pg.Pool;

  beforeAll(async () => {
    client = new pg.Client({ connectionString: URL });
    await client.connect();
    await resetSchema(client);
    await migrate(client);
    pool = new pg.Pool({ connectionString: URL, max: 4 });
  });

  afterAll(async () => {
    await pool?.end();
    await client?.end();
  });

  function indexerWith(neardata: FixtureNeardata, fastnear: FixtureFastnear, cfg: Partial<Config> = {}): Indexer {
    return new Indexer({
      config: { ...config, ...cfg },
      pool,
      neardata: neardata as unknown as NeardataSource,
      fastnear: fastnear as unknown as FastnearTxSource,
      rpc: new FixtureRpc() as unknown as NearRpc,
      log: silent,
    });
  }

  it("indexes the fixture chain into typed tables, with the expected rows", async () => {
    const neardata = new FixtureNeardata();
    neardata.head = CHAIN_TO;
    const indexer = indexerWith(neardata, new FixtureFastnear(), { startHeight: CHAIN_FROM });
    await indexer.run({ follow: false, stopAt: CHAIN_TO });

    const count = async (sql: string) => Number((await client.query<{ n: string }>(sql)).rows[0]?.n);
    // create_pool 0 and 1 (tusdc/tweth) and 4 (base-sepolia:eth/test1)
    expect(await count("select count(*)::text n from pools")).toBe(3);
    expect(await count("select count(*)::text n from pools where pool_id = 4 and token0 = 'base-sepolia:eth'")).toBe(1);
    // report_evm_deposit: ERC-20 (9000 test1) and native (0.003 ETH)
    expect(await count("select count(*)::text n from deposits where kind = 'evm'")).toBe(2);
    expect(await count("select count(*)::text n from deposits where kind = 'evm' and amount = 9000000000000000000000")).toBe(1);
    // ft_on_transfer: 100 tUSDC from windswapv4-user
    expect(await count("select count(*)::text n from deposits where kind = 'ft' and token_id = 'windswapv4-tusdc.testnet'")).toBe(1);
    // execute(MintPosition) on pool 4 and its position
    expect(await count("select count(*)::text n from liquidity_events where kind = 'mint' and pool_id = 4")).toBe(1);
    expect(await count("select count(*)::text n from positions where pool_id = 4 and liquidity = 767178844087728835")).toBe(1);
    // execute(SwapExactInput) on pool 4
    expect(await count("select count(*)::text n from swaps where via = 'signed' and amount_out = 297725470343984462")).toBe(1);
    // the failed execute(SwapExactInput) is a call with ok = false and no swap row
    expect(await count("select count(*)::text n from calls where ok = false and method = 'execute'")).toBe(1);
    expect(await count("select count(*)::text n from swaps where amount_out = 0")).toBe(0);
    const cursor = await client.query<{ last_height: string }>("select last_height from blocks_cursor");
    expect(Number(cursor.rows[0]?.last_height)).toBe(CHAIN_TO);
  }, 300_000);

  it("re-running from an earlier cursor changes nothing (idempotent)", async () => {
    const before = await dump(client);
    await client.query("update blocks_cursor set last_height = $1", [CHAIN_FROM - 1]);
    const neardata = new FixtureNeardata();
    neardata.head = CHAIN_TO;
    const indexer = indexerWith(neardata, new FixtureFastnear(), { startHeight: CHAIN_FROM });
    await indexer.run({ follow: false, stopAt: CHAIN_TO });
    expect(await dump(client)).toEqual(before);
  }, 300_000);

  it("the account-history run writes exactly the same rows as the block run", async () => {
    const blockRun = await dump(client);
    expect(blockRun.pools?.length).toBe(3);
    // Start over and follow the same chain through the account-history loop (run in account mode).
    await resetSchema(client);
    await migrate(client);
    const indexer = indexerWith(new FixtureNeardata(), new FixtureFastnear(), {
      startHeight: CHAIN_FROM,
      backfillMode: "account",
    });
    await indexer.run({ follow: false });
    const accountRun = await dump(client);
    expect(accountRun).toEqual(blockRun);
    const cursor = await client.query<{ last_height: string }>("select last_height from blocks_cursor");
    expect(Number(cursor.rows[0]?.last_height)).toBe(CHAIN_TO);
  }, 300_000);
});
