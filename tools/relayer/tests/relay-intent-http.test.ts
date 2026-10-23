import type { Server } from "node:http";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { createRelayServer, listen } from "../src/http.js";
import { cancelIntentMessage, intentMessage, RelayService } from "../src/relay/service.js";
import { recordingLog } from "./helpers.js";
import {
  CONTRACT,
  FRONTEND_ORIGIN,
  RELAYER,
  evmSigner,
  mockExecuteWriter,
  mockIntentReader,
  relayCfg,
  type MockExecuteWriter,
  type Signer,
} from "./relay-helpers.js";

const SWAP = {
  Swap: {
    token_in: "base-sepolia:eth",
    amount_in: "1000",
    path: [{ pool_id: 4, token_in: "base-sepolia:eth" }],
    min_amount_out: "900",
    to: `evm:0x${"ab".repeat(20)}`,
  },
};

let server: Server;
let base: string;
let writer: MockExecuteWriter;

async function start(config: Record<string, unknown> = {}): Promise<void> {
  writer = mockExecuteWriter({ intentId: 42 });
  const cfg = relayCfg({ port: 0, corsOrigins: [FRONTEND_ORIGIN], ...config });
  const service = new RelayService({
    config: cfg,
    contractId: CONTRACT,
    relayerAccountId: RELAYER,
    executeTgas: 300,
    writer,
    reader: mockIntentReader([{ id: 42, status: "Open", owner_hex: "cd".repeat(32) }]),
    log: recordingLog(),
  });
  server = createRelayServer({ service, config: cfg, log: recordingLog() });
  base = `http://127.0.0.1:${await listen(server, "127.0.0.1", 0)}`;
}

beforeEach(async () => {
  await start();
});

function stop(): Promise<void> {
  return new Promise<void>((resolve) => server.close(() => resolve()));
}

afterEach(stop);

const nowSec = () => Math.floor(Date.now() / 1000);

async function signedSubmit(signer: Signer, intent: Record<string, unknown>, nonce = 1_000_001) {
  const expires = nowSec() + 600;
  const signature = await signer.sign(intentMessage(CONTRACT, signer.wallet, nonce, expires, intent));
  return { wallet: signer.wallet, intent, nonce, expires, signature };
}

async function signedCancel(signer: Signer, id: number, nonce = 2_000_001) {
  const expires = nowSec() + 600;
  const signature = await signer.sign(cancelIntentMessage(CONTRACT, signer.wallet, id, nonce, expires));
  return { wallet: signer.wallet, id, nonce, expires, signature };
}

function post(path: string, body: unknown): Promise<Response> {
  return fetch(`${base}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
}

describe("POST /v1/intent", () => {
  it("returns 200 {txHash, intentId} and submits once", async () => {
    const body = await signedSubmit(await evmSigner(), SWAP);
    const res = await post("/v1/intent", body);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ txHash: "ITX1", intentId: 42 });
    expect(writer.intents).toEqual([body]);
  });

  it("accepts the envelope form with method submit_intent", async () => {
    const body = await signedSubmit(await evmSigner(), SWAP);
    const res = await post("/v1/intent", { contractId: CONTRACT, method: "submit_intent", args: body });
    expect(res.status).toBe(200);
    expect(writer.intents).toHaveLength(1);
  });

  it("returns 400 {error} for an invalid `to` and never calls NEAR", async () => {
    const body = await signedSubmit(await evmSigner(), { Swap: { ...SWAP.Swap, to: "evm:0xAB" } });
    const res = await post("/v1/intent", body);
    expect(res.status).toBe(400);
    expect(await res.json()).toMatchObject({ error: expect.stringMatching(/to must be/) });
    expect(writer.intents).toHaveLength(0);
  });

  it("returns 400 for a signature that does not verify", async () => {
    const body = await signedSubmit(await evmSigner(), SWAP);
    body.signature = `0x${"11".repeat(65)}`;
    const res = await post("/v1/intent", body);
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "signature does not verify for this wallet and intent" });
    expect(writer.intents).toHaveLength(0);
  });

  it("returns 400 for invalid JSON and 413 over the body limit", async () => {
    expect((await post("/v1/intent", "{not json")).status).toBe(400);
    await stop();
    await start({ maxBodyBytes: 256 });
    expect((await post("/v1/intent", { pad: "x".repeat(1000) })).status).toBe(413);
  });

  it("returns 405 for GET", async () => {
    const res = await fetch(`${base}/v1/intent`);
    expect(res.status).toBe(405);
    expect(res.headers.get("allow")).toBe("POST");
  });
});

describe("POST /v1/intent/cancel", () => {
  it("returns 200 and submits cancel_intent", async () => {
    const body = await signedCancel(await evmSigner(), 42);
    const res = await post("/v1/intent/cancel", body);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ txHash: "CTX1", result: null });
    expect(writer.cancels).toEqual([body]);
  });

  it("returns 400 for a cancel signed for another id", async () => {
    const body = await signedCancel(await evmSigner(), 42);
    const res = await post("/v1/intent/cancel", { ...body, id: 43 });
    expect(res.status).toBe(400);
    expect(writer.cancels).toHaveLength(0);
  });
});

describe("GET /v1/intent/:id", () => {
  it("proxies get_intent and returns the record", async () => {
    const res = await fetch(`${base}/v1/intent/42`);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ id: 42, status: "Open", owner_hex: "cd".repeat(32) });
  });

  it("returns 404 for an unknown id and 400 for a malformed one", async () => {
    const missing = await fetch(`${base}/v1/intent/7`);
    expect(missing.status).toBe(404);
    expect(await missing.json()).toEqual({ error: "intent not found" });
    expect((await fetch(`${base}/v1/intent/abc`)).status).toBe(400);
  });

  it("returns 405 for POST and 404 for a nested path", async () => {
    const posted = await fetch(`${base}/v1/intent/42`, { method: "POST" });
    expect(posted.status).toBe(405);
    expect(posted.headers.get("allow")).toBe("GET");
    expect((await fetch(`${base}/v1/intent/42/extra`)).status).toBe(404);
  });

  it("answers CORS preflight for an allowed origin", async () => {
    const res = await fetch(`${base}/v1/intent`, {
      method: "OPTIONS",
      headers: { origin: FRONTEND_ORIGIN, "access-control-request-method": "POST" },
    });
    expect(res.status).toBe(204);
    expect(res.headers.get("access-control-allow-origin")).toBe(FRONTEND_ORIGIN);
  });
});
