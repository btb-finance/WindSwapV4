// Verify MPC signatures locally against the derived keys from derive.mjs. No broadcast.
import { readFileSync, writeFileSync, readdirSync } from "node:fs";
import { secp256k1 } from "@noble/curves/secp256k1";
import { ed25519 as ed } from "@noble/curves/ed25519";
import { keccak_256 } from "@noble/hashes/sha3";
import { hexToBytes, bytesToHex, PAYLOAD_HEX } from "./common.mjs";

const derived = JSON.parse(readFileSync(new URL("./out/derived.json", import.meta.url), "utf8"));
const msg = hexToBytes(PAYLOAD_HEX);
const N = secp256k1.CURVE.n;

// The contract logs the return value as JSON after this marker in the CLI output.
function extractReturn(text) {
  const start = text.indexOf("Function execution return value (printed to stdout):");
  const end = text.indexOf('The "sign" call');
  if (start < 0 || end < 0) throw new Error("return value not found");
  return JSON.parse(text.slice(start + "Function execution return value (printed to stdout):".length, end));
}

const results = [];
function check(label, ok) { return { label, ok: !!ok }; }

for (const f of readdirSync(new URL("./out", import.meta.url)).filter((f) => /^sign_(evm|sol)_\d\.txt$/.test(f)).sort()) {
  const [, kind, run] = f.match(/^sign_(evm|sol)_(\d)\.txt$/);
  const resp = extractReturn(readFileSync(new URL(`./out/${f}`, import.meta.url), "utf8"));
  const row = { file: f, kind, run: Number(run), checks: [] };

  if (kind === "evm") {
    const d = derived.derived.evm;
    const bigR = secp256k1.ProjectivePoint.fromHex(resp.big_r.affine_point);
    const r = bigR.x % N; // r is the x coordinate of R, reduced mod n
    const s0 = BigInt("0x" + resp.s.scalar);
    let recid = resp.recovery_id;
    // Normalise to low-s (EIP-2) as Ethereum requires; flipping s flips the recovery parity.
    let s = s0;
    let flipped = false;
    if (s > N / 2n) { s = N - s; recid ^= 1; flipped = true; }
    row.s_normalised = flipped;
    row.r_hex = r.toString(16).padStart(64, "0");
    row.s_hex = s.toString(16).padStart(64, "0");
    row.recovery_id = resp.recovery_id;

    const sig = new secp256k1.Signature(r, s).addRecoveryBit(recid);
    const recovered = sig.recoverPublicKey(msg).toRawBytes(false); // 65 bytes, 0x04 prefix
    const recoveredXY = recovered.slice(1);
    const derivedXY = hexToBytes(d.pubkey_hex);
    const recoveredAddr = "0x" + bytesToHex(keccak_256(recoveredXY).slice(-20));
    row.recovered_pubkey_hex = bytesToHex(recoveredXY);
    row.recovered_evm_address = recoveredAddr;
    row.checks.push(check("R.x mod n == r (recomputed from big_r)", bigR.x % N === r));
    row.checks.push(check("recovered pubkey == derived pubkey", bytesToHex(recoveredXY) === d.pubkey_hex));
    row.checks.push(check("recovered EVM address == derived EVM address", recoveredAddr === d.evm_address));
    row.checks.push(check("standard ECDSA verify with derived pubkey", secp256k1.verify(new secp256k1.Signature(r, s), msg, derivedXY_to_full(derivedXY), { lowS: true })));
    // Negative control: a different message must not recover to the derived key.
    const otherMsg = keccak_256(new TextEncoder().encode("tampered"));
    const tamperedPub = sig.recoverPublicKey(otherMsg).toRawBytes(false).slice(1);
    row.checks.push(check("negative control: tampered msg does not recover derived key", bytesToHex(tamperedPub) !== d.pubkey_hex));
  } else {
    const d = derived.derived.sol;
    const sigBytes = Uint8Array.from(resp.signature);
    row.sig_len = sigBytes.length;
    row.sig_hex = bytesToHex(sigBytes);
    const pub = hexToBytes(d.pubkey_hex);
    row.checks.push(check("signature length is 64 bytes", sigBytes.length === 64));
    row.checks.push(check("ed25519 verify(sig, payload, derived pubkey)", ed.verify(sigBytes, msg, pub)));
    const tampered = new Uint8Array(msg); tampered[0] ^= 1;
    row.checks.push(check("negative control: tampered payload rejected", !ed.verify(sigBytes, tampered, pub)));
    const other = ed.getPublicKey(new Uint8Array(32).fill(7));
    row.checks.push(check("negative control: other key rejected", !ed.verify(sigBytes, msg, other)));
  }
  row.all_ok = row.checks.every((c) => c.ok);
  results.push(row);
}

// Uncompressed 64-byte XY -> 65-byte 0x04-prefixed point for noble's verify.
function derivedXY_to_full(xy) { return Buffer.concat([Buffer.from([4]), Buffer.from(xy)]); }

writeFileSync(new URL("./out/verify.json", import.meta.url), JSON.stringify(results, null, 2) + "\n");
for (const r of results) {
  console.log(`${r.file}  ${r.all_ok ? "PASS" : "FAIL"}`);
  for (const c of r.checks) console.log(`   ${c.ok ? "ok " : "BAD"} ${c.label}`);
}
console.log(`\n${results.filter((r) => r.all_ok).length}/${results.length} signatures verified`);
