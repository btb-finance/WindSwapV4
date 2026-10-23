import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { Hex } from "./types.js";

export interface QuarantinedDeposit {
  chainId: number;
  txHash: Hex;
  logIndex: number;
  asset: string;
  amount: string;
  from: Hex;
  blockNumber: string;
  error: string;
}

/**
 * Per-chain watcher state. `nextBlock` is the first block NOT yet fully processed.
 * `quarantined` holds deposits the contract rejected (e.g. over the per-deposit cap); they
 * need operator attention and are not retried automatically.
 */
export interface ChainState {
  chainName: string;
  chainId: number;
  nextBlock: number;
  quarantined: QuarantinedDeposit[];
  updatedAt: string;
}

export function statePath(stateDir: string, chainName: string): string {
  return join(stateDir, `${chainName}.json`);
}

export function freshState(chainName: string, chainId: number, startBlock: number): ChainState {
  return { chainName, chainId, nextBlock: startBlock, quarantined: [], updatedAt: new Date(0).toISOString() };
}

export async function loadState(stateDir: string, chainName: string): Promise<ChainState | null> {
  try {
    const text = await readFile(statePath(stateDir, chainName), "utf8");
    const parsed = JSON.parse(text) as ChainState;
    if (typeof parsed.nextBlock !== "number" || !Array.isArray(parsed.quarantined)) {
      throw new Error(`corrupt state file for ${chainName}`);
    }
    return parsed;
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw err;
  }
}

/** Atomic write: temp file in the same directory, then rename. */
export async function saveState(stateDir: string, state: ChainState): Promise<void> {
  await mkdir(stateDir, { recursive: true });
  const target = statePath(stateDir, state.chainName);
  const tmp = `${target}.tmp`;
  const next: ChainState = { ...state, updatedAt: new Date().toISOString() };
  await writeFile(tmp, JSON.stringify(next, null, 2) + "\n", "utf8");
  await rename(tmp, target);
}
