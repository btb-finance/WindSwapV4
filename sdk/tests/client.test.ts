import { describe, expect, it } from "vitest";
import { WindSwapClient, WindSwapError } from "../src/index.js";

/** Fake JSON-RPC: records each request and answers with `result` encoded as contract bytes. */
function fakeFetch(answer: (method: string, args: unknown) => unknown) {
  const calls: { url: string; body: any }[] = [];
  const fetchImpl = (async (url: string | URL | Request, init?: RequestInit) => {
    const body = JSON.parse(String(init?.body));
    calls.push({ url: String(url), body });
    const args = JSON.parse(Buffer.from(body.params.args_base64, "base64").toString("utf8"));
    const value = answer(body.params.method_name, args);
    const bytes = Array.from(new TextEncoder().encode(JSON.stringify(value)));
    return new Response(JSON.stringify({ jsonrpc: "2.0", id: "windswap-sdk", result: { result: bytes, logs: [] } }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }) as typeof fetch;
  return { fetchImpl, calls };
}

describe("WindSwapClient views (mocked RPC)", () => {
  it("sends call_function with base64 JSON args to the configured RPC and contract", async () => {
    const { fetchImpl, calls } = fakeFetch((method) => (method === "get_fee" ? 300 : null));
    const client = new WindSwapClient({ rpcUrl: "https://rpc.example", contractId: "dex.testnet", fetch: fetchImpl });
    expect(await client.getFee(1, "alice.testnet")).toBe(300);
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe("https://rpc.example");
    expect(calls[0]!.body.method).toBe("query");
    expect(calls[0]!.body.params).toMatchObject({
      request_type: "call_function",
      finality: "final",
      account_id: "dex.testnet",
      method_name: "get_fee",
    });
  });

  it("decodes results and forwards arguments with contract names", async () => {
    const pool = { pool_id: 1, token0: "a.testnet", token1: "b.testnet", initialized: true, tick_spacing: 3 };
    const { fetchImpl, calls } = fakeFetch((method, args) => {
      if (method === "get_pool") return pool;
      if (method === "quote_exact_input") return "974290305953384360";
      if (method === "get_tokens") return ["a.testnet", "b.testnet"];
      return args;
    });
    const client = new WindSwapClient({ fetch: fetchImpl });
    expect(await client.getPool(1)).toEqual(pool);
    expect(await client.getTokens()).toEqual(["a.testnet", "b.testnet"]);
    const quote = await client.quoteExactInput([{ pool_id: 1, token_in: "a.testnet" }], 1_000_000_000_000_000_000n);
    expect(quote).toBe("974290305953384360");
    const sent = JSON.parse(Buffer.from(calls[2]!.body.params.args_base64, "base64").toString("utf8"));
    expect(sent).toEqual({ path: [{ pool_id: 1, token_in: "a.testnet" }], amount_in: "1000000000000000000" });
  });

  it("maps getPosition and getBalance arguments", async () => {
    const { fetchImpl, calls } = fakeFetch(() => "0");
    const client = new WindSwapClient({ fetch: fetchImpl });
    await client.getBalance("alice.testnet", "a.testnet");
    await client.getPosition(0, "alice.testnet", -300, 300);
    const args0 = JSON.parse(Buffer.from(calls[0]!.body.params.args_base64, "base64").toString("utf8"));
    const args1 = JSON.parse(Buffer.from(calls[1]!.body.params.args_base64, "base64").toString("utf8"));
    expect(args0).toEqual({ account_id: "alice.testnet", token: "a.testnet" });
    expect(args1).toEqual({ pool_id: 0, account_id: "alice.testnet", tick_lower: -300, tick_upper: 300 });
  });

  it("throws WindSwapError when the RPC returns an error", async () => {
    const fetchImpl = (async () =>
      new Response(JSON.stringify({ jsonrpc: "2.0", id: "x", error: { message: "boom" } }), { status: 200 })) as typeof fetch;
    const client = new WindSwapClient({ fetch: fetchImpl });
    await expect(client.getPool(9)).rejects.toBeInstanceOf(WindSwapError);
  });

  it("throws WindSwapError when the contract panics", async () => {
    const fetchImpl = (async () =>
      new Response(
        JSON.stringify({ jsonrpc: "2.0", id: "x", result: { error: "Smart contract panicked: pool 9 does not exist" } }),
        { status: 200 },
      )) as typeof fetch;
    const client = new WindSwapClient({ fetch: fetchImpl });
    await expect(client.getPool(9)).rejects.toThrow(/pool 9 does not exist/);
  });

  it("exposes change-method builders bound to the contract id", () => {
    const client = new WindSwapClient({ contractId: "dex.testnet" });
    expect(client.actions.withdraw({ token: "t.testnet", amount: 1n }).receiverId).toBe("dex.testnet");
  });
});
