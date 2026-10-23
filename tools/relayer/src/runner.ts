import { createEvmReader } from "./evm.js";
import type { RelayerConfig } from "./config.js";
import { runDepositPass, type DepositPassStats } from "./watcher.js";
import { broadcastSigned, processPending, type WithdrawalPassStats } from "./withdrawals.js";
import type { EvmReader, Logger, NearReader, NearWriter } from "./types.js";

export interface RunnerDeps {
  config: RelayerConfig;
  near: NearReader;
  writer: NearWriter | null;
  /** EVM readers keyed by chainId. Tests inject mocks; the CLI builds viem clients. */
  readers: Map<number, EvmReader>;
  dryRun: boolean;
  log: Logger;
  shouldStop: () => boolean;
  /** Last logged status per intent id; pass the same map across loop passes. */
  intentStatus?: Map<number, string>;
}

export interface PassOptions {
  fromBlock?: bigint;
  toBlock?: bigint;
}

export interface PassSummary {
  deposits: DepositPassStats[];
  withdrawals: WithdrawalPassStats[];
  errors: string[];
}

export function buildReaders(config: RelayerConfig): Map<number, EvmReader> {
  return new Map(config.evm.map((c) => [c.chainId, createEvmReader(c)]));
}

/**
 * One full pass: deposits for every chain (report, then advance cursor), then withdrawals
 * (process pending, broadcast signed). Errors are collected per chain/stage so one bad chain or
 * RPC does not stop the rest.
 */
export async function runOnce(deps: RunnerDeps, opts: PassOptions = {}): Promise<PassSummary> {
  const summary: PassSummary = { deposits: [], withdrawals: [], errors: [] };
  const { config, log } = deps;

  for (const chain of config.evm) {
    const reader = deps.readers.get(chain.chainId);
    if (!reader) {
      summary.errors.push(`no reader for chain ${chain.chainId}`);
      continue;
    }
    try {
      const stats = await runDepositPass(
        { config, near: deps.near, writer: deps.writer, log, shouldStop: deps.shouldStop, intentStatus: deps.intentStatus },
        chain,
        reader,
        { dryRun: deps.dryRun, fromBlock: opts.fromBlock, toBlock: opts.toBlock },
      );
      summary.deposits.push(stats);
      log.info("deposit pass done", {
        chain: chain.name,
        from: stats.scannedFrom,
        to: stats.scannedTo,
        found: stats.found.length,
        reported: stats.reported,
        duplicates: stats.duplicates,
        quarantined: stats.quarantined,
        caughtUp: stats.caughtUp,
      });
    } catch (err) {
      summary.errors.push(`deposits ${chain.name}: ${String(err)}`);
      log.error("deposit pass failed", { chain: chain.name, error: String(err) });
    }
  }

  const wdeps = { near: deps.near, writer: deps.writer, log, limit: config.withdrawalLimit, dryRun: deps.dryRun };
  let pending: Awaited<ReturnType<NearReader["getPendingWithdrawals"]>> = [];
  let signed: Awaited<ReturnType<NearReader["getSignedWithdrawals"]>> = [];
  try {
    pending = await deps.near.getPendingWithdrawals(0, config.withdrawalLimit);
    signed = await deps.near.getSignedWithdrawals(0, config.withdrawalLimit);
  } catch (err) {
    // Dry-run tolerates missing views (the contract may not be deployed with them yet).
    if (deps.dryRun) {
      log.warn("withdrawal views unavailable in dry-run, skipping withdrawal stage", { error: String(err) });
    } else {
      summary.errors.push(`withdrawal views: ${String(err)}`);
      log.error("withdrawal views failed", { error: String(err) });
    }
  }

  const knownIds = new Set(config.evm.map((c) => c.chainId));
  for (const w of [...pending, ...signed]) {
    if (!knownIds.has(w.to_chain_id)) log.warn("withdrawal for unconfigured chain, ignored", { id: w.id, to_chain_id: w.to_chain_id });
  }

  for (const chain of config.evm) {
    const reader = deps.readers.get(chain.chainId);
    if (!reader) continue;
    const forChain = (list: typeof pending) => list.filter((w) => w.to_chain_id === chain.chainId);
    try {
      const p = await processPending(wdeps, chain, reader, forChain(pending));
      const b = await broadcastSigned(wdeps, chain, reader, forChain(signed));
      summary.withdrawals.push(p, b);
      if (p.errors + b.errors > 0) summary.errors.push(`withdrawals ${chain.name}: ${p.errors + b.errors} error(s)`);
    } catch (err) {
      summary.errors.push(`withdrawals ${chain.name}: ${String(err)}`);
      log.error("withdrawal pass failed", { chain: chain.name, error: String(err) });
    }
  }

  return summary;
}
