// Sum gas burnt and token burnt across every receipt of one sign transaction (RPC EXPERIMENTAL_tx_status).
import { RPC } from "./common.mjs";
const [txHash, sender] = process.argv.slice(2);
const res = await fetch(RPC, { method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "EXPERIMENTAL_tx_status",
    params: { tx_hash: txHash, sender_account_id: sender, wait_until: "EXECUTED_OPTIMISTIC" } }) });
const j = (await res.json()).result;
let gas = 0n, tokens = 0n, n = 0;
const rows = [];
for (const o of [j.transaction_outcome, ...j.receipts_outcome]) {
  const oc = o.outcome;
  gas += BigInt(oc.gas_burnt); tokens += BigInt(oc.tokens_burnt); n++;
  rows.push(`${oc.executor_id.padEnd(28)} gas_burnt=${oc.gas_burnt} tokens_burnt_yocto=${oc.tokens_burnt}`);
}
console.log(rows.join("\n"));
console.log(`receipts=${n} total_gas_burnt=${gas} total_tokens_burnt_yocto=${tokens} (${Number(tokens)/1e24} NEAR)`);
