// Captures real testnet data used by the decoder tests.
// Usage: node scripts/capture-fixtures.mjs [tx-hash ...]
// Writes test/fixtures/txs/<hash>.json (FastNEAR transaction tree: transaction, execution outcome,
// every receipt with its execution outcome and block height) and
// test/fixtures/blocks/<height>.json (neardata block at the height of each receipt's execution).
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "test", "fixtures");
const TX_API = process.env.FASTNEAR_TX_URL ?? "https://tx.test.fastnear.com";
const NEARDATA = process.env.NEARDATA_URL ?? "https://testnet.neardata.xyz";

const hashes = process.argv.slice(2);
if (hashes.length === 0) {
  console.error("usage: capture-fixtures.mjs <tx-hash> [...]");
  process.exit(1);
}

await mkdir(join(root, "txs"), { recursive: true });
await mkdir(join(root, "blocks"), { recursive: true });

// The API takes at most 20 hashes per request.
const transactions = [];
for (let i = 0; i < hashes.length; i += 20) {
  const res = await fetch(`${TX_API}/v0/transactions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ tx_hashes: hashes.slice(i, i + 20) }),
  });
  if (!res.ok) throw new Error(`transactions: HTTP ${res.status}`);
  transactions.push(...(await res.json()).transactions);
}

const heights = new Set();
for (const t of transactions) {
  const hash = t.transaction.hash;
  await writeFile(join(root, "txs", `${hash}.json`), JSON.stringify(t, null, 1) + "\n");
  console.log("tx", hash, "signer", t.transaction.signer_id, "height", t.execution_outcome.block_height);
  heights.add(t.execution_outcome.block_height);
  for (const r of t.receipts) heights.add(r.execution_outcome.block_height);
}

// neardata rate-limits (HTTP 429): retry with backoff.
async function getBlock(h) {
  for (let attempt = 1; ; attempt++) {
    const b = await fetch(`${NEARDATA}/v0/block/${h}`);
    if (b.ok) return b.json();
    if ((b.status === 429 || b.status >= 500) && attempt < 8) {
      await new Promise((r) => setTimeout(r, 500 * 2 ** attempt));
      continue;
    }
    throw new Error(`block ${h}: HTTP ${b.status}`);
  }
}

for (const h of [...heights].sort((a, b) => a - b)) {
  await writeFile(join(root, "blocks", `${h}.json`), JSON.stringify(await getBlock(h)) + "\n");
  console.log("block", h);
}
