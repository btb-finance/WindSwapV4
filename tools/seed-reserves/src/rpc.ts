/**
 * Read-only RPC: NEAR contract views and NEP-141 balances over the public NEAR JSON-RPC, and
 * Base Sepolia reads (vault balances) over its JSON-RPC. Nothing here sends a transaction.
 */

/** NEAR testnet RPCs, tried in turn when one rate-limits (HTTP 429) or fails. Override with NEAR_RPC. */
export const NEAR_RPCS: string[] = process.env.NEAR_RPC
  ? [process.env.NEAR_RPC]
  : ["https://rpc.testnet.near.org", "https://archival-rpc.testnet.near.org", "https://test.rpc.fastnear.com"];
export const NEAR_RPC = NEAR_RPCS[0];
export const BASE_RPC = process.env.BASE_RPC ?? "https://sepolia.base.org";

/** Public RPCs rate-limit; requests are spaced by MIN_GAP_MS and retried with backoff. */
const MIN_GAP_MS = 250;
let lastRequest = 0;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function withRetry<T>(label: string, fn: () => Promise<T>): Promise<T> {
  let lastError: unknown;
  for (let attempt = 1; attempt <= 8; attempt++) {
    const wait = lastRequest + MIN_GAP_MS - Date.now();
    if (wait > 0) await sleep(wait);
    lastRequest = Date.now();
    try {
      return await fn();
    } catch (e) {
      lastError = e;
      // A contract panic is a real answer, not a transient failure: do not retry it.
      if (e instanceof ViewError) throw e;
      await sleep(Math.min(1000 * attempt * attempt, 15000));
    }
  }
  throw new Error(`${label}: ${String(lastError)}`);
}

/** A view that the chain answered with an error (for example a panic such as "pool 9 does not exist"). */
export class ViewError extends Error {}

let rpcTurn = 0;

async function nearQuery(accountId: string, methodName: string, args: unknown): Promise<unknown> {
  const body = {
    jsonrpc: "2.0",
    id: "seed-reserves",
    method: "query",
    params: {
      request_type: "call_function",
      finality: "final",
      account_id: accountId,
      method_name: methodName,
      args_base64: Buffer.from(JSON.stringify(args)).toString("base64"),
    },
  };
  return withRetry(`${accountId}.${methodName}`, async () => {
    const endpoint = NEAR_RPCS[rpcTurn++ % NEAR_RPCS.length];
    const res = await fetch(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const json = (await res.json()) as {
      error?: { cause?: { name?: string; info?: unknown }; message?: string };
      result?: { result?: number[]; error?: string };
    };
    if (json.error) throw new ViewError(`${methodName}: ${JSON.stringify(json.error)}`);
    if (json.result?.error) throw new ViewError(`${methodName}: ${json.result.error}`);
    const bytes = json.result?.result;
    if (!bytes) throw new Error(`${methodName}: no result`);
    const text = Buffer.from(bytes).toString("utf8");
    return text.length === 0 ? null : JSON.parse(text);
  });
}

/** Calls a view of the Wind Swap contract. */
export function view<T>(contract: string, method: string, args: Record<string, unknown> = {}): Promise<T> {
  return nearQuery(contract, method, args) as Promise<T>;
}

/** NEP-141 `ft_balance_of`. Returns null when the token account has no such view. */
export async function ftBalanceOf(token: string, account: string): Promise<bigint | null> {
  try {
    const v = (await nearQuery(token, "ft_balance_of", { account_id: account })) as string;
    return BigInt(v);
  } catch {
    return null;
  }
}

/** NEP-148 decimals, or null if the token has no metadata. */
export async function ftDecimals(token: string): Promise<number | null> {
  try {
    const m = (await nearQuery(token, "ft_metadata", {})) as { decimals?: number };
    return typeof m?.decimals === "number" ? m.decimals : null;
  } catch {
    return null;
  }
}

async function evmCall(method: string, params: unknown[]): Promise<string> {
  return withRetry(`base.${method}`, async () => {
    const res = await fetch(BASE_RPC, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const json = (await res.json()) as { result?: string; error?: unknown };
    if (json.error || typeof json.result !== "string") throw new Error(`${method}: ${JSON.stringify(json.error)}`);
    return json.result;
  });
}

/** Native balance (wei) of an EVM address on Base Sepolia, at the latest block. */
export async function nativeBalance(address: string): Promise<bigint> {
  return BigInt(await evmCall("eth_getBalance", [address, "latest"]));
}

/** ERC-20 `balanceOf(holder)` on Base Sepolia, at the latest block. */
export async function erc20Balance(token: string, holder: string): Promise<bigint> {
  const data = "0x70a08231" + holder.toLowerCase().replace(/^0x/, "").padStart(64, "0");
  return BigInt(await evmCall("eth_call", [{ to: token, data }, "latest"]));
}
