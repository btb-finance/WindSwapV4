/** JSON over HTTP with retries on 429, 5xx and network errors (exponential backoff with jitter). */
export interface RetryOptions {
  attempts?: number;
  baseDelayMs?: number;
  label?: string;
  /** Per-request timeout. A stalled connection is retried like a network error. */
  timeoutMs?: number;
}

const DEFAULT_TIMEOUT_MS = 20_000;

export class HttpError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "HttpError";
  }
}

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export async function withRetry<T>(fn: () => Promise<T>, opts: RetryOptions = {}): Promise<T> {
  const attempts = opts.attempts ?? 8;
  const base = opts.baseDelayMs ?? 300;
  let lastError: unknown;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    try {
      return await fn();
    } catch (err) {
      lastError = err;
      const retryable =
        !(err instanceof HttpError) || err.status === 429 || err.status >= 500;
      if (!retryable || attempt === attempts) break;
      const delay = base * 2 ** (attempt - 1) * (0.5 + Math.random());
      await sleep(Math.min(delay, 15_000));
    }
  }
  const label = opts.label ? `${opts.label}: ` : "";
  throw new Error(`${label}${(lastError as Error)?.message ?? String(lastError)}`, {
    cause: lastError,
  });
}

export async function getJson<T>(url: string, opts?: RetryOptions): Promise<T> {
  return withRetry(async () => {
    const res = await fetch(url, {
      headers: { accept: "application/json" },
      signal: AbortSignal.timeout(opts?.timeoutMs ?? DEFAULT_TIMEOUT_MS),
    });
    if (!res.ok) throw new HttpError(res.status, `GET ${url} -> HTTP ${res.status}`);
    return (await res.json()) as T;
  }, opts);
}

export async function postJson<T>(url: string, body: unknown, opts?: RetryOptions): Promise<T> {
  return withRetry(async () => {
    const res = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(opts?.timeoutMs ?? DEFAULT_TIMEOUT_MS),
    });
    if (!res.ok) throw new HttpError(res.status, `POST ${url} -> HTTP ${res.status}`);
    return (await res.json()) as T;
  }, opts);
}
