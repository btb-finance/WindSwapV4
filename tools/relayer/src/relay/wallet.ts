import { createPublicKey, verify as edVerify } from "node:crypto";
import { recoverMessageAddress, type Hex } from "viem";
import { bad } from "./errors.js";

/** A wallet id in the canonical form the contract prints (`WalletId` Display). */
export type ParsedWallet =
  | { kind: "evm"; canonical: string; address: Hex }
  | { kind: "ed25519"; canonical: string; publicKey: Buffer };

const EVM_WALLET = /^evm:0x[0-9a-f]{40}$/;
const ED_PREFIX = "ed25519:";
const BASE58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
/** secp256k1 curve order / 2. The contract rejects high-s signatures. */
const SECP256K1_HALF_N = 0x7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0n;
/** DER prefix of an Ed25519 SubjectPublicKeyInfo, followed by the 32-byte key. */
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

export function base58Decode(s: string): Buffer | null {
  let num = 0n;
  for (const ch of s) {
    const i = BASE58.indexOf(ch);
    if (i < 0) return null;
    num = num * 58n + BigInt(i);
  }
  let hex = num === 0n ? "" : num.toString(16);
  if (hex.length % 2 === 1) hex = `0${hex}`;
  let leading = 0;
  for (const ch of s) {
    if (ch !== "1") break;
    leading++;
  }
  return Buffer.concat([Buffer.alloc(leading), Buffer.from(hex, "hex")]);
}

export function base58Encode(bytes: Uint8Array): string {
  let zeros = 0;
  while (zeros < bytes.length && bytes[zeros] === 0) zeros++;
  let num = 0n;
  for (const b of bytes) num = num * 256n + BigInt(b);
  let out = "";
  while (num > 0n) {
    out = BASE58[Number(num % 58n)] + out;
    num /= 58n;
  }
  return "1".repeat(zeros) + out;
}

/** Parses a wallet and requires the canonical spelling, since the contract signs its canonical form. */
export function parseWallet(raw: unknown): ParsedWallet {
  const fail = () => bad("wallet must be evm:0x<40 lowercase hex> or ed25519:<base58 32-byte public key>");
  if (typeof raw !== "string") throw fail();
  if (EVM_WALLET.test(raw)) return { kind: "evm", canonical: raw, address: raw.slice(4) as Hex };
  if (raw.startsWith(ED_PREFIX)) {
    const key = base58Decode(raw.slice(ED_PREFIX.length));
    if (key && key.length === 32 && base58Encode(key) === raw.slice(ED_PREFIX.length)) {
      return { kind: "ed25519", canonical: raw, publicKey: key };
    }
  }
  throw fail();
}

/**
 * Resolves to true when `signature` is a valid signature over the exact `message` for `wallet`.
 * Rejects with a 400 when the signature is malformed (wrong encoding or length).
 */
export async function verifyWalletSignature(wallet: ParsedWallet, message: string, signature: string): Promise<boolean> {
  if (wallet.kind === "evm") return verifyEvm(wallet.address, message, signature);
  return verifyEd25519(wallet.publicKey, message, signature);
}

async function verifyEvm(address: Hex, message: string, signature: string): Promise<boolean> {
  const hex = signature.startsWith("0x") ? signature.slice(2) : signature;
  if (!/^[0-9a-fA-F]{130}$/.test(hex)) throw bad("EVM signature must be 65 bytes of hex (r || s || v)");
  const s = BigInt(`0x${hex.slice(64, 128)}`);
  if (s === 0n || s > SECP256K1_HALF_N) return false;
  let v = parseInt(hex.slice(128, 130), 16);
  if (v === 0 || v === 1) v += 27;
  if (v !== 27 && v !== 28) return false;
  const sig = `0x${hex.slice(0, 128)}${v.toString(16)}` as Hex;
  const recovered = await recoverAddress(message, sig);
  return recovered?.toLowerCase() === address;
}

async function recoverAddress(message: string, signature: Hex): Promise<string | null> {
  try {
    return await recoverMessageAddress({ message, signature });
  } catch {
    return null;
  }
}

async function verifyEd25519(publicKey: Buffer, message: string, signature: string): Promise<boolean> {
  const hex = signature.startsWith("0x") ? signature.slice(2) : signature;
  // Same rule as the contract: 128 hex digits are hex, anything else is base58.
  const isHex = hex.length === 128 && /^[0-9a-fA-F]{128}$/.test(hex);
  const sig = isHex ? Buffer.from(hex, "hex") : base58Decode(signature);
  if (!sig || sig.length !== 64) {
    throw bad("Ed25519 signature must be 64 bytes, as hex or base58");
  }
  const key = createPublicKey({ key: Buffer.concat([ED25519_SPKI_PREFIX, publicKey]), format: "der", type: "spki" });
  return edVerify(null, Buffer.from(message, "utf8"), key, sig);
}
