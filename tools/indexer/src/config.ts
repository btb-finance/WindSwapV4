/** Runtime configuration, read from the environment (see README). */
export interface Config {
  databaseUrl: string;
  network: "testnet" | "mainnet";
  contractId: string;
  neardataUrl: string;
  fastnearTxUrl: string;
  nearRpcUrl: string;
  /** First block to index. Unset: the height of the contract's first transaction (found via FastNEAR). */
  startHeight: number | null;
  /** Last block to index for `backfill` (partial runs and cross-checks). Unset: the final head. */
  stopHeight: number | null;
  /**
   * How receipts are read, for both backlog and live following: "account" (FastNEAR account history plus
   * receipt trees; default, a few requests per poll) or "blocks" (every neardata block; rate-limited).
   */
  backfillMode: "account" | "blocks";
  /**
   * Account mode only: blocks behind the final head that are not read yet, so that the account index has
   * time to list transactions of those heights.
   */
  finalityMargin: number;
  /** Blocks fetched concurrently from neardata (blocks mode). */
  concurrency: number;
  /** Delay between polls of the chain head when the indexer is caught up. */
  pollMs: number;
  /** Blocks behind the head treated as "live": view-backed state is refreshed for those batches. */
  liveWindow: number;
}

const DEFAULTS = {
  testnet: {
    neardata: "https://testnet.neardata.xyz",
    tx: "https://tx.test.fastnear.com",
    rpc: "https://rpc.testnet.fastnear.com",
    contract: "windswapv4-btb.testnet",
  },
  mainnet: {
    neardata: "https://mainnet.neardata.xyz",
    tx: "https://tx.main.fastnear.com",
    rpc: "https://rpc.mainnet.fastnear.com",
    contract: "windswapv4-btb.mainnet",
  },
} as const;

function int(env: NodeJS.ProcessEnv, key: string, fallback: number): number {
  const raw = env[key];
  if (raw === undefined || raw === "") return fallback;
  const n = Number(raw);
  if (!Number.isInteger(n) || n < 0) throw new Error(`${key} must be a non-negative integer`);
  return n;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env, requireDb = true): Config {
  const network = env.NEAR_NETWORK === "mainnet" ? "mainnet" : "testnet";
  const d = DEFAULTS[network];
  const databaseUrl = env.DATABASE_URL ?? "";
  if (requireDb && !databaseUrl) throw new Error("DATABASE_URL is required");
  const backfillMode = env.BACKFILL_MODE ?? "account";
  if (backfillMode !== "account" && backfillMode !== "blocks") {
    throw new Error("BACKFILL_MODE must be 'account' or 'blocks'");
  }
  const startRaw = env.START_HEIGHT;
  return {
    databaseUrl,
    network,
    contractId: env.CONTRACT_ID || d.contract,
    neardataUrl: (env.NEARDATA_URL || d.neardata).replace(/\/$/, ""),
    fastnearTxUrl: (env.FASTNEAR_TX_URL || d.tx).replace(/\/$/, ""),
    nearRpcUrl: (env.NEAR_RPC_URL || d.rpc).replace(/\/$/, ""),
    startHeight: startRaw ? int(env, "START_HEIGHT", 0) : null,
    stopHeight: env.STOP_HEIGHT ? int(env, "STOP_HEIGHT", 0) : null,
    backfillMode,
    concurrency: int(env, "CONCURRENCY", 16),
    finalityMargin: int(env, "FINALITY_MARGIN", 10),
    pollMs: int(env, "POLL_MS", 2000),
    liveWindow: int(env, "LIVE_WINDOW", 100),
  };
}
