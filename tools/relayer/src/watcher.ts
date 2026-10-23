import type { EvmChainConfig, RelayerConfig } from "./config.js";
import { isContractRejection } from "./near.js";
import { planScanRange, scanRange } from "./deposits.js";
import { freshState, loadState, saveState, type ChainState } from "./state.js";
import type { AssetInfo, DepositCandidate, EvmReader, Logger, NearReader, NearWriter } from "./types.js";

export interface WatcherDeps {
  config: RelayerConfig;
  near: NearReader;
  /** null in dry-run: no NEAR writes, no state writes. */
  writer: NearWriter | null;
  log: Logger;
  shouldStop: () => boolean;
}

export interface DepositPassOptions {
  dryRun: boolean;
  fromBlock?: bigint;
  toBlock?: bigint;
}

export interface DepositPassStats {
  chain: string;
  scannedFrom?: bigint;
  scannedTo?: bigint;
  found: DepositCandidate[];
  reported: number;
  duplicates: number;
  quarantined: number;
  ignoredTransfers: number;
  revertedNative: number;
  caughtUp: boolean;
}

/** Asset keys registered for `chainId`. Dry-run falls back to config when the view is unavailable. */
export async function loadRegisteredAssets(deps: WatcherDeps, chainId: number, dryRun: boolean): Promise<Set<string>> {
  let assets: AssetInfo[];
  try {
    assets = await deps.near.getEvmAssets();
  } catch (err) {
    if (!dryRun) throw err;
    deps.log.warn("get_evm_assets unavailable, using fallbackAssets from config", { error: String(err) });
    assets = deps.config.fallbackAssets;
  }
  return new Set(
    assets.filter((a) => a.chain_id === chainId).map((a) => a.asset.toLowerCase()),
  );
}

export async function runDepositPass(
  deps: WatcherDeps,
  chain: EvmChainConfig,
  reader: EvmReader,
  opts: DepositPassOptions,
): Promise<DepositPassStats> {
  const { log } = deps;
  const registered = await loadRegisteredAssets(deps, chain.chainId, opts.dryRun);
  const stats: DepositPassStats = {
    chain: chain.name,
    found: [],
    reported: 0,
    duplicates: 0,
    quarantined: 0,
    ignoredTransfers: 0,
    revertedNative: 0,
    caughtUp: false,
  };

  // Dry-run never persists state, so it starts from --from-block, the saved cursor, or startBlock.
  let state: ChainState =
    (await loadState(deps.config.stateDir, chain.name)) ?? freshState(chain.name, chain.chainId, chain.startBlock);
  let from = opts.fromBlock ?? BigInt(Math.max(state.nextBlock, chain.startBlock));
  let head = await reader.blockNumber();

  while (!deps.shouldStop()) {
    let range = planScanRange({
      from,
      head,
      confirmations: chain.confirmations,
      batchSize: chain.scanBatchSize,
      toOverride: opts.toBlock,
    });
    if (!range) {
      head = await reader.blockNumber();
      range = planScanRange({
        from,
        head,
        confirmations: chain.confirmations,
        batchSize: chain.scanBatchSize,
        toOverride: opts.toBlock,
      });
      if (!range) {
        stats.caughtUp = true;
        break;
      }
    }
    stats.scannedFrom ??= range.from;
    stats.scannedTo = range.to;

    log.info("scanning batch", { chain: chain.name, from: range.from, to: range.to, head });
    const result = await scanRange({
      reader,
      chainId: chain.chainId,
      vault: chain.vault,
      range,
      registered,
      log,
      blockConcurrency: chain.blockConcurrency,
    });
    stats.ignoredTransfers += result.ignoredTransfers.length;
    stats.revertedNative += result.revertedNative;
    stats.found.push(...result.deposits);

    for (const dep of result.deposits) {
      const outcome = await reportOne(deps, dep, opts.dryRun);
      if (outcome === "reported") stats.reported++;
      else if (outcome === "duplicate") stats.duplicates++;
      else {
        stats.quarantined++;
        state.quarantined.push({
          chainId: dep.chainId,
          txHash: dep.txHash,
          logIndex: dep.logIndex,
          asset: dep.asset,
          amount: dep.amount.toString(),
          from: dep.from,
          blockNumber: dep.blockNumber.toString(),
          error: outcome.error,
        });
      }
    }

    // Cursor moves only after every deposit in the batch was reported or quarantined. A network
    // error above throws out of this function and the batch is retried on the next pass.
    from = range.to + 1n;
    if (!opts.dryRun) {
      state = { ...state, nextBlock: Number(from) };
      await saveState(deps.config.stateDir, state);
    }
  }
  return stats;
}

type ReportOutcome = "reported" | "duplicate" | { error: string };

async function reportOne(deps: WatcherDeps, dep: DepositCandidate, dryRun: boolean): Promise<ReportOutcome> {
  const args = {
    chain_id: dep.chainId,
    tx_hash: dep.txHash,
    log_index: dep.logIndex,
    from: dep.from,
    asset: dep.asset,
    amount: dep.amount.toString(),
  };
  if (dryRun || !deps.writer) {
    deps.log.info("[dry-run] would report_evm_deposit", { ...args });
    return "reported";
  }
  try {
    const fresh = await deps.writer.reportEvmDeposit(args);
    if (fresh) {
      deps.log.info("reported deposit", { ...args });
      return "reported";
    }
    deps.log.info("deposit already reported", { tx_hash: dep.txHash, log_index: dep.logIndex });
    return "duplicate";
  } catch (err) {
    if (!isContractRejection(err)) throw err;
    deps.log.error("contract rejected deposit, quarantined for operator review", {
      ...args,
      error: String(err),
    });
    return { error: String(err) };
  }
}

