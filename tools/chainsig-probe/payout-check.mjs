// Checks the contract's EVM payouts: the derived address of the contract, and a signed raw
// transaction returned by `request_evm_payout`. No broadcast.
//
// Usage:
//   node payout-check.mjs --contract <acct> [--path ethereum-1]
//       Prints the contract's derived EVM address (from the MPC contract view).
//   node payout-check.mjs --contract <acct> [--path ethereum-1] --raw 0x02...
//   node payout-check.mjs --contract <acct> [--path ethereum-1] --file <near-cli-output.txt>
//       Derives the address, decodes the raw transaction, recomputes the signing hash, recovers
//       the signer from (r, s, y_parity), and checks it equals the derived address.
//   node payout-check.mjs --address 0x... --raw 0x02...   (offline, no derivation)
//   node payout-check.mjs --self-test
//       Offline. Signs a transaction with a local key and checks it with the same code.
//
// Exit code 0 means every check passed.
import { readFileSync } from "node:fs";
import { secp256k1 } from "@noble/curves/secp256k1";
import { keccak_256 } from "@noble/hashes/sha3";
import { hexToBytes, bytesToHex, viewCall, parseNearKey, MPC } from "./common.mjs";

const N = secp256k1.CURVE.n;
const HALF_N = N / 2n;

// ---- RLP ----

function concatBytes(...parts) {
  return Uint8Array.from(parts.flatMap((p) => Array.from(p)));
}

function minimalBytes(n) {
  // Big-endian bytes of a non-negative bigint with no leading zeros. Zero is empty.
  if (n === 0n) return new Uint8Array();
  const hex = n.toString(16);
  return hexToBytes(hex.length % 2 ? "0" + hex : hex);
}

function rlpHeader(offset, len) {
  if (len <= 55) return Uint8Array.of(offset + len);
  const lb = minimalBytes(BigInt(len));
  return concatBytes(Uint8Array.of(offset + 55 + lb.length), lb);
}

function rlpEncodeBytes(data) {
  if (data.length === 1 && data[0] < 0x80) return data;
  return concatBytes(rlpHeader(0x80, data.length), data);
}

function rlpEncodeList(items) {
  const payload = concatBytes(...items);
  return concatBytes(rlpHeader(0xc0, payload.length), payload);
}

function readLength(buf, pos, n) {
  let v = 0;
  for (let i = 0; i < n; i++) v = v * 256 + buf[pos + i];
  return v;
}

// Reads one RLP item at `pos`. Returns its raw bytes, its payload, whether it is a list, and the
// end position. Throws if the item runs past the end of `buf`.
function readItem(buf, pos) {
  const b = buf[pos];
  if (b === undefined) throw new Error("RLP: unexpected end of input");
  let headerLen;
  let len;
  let isList = false;
  if (b < 0x80) {
    return { raw: buf.subarray(pos, pos + 1), payload: buf.subarray(pos, pos + 1), isList, end: pos + 1 };
  } else if (b <= 0xb7) {
    headerLen = 1;
    len = b - 0x80;
  } else if (b <= 0xbf) {
    headerLen = 1 + (b - 0xb7);
    len = readLength(buf, pos + 1, b - 0xb7);
  } else if (b <= 0xf7) {
    isList = true;
    headerLen = 1;
    len = b - 0xc0;
  } else {
    isList = true;
    headerLen = 1 + (b - 0xf7);
    len = readLength(buf, pos + 1, b - 0xf7);
  }
  const start = pos + headerLen;
  const end = start + len;
  if (end > buf.length) throw new Error("RLP: item longer than input");
  return { raw: buf.subarray(pos, end), payload: buf.subarray(start, end), isList, end };
}

function readList(payload) {
  const items = [];
  let pos = 0;
  while (pos < payload.length) {
    const item = readItem(payload, pos);
    items.push(item);
    pos = item.end;
  }
  return items;
}

function bigintOf(bytes) {
  return bytes.length === 0 ? 0n : BigInt("0x" + bytesToHex(bytes));
}

function padTo32(bytes) {
  if (bytes.length > 32) throw new Error("integer longer than 32 bytes");
  const out = new Uint8Array(32);
  out.set(bytes, 32 - bytes.length);
  return out;
}

// ---- Transaction ----

const FIELD_NAMES = ["chainId", "nonce", "maxPriorityFeePerGas", "maxFeePerGas", "gasLimit", "to", "value", "data", "accessList"];

// Decodes `0x02 || rlp([fields, y_parity, r, s])` and recomputes what was signed.
function decodeSignedTx(rawHex) {
  const raw = hexToBytes(rawHex.replace(/^0x/, ""));
  if (raw[0] !== 0x02) throw new Error(`not an EIP-1559 transaction (type byte ${raw[0]})`);
  const top = readItem(raw, 1);
  if (!top.isList || top.end !== raw.length) throw new Error("trailing bytes after the transaction list");
  const items = readList(top.payload);
  if (items.length !== 12) throw new Error(`expected 12 list items, got ${items.length}`);

  const unsigned = items.slice(0, 9);
  const signingHash = keccak_256(concatBytes(Uint8Array.of(0x02), rlpEncodeList(unsigned.map((i) => i.raw))));
  const fields = {};
  unsigned.forEach((item, i) => {
    if (i === 5 || i === 7) fields[FIELD_NAMES[i]] = "0x" + bytesToHex(item.payload);
    else if (i === 8) fields[FIELD_NAMES[i]] = "[]";
    else fields[FIELD_NAMES[i]] = bigintOf(item.payload).toString();
  });
  if (unsigned[7].payload.length !== 0) throw new Error("calldata is not empty (not a plain transfer)");
  if (!(unsigned[8].isList && unsigned[8].payload.length === 0)) throw new Error("access list is not empty");

  const yParity = Number(bigintOf(items[9].payload));
  const r = bigintOf(items[10].payload);
  const s = bigintOf(items[11].payload);
  return { fields, signingHash, yParity, r, s };
}

// Checks a signed raw transaction against an expected EVM address (0x-hex, lower case).
function verifyRawTx(rawHex, expectedAddress) {
  const tx = decodeSignedTx(rawHex);
  const checks = [];
  const check = (label, ok) => checks.push({ label, ok: !!ok });

  check("y_parity is 0 or 1", tx.yParity === 0 || tx.yParity === 1);
  check("r in [1, n)", tx.r > 0n && tx.r < N);
  check("s in [1, n/2] (low-s)", tx.s > 0n && tx.s <= HALF_N);

  let recovered = null;
  try {
    const sig = new secp256k1.Signature(tx.r, tx.s).addRecoveryBit(tx.yParity);
    recovered = sig.recoverPublicKey(tx.signingHash).toRawBytes(false);
  } catch (e) {
    check(`recover signer (${e.message})`, false);
  }
  let address = null;
  if (recovered) {
    address = "0x" + bytesToHex(keccak_256(recovered.slice(1)).slice(-20));
    check("recovered signer address equals expected address", address === expectedAddress.toLowerCase());
  }
  return { ok: checks.every((c) => c.ok), checks, fields: tx.fields, signingHash: "0x" + bytesToHex(tx.signingHash), signer: address };
}

// ---- Address ----

async function derivedAddress(contract, path) {
  const args = { path, predecessor: contract, domain_id: 0 };
  const nearKey = await viewCall("derived_public_key", args);
  const { kind, bytes } = parseNearKey(nearKey);
  if (kind !== "secp256k1" || bytes.length !== 64) throw new Error(`unexpected derived key: ${nearKey}`);
  return {
    mpc: MPC,
    view_args: args,
    near_key: nearKey,
    evm_address: "0x" + bytesToHex(keccak_256(bytes).slice(-20)),
  };
}

// ---- Self-test (offline) ----

function selfTest() {
  const sk = hexToBytes("42".repeat(32));
  const pub = secp256k1.getPublicKey(sk, false);
  const address = "0x" + bytesToHex(keccak_256(pub.slice(1)).slice(-20));

  const to = hexToBytes("22".repeat(20));
  // chainId, nonce, maxPriorityFeePerGas, maxFeePerGas, gasLimit, to, value, data, accessList
  const fields = [
    rlpEncodeBytes(minimalBytes(11155111n)),
    rlpEncodeBytes(minimalBytes(7n)),
    rlpEncodeBytes(minimalBytes(2_000_000_000n)),
    rlpEncodeBytes(minimalBytes(30_000_000_000n)),
    rlpEncodeBytes(minimalBytes(21000n)),
    rlpEncodeBytes(to),
    rlpEncodeBytes(minimalBytes(10n ** 18n)),
    rlpEncodeBytes(new Uint8Array()),
    rlpEncodeList([]),
  ];
  const hash = keccak_256(concatBytes(Uint8Array.of(0x02), rlpEncodeList(fields)));
  const sig = secp256k1.sign(hash, sk, { lowS: true });
  const encodedFields = [
    ...fields,
    rlpEncodeBytes(minimalBytes(BigInt(sig.recovery))),
    rlpEncodeBytes(minimalBytes(sig.r)),
    rlpEncodeBytes(minimalBytes(sig.s)),
  ];
  const raw = "0x02" + bytesToHex(rlpEncodeList(encodedFields));

  const good = verifyRawTx(raw, address);
  const wrong = verifyRawTx(raw, "0x" + "00".repeat(20));
  const results = [
    { label: "self-test: signed tx verifies against its own address", ok: good.ok },
    { label: "self-test: a different address is rejected", ok: !wrong.ok },
  ];
  for (const r of results) console.log(`${r.ok ? "PASS" : "FAIL"}  ${r.label}`);
  return results.every((r) => r.ok);
}

// ---- CLI ----

function parseArgs(argv) {
  const opts = { path: "ethereum-1" };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--self-test") opts.selfTest = true;
    else if (a === "--contract") opts.contract = argv[++i];
    else if (a === "--path") opts.path = argv[++i];
    else if (a === "--raw") opts.raw = argv[++i];
    else if (a === "--file") opts.file = argv[++i];
    else if (a === "--address") opts.address = argv[++i];
    else throw new Error(`unknown argument ${a}`);
  }
  return opts;
}

// The CLI prints the function return value after this marker. The raw transaction is the only
// 0x02-prefixed hex string in the output, so take the longest such match.
function rawFromFile(path) {
  const text = readFileSync(path, "utf8");
  const matches = text.match(/0x02[0-9a-fA-F]{20,}/g) ?? [];
  if (matches.length === 0) throw new Error(`no 0x02 transaction found in ${path}`);
  return matches.reduce((a, b) => (b.length > a.length ? b : a));
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.selfTest) {
    process.exitCode = selfTest() ? 0 : 1;
    return;
  }
  // With --address the derivation is skipped, so a raw transaction can be checked offline.
  let address = opts.address;
  if (!address) {
    if (!opts.contract) throw new Error("--contract <account> or --address <0x..> is required");
    const derived = await derivedAddress(opts.contract, opts.path);
    console.log(JSON.stringify(derived, null, 2));
    address = derived.evm_address;
  }
  const rawHex = opts.raw ?? (opts.file ? rawFromFile(opts.file) : null);
  if (!rawHex) return;

  const result = verifyRawTx(rawHex, address);
  console.log(JSON.stringify({ signing_hash: result.signingHash, fields: result.fields, signer: result.signer }, (_, v) => (typeof v === "bigint" ? v.toString() : v), 2));
  for (const c of result.checks) console.log(`${c.ok ? "PASS" : "FAIL"}  ${c.label}`);
  process.exitCode = result.ok ? 0 : 1;
}

main().catch((e) => {
  console.error(e.message);
  process.exitCode = 1;
});
