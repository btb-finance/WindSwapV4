import { describe, expect, it } from "vitest";
import { decodeReceipt } from "../src/decode/decoder.js";
import { parseEventLogs } from "../src/decode/events.js";
import type { Row } from "../src/decode/rows.js";
import { normalizeNeardataBlock } from "../src/near/normalize.js";
import {
  CONTRACT,
  RECEIVERS,
  blockJson,
  fixtureTxHashes,
  findReceipt,
  receiptsOfBlock,
  receiptsOfTx,
} from "./helpers.js";

const H = {
  deploy: "5o7RZTePgB38VCy6wfetwMeRUrF3wfjA59eNAZtVqRy9",
  createPool0: "AvcBuqWXcJwPXhstmXrtnzXVur9x7wAc3d3Tpvum7wGm",
  createPool4: "9VF8LMPq9Qms3zLv53DsHefZ1jcprCr1QEJtd5HAvVC9",
  swap: "3KqUbcbAxhxuGS87dUXbqpSZn7C5xcboTUJ56sjswQaL",
  swapExactInputRoute: "4PDEQ32ycG1jfgDzdcfMdE9MpfrJgQGtCTpGsVXvgPxC",
  executeSwapExactInput: "HHAHYDMdh5zopvTuyMLA3JFiGJvFqYVgsFx6Y1nABJoJ",
  executeMint: "GHjybdjqqunjyK27CffDYcqiBrJMH64GLJ6fB7WSrzx2",
  executeWithdrawEvm: "E76HJcsJgB3bn6eTNEeqRCPY244fab6mEcKg6fgdemZ4",
  executeFailed: "DKcrdafF4EFknBGYoGAJj7UWSj3ZZqL4A4JFoKYbwX2G",
  reportErc20: "FvuRMoYjcbGoCYAoX5pWep6g5zZ3mXPd7RyyUKYysfab",
  reportNative: "8EdUfBAQ9ccKECBgnRXyXWnFcCbkSWUW1uxU8ZePFmis",
  ftDeposit: "2D5dF21z5MS2fiSoz7AFSkPPpMoSEHGFqRyK7Dq2sGQv",
  withdrawEvmDirect: "DkXQMAhhz8arQdhqPzuq5GkJSGZ7nR3EKyU7d3N7WP3b",
  onWithdrawalSigned: "7vnyqXKW2DL6hyayy8GsMRiZSLuTzRBFALC3hVHnUzAa",
  markBroadcast: "BBZJG68PNX4j8n2uX76rfmbDEgqjbpWczqHrFQNkbUeg",
  createLock: "3KNKYKtJb9XJeogiu2a5mfhSLCn8Sc8GLKed7a8CzNc8",
  vote: "46XaNMzfpQbPmPHqewGrnPjtgMDChcuKaEr196jcHZoD",
  bribe: "JAvQy8Xw2d1cgDBZzDKtBTFF3XGQQUhcoqQRLkmp9Rj7",
  collect: "4THnUrLjHnZ2fNsvFUcgFdrHmNwFV92dmeS9JPoqSQcD",
  burn: "HgPr14w1VjNJ398EQK8GrrdyCxzDodNgaYHLqNqVNCbJ",
  addToken: "Ao63rMDj7EHbnqmABpQYqwSm4NPUVmcZbqgsMHXeg5Wm",
  registerAsset: "9Wft388kr6usUFJmBsV7DtP9ELHpQqevjRHwtx6PDE9X",
  createGauge: "72gEwKPPQuTV1P7BxvSX6WxjsYwi95fAMuZ9jYhVbPzE",
  executeStake: "CMgKwYJoSVWyLTPe4wLoNcyGDNyysrA2mGD8WgJAQzsZ",
};

/** Decodes every receipt of a fixture tx and returns all rows and calls. */
function decodeTx(hash: string) {
  const calls = [];
  const rows: Row[] = [];
  for (const r of receiptsOfTx(hash)) {
    const d = decodeReceipt(r, CONTRACT);
    calls.push(...d.calls);
    rows.push(...d.rows);
  }
  return { calls, rows };
}

const byKind = <K extends Row["kind"]>(rows: Row[], kind: K) => rows.filter((r): r is Extract<Row, { kind: K }> => r.kind === kind);

describe("pools", () => {
  it("decodes create_pool with its returned pool id (pool 0 and pool 4)", () => {
    const p0 = byKind(decodeTx(H.createPool0).rows, "pool");
    expect(p0).toHaveLength(1);
    expect(p0[0]).toMatchObject({ poolId: 0, token0: "tusdc", token1: "tweth", tickSpacing: 3 });

    const p4 = byKind(decodeTx(H.createPool4).rows, "pool");
    expect(p4).toMatchObject([{ poolId: 4, token0: "base-sepolia:eth", token1: "base-sepolia:test1", tickSpacing: 4 }]);
  });

  it("decodes add_token and register_evm_asset into tokens", () => {
    const t = byKind(decodeTx(H.addToken).rows, "token");
    expect(t).toMatchObject([{ tokenId: "windswapv4-tusdc.testnet", tokenKind: "nep141", listed: true }]);
    const e = byKind(decodeTx(H.registerAsset).rows, "token");
    expect(e).toMatchObject([{ tokenId: "base-sepolia:eth", tokenKind: "evm", chainId: 84532, asset: "native", decimals: 18 }]);
  });
});

describe("swaps", () => {
  it("decodes a direct single-pool swap (signed deltas, zero_for_one)", () => {
    const swaps = byKind(decodeTx(H.swap).rows, "swap");
    expect(swaps).toHaveLength(1);
    expect(swaps[0]).toMatchObject({
      exact: "input",
      via: "direct",
      zeroForOne: true,
      path: [{ poolId: 0, tokenIn: null }],
      amountIn: "1000000000000000000",
      amountOut: "989804920212634294",
      amount0: "1000000000000000000",
      amount1: "-989804920212634294",
    });
    expect(swaps[0]?.owner).toEqual({ account: CONTRACT });
  });

  it("decodes swap_exact_input over two pools as a route (no single pool id)", () => {
    const swaps = byKind(decodeTx(H.swapExactInputRoute).rows, "swap");
    expect(swaps).toMatchObject([
      {
        kind: "swap",
        exact: "input",
        via: "route",
        path: [
          { poolId: 1, tokenIn: "windswapv4-tusdc.testnet" },
          { poolId: 2, tokenIn: "windswapv4-tweth.testnet" },
        ],
        amountIn: "1000000000000000000",
        amountOut: "974290305953384360",
        amount0: null,
      },
    ]);
  });

  it("decodes execute(SwapExactInput) as a signed action for the wallet", () => {
    const d = decodeTx(H.executeSwapExactInput);
    const swaps = byKind(d.rows, "swap");
    expect(swaps).toMatchObject([
      {
        via: "signed",
        exact: "input",
        amountIn: "100000000000000",
        amountOut: "297725470343984462",
        path: [{ poolId: 4, tokenIn: "base-sepolia:eth" }],
        owner: { wallet: "evm:0x0d53794e20e0e4f96e07bcd8487ce557c6d9a4ac" },
      },
    ]);
    expect(d.calls[0]).toMatchObject({ method: "execute", decodedAs: "execute:SwapExactInput", ok: true });
  });

  it("decodes execute(Swap) from the fixture with the old single-pool form", () => {
    const hash = fixtureTxHashes().find((h) => h.startsWith("7vDCTQie"));
    expect(hash).toBeDefined();
    const swaps = byKind(decodeTx(hash as string).rows, "swap");
    expect(swaps[0]).toMatchObject({ via: "signed", zeroForOne: true, path: [{ poolId: 1, tokenIn: null }] });
  });

  it("a direct swap_exact_input is recorded as a call decoded as SwapExactInput", () => {
    const call = decodeTx(H.swapExactInputRoute).calls[0];
    expect(call).toMatchObject({ method: "swap_exact_input", decodedAs: "SwapExactInput", ok: true });
  });
});

describe("liquidity", () => {
  it("decodes execute(MintPosition) with the contract's returned amounts", () => {
    const liq = byKind(decodeTx(H.executeMint).rows, "liquidity");
    expect(liq).toMatchObject([
      {
        subkind: "mint",
        via: "signed",
        poolId: 4,
        tickLower: 76000,
        tickUpper: 84000,
        liquidity: "767178844087728835",
        amount0: "2500000000000000",
        amount1: "7732827575178231985",
      },
    ]);
  });

  it("decodes burn_position and collect", () => {
    const burn = byKind(decodeTx(H.burn).rows, "liquidity");
    expect(burn).toMatchObject([{ subkind: "burn", via: "direct", poolId: 0, liquidity: "100000000000000000000" }]);
    const collect = byKind(decodeTx(H.collect).rows, "liquidity");
    expect(collect).toMatchObject([{ subkind: "collect", liquidity: null, poolId: 0 }]);
  });
});

describe("deposits and withdrawals", () => {
  it("decodes an ft_on_transfer deposit credited to the sender", () => {
    const deposits = byKind(decodeTx(H.ftDeposit).rows, "deposit");
    expect(deposits).toMatchObject([
      { subkind: "ft", tokenId: "windswapv4-tusdc.testnet", amount: "100000000000000000000", owner: { account: "windswapv4-user.testnet" } },
    ]);
  });

  it("decodes report_evm_deposit for an ERC-20 and for the native asset", () => {
    expect(byKind(decodeTx(H.reportErc20).rows, "deposit")).toMatchObject([
      {
        subkind: "evm",
        chainId: 84532,
        asset: "0x2d330fee66912f3eb54086357c2eab46b998bd4f",
        amount: "9000000000000000000000",
        logIndex: 57,
        owner: { wallet: "evm:0x0d53794e20e0e4f96e07bcd8487ce557c6d9a4ac" },
      },
    ]);
    expect(byKind(decodeTx(H.reportNative).rows, "deposit")).toMatchObject([{ subkind: "evm", asset: "native", amount: "3000000000000000", logIndex: 0 }]);
  });

  it("decodes a signed WithdrawEvm and a direct withdraw_evm", () => {
    const signed = byKind(decodeTx(H.executeWithdrawEvm).rows, "withdrawal_request");
    expect(signed).toMatchObject([{ id: 1, tokenId: "base-sepolia:eth", amount: "4000000000000000", toChainId: 84532, via: "signed" }]);
    const direct = byKind(decodeTx(H.withdrawEvmDirect).rows, "withdrawal_request");
    expect(direct).toMatchObject([{ id: 0, tokenId: "base-sepolia:eth", amount: "10000000000000000", toChainId: null, via: "direct" }]);
  });

  it("follows a withdrawal through signing and broadcast", () => {
    const signed = byKind(decodeTx(H.onWithdrawalSigned).rows, "withdrawal_update");
    expect(signed).toMatchObject([{ id: 0, status: "signed", nonce: 1 }]);
    const bc = byKind(decodeTx(H.markBroadcast).rows, "withdrawal_update");
    expect(bc).toMatchObject([{ id: 0, status: "broadcast", evmTxHash: "0x0bdf6576a18e99a6c03f4f343e5301128fcfc35eb2bae06c0d71f6502ec10d2d" }]);
  });

  it("records a failed execute as a call with an error and no typed rows", () => {
    const d = decodeTx(H.executeFailed);
    expect(d.calls).toMatchObject([{ method: "execute", ok: false }]);
    expect(d.calls[0]?.error).toContain("ActionError");
    expect(d.rows.filter((r) => r.kind !== "call")).toEqual([]);
  });
});

describe("ve(3,3)", () => {
  it("decodes create_lock, vote, bribe, create_gauge and staking", () => {
    expect(byKind(decodeTx(H.createLock).rows, "lock_event")).toMatchObject([
      { subkind: "create", tokenId: 1, amount: "100000000000000000000000", duration: 126144000, permanent: false },
    ]);
    expect(byKind(decodeTx(H.vote).rows, "vote")).toMatchObject([
      { subkind: "vote", tokenId: 1, poolId: 4, weight: "70" },
      { subkind: "vote", tokenId: 1, poolId: 1, weight: "30" },
    ]);
    expect(byKind(decodeTx(H.bribe).rows, "bribe")).toMatchObject([
      { poolId: 1, token: "windswapv4-tusdc.testnet", amount: "5000000000000000000" },
    ]);
    expect(byKind(decodeTx(H.createGauge).rows, "gauge")).toMatchObject([{ subkind: "create", poolId: 1, gaugeId: 2 }]);
    expect(byKind(decodeTx(H.executeStake).rows, "gauge")).toMatchObject([{ subkind: "stake", poolId: 4, tickLower: 76000, tickUpper: 84000 }]);
  });

  it("records the deploy (new) as a call with no typed rows", () => {
    const d = decodeTx(H.deploy);
    expect(d.calls).toMatchObject([{ method: "new", ok: true, decodedAs: null }]);
    expect(d.rows).toEqual([]);
  });
});

describe("data sources agree", () => {
  it("neardata blocks and FastNEAR trees give the same receipts and decoded rows", () => {
    // Every fixture tree's contract receipts must appear, identically, in the neardata block at their height.
    const fromTrees = new Map<string, ReturnType<typeof receiptsOfTx>[number]>();
    for (const hash of fixtureTxHashes()) {
      for (const r of receiptsOfTx(hash)) fromTrees.set(r.receiptId, r);
    }
    const heights = [...new Set([...fromTrees.values()].map((r) => r.height))];
    let compared = 0;
    for (const h of heights) {
      const block = normalizeNeardataBlock(blockJson(h), RECEIVERS);
      for (const r of block.receipts) {
        const tree = fromTrees.get(r.receiptId);
        if (!tree) continue;
        expect(r).toEqual(tree);
        expect(decodeReceipt(r, CONTRACT)).toEqual(decodeReceipt(tree, CONTRACT));
        compared++;
      }
    }
    expect(compared).toBeGreaterThan(40);
  });

  it("receipts of a block carry tx hashes from neardata", () => {
    const receipts = receiptsOfBlock(274376494);
    expect(receipts.map((r) => r.txHash)).toContain("HHAHYDMdh5zopvTuyMLA3JFiGJvFqYVgsFx6Y1nABJoJ");
  });
});

describe("receipt shapes", () => {
  it("normalises unit-variant actions (CreateAccount is a bare string) without error", () => {
    const receipts = receiptsOfTx("8M2jFznPhkANiEE5nReJhyRhYhhy1r45P6Qa1r68E6HT");
    expect(receipts.length).toBeGreaterThan(0);
    const created = receipts.find((r) => r.actions.some((a) => a.kind === "Other" && a.name === "CreateAccount"));
    expect(created).toBeDefined();
    // A receipt with no function calls decodes to nothing.
    expect(decodeReceipt(created as NonNullable<typeof created>, CONTRACT)).toEqual({ calls: [], rows: [] });
  });
});

describe("events (NEP-297 ready)", () => {
  it("parses EVENT_JSON lines and ignores everything else", () => {
    const logs = [
      'EVENT_JSON:{"standard":"nep141","version":"1.0.0","event":"ft_transfer","data":[{"amount":"1"}]}',
      "evm deposit: plain log",
      "EVENT_JSON:not json",
    ];
    const parsed = parseEventLogs(logs);
    expect(parsed).toEqual([
      { logIndex: 0, standard: "nep141", version: "1.0.0", event: "ft_transfer", data: [{ amount: "1" }] },
    ]);
  });
});
