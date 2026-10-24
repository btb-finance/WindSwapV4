/**
 * Handlers for paths the chain has not exercised yet (fee and bribe claims, rebase, the epoch flip, wallet
 * links, WIND deposits with a lock message). Each receipt is built by hand in the shape neardata uses.
 */
import { describe, expect, it } from "vitest";
import { decodeReceipt } from "../src/decode/decoder.js";
import type { Row } from "../src/decode/rows.js";
import type { ReceiptExec } from "../src/near/types.js";
import { CONTRACT } from "./helpers.js";

const b64 = (s: string) => Buffer.from(s, "utf8").toString("base64");

function receipt(opts: {
  method: string;
  args: unknown;
  predecessor?: string;
  ret?: unknown;
  ok?: boolean;
  logs?: string[];
  signer?: string;
}): ReceiptExec {
  const ok = opts.ok !== false;
  return {
    receiptId: `synthetic-${opts.method}`,
    txHash: "0x",
    height: 1000,
    timestampMs: 1_700_000_000_000,
    predecessorId: opts.predecessor ?? "alice.testnet",
    receiverId: CONTRACT,
    signerId: opts.signer ?? opts.predecessor ?? "alice.testnet",
    actions: [{ kind: "FunctionCall", methodName: opts.method, argsText: JSON.stringify(opts.args), deposit: "0" }],
    status: ok
      ? { ok: true, value: opts.ret === undefined ? "" : b64(JSON.stringify(opts.ret)) === "" ? null : Buffer.from(b64(JSON.stringify(opts.ret)), "base64").toString("utf8"), chained: false }
      : { ok: false, error: '{"ActionError":"x"}' },
    logs: opts.logs ?? [],
  } as unknown as ReceiptExec;
}

const rowsOf = (r: ReceiptExec) => decodeReceipt(r, CONTRACT);
const of = <K extends Row["kind"]>(rows: Row[], kind: K) => rows.filter((r): r is Extract<Row, { kind: K }> => r.kind === kind);

describe("claims, rebase and the epoch flip", () => {
  it("claim_fees and claim_bribes return [token, amount] pairs per token", () => {
    const fees = rowsOf(receipt({ method: "claim_fees", args: { token_id: 1, gauges: [1] }, ret: [["tusdc", "5"]] }));
    expect(of(fees.rows, "claim")).toMatchObject([{ subkind: "fees", tokenId: 1, token: "tusdc", amount: "5", extra: { gauges: [1] } }]);
    const bribes = rowsOf(receipt({ method: "claim_bribes", args: { token_id: 1, gauges: [1], tokens: ["tusdc"] }, ret: [["tusdc", "7"], ["tweth", "9"]] }));
    expect(of(bribes.rows, "claim").map((c) => [c.subkind, c.token, c.amount])).toEqual([
      ["bribes", "tusdc", "7"],
      ["bribes", "tweth", "9"],
    ]);
  });

  it("claim_rebase and claim_gauge_rewards return one amount", () => {
    expect(of(rowsOf(receipt({ method: "claim_rebase", args: { token_id: 2 }, ret: "42" })).rows, "claim")).toMatchObject([
      { subkind: "rebase", tokenId: 2, amount: "42" },
    ]);
    expect(
      of(rowsOf(receipt({ method: "claim_gauge_rewards", args: { pool_id: 4, tick_lower: 1, tick_upper: 2 }, ret: "3" })).rows, "claim"),
    ).toMatchObject([{ subkind: "gauge_rewards", poolId: 4, amount: "3", extra: { tick_lower: 1, tick_upper: 2 } }]);
  });

  it("distribute with a value is an epoch; with a promise (no value) it is only a call", () => {
    expect(of(rowsOf(receipt({ method: "distribute", args: {}, ret: "1000" })).rows, "epoch")).toMatchObject([{ amount: "1000" }]);
    const chained = { ...receipt({ method: "distribute", args: {} }), status: { ok: true, value: null, chained: true } } as ReceiptExec;
    expect(of(rowsOf(chained).rows, "epoch")).toEqual([]);
  });

  it("on_distribute_minted records the epoch amount it returns", () => {
    const cb = receipt({ method: "on_distribute_minted", args: { amount: "1000" }, predecessor: CONTRACT, ret: "1000" });
    expect(of(rowsOf(cb).rows, "epoch")).toMatchObject([{ amount: "1000" }]);
  });

  it("execute(Distribute) (a unit variant, sent as a string) returns the minted amount", () => {
    const d = rowsOf(
      receipt({
        method: "execute",
        args: { wallet: "evm:0x70997970c51812dc3a010c7d01b50e0d17dc79c8", action: "Distribute", nonce: 7, expires: 1, signature: "0x" },
        ret: { Amount: "55" },
        predecessor: "relayer.testnet",
      }),
    );
    expect(d.calls[0]).toMatchObject({ method: "execute", decodedAs: "execute:Distribute", ok: true });
    expect(of(d.rows, "epoch")).toMatchObject([{ amount: "55" }]);
  });
});

describe("ve(3,3) lock lifecycle", () => {
  it("merge, split, permanence and delegation carry their ids", () => {
    expect(of(rowsOf(receipt({ method: "merge", args: { from: 3, to: 4 } })).rows, "lock_event")).toMatchObject([
      { subkind: "merge", tokenId: 3, extra: { into: 4 } },
    ]);
    expect(of(rowsOf(receipt({ method: "split", args: { token_id: 4, amount: "10" }, ret: ["5", "6"] })).rows, "lock_event")).toMatchObject([
      { subkind: "split", tokenId: 4, amount: "10", extra: { new_token_ids: ["5", "6"] } },
    ]);
    expect(of(rowsOf(receipt({ method: "lock_permanent", args: { token_id: 4 } })).rows, "lock_event")).toMatchObject([
      { subkind: "permanent", permanent: true },
    ]);
    expect(of(rowsOf(receipt({ method: "delegate", args: { delegator: 1, delegatee: 2 } })).rows, "lock_event")).toMatchObject([
      { subkind: "delegate", tokenId: 1, extra: { delegatee: 2 } },
    ]);
  });

  it("withdraw_lock returns the amount; reset and poke are votes without a pool", () => {
    expect(of(rowsOf(receipt({ method: "withdraw_lock", args: { token_id: 4 }, ret: "99" })).rows, "lock_event")).toMatchObject([
      { subkind: "withdraw", tokenId: 4, amount: "99" },
    ]);
    expect(of(rowsOf(receipt({ method: "reset", args: { token_id: 4 } })).rows, "vote")).toMatchObject([{ subkind: "reset", poolId: null }]);
    expect(of(rowsOf(receipt({ method: "poke", args: { token_id: 4 } })).rows, "vote")).toMatchObject([{ subkind: "poke" }]);
  });

  it("a WIND deposit with a lock message is a deposit with lockDuration; non-WIND deposits are only credits", () => {
    const d = of(
      rowsOf(receipt({ method: "ft_on_transfer", args: { sender_id: "bob.testnet", amount: "100", msg: '{"lock":{"duration":126144000}}' }, predecessor: "windswapv4-wind.testnet", ret: "0" })).rows,
      "deposit",
    );
    expect(d).toMatchObject([{ subkind: "ft", tokenId: "windswapv4-wind.testnet", lockDuration: 126144000, owner: { account: "bob.testnet" } }]);
  });

  it("an ft deposit whose msg names a wallet credits that wallet", () => {
    const d = of(
      rowsOf(receipt({ method: "ft_on_transfer", args: { sender_id: "bob.testnet", amount: "5", msg: '{"owner":"evm:0x70997970C51812dc3a010c7d01b50e0d17dc79c8"}' }, predecessor: "windswapv4-tusdc.testnet", ret: "0" })).rows,
      "deposit",
    );
    expect(d).toMatchObject([{ owner: { wallet: "evm:0x70997970c51812dc3a010c7d01b50e0d17dc79c8" }, lockDuration: null }]);
  });

  it("a refunded ft deposit (returns its amount) credits nothing", () => {
    const r = rowsOf(receipt({ method: "ft_on_transfer", args: { sender_id: "bob.testnet", amount: "5", msg: "" }, predecessor: "unlisted.testnet", ret: "5" }));
    expect(r.rows).toEqual([]);
    expect(r.calls[0]).toMatchObject({ method: "ft_on_transfer", ok: true });
  });
});

describe("wallets and withdrawals", () => {
  it("link_wallet links to the calling account; unlink removes the link", () => {
    const link = of(rowsOf(receipt({ method: "link_wallet", args: { wallet: "evm:0x70997970C51812dc3a010c7d01b50e0d17dc79c8", nonce: 1, expires: 9, signature: "0x" }, predecessor: "carol.testnet" })).rows, "wallet");
    expect(link).toMatchObject([{ wallet: "evm:0x70997970c51812dc3a010c7d01b50e0d17dc79c8", account: "carol.testnet" }]);
    const unlink = of(rowsOf(receipt({ method: "unlink_wallet", args: { wallet: "evm:0x70997970c51812dc3a010c7d01b50e0d17dc79c8" }, predecessor: "carol.testnet" })).rows, "wallet");
    expect(unlink).toMatchObject([{ account: null }]);
  });

  it("a failed call is recorded with its error and no rows", () => {
    const d = rowsOf(receipt({ method: "swap", args: { pool_id: 1, zero_for_one: true, amount_specified: "0", sqrt_price_limit_x96: "1" }, ok: false }));
    expect(d.calls).toMatchObject([{ method: "swap", ok: false, decodedAs: "Swap", error: '{"ActionError":"x"}' }]);
    expect(d.rows).toEqual([]);
  });

  it("an EVENT_JSON log becomes a chain_events row, and a malformed one is ignored", () => {
    const d = rowsOf(
      receipt({
        method: "report_evm_deposit",
        args: { chain_id: 84532, tx_hash: `0x${"ab".repeat(32)}`, log_index: 0, from: "0x" + "11".repeat(20), asset: "native", amount: "1" },
        ret: true,
        logs: ['EVENT_JSON:{"standard":"windswap","version":"1","event":"deposit","data":{"x":1}}', "EVENT_JSON:{bad"],
      }),
    );
    expect(of(d.rows, "event")).toMatchObject([{ standard: "windswap", event: "deposit", data: { x: 1 } }]);
  });
});
