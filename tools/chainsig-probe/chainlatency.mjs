// On-chain latency per sign tx: block timestamp of the request tx vs. the block that
// executed the last receipt (the MPC respond/resume). Uses the public testnet RPC.
import { RPC } from "./common.mjs";
const rpc = async (method, params) => (await (await fetch(RPC, { method: "POST",
  headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }) })).json()).result;
const blockTs = async (hash) => Number((await rpc("block", { block_id: hash })).header.timestamp_nanosec) / 1e9;
const out = [];
for (const h of process.argv.slice(2)) {
  const st = await rpc("EXPERIMENTAL_tx_status", { tx_hash: h, sender_account_id: "windswapv4-user.testnet", wait_until: "EXECUTED_OPTIMISTIC" });
  const t0 = await blockTs(st.transaction_outcome.block_hash);
  const last = st.receipts_outcome[st.receipts_outcome.length - 1];
  // Receipts are listed in execution order; the resume/callback is the last executed one.
  const ts = await Promise.all(st.receipts_outcome.map((r) => blockTs(r.block_hash)));
  const t1 = Math.max(...ts);
  out.push({ tx: h, t0_ms: 0, chain_latency_s: +(t1 - t0).toFixed(3), receipt_blocks: st.receipts_outcome.length });
  console.log(`${h}  chain_latency_s=${(t1 - t0).toFixed(3)}  receipts=${st.receipts_outcome.length}`);
}
