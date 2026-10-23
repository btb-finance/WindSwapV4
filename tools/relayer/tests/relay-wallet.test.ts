import { generateKeyPairSync, sign as edSign } from "node:crypto";
import { privateKeyToAccount, generatePrivateKey } from "viem/accounts";
import { describe, expect, it } from "vitest";
import { RelayError } from "../src/relay/errors.js";
import { base58Decode, base58Encode, parseWallet, verifyWalletSignature } from "../src/relay/wallet.js";

const MSG = "Wind Swap v4 action on windswapv4-btb.testnet by evm:0x0 nonce 1 expires 2: {}";

describe("base58", () => {
  it("round-trips bytes including leading zeros", () => {
    const bytes = Buffer.from("0000ff10203040", "hex");
    expect(base58Decode(base58Encode(bytes))?.equals(bytes)).toBe(true);
    expect(base58Encode(Buffer.alloc(32))).toBe("1".repeat(32));
  });

  it("rejects characters outside the alphabet", () => {
    expect(base58Decode("0OIl")).toBeNull();
  });
});

describe("parseWallet", () => {
  it("accepts canonical EVM and Ed25519 ids", async () => {
    const acct = privateKeyToAccount(generatePrivateKey());
    const evm = parseWallet(`evm:${acct.address.toLowerCase()}`);
    expect(evm.kind).toBe("evm");
    const { publicKey } = generateKeyPairSync("ed25519");
    const raw = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
    expect(parseWallet(`ed25519:${base58Encode(raw)}`).kind).toBe("ed25519");
  });

  it.each([
    ["mixed-case EVM address", `evm:0x${"AB".repeat(20)}`],
    ["EVM address without 0x", `evm:${"ab".repeat(20)}`],
    ["short EVM address", "evm:0xabcd"],
    ["non-canonical base58 key", "ed25519:" + "2".repeat(43)],
    ["unknown prefix", "near:alice.testnet"],
    ["not a string", 42],
  ])("rejects %s", (_name, value) => {
    expect(() => parseWallet(value)).toThrow(RelayError);
  });
});

describe("verifyWalletSignature", () => {
  it("verifies an EIP-191 personal_sign signature and rejects other messages", async () => {
    const acct = privateKeyToAccount(generatePrivateKey());
    const wallet = parseWallet(`evm:${acct.address.toLowerCase()}`);
    const sig = await acct.signMessage({ message: MSG });
    expect(await verifyWalletSignature(wallet, MSG, sig)).toBe(true);
    expect(await verifyWalletSignature(wallet, MSG + " ", sig)).toBe(false);
  });

  it("accepts v of 0/1 and rejects v outside that set", async () => {
    const acct = privateKeyToAccount(generatePrivateKey());
    const wallet = parseWallet(`evm:${acct.address.toLowerCase()}`);
    const sig = await acct.signMessage({ message: MSG });
    const v = parseInt(sig.slice(-2), 16);
    const low = sig.slice(0, -2) + (v - 27).toString(16).padStart(2, "0");
    expect(await verifyWalletSignature(wallet, MSG, low)).toBe(true);
    const bad = sig.slice(0, -2) + "05";
    expect(await verifyWalletSignature(wallet, MSG, bad)).toBe(false);
  });

  it("rejects high-s signatures, as the contract does", async () => {
    const acct = privateKeyToAccount(generatePrivateKey());
    const wallet = parseWallet(`evm:${acct.address.toLowerCase()}`);
    const sig = await acct.signMessage({ message: MSG });
    const n = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n;
    const s = BigInt(`0x${sig.slice(66, 130)}`);
    const highS = (n - s).toString(16).padStart(64, "0");
    const flipped = sig.slice(0, 66) + highS + sig.slice(130);
    const v = parseInt(sig.slice(130), 16);
    // Flipping s flips the recovery bit too; try both so the only reason to reject is high-s.
    const withV = flipped.slice(0, 130) + (v === 27 ? "1c" : "1b");
    expect(await verifyWalletSignature(wallet, MSG, withV)).toBe(false);
  });

  it("throws a 400 for a malformed EVM signature", async () => {
    const acct = privateKeyToAccount(generatePrivateKey());
    const wallet = parseWallet(`evm:${acct.address.toLowerCase()}`);
    await expect(verifyWalletSignature(wallet, MSG, "0x1234")).rejects.toMatchObject({ status: 400 });
  });

  it("verifies Ed25519 over the raw message bytes, in hex or base58", async () => {
    const { publicKey, privateKey } = generateKeyPairSync("ed25519");
    const raw = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
    const wallet = parseWallet(`ed25519:${base58Encode(raw)}`);
    const sig = edSign(null, Buffer.from(MSG, "utf8"), privateKey);
    expect(await verifyWalletSignature(wallet, MSG, sig.toString("hex"))).toBe(true);
    expect(await verifyWalletSignature(wallet, MSG, base58Encode(sig))).toBe(true);
    expect(await verifyWalletSignature(wallet, MSG + "x", sig.toString("hex"))).toBe(false);
  });

  it("throws a 400 for an Ed25519 signature of the wrong length", async () => {
    const { publicKey } = generateKeyPairSync("ed25519");
    const raw = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
    const wallet = parseWallet(`ed25519:${base58Encode(raw)}`);
    await expect(verifyWalletSignature(wallet, MSG, "abcd")).rejects.toMatchObject({ status: 400 });
  });
});
