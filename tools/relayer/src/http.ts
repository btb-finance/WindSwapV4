import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { RelayConfig } from "./config.js";
import { RelayError } from "./relay/errors.js";
import type { RelayService } from "./relay/service.js";
import type { Logger } from "./types.js";

export interface RelayHttpOptions {
  service: RelayService;
  config: RelayConfig;
  log: Logger;
}

/**
 * HTTP surface of the relay:
 *   POST /v1/execute        body {contractId, method:"execute", args:{wallet, action, nonce, expires, signature}}
 *                           -> 200 {txHash, result?} | 4xx/5xx {error}
 *   GET  /v1/health         -> {ok, contractId, relayerAccountId}
 *   GET  /v1/nonce-hint     ?wallet=<optional> -> {nonce}
 *   POST /v1/intent         body {wallet, intent, nonce, expires, signature} (or the envelope with
 *                           method "submit_intent") -> 200 {txHash, intentId} | 4xx/5xx {error}
 *   POST /v1/intent/cancel  body {wallet, id, nonce, expires, signature} (or method "cancel_intent")
 *                           -> 200 {txHash, result?} | 4xx/5xx {error}
 *   GET  /v1/intent/:id     -> the get_intent view record, 404 {error} when absent
 */
export function createRelayServer(opts: RelayHttpOptions): Server {
  const { service, config, log } = opts;

  return createServer((req, res) => {
    handle(req, res, service, config).catch((err: unknown) => {
      if (err instanceof RelayError) {
        sendError(res, err);
        return;
      }
      log.error("relay request failed", { url: req.url, error: String(err) });
      if (!res.headersSent) sendJson(res, 500, { error: "internal error" });
    });
  });
}

async function handle(req: IncomingMessage, res: ServerResponse, service: RelayService, config: RelayConfig): Promise<void> {
  applyCors(req, res, config);
  if (req.method === "OPTIONS") {
    res.writeHead(204);
    res.end();
    return;
  }

  const url = new URL(req.url ?? "/", "http://relay.local");
  const ip = clientIp(req, config.trustProxy);

  switch (url.pathname) {
    case "/v1/execute": {
      if (req.method !== "POST") throw methodNotAllowed(res, "POST");
      const raw = await readJsonBody(req, config.maxBodyBytes, res);
      sendJson(res, 200, await service.execute(raw, ip));
      return;
    }
    case "/v1/health": {
      if (req.method !== "GET") throw methodNotAllowed(res, "GET");
      sendJson(res, 200, service.health());
      return;
    }
    case "/v1/nonce-hint": {
      if (req.method !== "GET") throw methodNotAllowed(res, "GET");
      sendJson(res, 200, service.nonceHint(url.searchParams.get("wallet") ?? undefined, ip));
      return;
    }
    case "/v1/intent": {
      if (req.method !== "POST") throw methodNotAllowed(res, "POST");
      const raw = await readJsonBody(req, config.maxBodyBytes, res);
      sendJson(res, 200, await service.submitIntent(raw, ip));
      return;
    }
    case "/v1/intent/cancel": {
      if (req.method !== "POST") throw methodNotAllowed(res, "POST");
      const raw = await readJsonBody(req, config.maxBodyBytes, res);
      sendJson(res, 200, await service.cancelIntent(raw, ip));
      return;
    }
    default: {
      const intent = /^\/v1\/intent\/([^/]+)$/.exec(url.pathname);
      if (intent) {
        if (req.method !== "GET") throw methodNotAllowed(res, "GET");
        sendJson(res, 200, await service.getIntent(intent[1] as string, ip));
        return;
      }
      sendJson(res, 404, { error: "not found" });
    }
  }
}

function methodNotAllowed(res: ServerResponse, allow: string): RelayError {
  res.setHeader("allow", allow);
  return new RelayError(405, `method not allowed; use ${allow}`);
}

function applyCors(req: IncomingMessage, res: ServerResponse, config: RelayConfig): void {
  const origin = req.headers.origin;
  if (typeof origin !== "string" || !config.corsOrigins.includes(origin)) return;
  res.setHeader("access-control-allow-origin", origin);
  res.setHeader("vary", "Origin");
  res.setHeader("access-control-allow-methods", "GET, POST, OPTIONS");
  res.setHeader("access-control-allow-headers", "content-type");
  res.setHeader("access-control-max-age", "600");
}

/** The client address used for the per-IP limit. X-Forwarded-For is read only when `trustProxy` is set. */
export function clientIp(req: IncomingMessage, trustProxy: boolean): string {
  if (trustProxy) {
    const xff = req.headers["x-forwarded-for"];
    const first = (Array.isArray(xff) ? xff[0] : xff)?.split(",")[0]?.trim();
    if (first) return first;
  }
  return req.socket.remoteAddress ?? "unknown";
}

function readJsonBody(req: IncomingMessage, maxBytes: number, res: ServerResponse): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const declared = Number(req.headers["content-length"] ?? 0);
    if (declared > maxBytes) {
      res.setHeader("connection", "close");
      reject(new RelayError(413, `body larger than ${maxBytes} bytes`));
      return;
    }
    const chunks: Buffer[] = [];
    let size = 0;
    let done = false;
    req.on("data", (chunk: Buffer) => {
      if (done) return;
      size += chunk.length;
      if (size > maxBytes) {
        done = true;
        res.setHeader("connection", "close");
        reject(new RelayError(413, `body larger than ${maxBytes} bytes`));
        return;
      }
      chunks.push(chunk);
    });
    req.on("error", (err) => {
      if (!done) reject(err);
    });
    req.on("end", () => {
      if (done) return;
      done = true;
      try {
        resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")) as unknown);
      } catch {
        reject(new RelayError(400, "body is not valid JSON"));
      }
    });
  });
}

function sendError(res: ServerResponse, err: RelayError): void {
  if (res.headersSent) return;
  const extra: Record<string, string> = {};
  if (err.retryAfter !== undefined) extra["retry-after"] = String(err.retryAfter);
  sendJson(res, err.status, { error: err.message }, extra);
}

function sendJson(res: ServerResponse, status: number, body: unknown, extra: Record<string, string> = {}): void {
  const data = JSON.stringify(body, (_k, v: unknown) => (typeof v === "bigint" ? v.toString() : v));
  res.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "content-length": Buffer.byteLength(data),
    "cache-control": "no-store",
    ...extra,
  });
  res.end(data);
}

/** Starts listening. Resolves with the bound port (useful when `port` is 0). */
export function listen(server: Server, host: string, port: number): Promise<number> {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, () => {
      server.off("error", reject);
      const addr = server.address();
      resolve(typeof addr === "object" && addr ? addr.port : port);
    });
  });
}
