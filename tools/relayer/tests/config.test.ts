import { describe, expect, it } from "vitest";
import { parseConfig } from "../src/config.js";

const base = {
  near: { rpcUrl: "https://rpc.testnet.near.org", contractId: "windswapv4-btb.testnet", relayerAccountId: "r.testnet", credentialsPath: "/tmp/x.json" },
  evm: [
    {
      name: "base-sepolia",
      chainId: 84532,
      rpcUrl: "https://sepolia.base.org",
      vault: "0xaa817ca355475d22d5a410f401dc877ec401259e",
      confirmations: 3,
      startBlock: 49070000,
      scanBatchSize: 10,
    },
  ],
};

describe("parseConfig", () => {
  it("applies defaults", () => {
    const cfg = parseConfig(base, {});
    expect(cfg.evm[0]?.requestsPerSecond).toBe(5);
    expect(cfg.near.gasTgas).toEqual({ report: 50, process: 200, mark: 30, execute: 300, submitIntent: 300, cancelIntent: 300 });
    expect(cfg.stateDir).toBe("state");
  });

  it("lets env override NEAR settings", () => {
    const cfg = parseConfig(base, { RELAYER_CONTRACT_ID: "other.testnet", RELAYER_ACCOUNT_ID: "bot.testnet" });
    expect(cfg.near.contractId).toBe("other.testnet");
    expect(cfg.near.relayerAccountId).toBe("bot.testnet");
  });

  it("rejects a malformed vault address", () => {
    const bad = { ...base, evm: [{ ...base.evm[0], vault: "0x1234" }] };
    expect(() => parseConfig(bad, {})).toThrow(/vault/);
  });

  it("rejects duplicate chain names", () => {
    const dup = { ...base, evm: [base.evm[0], base.evm[0]] };
    expect(() => parseConfig(dup, {})).toThrow(/unique/);
  });

  it("rejects negative confirmations", () => {
    const bad = { ...base, evm: [{ ...base.evm[0], confirmations: -1 }] };
    expect(() => parseConfig(bad, {})).toThrow(/confirmations/);
  });
});
