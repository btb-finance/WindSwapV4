import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import type { AssetInfo, Hex } from "./types.js";

export interface EvmChainConfig {
  /** Short name, also used for the state file: state/<name>.json */
  name: string;
  chainId: number;
  rpcUrl: string;
  vault: Hex;
  /** Blocks to wait before a block is considered final enough to read. */
  confirmations: number;
  startBlock: number;
  scanBatchSize: number;
  requestsPerSecond: number;
  /** Parallel getBlock / getTransactionReceipt calls within one scan batch. Rate limit still applies. */
  blockConcurrency: number;
  /** Dry-run only: fee cap used when the contract view get_evm_chain is unavailable. */
  maxFeePerGasCapFallback?: string;
  /**
   * Register unknown ERC-20 tokens that send to the vault, reading decimals and symbol from the
   * token. Off by default. The contract only accepts it on an enabled chain.
   */
  autoRegister?: boolean;
  /** Lowercase token addresses never auto-registered (their deposits are ignored). */
  denyTokens?: Hex[];
}

export interface NearConfig {
  rpcUrl: string;
  contractId: string;
  relayerAccountId: string;
  credentialsPath: string;
  gasTgas: { report: number; process: number; mark: number; execute: number };
}

export interface RateLimitRule {
  /** Burst size: requests allowed back to back. */
  capacity: number;
  /** Tokens added per second. */
  refillPerSecond: number;
}

export interface RelayConfig {
  host: string;
  port: number;
  /** Exact origins allowed to call the relay from a browser. Empty = no CORS headers. */
  corsOrigins: string[];
  /** Use the first X-Forwarded-For entry as the client IP. Only enable behind a trusted proxy. */
  trustProxy: boolean;
  maxBodyBytes: number;
  /** `expires` may be at most this many seconds ahead of now. */
  maxExpirySeconds: number;
  /** Action variants accepted by the relay. "*" = every variant the contract defines. */
  allowedActions: string[] | "*";
  rateLimit: { perIp: RateLimitRule; perWallet: RateLimitRule };
  /** Gas (in TGas) the relayer may reserve per UTC day. Each submission reserves `gasTgas.execute`. */
  dailyGasBudgetTgas: number;
  /** How long a successful (wallet, nonce) result is replayed for identical requests. */
  idempotencyTtlSeconds: number;
}

export interface RelayerConfig {
  near: NearConfig;
  evm: EvmChainConfig[];
  pollIntervalMs: number;
  stateDir: string;
  /** HTTP relay (`relayer serve`, or `run --http`). Defaults apply when absent. */
  relay: RelayConfig;
  /** Page size for the withdrawal views. */
  withdrawalLimit: number;
  /** Dry-run only: used when get_evm_assets is unavailable on the contract. */
  fallbackAssets: AssetInfo[];
}

const HEX_ADDRESS = /^0x[0-9a-fA-F]{40}$/;

/** A list of 0x addresses, lower-cased. Absent means empty. */
function denyList(v: unknown, path: string): Hex[] {
  if (v === undefined) return [];
  if (!Array.isArray(v)) throw new Error(`${path} must be an array of addresses`);
  return v.map((a, i) => {
    if (typeof a !== "string" || !HEX_ADDRESS.test(a)) throw new Error(`${path}[${i}] is not a 0x address`);
    return a.toLowerCase() as Hex;
  });
}

function obj(v: unknown, path: string): Record<string, unknown> {
  if (typeof v !== "object" || v === null || Array.isArray(v)) throw new Error(`${path} must be an object`);
  return v as Record<string, unknown>;
}

function str(o: Record<string, unknown>, key: string, path: string): string {
  const v = o[key];
  if (typeof v !== "string" || v.length === 0) throw new Error(`${path}.${key} must be a non-empty string`);
  return v;
}

function int(o: Record<string, unknown>, key: string, path: string, min: number): number {
  const v = o[key];
  if (typeof v !== "number" || !Number.isInteger(v) || v < min) {
    throw new Error(`${path}.${key} must be an integer >= ${min}`);
  }
  return v;
}

function optStr(o: Record<string, unknown>, key: string): string | undefined {
  const v = o[key];
  return typeof v === "string" && v.length > 0 ? v : undefined;
}

function expandHome(p: string): string {
  return p === "~" || p.startsWith("~/") ? homedir() + p.slice(1) : p;
}

/** Validates a parsed config object and applies env overrides for the NEAR section. */
export function parseConfig(raw: unknown, env: NodeJS.ProcessEnv = process.env): RelayerConfig {
  const root = obj(raw, "config");
  const nearRaw = obj(root.near, "near");
  const gasRaw = nearRaw.gasTgas === undefined ? {} : obj(nearRaw.gasTgas, "near.gasTgas");

  const near: NearConfig = {
    rpcUrl: env.RELAYER_NEAR_RPC ?? str(nearRaw, "rpcUrl", "near"),
    contractId: env.RELAYER_CONTRACT_ID ?? str(nearRaw, "contractId", "near"),
    relayerAccountId: env.RELAYER_ACCOUNT_ID ?? str(nearRaw, "relayerAccountId", "near"),
    credentialsPath: expandHome(env.RELAYER_CREDENTIALS ?? str(nearRaw, "credentialsPath", "near")),
    gasTgas: {
      report: typeof gasRaw.report === "number" ? gasRaw.report : 50,
      process: typeof gasRaw.process === "number" ? gasRaw.process : 200,
      mark: typeof gasRaw.mark === "number" ? gasRaw.mark : 30,
      execute: typeof gasRaw.execute === "number" ? gasRaw.execute : 300,
    },
  };

  if (!Array.isArray(root.evm) || root.evm.length === 0) throw new Error("evm must be a non-empty array");
  const evm: EvmChainConfig[] = root.evm.map((c, i) => {
    const o = obj(c, `evm[${i}]`);
    const vault = str(o, "vault", `evm[${i}]`);
    if (!HEX_ADDRESS.test(vault)) throw new Error(`evm[${i}].vault is not a 0x address`);
    const cfg: EvmChainConfig = {
      name: str(o, "name", `evm[${i}]`),
      chainId: int(o, "chainId", `evm[${i}]`, 1),
      rpcUrl: str(o, "rpcUrl", `evm[${i}]`),
      vault: vault as Hex,
      confirmations: int(o, "confirmations", `evm[${i}]`, 0),
      startBlock: int(o, "startBlock", `evm[${i}]`, 0),
      scanBatchSize: int(o, "scanBatchSize", `evm[${i}]`, 1),
      requestsPerSecond: typeof o.requestsPerSecond === "number" && o.requestsPerSecond > 0 ? o.requestsPerSecond : 5,
      blockConcurrency: typeof o.blockConcurrency === "number" && o.blockConcurrency >= 1 ? Math.floor(o.blockConcurrency) : 4,
    };
    const cap = optStr(o, "maxFeePerGasCapFallback");
    if (cap !== undefined) cfg.maxFeePerGasCapFallback = cap;
    if (o.autoRegister !== undefined && typeof o.autoRegister !== "boolean") {
      throw new Error(`evm[${i}].autoRegister must be true or false`);
    }
    cfg.autoRegister = o.autoRegister === true;
    cfg.denyTokens = denyList(o.denyTokens, `evm[${i}].denyTokens`);
    return cfg;
  });
  const names = new Set(evm.map((c) => c.name));
  if (names.size !== evm.length) throw new Error("evm[].name must be unique");

  const fallbackAssets: AssetInfo[] = Array.isArray(root.fallbackAssets)
    ? (root.fallbackAssets as AssetInfo[])
    : [];

  return {
    near,
    evm,
    relay: parseRelay(root.relay, env),
    pollIntervalMs: typeof root.pollIntervalMs === "number" ? root.pollIntervalMs : 15_000,
    stateDir: typeof root.stateDir === "string" ? root.stateDir : "state",
    withdrawalLimit: typeof root.withdrawalLimit === "number" ? root.withdrawalLimit : 20,
    fallbackAssets,
  };
}

const ALL_ACTIONS_MARK = "*";

function rule(v: unknown, path: string): RateLimitRule {
  const o = obj(v, path);
  const capacity = o.capacity;
  const refill = o.refillPerSecond;
  if (typeof capacity !== "number" || !Number.isInteger(capacity) || capacity < 1) {
    throw new Error(`${path}.capacity must be an integer >= 1`);
  }
  if (typeof refill !== "number" || !(refill >= 0)) throw new Error(`${path}.refillPerSecond must be a number >= 0`);
  return { capacity, refillPerSecond: refill };
}

/** Parses the optional `relay` section and env overrides (RELAYER_HTTP_HOST, RELAYER_HTTP_PORT, RELAYER_CORS_ORIGINS). */
export function parseRelay(raw: unknown, env: NodeJS.ProcessEnv): RelayConfig {
  const o = raw === undefined ? {} : obj(raw, "relay");
  const limits = o.rateLimit === undefined ? {} : obj(o.rateLimit, "relay.rateLimit");
  const portRaw = env.RELAYER_HTTP_PORT ?? o.port ?? 8787;
  const port = typeof portRaw === "string" ? Number(portRaw) : portRaw;
  if (typeof port !== "number" || !Number.isInteger(port) || port < 0 || port > 65535) {
    throw new Error("relay.port must be an integer 0-65535");
  }
  const originsRaw: unknown = env.RELAYER_CORS_ORIGINS ?? o.corsOrigins ?? [];
  const corsOrigins = typeof originsRaw === "string"
    ? originsRaw.split(",").map((x) => x.trim()).filter(Boolean)
    : (originsRaw as unknown[]).map((x) => String(x));
  const allowed = o.allowedActions ?? ALL_ACTIONS_MARK;
  if (allowed !== ALL_ACTIONS_MARK && !(Array.isArray(allowed) && allowed.every((x) => typeof x === "string"))) {
    throw new Error('relay.allowedActions must be "*" or an array of variant names');
  }
  const intOr = (v: unknown, def: number, path: string, min: number): number => {
    if (v === undefined) return def;
    if (typeof v !== "number" || !Number.isInteger(v) || v < min) throw new Error(`${path} must be an integer >= ${min}`);
    return v;
  };
  return {
    host: typeof env.RELAYER_HTTP_HOST === "string" && env.RELAYER_HTTP_HOST.length > 0
      ? env.RELAYER_HTTP_HOST
      : typeof o.host === "string" ? o.host : "127.0.0.1",
    port,
    corsOrigins,
    trustProxy: o.trustProxy === true,
    maxBodyBytes: intOr(o.maxBodyBytes, 16_384, "relay.maxBodyBytes", 256),
    maxExpirySeconds: intOr(o.maxExpirySeconds, 86_400, "relay.maxExpirySeconds", 1),
    allowedActions: allowed as string[] | "*",
    rateLimit: {
      perIp: limits.perIp === undefined ? { capacity: 20, refillPerSecond: 0.5 } : rule(limits.perIp, "relay.rateLimit.perIp"),
      perWallet: limits.perWallet === undefined ? { capacity: 5, refillPerSecond: 0.05 } : rule(limits.perWallet, "relay.rateLimit.perWallet"),
    },
    dailyGasBudgetTgas: intOr(o.dailyGasBudgetTgas, 30_000, "relay.dailyGasBudgetTgas", 1),
    idempotencyTtlSeconds: intOr(o.idempotencyTtlSeconds, 86_400, "relay.idempotencyTtlSeconds", 1),
  };
}

export function loadConfig(path: string, env: NodeJS.ProcessEnv = process.env): RelayerConfig {
  return parseConfig(JSON.parse(readFileSync(path, "utf8")), env);
}
