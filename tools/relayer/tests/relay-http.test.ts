import type { Server } from "node:http";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { createRelayServer, listen } from "../src/http.js";
import { RelayService } from "../src/relay/service.js";
import { recordingLog } from "./helpers.js";
import {
  CONTRACT,
  FRONTEND_ORIGIN,
  RELAYER,
  VOTE,
  evmSigner,
  mockExecuteWriter,
  mockIntentReader,
  relayCfg,
  signedEnvelope,
  type MockExecuteWriter,
} from "./relay-helpers.js";

let server: Server;
let base: string;
let writer: MockExecuteWriter;

async function start(config: Record<string, unknown> = {}): Promise<void> {
  writer = mockExecuteWriter({ result: { Vote: "Done" } });
  const cfg = relayCfg({ port: 0, corsOrigins: [FRONTEND_ORIGIN], ...config });
  const service = new RelayService({
    config: cfg,
    contractId: CONTRACT,
    relayerAccountId: RELAYER,
    executeTgas: 300,
    writer,
    reader: mockIntentReader(),
    log: recordingLog(),
  });
  server = createRelayServer({ service, config: cfg, log: recordingLog() });
  const port = await listen(server, "127.0.0.1", 0);
  base = `http://127.0.0.1:${port}`;
}

beforeEach(async () => {
  await start();
});

function stop(): Promise<void> {
  return new Promise<void>((resolve) => server.close(() => resolve()));
}

afterEach(stop);

async function post(path: string, body: unknown, headers: Record<string, string> = {}): Promise<Response> {
  return fetch(`${base}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json", ...headers },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
}

describe("POST /v1/execute", () => {
  it("returns 200 {txHash, result} for a valid signed action and submits once", async () => {
    const env = await signedEnvelope(await evmSigner(), VOTE);
    const res = await post("/v1/execute", env);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ txHash: "TX1", result: { Vote: "Done" } });
    expect(writer.calls).toEqual([env.args]);
  });

  it("returns 400 {error} for a bad signature and never calls NEAR", async () => {
    const env = await signedEnvelope(await evmSigner(), VOTE);
    env.args.signature = "0x" + "11".repeat(65);
    const res = await post("/v1/execute", env);
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "signature does not verify for this wallet and action" });
    expect(writer.calls).toHaveLength(0);
  });

  it("returns 400 for invalid JSON and for a body over the size limit", async () => {
    const bad = await post("/v1/execute", "{not json");
    expect(bad.status).toBe(400);
    expect(await bad.json()).toEqual({ error: "body is not valid JSON" });

    await stop();
    await start({ maxBodyBytes: 256 });
    const big = await post("/v1/execute", { pad: "x".repeat(1000) });
    expect(big.status).toBe(413);
  });

  it("returns 405 for GET", async () => {
    const res = await fetch(`${base}/v1/execute`);
    expect(res.status).toBe(405);
    expect(res.headers.get("allow")).toBe("POST");
  });

  it("returns 429 with Retry-After when the wallet is rate limited", async () => {
    await stop();
    await start({ rateLimit: { perIp: { capacity: 50, refillPerSecond: 10 }, perWallet: { capacity: 1, refillPerSecond: 0.01 } } });
    const signer = await evmSigner();
    expect((await post("/v1/execute", await signedEnvelope(signer, VOTE, { nonce: 1 }))).status).toBe(200);
    const res = await post("/v1/execute", await signedEnvelope(signer, VOTE, { nonce: 2 }));
    expect(res.status).toBe(429);
    expect(Number(res.headers.get("retry-after"))).toBeGreaterThan(0);
  });

  it("returns the contract panic as 400", async () => {
    await stop();
    writer = mockExecuteWriter({ error: new Error("Smart contract panicked: action has expired") });
    const cfg = relayCfg({ port: 0 });
    const service = new RelayService({ config: cfg, contractId: CONTRACT, relayerAccountId: RELAYER, executeTgas: 300, writer, reader: mockIntentReader(), log: recordingLog() });
    server = createRelayServer({ service, config: cfg, log: recordingLog() });
    base = `http://127.0.0.1:${await listen(server, "127.0.0.1", 0)}`;
    const res = await post("/v1/execute", await signedEnvelope(await evmSigner(), VOTE));
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "action has expired" });
  });
});

describe("GET endpoints", () => {
  it("health reports the contract and relayer account", async () => {
    const res = await fetch(`${base}/v1/health`);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true, contractId: CONTRACT, relayerAccountId: RELAYER });
  });

  it("nonce-hint returns a safe integer, and rejects a malformed wallet", async () => {
    const res = await fetch(`${base}/v1/nonce-hint`);
    const { nonce } = (await res.json()) as { nonce: number };
    expect(Number.isSafeInteger(nonce)).toBe(true);
    const signer = await evmSigner();
    expect((await fetch(`${base}/v1/nonce-hint?wallet=${signer.wallet}`)).status).toBe(200);
    expect((await fetch(`${base}/v1/nonce-hint?wallet=bob.testnet`)).status).toBe(400);
  });

  it("returns 404 for unknown paths", async () => {
    expect((await fetch(`${base}/nope`)).status).toBe(404);
  });
});

describe("CORS", () => {
  it("answers preflight and echoes an allowed origin only", async () => {
    const ok = await fetch(`${base}/v1/execute`, {
      method: "OPTIONS",
      headers: { origin: FRONTEND_ORIGIN, "access-control-request-method": "POST" },
    });
    expect(ok.status).toBe(204);
    expect(ok.headers.get("access-control-allow-origin")).toBe(FRONTEND_ORIGIN);
    expect(ok.headers.get("access-control-allow-headers")).toContain("content-type");

    const other = await fetch(`${base}/v1/health`, { headers: { origin: "https://evil.example" } });
    expect(other.headers.get("access-control-allow-origin")).toBeNull();
  });
});
