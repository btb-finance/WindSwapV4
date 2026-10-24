import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { normalizeNeardataBlock, normalizeTxTree, type RawTxTree } from "../src/near/normalize.js";
import type { ReceiptExec } from "../src/near/types.js";

export const CONTRACT = "windswapv4-btb.testnet";
export const RECEIVERS = new Set([CONTRACT]);

const FIX = join(import.meta.dirname, "fixtures");

export function txTree(hash: string): RawTxTree {
  return JSON.parse(readFileSync(join(FIX, "txs", `${hash}.json`), "utf8")) as RawTxTree;
}

export function blockJson(height: number): unknown {
  return JSON.parse(readFileSync(join(FIX, "blocks", `${height}.json`), "utf8"));
}

export function fixtureTxHashes(): string[] {
  return readdirSync(join(FIX, "txs"))
    .filter((f) => f.endsWith(".json"))
    .map((f) => f.replace(/\.json$/, ""));
}

/** All contract receipts of a fixture transaction, from the FastNEAR tree. */
export function receiptsOfTx(hash: string): ReceiptExec[] {
  return normalizeTxTree(txTree(hash), RECEIVERS);
}

/** Receipts of a fixture block, from neardata, restricted to the contract. */
export function receiptsOfBlock(height: number): ReceiptExec[] {
  return normalizeNeardataBlock(blockJson(height), RECEIVERS).receipts;
}

export function findReceipt(hash: string, method: string): ReceiptExec {
  const r = receiptsOfTx(hash).find((x) => x.actions.some((a) => a.kind === "FunctionCall" && a.methodName === method));
  if (!r) throw new Error(`no ${method} receipt in ${hash}`);
  return r;
}
