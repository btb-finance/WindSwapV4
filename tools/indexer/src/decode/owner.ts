import { createHash } from "node:crypto";

/**
 * Owner ids mirror the contract (accounts.rs): a NEAR account owns `sha256(account id)`; a wallet that is
 * not linked owns `sha256(canonical wallet string)`; a linked wallet owns the id of its account.
 */
export function sha256Hex(input: string): string {
  return createHash("sha256").update(input, "utf8").digest("hex");
}

export function accountOwnerId(account: string): string {
  return sha256Hex(account);
}

/** Canonical wallet form (`evm:0x<lower hex>`, `ed25519:<base58>`, `near:<account>`), or null if malformed. */
export function canonicalWallet(raw: string): string | null {
  if (raw.startsWith("near:")) {
    const account = raw.slice(5);
    return account.length > 0 ? `near:${account}` : null;
  }
  if (raw.startsWith("evm:")) {
    const hex = raw.slice(4).replace(/^0x/i, "");
    return /^[0-9a-fA-F]{40}$/.test(hex) ? `evm:0x${hex.toLowerCase()}` : null;
  }
  if (raw.startsWith("ed25519:")) {
    return raw.length > "ed25519:".length ? raw : null;
  }
  return null;
}

/** Lower-case 0x form of an EVM address, or null if the text is not an address. */
export function normalizeEvmAddress(raw: string): string | null {
  const hex = raw.replace(/^0x/i, "");
  return /^[0-9a-fA-F]{40}$/.test(hex) ? `0x${hex.toLowerCase()}` : null;
}

/** Who a row belongs to: a NEAR account (direct call) or a wallet (signed action, deposit for a wallet). */
export type OwnerRef = { account: string } | { wallet: string };
