import { describe, expect, it } from "vitest";
import { RelayError } from "../src/relay/errors.js";
import { checkIntent } from "../src/relay/schema.js";
import {
  actionMessage,
  cancelIntentMessage,
  intentMessage,
  parseCancelBody,
  parseIntentBody,
  RelayService,
  type IntentReader,
  type RelayServiceOptions,
} from "../src/relay/service.js";
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

const T0 = 1_760_000_000_000; // ms
const T0_SEC = Math.floor(T0 / 1000);
const ADDR = "0x" + "ab".repeat(20);

const SWAP = {
  Swap: {
    token_in: "base-sepolia:eth",
    amount_in: "1000",
    path: [{ pool_id: 4, token_in: "base-sepolia:eth" }],
    min_amount_out: "900",
    to: "internal",
  },
};

const ADD = {
  AddLiquidity: {
    token_in: "base-sepolia:eth",
    amount_in: "1000",
    pool_id: 4,
    tick_lower: -60,
    tick_upper: 60,
    min_liquidity: "5",
  },
};

function makeService(opts: { writer?: MockExecuteWriter; reader?: IntentReader; config?: Record<string, unknown> } = {}) {
  const writer = opts.writer ?? mockExecuteWriter({ intentId: 7 });
  const service = new RelayService({
    config: relayCfg(opts.config ?? {}),
    contractId: CONTRACT,
    relayerAccountId: RELAYER,
    executeTgas: 300,
    writer,
    reader: opts.reader ?? mockIntentReader(),
    log: recordingLog(),
    now: () => T0,
  } satisfies RelayServiceOptions);
  return { service, writer };
}

/** The body the wallet signs for an intent, using the exact message the contract rebuilds. */
async function signedIntent(
  signer: Signer,
  intent: Record<string, unknown>,
  nonce = 101,
  expires = T0_SEC + 600,
) {
  const signature = await signer.sign(intentMessage(CONTRACT, signer.wallet, nonce, expires, intent));
  return { wallet: signer.wallet, intent, nonce, expires, signature };
}

async function signedCancel(signer: Signer, id: number, nonce = 201, expires = T0_SEC + 600) {
  const signature = await signer.sign(cancelIntentMessage(CONTRACT, signer.wallet, id, nonce, expires));
  return { wallet: signer.wallet, id, nonce, expires, signature };
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

describe("checkIntent: field order and the to format", () => {
  it("accepts Swap with each form of `to`", () => {
    for (const to of ["internal", "near:alice.testnet", "near:windswapv4-btb.testnet", `evm:${ADDR}`]) {
      const intent = { Swap: { ...SWAP.Swap, to } };
      expect(checkIntent(intent)).toBe("Swap");
    }
  });

  it("rejects a `to` that is not internal, near:<account> or evm:0x<40 lowercase hex>", () => {
    for (const to of ["Internal", "near:", "near:Alice.testnet", "bob", `evm:0x${"AB".repeat(20)}`, `evm:${"ab".repeat(20)}`, "evm:0x12"]) {
      expect(() => checkIntent({ Swap: { ...SWAP.Swap, to } }), to).toThrow(/to must be/);
    }
  });

  it("accepts AddLiquidity with the exact field order", () => {
    expect(checkIntent(ADD)).toBe("AddLiquidity");
  });

  it("rejects Swap fields out of declaration order (token_in, amount_in, path, min_amount_out, to)", () => {
    const reordered = { Swap: { amount_in: "1000", token_in: "base-sepolia:eth", path: SWAP.Swap.path, min_amount_out: "900", to: "internal" } };
    expect(() => checkIntent(reordered)).toThrow(/fields must be exactly \[token_in, amount_in, path, min_amount_out, to\] in that order/);
    // Same keys, but `to` before `min_amount_out` is still out of order.
    expect(() => checkIntent({ Swap: { token_in: "x", amount_in: "1", path: [], to: "internal", min_amount_out: "1" } })).toThrow(/in that order/);
  });

  it("rejects AddLiquidity fields out of order, missing, or extra", () => {
    // Built key by key: object spread would keep the original key positions.
    const reordered = {
      AddLiquidity: { amount_in: "1000", token_in: "base-sepolia:eth", pool_id: 4, tick_lower: -60, tick_upper: 60, min_liquidity: "5" },
    };
    expect(() => checkIntent(reordered)).toThrow(/in that order/);
    const { min_liquidity: _drop, ...missing } = ADD.AddLiquidity;
    expect(() => checkIntent({ AddLiquidity: missing })).toThrow(/in that order/);
    expect(() => checkIntent({ AddLiquidity: { ...ADD.AddLiquidity, extra: 1 } })).toThrow(/in that order/);
  });

  it("checks amounts and ticks by type and range", () => {
    expect(() => checkIntent({ Swap: { ...SWAP.Swap, amount_in: 1000 } })).toThrow(/amount_in must be/);
    expect(() => checkIntent({ Swap: { ...SWAP.Swap, amount_in: "-1" } })).toThrow(/amount_in/);
    expect(() => checkIntent({ Swap: { ...SWAP.Swap, amount_in: (1n << 128n).toString() } })).toThrow(/out of range/);
    expect(() => checkIntent({ AddLiquidity: { ...ADD.AddLiquidity, tick_lower: 2 ** 31 } })).toThrow(/32-bit/);
    expect(() => checkIntent({ AddLiquidity: { ...ADD.AddLiquidity, min_liquidity: 5 } })).toThrow(/min_liquidity must be/);
  });

  it("rejects an unknown variant or more than one variant key", () => {
    expect(() => checkIntent({ Bridge: {} })).toThrow(/unknown intent variant "Bridge"/);
    expect(() => checkIntent({ ...SWAP, ...ADD })).toThrow(/exactly one variant key/);
    expect(() => checkIntent("Swap")).toThrow(/exactly one variant key/);
  });
});

describe("request parsing", () => {
  it("accepts the bare body and the envelope for submit_intent and cancel_intent", async () => {
    const body = await signedIntent(await evmSigner(), SWAP);
    expect(parseIntentBody(body, CONTRACT)).toEqual(body);
    expect(parseIntentBody({ contractId: CONTRACT, method: "submit_intent", args: body }, CONTRACT)).toEqual(body);

    const cancel = { wallet: body.wallet, id: 3, nonce: 4, expires: 5, signature: "0x" };
    expect(parseCancelBody(cancel, CONTRACT)).toEqual(cancel);
    expect(parseCancelBody({ contractId: CONTRACT, method: "cancel_intent", args: cancel }, CONTRACT)).toEqual(cancel);
  });

  it("rejects the wrong method, a wrong contract, unknown and missing fields", () => {
    const args = { wallet: "x", intent: SWAP, nonce: 1, expires: 2, signature: "0x" };
    expect(() => parseIntentBody({ contractId: CONTRACT, method: "execute", args }, CONTRACT)).toThrow(/method must be "submit_intent"/);
    expect(() => parseIntentBody({ contractId: "other.testnet", method: "submit_intent", args }, CONTRACT)).toThrow(/contractId/);
    expect(() => parseIntentBody({ ...args, action: VOTE }, CONTRACT)).toThrow(/unknown field\(s\): action/);
    expect(() => parseIntentBody({ wallet: "x", intent: SWAP, nonce: 1, expires: 2 }, CONTRACT)).toThrow(/missing field: signature/);
    expect(() => parseCancelBody({ wallet: "x", nonce: 1, expires: 2, signature: "0x" }, CONTRACT)).toThrow(/missing field: id/);
    expect(() => parseCancelBody({ wallet: "x", id: -1, nonce: 1, expires: 2, signature: "0x" }, CONTRACT)).toThrow(/id must be/);
  });
});

describe("RelayService.submitIntent", () => {
  it("happy path (EVM): verifies locally, submits once with the intent as received, returns txHash and intentId", async () => {
    const { service, writer } = makeService();
    const body = await signedIntent(await evmSigner(), SWAP);
    const out = await service.submitIntent(body, "1.2.3.4");
    expect(out).toEqual({ txHash: "ITX1", intentId: 7 });
    expect(writer.intents).toEqual([body]);
    expect(writer.calls).toHaveLength(0);
  });

  it("happy path (Ed25519) for AddLiquidity", async () => {
    const { service, writer } = makeService();
    const body = await signedIntent(ed25519Signer(), ADD);
    await expect(service.submitIntent(body, "1.2.3.4")).resolves.toMatchObject({ txHash: "ITX1" });
    expect(writer.intents[0]?.intent).toEqual(ADD);
  });

  it("accepts the envelope form", async () => {
    const { service, writer } = makeService();
    const body = await signedIntent(await evmSigner(), SWAP);
    await service.submitIntent({ contractId: CONTRACT, method: "submit_intent", args: body }, "1.2.3.4");
    expect(writer.intents).toHaveLength(1);
  });

  it("rejects a signature over a different intent, or over an execute action, before any NEAR call", async () => {
    const { service, writer } = makeService();
    const signer = await evmSigner();
    const body = await signedIntent(signer, SWAP);
    const tampered = { ...body, intent: { Swap: { ...SWAP.Swap, min_amount_out: "1" } } };
    await expectStatus(service.submitIntent(tampered, "ip"), 400, /signature does not verify for this wallet and intent/);

    const execMsg = actionMessage(CONTRACT, signer.wallet, 101, body.expires, VOTE);
    const wrongDomain = { ...body, signature: await signer.sign(execMsg) };
    await expectStatus(service.submitIntent(wrongDomain, "ip"), 400, /signature does not verify/);
    expect(writer.intents).toHaveLength(0);
  });

  it("rejects an expired intent and an expiry too far ahead, before any NEAR call", async () => {
    const { service, writer } = makeService();
    const signer = await evmSigner();
    await expectStatus(service.submitIntent(await signedIntent(signer, SWAP, 1, T0_SEC - 1), "ip"), 400, /intent has expired/);
    await expectStatus(service.submitIntent(await signedIntent(signer, SWAP, 2, T0_SEC + 86_401), "ip"), 400, /at most 86400/);
    expect(writer.intents).toHaveLength(0);
  });

  it("rejects an invalid `to` before any NEAR call", async () => {
    const { service, writer } = makeService();
    const body = await signedIntent(await evmSigner(), { Swap: { ...SWAP.Swap, to: "evm:0xAB" } });
    await expectStatus(service.submitIntent(body, "ip"), 400, /to must be/);
    expect(writer.intents).toHaveLength(0);
  });

  it("is idempotent per (wallet, nonce): an identical retry returns the stored result without a second submission", async () => {
    const { service, writer } = makeService();
    const body = await signedIntent(await evmSigner(), SWAP, 55);
    const first = await service.submitIntent(body, "ip");
    const second = await service.submitIntent(body, "ip");
    expect(second).toEqual(first);
    expect(writer.intents).toHaveLength(1);
  });

  it("returns 409 when the same nonce is used for a different intent", async () => {
    const { service, writer } = makeService();
    const signer = await evmSigner();
    await service.submitIntent(await signedIntent(signer, SWAP, 55), "ip");
    await expectStatus(service.submitIntent(await signedIntent(signer, ADD, 55), "ip"), 409, /already used/);
    expect(writer.intents).toHaveLength(1);
  });

  it("shares nonces with execute: an action and an intent with the same nonce conflict", async () => {
    const { service, writer } = makeService();
    const signer = await evmSigner();
    const exec = await signedEnvelope(signer, VOTE, { nonce: 900, expires: T0_SEC + 600 });
    await service.execute(exec, "ip");
    await expectStatus(service.submitIntent(await signedIntent(signer, SWAP, 900), "ip"), 409);
    expect(writer.calls).toHaveLength(1);
    expect(writer.intents).toHaveLength(0);
  });

  it("maps a contract panic to 400 with the panic message", async () => {
    const { service } = makeService({ writer: mockExecuteWriter({ error: new Error("Smart contract panicked: intent amount exceeds cap") }) });
    await expectStatus(service.submitIntent(await signedIntent(await evmSigner(), SWAP), "ip"), 400, /^intent amount exceeds cap$/);
  });

  it("maps a NEAR RPC failure to 502", async () => {
    const { service } = makeService({ writer: mockExecuteWriter({ error: new Error("connect ECONNREFUSED") }) });
    await expectStatus(service.submitIntent(await signedIntent(await evmSigner(), SWAP), "ip"), 502, /Retrying with the same nonce is safe/);
  });

  it("charges the intent gas to the daily budget", async () => {
    const { service } = makeService({ config: { dailyGasBudgetTgas: 300 } });
    const signer = await evmSigner();
    await service.submitIntent(await signedIntent(signer, SWAP, 1), "ip");
    await expectStatus(service.submitIntent(await signedIntent(signer, SWAP, 2), "ip"), 503);
  });
});

describe("RelayService.cancelIntent", () => {
  it("submits cancel_intent with the args as received, signed over the cancel message", async () => {
    const { service, writer } = makeService();
    const body = await signedCancel(await evmSigner(), 12);
    const out = await service.cancelIntent(body, "ip");
    expect(out).toEqual({ txHash: "CTX1", result: null });
    expect(writer.cancels).toEqual([{ ...body }]);
    expect(writer.intents).toHaveLength(0);
  });

  it("rejects a cancel signed with a different intent id", async () => {
    const { service, writer } = makeService();
    const body = await signedCancel(await evmSigner(), 12);
    await expectStatus(service.cancelIntent({ ...body, id: 13 }, "ip"), 400, /signature does not verify for this wallet and cancel request/);
    expect(writer.cancels).toHaveLength(0);
  });

  it("is idempotent per (wallet, nonce) and shares nonces with submit_intent", async () => {
    const { service, writer } = makeService();
    const signer = await evmSigner();
    const body = await signedCancel(signer, 12, 300);
    await service.cancelIntent(body, "ip");
    await service.cancelIntent(body, "ip");
    expect(writer.cancels).toHaveLength(1);
    await expectStatus(service.submitIntent(await signedIntent(signer, SWAP, 300), "ip"), 409);
  });

  it("works for Ed25519 wallets", async () => {
    const { service } = makeService();
    await expect(service.cancelIntent(await signedCancel(ed25519Signer(), 1), "ip")).resolves.toMatchObject({ txHash: "CTX1" });
  });
});

describe("RelayService.getIntent (get_intent view proxy)", () => {
  const record = { id: 9, owner_hex: "cd".repeat(32), status: "Open" };

  it("returns the view record", async () => {
    const { service } = makeService({ reader: mockIntentReader([record]) });
    await expect(service.getIntent("9", "ip")).resolves.toEqual(record);
  });

  it("returns 404 when the contract has no such intent", async () => {
    const { service } = makeService({ reader: mockIntentReader([record]) });
    await expectStatus(service.getIntent("10", "ip"), 404, /not found/);
  });

  it("rejects a malformed id before calling NEAR", async () => {
    const reads: number[] = [];
    const reader: IntentReader = {
      async getIntent(id) {
        reads.push(id);
        return null;
      },
    };
    const { service } = makeService({ reader });
    for (const bad of ["abc", "-1", "1.5", "01", "9007199254740993"]) {
      await expectStatus(service.getIntent(bad, "ip"), 400, /non-negative integer/);
    }
    expect(reads).toHaveLength(0);
  });

  it("maps a view failure to 502", async () => {
    const reader: IntentReader = {
      async getIntent() {
        throw new Error("NEAR RPC HTTP 503 for get_intent");
      },
    };
    const { service } = makeService({ reader });
    await expectStatus(service.getIntent("9", "ip"), 502);
  });
});
