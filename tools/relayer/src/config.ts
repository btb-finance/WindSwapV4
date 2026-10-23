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
}

export interface NearConfig {
  rpcUrl: string;
  contractId: string;
  relayerAccountId: string;
  credentialsPath: string;
  gasTgas: { report: number; process: number; mark: number };
}

export interface RelayerConfig {
  near: NearConfig;
  evm: EvmChainConfig[];
  pollIntervalMs: number;
  stateDir: string;
  /** Page size for the withdrawal views. */
  withdrawalLimit: number;
  /** Dry-run only: used when get_evm_assets is unavailable on the contract. */
  fallbackAssets: AssetInfo[];
}

const HEX_ADDRESS = /^0x[0-9a-fA-F]{40}$/;

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
    pollIntervalMs: typeof root.pollIntervalMs === "number" ? root.pollIntervalMs : 15_000,
    stateDir: typeof root.stateDir === "string" ? root.stateDir : "state",
    withdrawalLimit: typeof root.withdrawalLimit === "number" ? root.withdrawalLimit : 20,
    fallbackAssets,
  };
}

export function loadConfig(path: string, env: NodeJS.ProcessEnv = process.env): RelayerConfig {
  return parseConfig(JSON.parse(readFileSync(path, "utf8")), env);
}
