// Derive foreign-chain keys and addresses for PREDECESSOR via the MPC contract view.
import { writeFileSync, mkdirSync } from "node:fs";
import { secp256k1 } from "@noble/curves/secp256k1";
import { keccak_256 } from "@noble/hashes/sha3";
import { base58 } from "@scure/base";
import { CASES, MPC, PREDECESSOR, viewCall, parseNearKey, bytesToHex } from "./common.mjs";

const out = { mpc: MPC, predecessor: PREDECESSOR, derived: {} };
for (const c of CASES) {
  const args = { path: c.path, predecessor: PREDECESSOR, domain_id: c.domainId };
  const nearKey = await viewCall("derived_public_key", args);
  const { kind, bytes } = parseNearKey(nearKey);
  const entry = { near_key: nearKey, view_args: args, key_kind: kind, pubkey_hex: bytesToHex(bytes) };
  if (c.curve === "secp256k1") {
    // NEAR returns the 64-byte uncompressed point without the 0x04 prefix.
    if (bytes.length !== 64) throw new Error(`unexpected secp256k1 length ${bytes.length}`);
    const uncompressed = Buffer.concat([Buffer.from([0x04]), Buffer.from(bytes)]);
    const compressed = secp256k1.ProjectivePoint.fromHex(uncompressed).toRawBytes(true);
    const addr = keccak_256(bytes).slice(-20);
    entry.compressed_hex = bytesToHex(compressed);
    entry.evm_address = "0x" + bytesToHex(addr);
  } else {
    if (bytes.length !== 32) throw new Error(`unexpected ed25519 length ${bytes.length}`);
    entry.solana_address = base58.encode(bytes);
  }
  out.derived[c.name] = entry;
}
mkdirSync(new URL("./out", import.meta.url), { recursive: true });
writeFileSync(new URL("./out/derived.json", import.meta.url), JSON.stringify(out, null, 2) + "\n");
console.log(JSON.stringify(out, null, 2));
