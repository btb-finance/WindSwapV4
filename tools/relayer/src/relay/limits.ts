import type { RateLimitRule } from "../config.js";

/** Token bucket keyed by client or wallet. Keeps at most `maxKeys` buckets (least recently used go first). */
export class TokenBucket {
  private readonly buckets = new Map<string, { tokens: number; at: number }>();

  constructor(
    private readonly rule: RateLimitRule,
    private readonly maxKeys = 50_000,
  ) {}

  /** Takes one token. Returns 0 when allowed, otherwise the seconds until the next token. */
  take(key: string, nowMs: number): number {
    const prev = this.buckets.get(key);
    const elapsed = prev ? Math.max(0, nowMs - prev.at) / 1000 : 0;
    const tokens = Math.min(this.rule.capacity, (prev?.tokens ?? this.rule.capacity) + elapsed * this.rule.refillPerSecond);
    if (tokens < 1) {
      this.store(key, { tokens, at: nowMs });
      return this.rule.refillPerSecond > 0 ? Math.ceil((1 - tokens) / this.rule.refillPerSecond) : 3600;
    }
    this.store(key, { tokens: tokens - 1, at: nowMs });
    return 0;
  }

  private store(key: string, value: { tokens: number; at: number }): void {
    this.buckets.delete(key);
    this.buckets.set(key, value);
    if (this.buckets.size > this.maxKeys) {
      const oldest = this.buckets.keys().next();
      if (!oldest.done) this.buckets.delete(oldest.value);
    }
  }
}

const DAY_MS = 86_400_000;

/** Gas the relayer may reserve per UTC day. Reservations are not refunded, so a failed call still counts. */
export class DailyGasBudget {
  private day = -1;
  private used = 0;

  constructor(private readonly limitTgas: number) {}

  /** Reserves `tgas` if it fits in today's budget. */
  reserve(tgas: number, nowMs: number): boolean {
    this.roll(nowMs);
    if (this.used + tgas > this.limitTgas) return false;
    this.used += tgas;
    return true;
  }

  usedToday(nowMs: number): number {
    this.roll(nowMs);
    return this.used;
  }

  private roll(nowMs: number): void {
    const day = Math.floor(nowMs / DAY_MS);
    if (day !== this.day) {
      this.day = day;
      this.used = 0;
    }
  }
}
