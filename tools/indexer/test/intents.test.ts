/** Intents (submit / execute / cancel) and one-step transfers, v8/v9 contract. Built by hand in neardata shape. */
import { describe, expect, it } from "vitest";
import { decodeReceipt } from "../src/decode/decoder.js";
import type { Row } from "../src/decode/rows.js";
import type { ReceiptExec } from "../src/near/types.js";
import { CONTRACT } from "./helpers.js";

function receipt(method: string, args: unknown, opts: { predecessor?: string; ret?: unknown; ok?: boolean } = {}): ReceiptExec {
  const retText = opts.ret === undefined ? null : JSON.stringify(opts.ret);
  return {
    receiptId: `synthetic-${method}-${Math.random().toString(36).slice(2)}`,
    txHash: "0x",
    height: 1000,
    timestampMs: 1_700_000_000_000,
    predecessorId: opts.predecessor ?? "alice.testnet",
    receiverId: CONTRACT,
    signerId: opts.predecessor ?? "alice.testnet",
    actions: [{ kind: "FunctionCall", methodName: method, argsText: JSON.stringify(args), deposit: "0" }],
    status: opts.ok === false
      ? { ok: false, error: '{"ActionError":"x"}' }
      : { ok: true, value: retText === null ? null : Buffer.from(retText, "utf8").toString("base64") === "" ? null : retText, chained: false },
    logs: [],
  } as unknown as ReceiptExec;
}

const of = <K extends Row["kind"]>(rows: Row[], kind: K) => rows.filter((r): r is Extract<Row, { kind: K }> => r.kind === kind);

describe("intents", () => {
  it("submit_intent records the intent under its returned id", () => {
    const d = decodeReceipt(
      receipt(
        "submit_intent",
        {
          wallet: "evm:0x70997970C51812dc3a010c7d01b50e0d17dc79c8",
          nonce: 300,
          expires: 1792772332,
          signature: "0x",
          intent: { Swap: { token_in: "base-sepolia:eth", amount_in: "1000", path: [{ pool_id: 4, token_in: "base-sepolia:eth" }], min_amount_out: "1", to: "internal" } },
        },
        { ret: 7, predecessor: "relayer.testnet" },
      ),
      CONTRACT,
    );
    expect(d.calls[0]).toMatchObject({ method: "submit_intent", decodedAs: "submit_intent", ok: true });
    expect(of(d.rows, "intent")).toMatchObject([
      { subkind: "submit", id: 7, intentKind: "swap", tokenIn: "base-sepolia:eth", amountIn: "1000", status: "open", nonce: 300 },
    ]);
  });

  it("execute_intent stores the status and result of the returned view", () => {
    const d = decodeReceipt(
      receipt("execute_intent", { id: 7 }, { ret: { id: 7, status: "Executed", result: { Swap: { amount_out: "5", to: "internal" } }, reason: null }, predecessor: "anyone.testnet" }),
      CONTRACT,
    );
    expect(of(d.rows, "intent")).toMatchObject([{ subkind: "execute", id: 7, status: "Executed", reason: null }]);
  });

  it("cancel_intent and cancel_intent_near mark the intent failed", () => {
    const a = decodeReceipt(receipt("cancel_intent", { wallet: "evm:0x70997970c51812dc3a010c7d01b50e0d17dc79c8", id: 7, nonce: 1, expires: 9, signature: "0x" }), CONTRACT);
    expect(of(a.rows, "intent")).toMatchObject([{ subkind: "cancel", id: 7, status: "failed", reason: "cancelled by owner" }]);
    const b = decodeReceipt(receipt("cancel_intent_near", { id: 8 }), CONTRACT);
    expect(of(b.rows, "intent")).toMatchObject([{ id: 8, status: "failed" }]);
  });
});

describe("one-step transfers", () => {
  it("a successful swap message is a one_step row, not a deposit", () => {
    const msg = JSON.stringify({ swap: { path: [{ pool_id: 1, token_in: "windswapv4-tusdc.testnet" }], min_amount_out: "1" } });
    const d = decodeReceipt(receipt("ft_on_transfer", { sender_id: "bob.testnet", amount: "5", msg }, { predecessor: "windswapv4-tusdc.testnet", ret: "0" }), CONTRACT);
    expect(of(d.rows, "one_step")).toMatchObject([{ subkind: "swap", tokenId: "windswapv4-tusdc.testnet", amount: "5", owner: { account: "bob.testnet" } }]);
    expect(of(d.rows, "deposit")).toEqual([]);
  });

  it("a refunded one-step message (returns its amount) records nothing", () => {
    const msg = JSON.stringify({ add_liquidity: { pool_id: 1, tick_lower: -60, tick_upper: 60, zap: false } });
    const d = decodeReceipt(receipt("ft_on_transfer", { sender_id: "bob.testnet", amount: "5", msg }, { predecessor: "windswapv4-tusdc.testnet", ret: "5" }), CONTRACT);
    expect(d.rows).toEqual([]);
  });

  it("report_evm_deposit with decimals and symbol emits token metadata", () => {
    const d = decodeReceipt(
      receipt("report_evm_deposit", { chain_id: 84532, tx_hash: `0x${"cd".repeat(32)}`, log_index: 1, from: `0x${"22".repeat(20)}`, asset: `0x${"33".repeat(20)}`, amount: "1", decimals: 6, symbol: "USDC" }, { ret: true }),
      CONTRACT,
    );
    expect(of(d.rows, "token_meta")).toMatchObject([{ chainId: 84532, decimals: 6, symbol: "USDC" }]);
  });
});
