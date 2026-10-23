/** A request the relay refuses. `status` is the HTTP status returned to the client. */
export class RelayError extends Error {
  readonly status: number;
  /** Seconds until a retry can succeed (rate limits). Sent as Retry-After. */
  readonly retryAfter?: number;

  constructor(status: number, message: string, retryAfter?: number) {
    super(message);
    this.name = "RelayError";
    this.status = status;
    if (retryAfter !== undefined) this.retryAfter = retryAfter;
  }
}

export function bad(message: string): RelayError {
  return new RelayError(400, message);
}
