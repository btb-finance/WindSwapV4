// Shared helpers for the Chain Signatures probe (Phase 0b).
import { keccak_256 } from "@noble/hashes/sha3";
import { base58 } from "@scure/base";

export const RPC = "https://rpc.testnet.near.org";
export const MPC = "v1.signer-prod.testnet";
export const PREDECESSOR = "windswapv4-user.testnet";

export const CASES = [
  { name: "evm", path: "ethereum-1", domainId: 0, curve: "secp256k1" },
  { name: "sol", path: "solana-1", domainId: 1, curve: "ed25519" },
];

// Fixed probe payload: keccak256("windswap v4 chain signatures probe").
export const PROBE_MESSAGE = "windswap v4 chain signatures probe";
export const PAYLOAD_HEX = Buffer.from(
  keccak_256(new TextEncoder().encode(PROBE_MESSAGE)),
).toString("hex");

export function hexToBytes(hex) {
  return Uint8Array.from(Buffer.from(hex, "hex"));
}
export function bytesToHex(bytes) {
  return Buffer.from(bytes).toString("hex");
}

// NEAR key strings look like "secp256k1:<base58>" or "ed25519:<base58>".
export function parseNearKey(s) {
  const [kind, b58] = s.split(":");
  return { kind, bytes: base58.decode(b58) };
}

// Read-only view call through the public JSON-RPC (no CLI, no signer).
export async function viewCall(method, argsObj) {
  const body = {
    jsonrpc: "2.0",
    id: "probe",
    method: "query",
    params: {
      request_type: "call_function",
      finality: "final",
      account_id: MPC,
      method_name: method,
      args_base64: Buffer.from(JSON.stringify(argsObj)).toString("base64"),
    },
  };
  const res = await fetch(RPC, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const json = await res.json();
  if (json.error) throw new Error(JSON.stringify(json.error));
  const out = Buffer.from(json.result.result).toString("utf8");
  return JSON.parse(out);
}
