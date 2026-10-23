import { generateKeyPairSync, sign as edSign, type KeyObject } from "node:crypto";
import { privateKeyToAccount, generatePrivateKey } from "viem/accounts";
import { parseRelay, type RelayConfig } from "../src/config.js";
import { actionMessage } from "../src/relay/service.js";
import { base58Encode } from "../src/relay/wallet.js";
import type { ExecuteArgs, ExecuteOutcome, IntentCancelArgs, IntentSubmitArgs, IntentRecord } from "../src/types.js";

export const CONTRACT = "windswapv4-btb.testnet";
export const RELAYER = "windswapv4-relayer.testnet";
export const FRONTEND_ORIGIN = "https://v4.example.test";

export interface Signer {
  wallet: string;
  /** Signs the exact message string the way the wallet does (EIP-191 for EVM, raw bytes for Ed25519). */
  sign(message: string): Promise<string>;
}

export async function evmSigner(): Promise<Signer> {
  const account = privateKeyToAccount(generatePrivateKey());
  return {
    wallet: `evm:${account.address.toLowerCase()}`,
    sign: (message) => account.signMessage({ message }),
  };
}

export function ed25519Signer(): Signer {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const raw = (publicKey as KeyObject).export({ format: "der", type: "spki" }).subarray(-32);
  return {
    wallet: `ed25519:${base58Encode(raw)}`,
    sign: async (message) => edSign(null, Buffer.from(message, "utf8"), privateKey).toString("hex"),
  };
}

export function nowSec(): number {
  return Math.floor(Date.now() / 1000);
}

/** A complete, validly signed execute envelope for `action`. */
export async function signedEnvelope(
  signer: Signer,
  action: Record<string, unknown>,
  opts: { nonce?: number; expires?: number; contractId?: string } = {},
): Promise<{ contractId: string; method: string; args: ExecuteArgs }> {
  const nonce = opts.nonce ?? 1_750_000_000_000_001;
  const expires = opts.expires ?? nowSec() + 600;
  const message = actionMessage(CONTRACT, signer.wallet, nonce, expires, action);
  const signature = await signer.sign(message);
  return {
    contractId: opts.contractId ?? CONTRACT,
    method: "execute",
    args: { wallet: signer.wallet, action, nonce, expires, signature },
  };
}

export const VOTE = { Vote: { token_id: 1, pools: [4, 1], weights: ["70", "30"] } };

export interface MockExecuteWriter {
  calls: ExecuteArgs[];
  intents: IntentSubmitArgs[];
  cancels: IntentCancelArgs[];
  executeAction(args: ExecuteArgs): Promise<ExecuteOutcome>;
  submitIntent(args: IntentSubmitArgs): Promise<ExecuteOutcome>;
  cancelIntent(args: IntentCancelArgs): Promise<ExecuteOutcome>;
}

/**
 * Mock NEAR client for the relay's change calls. Never talks to the network. `execute` returns
 * `result` (default `{Vote: "Done"}`); `submit_intent` returns `intentId` as the u64 result.
 * `error` makes every call reject with that error.
 */
export function mockExecuteWriter(
  opts: { error?: Error; result?: unknown; gate?: Promise<void>; intentId?: number } = {},
): MockExecuteWriter {
  const calls: ExecuteArgs[] = [];
  const intents: IntentSubmitArgs[] = [];
  const cancels: IntentCancelArgs[] = [];
  let txCount = 0;
  return {
    calls,
    intents,
    cancels,
    async executeAction(args) {
      calls.push(args);
      if (opts.gate) await opts.gate;
      if (opts.error) throw opts.error;
      return { txHash: `TX${calls.length}`, result: opts.result ?? { Vote: "Done" } };
    },
    async submitIntent(args) {
      intents.push(args);
      if (opts.error) throw opts.error;
      txCount++;
      return { txHash: `ITX${txCount}`, result: opts.intentId ?? intents.length - 1 };
    },
    async cancelIntent(args) {
      cancels.push(args);
      if (opts.error) throw opts.error;
      txCount++;
      return { txHash: `CTX${txCount}`, result: null };
    },
  };
}

export interface MockIntentReader {
  records: Map<number, IntentRecord>;
  getIntent(id: number): Promise<IntentRecord | null>;
}

/** Mock of the `get_intent` view. Unknown ids resolve to null, like the contract's Option. */
export function mockIntentReader(records: IntentRecord[] = []): MockIntentReader {
  const map = new Map(records.map((r) => [r.id, r] as const));
  return {
    records: map,
    async getIntent(id) {
      return map.get(id) ?? null;
    },
  };
}

/** Relay config with the given overrides; everything else is the documented default. */
export function relayCfg(overrides: Record<string, unknown> = {}): RelayConfig {
  return parseRelay(overrides, {});
}
