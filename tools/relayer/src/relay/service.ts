import { createHash, randomInt } from "node:crypto";
import type { RelayConfig } from "../config.js";
import { isContractRejection } from "../near.js";
import type { ExecuteArgs, ExecuteOutcome, Logger } from "../types.js";
import { bad, RelayError } from "./errors.js";
import { DailyGasBudget, TokenBucket } from "./limits.js";
import { checkAction, isPlainObject } from "./schema.js";
import { parseWallet, verifyWalletSignature, type ParsedWallet } from "./wallet.js";

/** The one NEAR call the relay makes. `NearWriter` satisfies it; tests pass a mock. */
export interface ExecuteWriter {
  executeAction(args: ExecuteArgs): Promise<ExecuteOutcome>;
}

export interface RelayServiceOptions {
  config: RelayConfig;
  contractId: string;
  relayerAccountId: string;
  /** TGas reserved per submission (`near.gasTgas.execute`). */
  executeTgas: number;
  writer: ExecuteWriter;
  log: Logger;
  /** Clock in milliseconds. Tests inject a fake. */
  now?: () => number;
}

export interface ExecuteResponse {
  txHash: string;
  result?: unknown;
}

const ENVELOPE_KEYS = ["wallet", "action", "nonce", "expires", "signature"] as const;
const MAX_SIGNATURE_LENGTH = 256;
const SWEEP_EVERY_MS = 60_000;
const MAX_IDEMPOTENCY_ENTRIES = 100_000;

interface IdempotencyEntry {
  fingerprint: string;
  promise: Promise<ExecuteResponse>;
  /** Set once the submission succeeded; failed entries are removed so the nonce can be retried. */
  expiresAt: number;
}

/** The exact string the wallet signs. Must match the contract's `action_message`. */
export function actionMessage(contractId: string, wallet: string, nonce: number, expires: number, action: unknown): string {
  return `Wind Swap v4 action on ${contractId} by ${wallet} nonce ${nonce} expires ${expires}: ${JSON.stringify(action)}`;
}

/**
 * Reads the request body. The documented shape is the NEAR-style envelope the frontend posts:
 * `{contractId, method: "execute", args: {wallet, action, nonce, expires, signature}}`.
 * The bare `args` object is accepted too.
 */
export function parseExecuteBody(raw: unknown, contractId: string): ExecuteArgs {
  if (!isPlainObject(raw)) throw bad("body must be a JSON object");
  let args: unknown = raw;
  if ("contractId" in raw || "method" in raw || "args" in raw) {
    if (raw.contractId !== contractId) throw bad(`contractId must be "${contractId}"`);
    if (raw.method !== "execute") throw bad('method must be "execute"');
    args = raw.args;
  }
  if (!isPlainObject(args)) throw bad("args must be an object with wallet, action, nonce, expires, signature");
  const keys = Object.keys(args);
  const unknownKeys = keys.filter((k) => !(ENVELOPE_KEYS as readonly string[]).includes(k));
  if (unknownKeys.length > 0) throw bad(`unknown field(s): ${unknownKeys.join(", ")}`);
  for (const k of ENVELOPE_KEYS) {
    if (!(k in args)) throw bad(`missing field: ${k}`);
  }
  const { wallet, action, nonce, expires, signature } = args as Record<string, unknown>;
  if (typeof nonce !== "number" || !Number.isSafeInteger(nonce) || nonce < 0) {
    throw bad("nonce must be a non-negative integer (a JSON number)");
  }
  if (typeof expires !== "number" || !Number.isSafeInteger(expires) || expires <= 0) {
    throw bad("expires must be a unix time in seconds (a JSON number)");
  }
  if (typeof signature !== "string" || signature.length === 0 || signature.length > MAX_SIGNATURE_LENGTH) {
    throw bad("signature must be a hex or base58 string");
  }
  return { wallet: wallet as string, action, nonce, expires, signature };
}

export class RelayService {
  private readonly ipLimiter: TokenBucket;
  private readonly walletLimiter: TokenBucket;
  private readonly budget: DailyGasBudget;
  private readonly entries = new Map<string, IdempotencyEntry>();
  private readonly queues = new Map<string, Promise<unknown>>();
  private lastSweep = 0;
  private readonly now: () => number;

  constructor(private readonly opts: RelayServiceOptions) {
    this.ipLimiter = new TokenBucket(opts.config.rateLimit.perIp);
    this.walletLimiter = new TokenBucket(opts.config.rateLimit.perWallet);
    this.budget = new DailyGasBudget(opts.config.dailyGasBudgetTgas);
    this.now = opts.now ?? (() => Date.now());
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
    const nowSec = Math.floor(nowMs / 1000);
    if (req.expires <= nowSec) throw bad("action has expired");
    if (req.expires - nowSec > this.opts.config.maxExpirySeconds) {
      throw bad(`expires must be at most ${this.opts.config.maxExpirySeconds} seconds in the future`);
    }
    const variant = checkAction(req.action, this.opts.config.allowedActions);
    const wallet: ParsedWallet = parseWallet(req.wallet);
    const message = actionMessage(this.opts.contractId, wallet.canonical, req.nonce, req.expires, req.action);
    if (!(await verifyWalletSignature(wallet, message, req.signature))) {
      throw bad("signature does not verify for this wallet and action");
    }

    // Idempotency: an identical retry gets the stored result. A different body for the same nonce is a conflict.
    this.sweep(nowMs);
    const key = `${wallet.canonical}|${req.nonce}`;
    const fingerprint = createHash("sha256").update(message).update("\n").update(req.signature).digest("hex");
    const existing = this.entries.get(key);
    if (existing) {
      if (existing.fingerprint !== fingerprint) {
        throw new RelayError(409, "this nonce was already used for this wallet with a different action");
      }
      return existing.promise;
    }

    this.takeWallet(wallet.canonical);
    const args: ExecuteArgs = { wallet: wallet.canonical, action: req.action, nonce: req.nonce, expires: req.expires, signature: req.signature };
    const promise = this.enqueue(wallet.canonical, () => this.submit(args, variant, nowMs));
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

  private async submit(args: ExecuteArgs, variant: string, nowMs: number): Promise<ExecuteResponse> {
    if (!this.budget.reserve(this.opts.executeTgas, nowMs)) {
      throw new RelayError(503, "daily gas budget for the relayer is used up; try again after 00:00 UTC");
    }
    try {
      const out = await this.opts.writer.executeAction(args);
      this.opts.log.info("relay execute ok", { wallet: args.wallet, nonce: args.nonce, variant, txHash: out.txHash });
      return out.result === undefined ? { txHash: out.txHash } : { txHash: out.txHash, result: out.result };
    } catch (err) {
      if (isContractRejection(err)) {
        const message = panicMessage(err);
        this.opts.log.warn("relay execute rejected by contract", { wallet: args.wallet, nonce: args.nonce, variant, error: message });
        throw new RelayError(400, message);
      }
      this.opts.log.error("relay execute failed", { wallet: args.wallet, nonce: args.nonce, variant, error: String(err) });
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
