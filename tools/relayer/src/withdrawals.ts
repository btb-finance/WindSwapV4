import { computeFees } from "./fees.js";
import { isContractRejection } from "./near.js";
import { planBroadcast, type SignedWithdrawal } from "./broadcast.js";
import { txHashOf } from "./evm.js";
import type { EvmChainConfig } from "./config.js";
import type { EvmReader, Hex, Logger, NearReader, NearWriter, WithdrawalRecord } from "./types.js";

export interface WithdrawalDeps {
  near: NearReader;
  writer: NearWriter | null;
  log: Logger;
  limit: number;
  dryRun: boolean;
}

export interface WithdrawalPassStats {
  chain: string;
  processed: number;
  broadcast: number;
  marked: number;
  errors: number;
}

function normalizeHex(raw: string): Hex {
  return (raw.startsWith("0x") ? raw : "0x" + raw) as Hex;
}

function errText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Pending withdrawals for one chain, processed in id order and one at a time. Stops at the first
 * failure so later withdrawals never jump ahead. A contract rejection for a withdrawal that is no
 * longer Pending (someone else processed it) is skipped.
 */
export async function processPending(
  deps: WithdrawalDeps,
  chain: EvmChainConfig,
  reader: EvmReader,
  pending: WithdrawalRecord[],
): Promise<WithdrawalPassStats> {
  const stats: WithdrawalPassStats = { chain: chain.name, processed: 0, broadcast: 0, marked: 0, errors: 0 };
  if (pending.length === 0) return stats;

  let capPerGas: bigint;
  const info = await deps.near.getEvmChain(chain.chainId).catch((err: unknown) => {
    if (!deps.dryRun) throw err;
    deps.log.warn("get_evm_chain unavailable, using maxFeePerGasCapFallback", { error: errText(err) });
    return null;
  });
  if (info) capPerGas = BigInt(info.max_fee_per_gas_cap);
  else if (chain.maxFeePerGasCapFallback) capPerGas = BigInt(chain.maxFeePerGasCapFallback);
  else throw new Error(`no fee cap for chain ${chain.chainId}`);

  const fee = await reader.feeData();
  const ordered = [...pending].sort((a, b) => a.id - b.id);
  for (const w of ordered) {
    const { maxFeePerGas, maxPriorityFeePerGas } = computeFees({
      baseFeePerGas: fee.baseFeePerGas,
      priorityFeePerGas: fee.priorityFeePerGas,
      capPerGas,
    });
    const args = {
      id: w.id,
      max_fee_per_gas: maxFeePerGas.toString(),
      max_priority_fee_per_gas: maxPriorityFeePerGas.toString(),
    };
    if (deps.dryRun || !deps.writer) {
      deps.log.info("[dry-run] would process_withdrawal", { ...args, to: w.to_address, amount: w.amount });
      stats.processed++;
      continue;
    }
    try {
      await deps.writer.processWithdrawal(args);
      stats.processed++;
      deps.log.info("processed withdrawal", args);
    } catch (err) {
      stats.errors++;
      if (isContractRejection(err)) {
        const current = await deps.near.getWithdrawal(w.id).catch(() => null);
        if (current && current.status !== "Pending") {
          deps.log.warn("withdrawal no longer pending, skipping", { id: w.id, status: current.status });
          continue;
        }
      }
      deps.log.error("process_withdrawal failed, stopping chain pass", { id: w.id, error: errText(err) });
      break;
    }
  }
  return stats;
}

/**
 * Broadcasts signed withdrawals for one chain in nonce order. Only nonces starting at the vault's
 * pending nonce are sent. Stale entries (nonce already used) are marked broadcast if their tx hash
 * is known to the node, otherwise they are reported for manual review.
 */
export async function broadcastSigned(
  deps: WithdrawalDeps,
  chain: EvmChainConfig,
  reader: EvmReader,
  signed: WithdrawalRecord[],
): Promise<WithdrawalPassStats> {
  const stats: WithdrawalPassStats = { chain: chain.name, processed: 0, broadcast: 0, marked: 0, errors: 0 };
  if (signed.length === 0) return stats;

  const entries: SignedWithdrawal[] = [];
  for (const w of signed) {
    if (w.nonce === undefined || w.nonce === null || !w.raw_tx) {
      deps.log.error("signed withdrawal missing nonce or raw_tx", { id: w.id });
      stats.errors++;
      continue;
    }
    const rawTx = normalizeHex(w.raw_tx);
    entries.push({ id: w.id, nonce: Number(w.nonce), rawTx, txHash: txHashOf(rawTx) });
  }

  const pendingNonce = await reader.nonce(chain.vault, "pending");
  const plan = planBroadcast(entries, pendingNonce);
  if (plan.problem) {
    deps.log.error("signed withdrawal set is inconsistent, nothing broadcast", { chain: chain.name, problem: plan.problem });
    stats.errors++;
    return stats;
  }

  for (const w of plan.stale) {
    const known = await reader.hasTransaction(w.txHash);
    if (known) {
      await markBroadcast(deps, stats, w, w.txHash, "stale nonce, tx already known to node");
    } else {
      stats.errors++;
      deps.log.error("stale signed withdrawal, nonce used by an unknown tx, manual review needed", {
        id: w.id,
        nonce: w.nonce,
        pendingNonce,
      });
    }
  }

  for (const w of plan.ready) {
    if (deps.dryRun || !deps.writer) {
      deps.log.info("[dry-run] would broadcast withdrawal", { id: w.id, nonce: w.nonce, txHash: w.txHash });
      stats.broadcast++;
      continue;
    }
    let hash: Hex;
    try {
      hash = await reader.sendRawTransaction(w.rawTx);
      stats.broadcast++;
    } catch (err) {
      const text = errText(err).toLowerCase();
      if (/already known|known transaction|already imported/.test(text)) {
        hash = w.txHash;
      } else if (/nonce too low/.test(text) && (await reader.hasTransaction(w.txHash))) {
        hash = w.txHash;
      } else {
        stats.errors++;
        deps.log.error("broadcast failed, stopping chain pass", { id: w.id, nonce: w.nonce, error: errText(err) });
        break;
      }
    }
    await markBroadcast(deps, stats, w, hash, "broadcast");
  }

  return stats;
}

async function markBroadcast(
  deps: WithdrawalDeps,
  stats: WithdrawalPassStats,
  w: SignedWithdrawal,
  hash: Hex,
  why: string,
): Promise<void> {
  if (deps.dryRun || !deps.writer) {
    deps.log.info("[dry-run] would mark_withdrawal_broadcast", { id: w.id, tx_hash: hash, why });
    stats.marked++;
    return;
  }
  try {
    await deps.writer.markWithdrawalBroadcast({ id: w.id, tx_hash: hash });
    stats.marked++;
    deps.log.info("marked withdrawal broadcast", { id: w.id, tx_hash: hash, why });
  } catch (err) {
    // Not fatal: the next pass sees nonce < pendingNonce, finds the tx by hash, and marks it then.
    stats.errors++;
    deps.log.error("mark_withdrawal_broadcast failed, will retry next pass", { id: w.id, error: errText(err) });
  }
}
