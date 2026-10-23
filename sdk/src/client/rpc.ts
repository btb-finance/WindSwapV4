/** Minimal NEAR JSON-RPC client for read-only contract calls (`query` / `call_function`). */

export const DEFAULT_RPC_URL = "https://rpc.testnet.near.org";
export const DEFAULT_CONTRACT_ID = "windswapv4-btb.testnet";

export class WindSwapError extends Error {
  constructor(message: string, readonly details?: unknown) {
    super(message);
    this.name = "WindSwapError";
  }
}

function toBase64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary);
}

/**
 * Calls a view method. `args` is JSON-encoded and sent as base64. The result bytes are decoded
 * as JSON. A contract panic or RPC error throws `WindSwapError`.
 */
export async function callView<T>(
  rpcUrl: string,
  contractId: string,
  method: string,
  args: Record<string, unknown>,
  fetchImpl: typeof fetch,
): Promise<T> {
  const body = {
    jsonrpc: "2.0",
    id: "windswap-sdk",
    method: "query",
    params: {
      request_type: "call_function",
      finality: "final",
      account_id: contractId,
      method_name: method,
      args_base64: toBase64(JSON.stringify(args)),
    },
  };

  const response = await fetchImpl(rpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    throw new WindSwapError(`RPC HTTP ${response.status} ${response.statusText}`);
  }

  const json = (await response.json()) as {
    error?: { message?: string; data?: unknown };
    result?: { result?: number[]; error?: string };
  };
  if (json.error) {
    throw new WindSwapError(`RPC error calling ${method}: ${json.error.message ?? "unknown"}`, json.error);
  }
  const result = json.result;
  if (!result) throw new WindSwapError(`RPC returned no result for ${method}`);
  if (result.error) {
    throw new WindSwapError(`${method} failed: ${result.error}`, result.error);
  }
  if (!result.result) throw new WindSwapError(`RPC returned no result bytes for ${method}`);

  const text = new TextDecoder().decode(Uint8Array.from(result.result));
  return JSON.parse(text) as T;
}
