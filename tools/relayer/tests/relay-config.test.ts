import { describe, expect, it } from "vitest";
import { parseRelay } from "../src/config.js";

describe("parseRelay", () => {
  it("applies documented defaults when the section is absent", () => {
    const r = parseRelay(undefined, {});
    expect(r).toMatchObject({
      host: "127.0.0.1",
      port: 8787,
      corsOrigins: [],
      trustProxy: false,
      maxBodyBytes: 16_384,
      maxExpirySeconds: 86_400,
      allowedActions: "*",
      dailyGasBudgetTgas: 30_000,
      idempotencyTtlSeconds: 86_400,
    });
    expect(r.rateLimit.perIp).toEqual({ capacity: 20, refillPerSecond: 0.5 });
    expect(r.rateLimit.perWallet).toEqual({ capacity: 5, refillPerSecond: 0.05 });
  });

  it("reads env overrides for host, port and CORS origins", () => {
    const r = parseRelay({ corsOrigins: ["https://a.example"] }, {
      RELAYER_HTTP_HOST: "0.0.0.0",
      RELAYER_HTTP_PORT: "9000",
      RELAYER_CORS_ORIGINS: "https://x.example, https://y.example",
    });
    expect(r.host).toBe("0.0.0.0");
    expect(r.port).toBe(9000);
    expect(r.corsOrigins).toEqual(["https://x.example", "https://y.example"]);
  });

  it("accepts an explicit allow-list of actions", () => {
    expect(parseRelay({ allowedActions: ["Vote", "Poke"] }, {}).allowedActions).toEqual(["Vote", "Poke"]);
  });

  it("rejects invalid values", () => {
    expect(() => parseRelay({ port: 70000 }, {})).toThrow(/relay.port/);
    expect(() => parseRelay({ rateLimit: { perIp: { capacity: 0, refillPerSecond: 1 } } }, {})).toThrow(/capacity/);
    expect(() => parseRelay({ allowedActions: "Vote" }, {})).toThrow(/allowedActions/);
    expect(() => parseRelay({ dailyGasBudgetTgas: 0 }, {})).toThrow(/dailyGasBudgetTgas/);
  });
});
