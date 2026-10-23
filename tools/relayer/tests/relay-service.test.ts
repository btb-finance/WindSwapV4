import { describe, expect, it } from "vitest";
import { RelayError } from "../src/relay/errors.js";
import { actionMessage, panicMessage, parseExecuteBody, RelayService, type RelayServiceOptions } from "../src/relay/service.js";
import { recordingLog } from "./helpers.js";
import {
  CONTRACT,
  RELAYER,
  VOTE,
  ed25519Signer,
  evmSigner,
  mockExecuteWriter,
  mockIntentReader,
  relayCfg,
  signedEnvelope,
  type MockExecuteWriter,
  type Signer,
} from "./relay-helpers.js";

const T0 = 1_760_000_000_000; // ms; 2025-10-09T06:13:20Z

function makeService(opts: {
  writer?: MockExecuteWriter;
  config?: Record<string, unknown>;
  now?: () => number;
} = {}) {
  const writer = opts.writer ?? mockExecuteWriter();
  const clock = { t: T0 };
  const service = new RelayService({
    config: relayCfg(opts.config ?? {}),
    contractId: CONTRACT,
    relayerAccountId: RELAYER,
    executeTgas: 300,
    writer,
    reader: mockIntentReader(),
    log: recordingLog(),
    now: opts.now ?? (() => clock.t),
  } satisfies RelayServiceOptions);
  return { service, writer, clock };
}

/** Envelope with expiry relative to the fake clock (T0), not the wall clock. */
async function envelopeAt(signer: Signer, action: Record<string, unknown>, nonce: number, expires = Math.floor(T0 / 1000) + 600) {
  const { sign } = signer;
  const message = actionMessage(CONTRACT, signer.wallet, nonce, expires, action);
  return {
    contractId: CONTRACT,
    method: "execute",
    args: { wallet: signer.wallet, action, nonce, expires, signature: await sign(message) },
  };
}

async function expectStatus(p: Promise<unknown>, status: number, text?: RegExp): Promise<void> {
  try {
    await p;
  } catch (err) {
    expect(err).toBeInstanceOf(RelayError);
    expect((err as RelayError).status).toBe(status);
    if (text) expect((err as RelayError).message).toMatch(text);
    return;
  }
  throw new Error(`expected status ${status}`);
}

describe("parseExecuteBody", () => {
  it("accepts the envelope and the bare args object", () => {
    const args = { wallet: "evm:0x" + "a".repeat(40), action: VOTE, nonce: 1, expires: 2, signature: "0x" };
    expect(parseExecuteBody({ contractId: CONTRACT, method: "execute", args }, CONTRACT)).toEqual(args);
    expect(parseExecuteBody(args, CONTRACT)).toEqual(args);
  });

  it("rejects the wrong contract, the wrong method, and unknown fields", () => {
    const args = { wallet: "x", action: VOTE, nonce: 1, expires: 2, signature: "0x" };
    expect(() => parseExecuteBody({ contractId: "other.testnet", method: "execute", args }, CONTRACT)).toThrow(/contractId/);
    expect(() => parseExecuteBody({ contractId: CONTRACT, method: "swap", args }, CONTRACT)).toThrow(/method/);
    expect(() => parseExecuteBody({ ...args, gas: 1 }, CONTRACT)).toThrow(/unknown field/);
    expect(() => parseExecuteBody({ wallet: "x", action: VOTE, nonce: 1, expires: 2 }, CONTRACT)).toThrow(/missing field: signature/);
  });

  it("requires nonce and expires as JSON numbers", () => {
    const base = { wallet: "x", action: VOTE, expires: 2, signature: "0x" };
    expect(() => parseExecuteBody({ ...base, nonce: "1" }, CONTRACT)).toThrow(/nonce/);
    expect(() => parseExecuteBody({ ...base, nonce: 1.5 }, CONTRACT)).toThrow(/nonce/);
    expect(() => parseExecuteBody({ ...base, nonce: 1, expires: "2" }, CONTRACT)).toThrow(/expires/);
  });
});

describe("RelayService.execute", () => {
  it("happy path: verifies locally, submits once with the args as received, returns txHash and result", async () => {
    const writer = mockExecuteWriter({ result: { Vote: "Done" } });
    const { service } = makeService({ writer });
    const signer = await evmSigner();
    const env = await envelopeAt(signer, VOTE, 42);
    const out = await service.execute(env, "1.2.3.4");
    expect(out).toEqual({ txHash: "TX1", result: { Vote: "Done" } });
    expect(writer.calls).toEqual([env.args]);
  });

  it("works for Ed25519 wallets", async () => {
    const writer = mockExecuteWriter();
    const { service } = makeService({ writer });
    const env = await envelopeAt(ed25519Signer(), VOTE, 7);
    await expect(service.execute(env, "1.2.3.4")).resolves.toMatchObject({ txHash: "TX1" });
  });

  it("rejects an expired action before any NEAR call", async () => {
    const writer = mockExecuteWriter();
    const { service } = makeService({ writer });
    const env = await envelopeAt(await evmSigner(), VOTE, 1, Math.floor(T0 / 1000) - 1);
    await expectStatus(service.execute(env, "ip"), 400, /expired/);
    expect(writer.calls).toHaveLength(0);
  });

  it("rejects expiry further ahead than maxExpirySeconds", async () => {
    const writer = mockExecuteWriter();
    const { service } = makeService({ writer });
    const env = await envelopeAt(await evmSigner(), VOTE, 1, Math.floor(T0 / 1000) + 86_401);
    await expectStatus(service.execute(env, "ip"), 400, /at most 86400/);
    expect(writer.calls).toHaveLength(0);
  });

  it("rejects a signature by another wallet, or over a different action, before any NEAR call", async () => {
    const writer = mockExecuteWriter();
    const { service } = makeService({ writer });
    const victim = await evmSigner();
    const attacker = await evmSigner();
    const forged = await envelopeAt(attacker, VOTE, 5);
    forged.args.wallet = victim.wallet;
    await expectStatus(service.execute(forged, "ip"), 400, /signature does not verify/);

    const signed = await envelopeAt(victim, VOTE, 6);
    signed.args.action = { Vote: { token_id: 2, pools: [4, 1], weights: ["70", "30"] } };
    await expectStatus(service.execute(signed, "ip"), 400, /signature does not verify/);
    expect(writer.calls).toHaveLength(0);
  });

  it("rejects an action whose key order differs from the contract's declaration order", async () => {
    const writer = mockExecuteWriter();
    const { service } = makeService({ writer });
    const signer = await evmSigner();
    const reordered = { Vote: { token_id: 1, weights: ["70", "30"], pools: [4, 1] } };
    const env = await envelopeAt(signer, reordered, 9);
    await expectStatus(service.execute(env, "ip"), 400, /in that order/);
    expect(writer.calls).toHaveLength(0);
  });

  it("enforces allowedActions", async () => {
    const writer = mockExecuteWriter();
    const { service } = makeService({ writer, config: { allowedActions: ["Vote"] } });
    const signer = await evmSigner();
    await expectStatus(service.execute(await envelopeAt(signer, { Reset: { token_id: 1 } }, 1), "ip"), 403);
    await expect(service.execute(await envelopeAt(signer, VOTE, 2), "ip")).resolves.toMatchObject({ txHash: "TX1" });
  });

  it("maps a contract panic to 400 with the panic message", async () => {
    const writer = mockExecuteWriter({
      error: new Error("Smart contract panicked: action nonce was already used for this wallet"),
    });
    const { service } = makeService({ writer });
    const env = await envelopeAt(await evmSigner(), VOTE, 3);
    await expectStatus(service.execute(env, "ip"), 400, /^action nonce was already used/);
  });

  it("maps a NEAR RPC failure to 502 without leaking the cause", async () => {
    const writer = mockExecuteWriter({ error: new Error("connect ECONNREFUSED 10.0.0.1:443") });
    const { service } = makeService({ writer });
    const env = await envelopeAt(await evmSigner(), VOTE, 4);
    await expectStatus(service.execute(env, "ip"), 502, /Retrying with the same nonce is safe/);
  });

  it("is idempotent for (wallet, nonce): an identical retry returns the stored result without a second submission", async () => {
    const writer = mockExecuteWriter();
    const { service } = makeService({ writer });
    const env = await envelopeAt(await evmSigner(), VOTE, 11);
    const first = await service.execute(env, "ip");
    const second = await service.execute(env, "ip");
    expect(second).toEqual(first);
    expect(writer.calls).toHaveLength(1);
  });

  it("returns 409 when the same (wallet, nonce) is reused for a different action", async () => {
    const writer = mockExecuteWriter();
    const { service } = makeService({ writer });
    const signer = await evmSigner();
    await service.execute(await envelopeAt(signer, VOTE, 12), "ip");
    await expectStatus(
      service.execute(await envelopeAt(signer, { Reset: { token_id: 1 } }, 12), "ip"),
      409,
    );
    expect(writer.calls).toHaveLength(1);
  });

  it("deduplicates concurrent identical requests while the first is in flight", async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const writer = mockExecuteWriter({ gate });
    const { service } = makeService({ writer });
    const env = await envelopeAt(await evmSigner(), VOTE, 13);
    const a = service.execute(env, "ip");
    const b = service.execute(env, "ip");
    release();
    expect(await a).toEqual(await b);
    expect(writer.calls).toHaveLength(1);
  });

  it("does not remember a failed submission, so the same nonce can be retried", async () => {
    let fail = true;
    const calls: unknown[] = [];
    const writer = {
      ...mockExecuteWriter(),
      calls: calls as MockExecuteWriter["calls"],
      async executeAction(args: Parameters<MockExecuteWriter["executeAction"]>[0]) {
        calls.push(args);
        if (fail) throw new Error("network down");
        return { txHash: "OK" };
      },
    };
    const { service } = makeService({ writer });
    const env = await envelopeAt(await evmSigner(), VOTE, 14);
    await expectStatus(service.execute(env, "ip"), 502);
    fail = false;
    await expect(service.execute(env, "ip")).resolves.toEqual({ txHash: "OK" });
    expect(calls).toHaveLength(2);
  });

  it("serialises submissions per wallet", async () => {
    const order: string[] = [];
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const writer = {
      ...mockExecuteWriter(),
      calls: [] as MockExecuteWriter["calls"],
      async executeAction(args: Parameters<MockExecuteWriter["executeAction"]>[0]) {
        order.push(`start ${args.nonce}`);
        if (args.nonce === 21) await gate;
        order.push(`end ${args.nonce}`);
        return { txHash: `T${args.nonce}` };
      },
    };
    const { service } = makeService({ writer });
    const signer = await evmSigner();
    const first = service.execute(await envelopeAt(signer, VOTE, 21), "ip");
    const second = service.execute(await envelopeAt(signer, VOTE, 22), "ip");
    await new Promise((r) => setTimeout(r, 10));
    expect(order).toEqual(["start 21"]);
    release();
    await Promise.all([first, second]);
    expect(order).toEqual(["start 21", "end 21", "start 22", "end 22"]);
  });

  it("enforces the per-wallet rate limit after validation, with Retry-After", async () => {
    const writer = mockExecuteWriter();
    const { service } = makeService({
      writer,
      config: { rateLimit: { perIp: { capacity: 100, refillPerSecond: 10 }, perWallet: { capacity: 2, refillPerSecond: 0.1 } } },
    });
    const signer = await evmSigner();
    await service.execute(await envelopeAt(signer, VOTE, 31), "ip");
    await service.execute(await envelopeAt(signer, VOTE, 32), "ip");
    try {
      await service.execute(await envelopeAt(signer, VOTE, 33), "ip");
      expect.unreachable();
    } catch (err) {
      expect(err).toMatchObject({ status: 429 });
      expect((err as RelayError).retryAfter).toBeGreaterThan(0);
    }
    expect(writer.calls).toHaveLength(2);
  });

  it("does not let invalid requests consume a victim's wallet budget", async () => {
    const writer = mockExecuteWriter();
    const { service } = makeService({
      writer,
      config: { rateLimit: { perIp: { capacity: 100, refillPerSecond: 10 }, perWallet: { capacity: 1, refillPerSecond: 0 } } },
    });
    const victim = await evmSigner();
    const attacker = await evmSigner();
    for (let i = 0; i < 5; i++) {
      const forged = await envelopeAt(attacker, VOTE, 100 + i);
      forged.args.wallet = victim.wallet;
      await expectStatus(service.execute(forged, "ip"), 400);
    }
    await expect(service.execute(await envelopeAt(victim, VOTE, 200), "ip")).resolves.toMatchObject({ txHash: "TX1" });
  });

  it("enforces the per-IP rate limit before doing any work", async () => {
    const writer = mockExecuteWriter();
    const { service } = makeService({
      writer,
      config: { rateLimit: { perIp: { capacity: 1, refillPerSecond: 0 }, perWallet: { capacity: 100, refillPerSecond: 10 } } },
    });
    const signer = await evmSigner();
    await service.execute(await envelopeAt(signer, VOTE, 41), "9.9.9.9");
    await expectStatus(service.execute(await envelopeAt(signer, VOTE, 42), "9.9.9.9"), 429);
    await expect(service.execute(await envelopeAt(signer, VOTE, 43), "8.8.8.8")).resolves.toBeDefined();
  });

  it("stops submitting when the daily gas budget is used up, and resets on the next UTC day", async () => {
    const writer = mockExecuteWriter();
    const { service, clock } = makeService({ writer, config: { dailyGasBudgetTgas: 600 } });
    const signer = await evmSigner();
    await service.execute(await envelopeAt(signer, VOTE, 51), "ip");
    await service.execute(await envelopeAt(signer, VOTE, 52), "ip");
    await expectStatus(service.execute(await envelopeAt(signer, VOTE, 53), "ip"), 503, /gas budget/);
    expect(writer.calls).toHaveLength(2);
    clock.t += 86_400_000;
    await expect(service.execute(await envelopeAt(signer, VOTE, 54, Math.floor(clock.t / 1000) + 600), "ip")).resolves.toBeDefined();
  });

  it("forgets successful results after the idempotency TTL", async () => {
    const writer = mockExecuteWriter();
    const { service, clock } = makeService({ writer, config: { idempotencyTtlSeconds: 60 } });
    const signer = await evmSigner();
    const env = await envelopeAt(signer, VOTE, 61, Math.floor(T0 / 1000) + 600);
    await service.execute(env, "ip");
    clock.t += 61_000;
    // Past the TTL the relay forgets the result and submits again. The contract is the real guard:
    // it refuses an already-used (wallet, nonce), so the second call would panic on chain.
    await service.execute(env, "ip");
    expect(writer.calls).toHaveLength(2);
  });

  it("reports the relayer account and contract from health()", () => {
    const { service } = makeService();
    expect(service.health()).toEqual({ ok: true, contractId: CONTRACT, relayerAccountId: RELAYER });
  });

  it("hands out nonce hints that are safe integers", () => {
    const { service } = makeService();
    const { nonce } = service.nonceHint(undefined, "ip");
    expect(Number.isSafeInteger(nonce)).toBe(true);
    expect(() => service.nonceHint("nope", "ip")).toThrow(RelayError);
  });
});

describe("panicMessage", () => {
  it("extracts the contract message from a near-js style error", () => {
    expect(panicMessage(new Error("ExecutionError: Smart contract panicked: wallet signature does not verify"))).toBe(
      "wallet signature does not verify",
    );
  });

  it("falls back to the whole message when there is no panic text", () => {
    expect(panicMessage(new Error("boom"))).toBe("boom");
  });
});
