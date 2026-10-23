import { createHash, randomInt } from "node:crypto";
import type { RelayConfig } from "../config.js";
import { isContractRejection } from "../near.js";
import type {
  ExecuteArgs,
  ExecuteOutcome,
  IntentCancelArgs,
  IntentRecord,
  IntentSubmitArgs,
  Logger,
  NearReader,
  NearWriter,
} from "../types.js";
import { bad, RelayError } from "./errors.js";
import { DailyGasBudget, TokenBucket } from "./limits.js";
import { checkAction, checkIntent, isPlainObject } from "./schema.js";
import { parseWallet, verifyWalletSignature, type ParsedWallet } from "./wallet.js";

/** The NEAR change calls the relay makes. `NearWriter` satisfies it; tests pass a mock. */
export type RelayWriter = Pick<NearWriter, "executeAction" | "submitIntent" | "cancelIntent">;

/** The NEAR view the GET intent route proxies. */
export type IntentReader = Pick<NearReader, "getIntent">;

export interface RelayServiceOptions {
  config: RelayConfig;
  contractId: string;
  relayerAccountId: string;
  /** TGas reserved per `execute` submission (`near.gasTgas.execute`). */
  executeTgas: number;
  /** TGas reserved per intent call (`near.gasTgas.submitIntent` / `near.gasTgas.cancelIntent`). Default 300 each. */
  intentTgas?: { submit: number; cancel: number };
  writer: RelayWriter;
  reader: IntentReader;
  log: Logger;
  /** Clock in milliseconds. Tests inject a fake. */
  now?: () => number;
}

export interface ExecuteResponse {
  txHash: string;
  result?: unknown;
}

export interface IntentResponse {
  txHash: string;
  /** The `u64` id returned by `submit_intent`. */
  intentId: unknown;
}

const ENVELOPE_KEYS = ["wallet", "action", "nonce", "expires", "signature"] as const;
const INTENT_ENVELOPE_KEYS = ["wallet", "intent", "nonce", "expires", "signature"] as const;
const CANCEL_ENVELOPE_KEYS = ["wallet", "id", "nonce", "expires", "signature"] as const;
const MAX_SIGNATURE_LENGTH = 256;
const SWEEP_EVERY_MS = 60_000;
const MAX_IDEMPOTENCY_ENTRIES = 100_000;
const DEFAULT_INTENT_TGAS = 300;

interface IdempotencyEntry {
  fingerprint: string;
  promise: Promise<unknown>;
  /** Set once the submission succeeded; failed entries are removed so the nonce can be retried. */
  expiresAt: number;
}

type RelayKind = "execute" | "submit_intent" | "cancel_intent";

/** The exact string the wallet signs. Must match the contract's `action_message`. */
export function actionMessage(contractId: string, wallet: string, nonce: number, expires: number, action: unknown): string {
  return `Wind Swap v4 action on ${contractId} by ${wallet} nonce ${nonce} expires ${expires}: ${JSON.stringify(action)}`;
}

/** The exact string an EVM or Ed25519 wallet signs to submit an intent. Must match the contract. */
export function intentMessage(contractId: string, wallet: string, nonce: number, expires: number, intent: unknown): string {
  return `Wind Swap v4 intent on ${contractId} by ${wallet} nonce ${nonce} expires ${expires}: ${JSON.stringify(intent)}`;
}

/** The exact string a wallet signs to cancel intent `id`. Has no JSON suffix. */
export function cancelIntentMessage(contractId: string, wallet: string, id: number, nonce: number, expires: number): string {
  return `Wind Swap v4 cancel intent ${id} on ${contractId} by ${wallet} nonce ${nonce} expires ${expires}`;
}

/**
 * Unwraps the request body. Accepts the NEAR-style envelope `{contractId, method, args}` or the bare
 * `args` object, then checks that the field names are exactly `keys`.
 */
function unwrapArgs(raw: unknown, contractId: string, method: string, keys: readonly string[]): Record<string, unknown> {
  if (!isPlainObject(raw)) throw bad("body must be a JSON object");
  let args: unknown = raw;
  if ("contractId" in raw || "method" in raw || "args" in raw) {
    if (raw.contractId !== contractId) throw bad(`contractId must be "${contractId}"`);
    if (raw.method !== method) throw bad(`method must be "${method}"`);
    args = raw.args;
  }
  if (!isPlainObject(args)) throw bad(`args must be an object with ${keys.join(", ")}`);
  const unknownKeys = Object.keys(args).filter((k) => !keys.includes(k));
  if (unknownKeys.length > 0) throw bad(`unknown field(s): ${unknownKeys.join(", ")}`);
  for (const k of keys) {
    if (!(k in args)) throw bad(`missing field: ${k}`);
  }
  return args;
}

/** Checks the fields every signed request carries: nonce, expires, signature. */
function signedFields(args: Record<string, unknown>): { nonce: number; expires: number; signature: string } {
  const { nonce, expires, signature } = args;
  if (typeof nonce !== "number" || !Number.isSafeInteger(nonce) || nonce < 0) {
    throw bad("nonce must be a non-negative integer (a JSON number)");
  }
  if (typeof expires !== "number" || !Number.isSafeInteger(expires) || expires <= 0) {
    throw bad("expires must be a unix time in seconds (a JSON number)");
  }
  if (typeof signature !== "string" || signature.length === 0 || signature.length > MAX_SIGNATURE_LENGTH) {
    throw bad("signature must be a hex or base58 string");
  }
  return { nonce, expires, signature };
}

/**
 * Reads the request body of `POST /v1/execute`. The documented shape is the NEAR-style envelope the
 * frontend posts: `{contractId, method: "execute", args: {wallet, action, nonce, expires, signature}}`.
 * The bare `args` object is accepted too.
 */
export function parseExecuteBody(raw: unknown, contractId: string): ExecuteArgs {
  const args = unwrapArgs(raw, contractId, "execute", ENVELOPE_KEYS);
  return { wallet: args.wallet as string, action: args.action, ...signedFields(args) };
}

/** Reads the request body of `POST /v1/intent`. The intent itself is checked by `checkIntent`. */
export function parseIntentBody(raw: unknown, contractId: string): IntentSubmitArgs & { wallet: string } {
  const args = unwrapArgs(raw, contractId, "submit_intent", INTENT_ENVELOPE_KEYS);
  return { wallet: args.wallet as string, intent: args.intent, ...signedFields(args) };
}

/** Reads the request body of `POST /v1/intent/cancel`. */
export function parseCancelBody(raw: unknown, contractId: string): IntentCancelArgs {
  const args = unwrapArgs(raw, contractId, "cancel_intent", CANCEL_ENVELOPE_KEYS);
  const { id } = args;
  if (typeof id !== "number" || !Number.isSafeInteger(id) || id < 0) {
    throw bad("id must be a non-negative integer (a JSON number)");
  }
  return { wallet: args.wallet as string, id, ...signedFields(args) };
}

export class RelayService {
  private readonly ipLimiter: TokenBucket;
  private readonly walletLimiter: TokenBucket;
  private readonly budget: DailyGasBudget;
  private readonly entries = new Map<string, IdempotencyEntry>();
  private readonly queues = new Map<string, Promise<unknown>>();
  private lastSweep = 0;
  private readonly now: () => number;
  private readonly intentTgas: { submit: number; cancel: number };

  constructor(private readonly opts: RelayServiceOptions) {
    this.ipLimiter = new TokenBucket(opts.config.rateLimit.perIp);
    this.walletLimiter = new TokenBucket(opts.config.rateLimit.perWallet);
    this.budget = new DailyGasBudget(opts.config.dailyGasBudgetTgas);
    this.now = opts.now ?? (() => Date.now());
    this.intentTgas = {
      submit: opts.intentTgas?.submit ?? DEFAULT_INTENT_TGAS,
      cancel: opts.intentTgas?.cancel ?? DEFAULT_INTENT_TGAS,
    };
  }

  health(): { ok: true; contractId: string; relayerAccountId: string } {
    return { ok: true, contractId: this.opts.contractId, relayerAccountId: this.opts.relayerAccountId };
  }

  /** A nonce that is very unlikely to be used: milliseconds times 1000 plus a random part. */
  nonceHint(walletParam: string | undefined, ip: string): { nonce: number } {
    this.takeIp(ip);
    if (walletParam !== undefined) parseWallet(walletParam);
    return { nonce: Date.now() * 1000 + randomInt(1_000_000) };
  }

  /** Validates an execute request and, if it passes, submits it to NEAR once for this (wallet, nonce). */
  async execute(raw: unknown, ip: string): Promise<ExecuteResponse> {
    const nowMs = this.now();
    this.takeIp(ip);

    const req = parseExecuteBody(raw, this.opts.contractId);
    this.checkExpiry(req.expires, nowMs, "action");
    const variant = checkAction(req.action, this.opts.config.allowedActions);
    const wallet: ParsedWallet = parseWallet(req.wallet);
    const message = actionMessage(this.opts.contractId, wallet.canonical, req.nonce, req.expires, req.action);
    await this.checkSignature(wallet, message, req.signature, "action");

    const args: ExecuteArgs = { wallet: wallet.canonical, action: req.action, nonce: req.nonce, expires: req.expires, signature: req.signature };
    return this.dispatch(wallet.canonical, req.nonce, message, req.signature, nowMs, () =>
      this.submit("execute", this.opts.executeTgas, { wallet: wallet.canonical, nonce: req.nonce, variant }, nowMs, () =>
        this.opts.writer.executeAction(args),
      ),
    );
  }

  /**
   * Validates an intent and, if it passes, submits it with `submit_intent` once for this (wallet, nonce).
   * Nonces are shared with `execute`, so an intent and an action with the same nonce conflict (409).
   */
  async submitIntent(raw: unknown, ip: string): Promise<IntentResponse> {
    const nowMs = this.now();
    this.takeIp(ip);

    const req = parseIntentBody(raw, this.opts.contractId);
    this.checkExpiry(req.expires, nowMs, "intent");
    const variant = checkIntent(req.intent);
    const wallet = parseWallet(req.wallet);
    const message = intentMessage(this.opts.contractId, wallet.canonical, req.nonce, req.expires, req.intent);
    await this.checkSignature(wallet, message, req.signature, "intent");

    const args: IntentSubmitArgs = {
      wallet: wallet.canonical,
      intent: req.intent,
      nonce: req.nonce,
      expires: req.expires,
      signature: req.signature,
    };
    const out = await this.dispatch(wallet.canonical, req.nonce, message, req.signature, nowMs, () =>
      this.submit("submit_intent", this.intentTgas.submit, { wallet: wallet.canonical, nonce: req.nonce, variant }, nowMs, () =>
        this.opts.writer.submitIntent(args),
      ),
    );
    return { txHash: out.txHash, intentId: out.result ?? null };
  }

  /** Validates a cancel request and submits `cancel_intent` once for this (wallet, nonce). */
  async cancelIntent(raw: unknown, ip: string): Promise<ExecuteResponse> {
    const nowMs = this.now();
    this.takeIp(ip);

    const req = parseCancelBody(raw, this.opts.contractId);
    this.checkExpiry(req.expires, nowMs, "cancel request");
    const wallet = parseWallet(req.wallet);
    const message = cancelIntentMessage(this.opts.contractId, wallet.canonical, req.id, req.nonce, req.expires);
    await this.checkSignature(wallet, message, req.signature, "cancel request");

    const args: IntentCancelArgs = {
      wallet: wallet.canonical,
      id: req.id,
      nonce: req.nonce,
      expires: req.expires,
      signature: req.signature,
    };
    return this.dispatch(wallet.canonical, req.nonce, message, req.signature, nowMs, () =>
      this.submit("cancel_intent", this.intentTgas.cancel, { wallet: wallet.canonical, nonce: req.nonce, id: req.id }, nowMs, () =>
        this.opts.writer.cancelIntent(args),
      ),
    );
  }

  /** Proxies the `get_intent` view. No gas. 404 when the contract has no such intent. */
  async getIntent(idParam: string, ip: string): Promise<IntentRecord> {
    this.takeIp(ip);
    const id = Number(idParam);
    if (!/^(0|[1-9][0-9]*)$/.test(idParam) || !Number.isSafeInteger(id)) {
      throw bad("intent id must be a non-negative integer");
    }
    let record: IntentRecord | null;
    try {
      record = await this.opts.reader.getIntent(id);
    } catch (err) {
      this.opts.log.error("get_intent failed", { id, error: String(err) });
      throw new RelayError(502, "NEAR view get_intent failed; try again");
    }
    if (record === null) throw new RelayError(404, "intent not found");
    return record;
  }

  private checkExpiry(expires: number, nowMs: number, what: string): void {
    const nowSec = Math.floor(nowMs / 1000);
    if (expires <= nowSec) throw bad(`${what} has expired`);
    if (expires - nowSec > this.opts.config.maxExpirySeconds) {
      throw bad(`expires must be at most ${this.opts.config.maxExpirySeconds} seconds in the future`);
    }
  }

  private async checkSignature(wallet: ParsedWallet, message: string, signature: string, what: string): Promise<void> {
    if (!(await verifyWalletSignature(wallet, message, signature))) {
      throw bad(`signature does not verify for this wallet and ${what}`);
    }
  }

  /**
   * Shared path for every signed change call: idempotency per (wallet, nonce), the per-wallet queue
   * and the per-wallet rate limit. An identical retry gets the stored result; a different body for
   * the same nonce is a conflict.
   */
  private dispatch<T>(
    wallet: string,
    nonce: number,
    message: string,
    signature: string,
    nowMs: number,
    run: () => Promise<T>,
  ): Promise<T> {
    this.sweep(nowMs);
    const key = `${wallet}|${nonce}`;
    const fingerprint = createHash("sha256").update(message).update("\n").update(signature).digest("hex");
    const existing = this.entries.get(key);
    if (existing) {
      if (existing.fingerprint !== fingerprint) {
        throw new RelayError(409, "this nonce was already used for this wallet with a different request");
      }
      return existing.promise as Promise<T>;
    }

    this.takeWallet(wallet);
    const promise = this.enqueue(wallet, run);
    const entry: IdempotencyEntry = { fingerprint, promise, expiresAt: Number.POSITIVE_INFINITY };
    this.entries.set(key, entry);
    promise.then(
      () => {
        entry.expiresAt = this.now() + this.opts.config.idempotencyTtlSeconds * 1000;
      },
      () => {
        if (this.entries.get(key) === entry) this.entries.delete(key);
      },
    );
    return promise;
  }

  /** Runs submissions for one wallet one at a time, so two nonces for the same wallet never race. */
  private enqueue<T>(key: string, task: () => Promise<T>): Promise<T> {
    const prev = this.queues.get(key) ?? Promise.resolve();
    const run = prev.then(task, task);
    const tail = run.then(
      () => undefined,
      () => undefined,
    );
    this.queues.set(key, tail);
    void tail.then(() => {
      if (this.queues.get(key) === tail) this.queues.delete(key);
    });
    return run;
  }

  private async submit(
    kind: RelayKind,
    tgas: number,
    fields: Record<string, unknown>,
    nowMs: number,
    call: () => Promise<ExecuteOutcome>,
  ): Promise<ExecuteResponse> {
    if (!this.budget.reserve(tgas, nowMs)) {
      throw new RelayError(503, "daily gas budget for the relayer is used up; try again after 00:00 UTC");
    }
    try {
      const out = await call();
      this.opts.log.info(`relay ${kind} ok`, { ...fields, txHash: out.txHash });
      return out.result === undefined ? { txHash: out.txHash } : { txHash: out.txHash, result: out.result };
    } catch (err) {
      if (isContractRejection(err)) {
        const message = panicMessage(err);
        this.opts.log.warn(`relay ${kind} rejected by contract`, { ...fields, error: message });
        throw new RelayError(400, message);
      }
      this.opts.log.error(`relay ${kind} failed`, { ...fields, error: String(err) });
      throw new RelayError(
        502,
        "NEAR submission failed. Retrying with the same nonce is safe: the contract refuses a nonce that was already used.",
      );
    }
  }

  private takeIp(ip: string): void {
    const wait = this.ipLimiter.take(ip, this.now());
    if (wait > 0) throw new RelayError(429, "rate limit exceeded for this client", wait);
  }

  private takeWallet(wallet: string): void {
    const wait = this.walletLimiter.take(wallet, this.now());
    if (wait > 0) throw new RelayError(429, "rate limit exceeded for this wallet", wait);
  }

  private sweep(nowMs: number): void {
    if (nowMs - this.lastSweep < SWEEP_EVERY_MS && this.entries.size < MAX_IDEMPOTENCY_ENTRIES) return;
    this.lastSweep = nowMs;
    for (const [k, e] of this.entries) {
      if (e.expiresAt <= nowMs) this.entries.delete(k);
    }
  }
}

/** The contract's panic text, e.g. "action has expired", from a near-js error message. */
export function panicMessage(err: unknown): string {
  const text = err instanceof Error ? err.message : String(err);
  const m = /panicked:\s*(.+)$/s.exec(text);
  const msg = (m?.[1] ?? text).trim().replace(/["']+$/, "");
  return msg.length > 300 ? `${msg.slice(0, 300)}...` : msg;
}
