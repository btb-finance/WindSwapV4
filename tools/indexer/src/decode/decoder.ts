/**
 * Decodes one contract receipt (an action receipt whose receiver is the contract) into:
 *  - one `call` row per function-call action (method, args, return value, success, logs, error), and
 *  - typed rows (pools, swaps, liquidity, deposits, withdrawals, locks, votes, ...) for successful calls.
 *
 * Direct methods and `execute` (signed actions) share handlers: `execute` unwraps the inner action and
 * its ActionResult, so both paths produce the same rows, with `via` telling them apart.
 *
 * Until the contract emits NEP-297 events, everything comes from calls. Receipt logs are parsed for
 * `EVENT_JSON:` lines anyway (see events.ts), so typed event handlers can be added later.
 */
import type { ReceiptExec, ReceiptAction } from "../near/types.js";
import { DecodeError, arr, asObject, bool, decimal, int, parseArgs, parseReturn, splitSign, str, type Json } from "./args.js";
import { eventRows } from "./events.js";
import { canonicalWallet, normalizeEvmAddress, type OwnerRef } from "./owner.js";
import type { CallRow, Decoded, Meta, Row, Via } from "./rows.js";

/** Direct contract methods whose semantics equal a signed action of the same name. */
const DIRECT_TO_ACTION: Record<string, string> = {
  swap: "Swap",
  swap_exact_input: "SwapExactInput",
  mint_position: "MintPosition",
  burn_position: "BurnPosition",
  collect: "Collect",
  withdraw_evm: "WithdrawEvm",
  create_lock: "CreateLock",
  increase_amount: "IncreaseAmount",
  increase_unlock_time: "IncreaseUnlockTime",
  withdraw_lock: "WithdrawLock",
  merge: "Merge",
  split: "Split",
  lock_permanent: "LockPermanent",
  unlock_permanent: "UnlockPermanent",
  delegate: "Delegate",
  vote: "Vote",
  reset: "Reset",
  poke: "Poke",
  stake_position: "StakePosition",
  unstake_position: "UnstakePosition",
  claim_gauge_rewards: "ClaimGaugeRewards",
  claim_fees: "ClaimFees",
  claim_bribes: "ClaimBribes",
  bribe: "Bribe",
  claim_rebase: "ClaimRebase",
  distribute: "Distribute",
};

interface Ctx {
  receipt: ReceiptExec;
  actionIndex: number;
  owner: OwnerRef;
  via: Via;
}

/** Returns a function that yields the metadata of the next row of one call (`seq` counts them). */
function makeMeta(c: Ctx): () => Meta {
  let seq = 0;
  const { receipt, actionIndex } = c;
  return () => ({
    receiptId: receipt.receiptId,
    actionIndex,
    txHash: receipt.txHash,
    height: receipt.height,
    timestampMs: receipt.timestampMs,
    seq: seq++,
  });
}

/** Unwraps an ActionResult (`{"Variant": value}`); `"Done"` and absent values give undefined. */
function unwrapResult(ret: unknown): unknown {
  if (ret === undefined || ret === "Done" || ret === null) return undefined;
  if (typeof ret === "object" && !Array.isArray(ret)) {
    const values = Object.values(ret as Json);
    return values.length === 1 ? values[0] : undefined;
  }
  return ret;
}

function pair(v: unknown, what: string): [string, string] {
  const a = arr(v, what);
  if (a.length !== 2) throw new DecodeError(`${what} must have 2 entries`);
  return [decimal(a[0], `${what}[0]`), decimal(a[1], `${what}[1]`)];
}

/** Magnitudes of a signed pool delta pair: amount paid in and amount paid out. */
function swapAmounts(d0: string, d1: string, zeroForOne: boolean): { amountIn: string; amountOut: string } {
  const s0 = splitSign(d0);
  const s1 = splitSign(d1);
  const in0 = s0.negative ? "0" : s0.magnitude;
  const in1 = s1.negative ? "0" : s1.magnitude;
  const out0 = s0.negative ? s0.magnitude : "0";
  const out1 = s1.negative ? s1.magnitude : "0";
  return zeroForOne
    ? { amountIn: in0, amountOut: out1 }
    : { amountIn: in1, amountOut: out0 };
}

function pathOf(v: unknown): Array<{ poolId: number; tokenIn: string | null }> {
  const hops = arr(v, "path");
  if (hops.length === 0) throw new DecodeError("path is empty");
  return hops.map((h, i) => {
    const o = asObject(h, `path[${i}]`);
    return {
      poolId: int(o.pool_id, `path[${i}].pool_id`),
      tokenIn: str(o.token_in, `path[${i}].token_in`),
    };
  });
}

/** Handles one contract action (direct or unwrapped from `execute`). `ret` is the parsed return value. */
function runAction(name: string, a: Json, ret: unknown, c: Ctx, m: () => Meta): Row[] {
  const rows: Row[] = [];
  const meta = m;
  const via = c.via;
  const owner = c.owner;

  switch (name) {
    case "Swap": {
      const zeroForOne = bool(a.zero_for_one, "zero_for_one");
      const poolId = int(a.pool_id, "pool_id");
      let amountIn: string | null = null;
      let amountOut: string | null = null;
      let amount0: string | null = null;
      let amount1: string | null = null;
      if (ret !== undefined) {
        [amount0, amount1] = pair(ret, "swap result");
        ({ amountIn, amountOut } = swapAmounts(amount0, amount1, zeroForOne));
      }
      if (amountIn === null || amountOut === null) break;
      rows.push({
        kind: "swap", ...meta(), exact: decimal(a.amount_specified, "amount_specified").startsWith("-") ? "output" : "input",
        via, owner, path: [{ poolId, tokenIn: null }], zeroForOne, amount0, amount1, amountIn, amountOut,
      });
      break;
    }
    case "SwapExactInput": {
      const path = pathOf(a.path);
      if (typeof ret !== "string" && typeof ret !== "number") break;
      rows.push({
        kind: "swap", ...meta(), exact: "input", via: path.length > 1 && via === "direct" ? "route" : via, owner,
        path, zeroForOne: null, amount0: null, amount1: null,
        amountIn: decimal(a.amount_in, "amount_in"), amountOut: decimal(ret, "amount out"),
      });
      break;
    }
    case "SwapExactOutput": {
      const path = pathOf(a.path);
      if (typeof ret !== "string" && typeof ret !== "number") break;
      rows.push({
        kind: "swap", ...meta(), exact: "output", via: path.length > 1 && via === "direct" ? "route" : via, owner,
        path, zeroForOne: null, amount0: null, amount1: null,
        amountIn: decimal(ret, "amount in"), amountOut: decimal(a.amount_out, "amount_out"),
      });
      break;
    }
    case "MintPosition":
    case "BurnPosition": {
      if (ret === undefined) break;
      const [amount0, amount1] = pair(ret, "amounts");
      rows.push({
        kind: "liquidity", ...meta(), subkind: name === "MintPosition" ? "mint" : "burn", via, owner,
        poolId: int(a.pool_id, "pool_id"), tickLower: int(a.tick_lower, "tick_lower"),
        tickUpper: int(a.tick_upper, "tick_upper"), liquidity: decimal(a.liquidity, "liquidity"), amount0, amount1,
      });
      break;
    }
    case "Collect": {
      if (ret === undefined) break;
      const [amount0, amount1] = pair(ret, "collected");
      rows.push({
        kind: "liquidity", ...meta(), subkind: "collect", via, owner,
        poolId: int(a.pool_id, "pool_id"), tickLower: int(a.tick_lower, "tick_lower"),
        tickUpper: int(a.tick_upper, "tick_upper"), liquidity: null, amount0, amount1,
      });
      break;
    }
    case "WithdrawEvm": {
      // Direct `withdraw_evm` takes `token_id` and no chain; the signed action takes `token` and `to_chain_id`.
      const tokenId = str(a.token ?? a.token_id, "token");
      const id = typeof ret === "number" ? ret : ret === undefined ? null : int(ret, "withdrawal id");
      if (id === null) break;
      rows.push({
        kind: "withdrawal_request", ...meta(), id, owner, tokenId,
        amount: decimal(a.amount, "amount"),
        toChainId: a.to_chain_id === undefined ? null : int(a.to_chain_id, "to_chain_id"),
        toAddress: str(a.to_address, "to_address"), via,
      });
      break;
    }
    case "CreateLock": {
      rows.push({
        kind: "lock_event", ...meta(), subkind: "create", via, owner,
        tokenId: typeof ret === "number" ? ret : null,
        amount: decimal(a.amount, "amount"), duration: int(a.duration, "duration"),
        permanent: a.permanent === undefined ? false : bool(a.permanent, "permanent"), extra: {},
      });
      break;
    }
    case "IncreaseAmount": {
      rows.push({
        kind: "lock_event", ...meta(), subkind: "increase_amount", via, owner,
        tokenId: int(a.token_id, "token_id"), amount: decimal(a.amount, "amount"), duration: null, permanent: null, extra: {},
      });
      break;
    }
    case "IncreaseUnlockTime": {
      rows.push({
        kind: "lock_event", ...meta(), subkind: "increase_unlock_time", via, owner,
        tokenId: int(a.token_id, "token_id"), amount: null, duration: int(a.duration, "duration"), permanent: null, extra: {},
      });
      break;
    }
    case "WithdrawLock": {
      rows.push({
        kind: "lock_event", ...meta(), subkind: "withdraw", via, owner,
        tokenId: int(a.token_id, "token_id"), amount: typeof ret === "string" ? decimal(ret, "amount") : null,
        duration: null, permanent: null, extra: {},
      });
      break;
    }
    case "Merge": {
      rows.push({
        kind: "lock_event", ...meta(), subkind: "merge", via, owner,
        tokenId: int(a.from, "from"), amount: null, duration: null, permanent: null,
        extra: { into: int(a.to, "to") },
      });
      break;
    }
    case "Split": {
      const newIds = Array.isArray(ret) ? ret : null;
      rows.push({
        kind: "lock_event", ...meta(), subkind: "split", via, owner,
        tokenId: int(a.token_id, "token_id"), amount: decimal(a.amount, "amount"), duration: null, permanent: null,
        extra: { new_token_ids: newIds },
      });
      break;
    }
    case "LockPermanent":
    case "UnlockPermanent": {
      const lock = name === "LockPermanent";
      rows.push({
        kind: "lock_event", ...meta(), subkind: lock ? "permanent" : "unpermanent", via, owner,
        tokenId: int(a.token_id, "token_id"), amount: null, duration: null, permanent: lock, extra: {},
      });
      break;
    }
    case "Delegate": {
      rows.push({
        kind: "lock_event", ...meta(), subkind: "delegate", via, owner,
        tokenId: int(a.delegator, "delegator"), amount: null, duration: null, permanent: null,
        extra: { delegatee: int(a.delegatee, "delegatee") },
      });
      break;
    }
    case "Vote": {
      const pools = arr(a.pools, "pools");
      const weights = arr(a.weights, "weights");
      if (pools.length !== weights.length) throw new DecodeError("pools and weights differ in length");
      const tokenId = int(a.token_id, "token_id");
      pools.forEach((p, i) => {
        rows.push({
          kind: "vote", ...meta(), subkind: "vote", via, owner, tokenId,
          poolId: int(p, `pools[${i}]`), weight: decimal(weights[i], `weights[${i}]`),
        });
      });
      break;
    }
    case "Reset":
    case "Poke": {
      rows.push({
        kind: "vote", ...meta(), subkind: name === "Reset" ? "reset" : "poke", via, owner,
        tokenId: int(a.token_id, "token_id"), poolId: null, weight: null,
      });
      break;
    }
    case "StakePosition":
    case "UnstakePosition": {
      rows.push({
        kind: "gauge", ...meta(), subkind: name === "StakePosition" ? "stake" : "unstake", via, owner,
        poolId: int(a.pool_id, "pool_id"), gaugeId: null,
        tickLower: int(a.tick_lower, "tick_lower"), tickUpper: int(a.tick_upper, "tick_upper"),
      });
      break;
    }
    case "ClaimGaugeRewards": {
      if (typeof ret !== "string") break;
      rows.push({
        kind: "claim", ...meta(), subkind: "gauge_rewards", via, owner, tokenId: null,
        poolId: int(a.pool_id, "pool_id"), token: null, amount: decimal(ret, "amount"),
        extra: { tick_lower: int(a.tick_lower, "tick_lower"), tick_upper: int(a.tick_upper, "tick_upper") },
      });
      break;
    }
    case "ClaimFees":
    case "ClaimBribes": {
      if (ret === undefined) break;
      const subkind = name === "ClaimFees" ? "fees" : "bribes";
      const entries = arr(ret, "claimed");
      const tokenId = int(a.token_id, "token_id");
      const gauges = arr(a.gauges, "gauges");
      entries.forEach((e) => {
        const entry = arr(e, "claimed entry");
        const token = str(entry[0], "claimed token");
        const amount = decimal(entry[1], "claimed amount");
        rows.push({
          kind: "claim", ...meta(), subkind, via, owner, tokenId, poolId: null, token, amount, extra: { gauges },
        });
      });
      break;
    }
    case "ClaimRebase": {
      if (typeof ret !== "string") break;
      rows.push({
        kind: "claim", ...meta(), subkind: "rebase", via, owner, tokenId: int(a.token_id, "token_id"),
        poolId: null, token: null, amount: decimal(ret, "amount"), extra: {},
      });
      break;
    }
    case "Bribe": {
      rows.push({
        kind: "bribe", ...meta(), via, owner, poolId: int(a.pool_id, "pool_id"),
        token: str(a.token, "token"), amount: decimal(a.amount, "amount"),
      });
      break;
    }
    case "Distribute": {
      if (typeof ret === "string") rows.push({ kind: "epoch", ...meta(), amount: decimal(ret, "amount") });
      break;
    }
    default:
      break;
  }
  return rows;
}

function ownerFromWallet(raw: unknown): { owner: OwnerRef; wallet: string } {
  const text = str(raw, "wallet");
  const wallet = canonicalWallet(text);
  if (wallet === null) throw new DecodeError(`wallet ${text} is not a canonical wallet`);
  if (wallet.startsWith("near:")) return { owner: { account: wallet.slice(5) }, wallet };
  return { owner: { wallet }, wallet };
}

/** Rows of a successful `execute`: the signed action run for the wallet's owner. */
function runExecute(a: Json, ret: unknown, c: Ctx, m: () => Meta): Row[] {
  const { owner } = ownerFromWallet(a.wallet);
  const action = a.action;
  let name: string;
  let inner: Json;
  if (typeof action === "string") {
    name = action;
    inner = {};
  } else {
    const obj = asObject(action, "action");
    const keys = Object.keys(obj);
    if (keys.length !== 1) throw new DecodeError("action must have exactly one variant");
    name = keys[0] as string;
    inner = asObject(obj[name], `action.${name}`);
  }
  const signed: Ctx = { ...c, owner, via: "signed" };
  return runAction(name, inner, unwrapResult(ret), signed, m);
}

/** Direct methods that are not signed actions. */
function runDirectOnly(method: string, a: Json, ret: unknown, c: Ctx, m: () => Meta): Row[] {
  const meta = m;
  const rows: Row[] = [];
  switch (method) {
    case "create_pool": {
      if (ret === undefined) break;
      rows.push({
        kind: "pool", ...meta(), poolId: int(ret, "pool id"), token0: str(a.token0, "token0"),
        token1: str(a.token1, "token1"), tickSpacing: int(a.tick_spacing, "tick_spacing"),
        sqrtPriceX96: decimal(a.sqrt_price_x96, "sqrt_price_x96"),
      });
      break;
    }
    case "add_token":
    case "remove_token": {
      rows.push({
        kind: "token", ...meta(), tokenId: str(a.token, "token"), tokenKind: "nep141", chainId: null,
        asset: null, decimals: null, listed: method === "add_token",
      });
      break;
    }
    case "register_evm_asset": {
      const asset = str(a.asset, "asset");
      const normalized = normalizeEvmAddress(asset) ?? asset.toLowerCase();
      rows.push({
        kind: "token", ...meta(), tokenId: str(a.token_id, "token_id"), tokenKind: "evm",
        chainId: int(a.chain_id, "chain_id"), asset: normalized, decimals: int(a.decimals, "decimals"), listed: true,
      });
      break;
    }
    case "set_wind_token": {
      rows.push({ kind: "setting", ...meta(), key: "wind_token", value: str(a.account_id, "account_id") });
      break;
    }
    case "create_gauge": {
      if (ret === undefined) break;
      rows.push({
        kind: "gauge", ...meta(), subkind: "create", via: "direct", owner: null, poolId: int(a.pool_id, "pool_id"),
        gaugeId: int(ret, "gauge id"), tickLower: null, tickUpper: null,
      });
      break;
    }
    case "swap_exact_output": {
      rows.push(...runAction("SwapExactOutput", a, ret, { ...c, via: "direct" }, m));
      break;
    }
    case "report_evm_deposit": {
      if (ret !== true) break; // false: already credited (duplicate report)
      if (a.decimals !== undefined || a.symbol !== undefined) {
        const asset = normalizeEvmAddress(str(a.asset, "asset")) ?? str(a.asset, "asset");
        rows.push({
          kind: "token_meta", ...meta(), chainId: int(a.chain_id, "chain_id"), asset,
          decimals: a.decimals === undefined || a.decimals === null ? null : int(a.decimals, "decimals"),
          symbol: typeof a.symbol === "string" ? a.symbol : null,
        });
      }
      const from = str(a.from, "from");
      const fromAddr = normalizeEvmAddress(from);
      if (fromAddr === null) throw new DecodeError("from is not an EVM address");
      rows.push({
        kind: "deposit", ...meta(), subkind: "evm", owner: { wallet: `evm:${fromAddr}` }, tokenId: null,
        chainId: int(a.chain_id, "chain_id"), asset: normalizeEvmAddress(str(a.asset, "asset")) ?? str(a.asset, "asset"),
        amount: decimal(a.amount, "amount"), sender: null, txHashEvm: normalizeTxHash(str(a.tx_hash, "tx_hash")),
        logIndex: int(a.log_index, "log_index"), fromAddress: fromAddr, lockDuration: null,
      });
      break;
    }
    case "on_withdrawal_signed": {
      if (ret !== true) break;
      const tx = asObject(a.tx, "tx");
      rows.push({
        kind: "withdrawal_update", ...meta(), id: int(a.id, "id"), status: "signed",
        nonce: int(tx.nonce, "nonce"), evmTxHash: null,
      });
      break;
    }
    case "mark_withdrawal_broadcast": {
      rows.push({
        kind: "withdrawal_update", ...meta(), id: int(a.id, "id"), status: "broadcast", nonce: null,
        evmTxHash: normalizeTxHash(str(a.tx_hash, "tx_hash")),
      });
      break;
    }
    case "link_wallet":
    case "unlink_wallet": {
      const wallet = canonicalWallet(str(a.wallet, "wallet"));
      if (wallet === null) throw new DecodeError("wallet is not canonical");
      rows.push({
        kind: "wallet", ...meta(), wallet,
        account: method === "link_wallet" ? c.receipt.predecessorId : null,
      });
      break;
    }
    case "submit_intent": {
      if (typeof ret !== "number" && typeof ret !== "string") break;
      const wallet = canonicalWallet(str(a.wallet, "wallet"));
      if (wallet === null) throw new DecodeError("wallet is not canonical");
      const intent = asObject(a.intent, "intent");
      const variant = Object.keys(intent)[0];
      if (variant !== "Swap" && variant !== "AddLiquidity") throw new DecodeError("unknown intent variant");
      const spec = asObject(intent[variant], `intent.${variant}`);
      rows.push({
        kind: "intent", ...meta(), subkind: "submit", id: int(ret, "intent id"),
        owner: ownerFromWallet(wallet).owner,
        intentKind: variant === "Swap" ? "swap" : "add_liquidity",
        tokenIn: str(spec.token_in, "token_in"), amountIn: decimal(spec.amount_in, "amount_in"), spec,
        nonce: int(a.nonce, "nonce"), expires: int(a.expires, "expires"),
        status: "open", result: null, reason: null,
      });
      break;
    }
    case "execute_intent": {
      // Permissionless. The return is the IntentView after the run (status, result, reason).
      if (ret === undefined || typeof ret !== "object" || ret === null) break;
      const view = ret as Json;
      rows.push({
        kind: "intent", ...meta(), subkind: "execute", id: int(a.id, "id"), owner: null, intentKind: null,
        tokenIn: null, amountIn: null, spec: null, nonce: null, expires: null,
        status: str(view.status, "status"), result: view.result ?? null,
        reason: typeof view.reason === "string" ? view.reason : null,
      });
      break;
    }
    case "cancel_intent":
    case "cancel_intent_near": {
      // The contract marks a cancelled intent Failed with this reason (intents.rs cancel_checked).
      rows.push({
        kind: "intent", ...meta(), subkind: "cancel", id: int(a.id, "id"), owner: null, intentKind: null,
        tokenIn: null, amountIn: null, spec: null, nonce: null, expires: null,
        status: "failed", result: null, reason: "cancelled by owner",
      });
      break;
    }
    case "ft_on_transfer": {
      // Only called by a token contract (predecessor). `ret` "0" means the amount was credited.
      if (ret !== "0") break;
      const sender = str(a.sender_id, "sender_id");
      const msgText = typeof a.msg === "string" ? a.msg : "";
      const oneStep = parseOneStep(msgText);
      if (oneStep !== null) {
        // One-step message: the amount is spent in the same call, so no deposit row is written.
        const kind = oneStep.swap !== undefined ? "swap" : "add_liquidity";
        const spec = (oneStep.swap ?? oneStep.add_liquidity ?? {}) as Record<string, unknown>;
        const owner: OwnerRef = oneStep.owner !== undefined ? ownerFromWallet(oneStep.owner).owner : { account: sender };
        rows.push({
          kind: "one_step", ...meta(), subkind: kind, owner, sender,
          tokenId: c.receipt.predecessorId, amount: decimal(a.amount, "amount"), spec,
        });
        break;
      }
      const msg = msgText.trim() === "" ? null : parseMsg(msgText);
      let depositOwner: OwnerRef = { account: sender };
      if (msg?.owner !== undefined) depositOwner = ownerFromWallet(msg.owner).owner;
      rows.push({
        kind: "deposit", ...meta(), subkind: "ft", owner: depositOwner,
        tokenId: c.receipt.predecessorId, chainId: null, asset: null, amount: decimal(a.amount, "amount"),
        sender, txHashEvm: null, logIndex: null, fromAddress: null,
        lockDuration: msg?.lock !== undefined ? int(msg.lock.duration, "lock.duration") : null,
      });
      break;
    }
    case "on_distribute_minted": {
      if (typeof ret === "string") rows.push({ kind: "epoch", ...meta(), amount: decimal(ret, "amount") });
      break;
    }
    case "execute": {
      rows.push(...runExecute(a, ret, c, m));
      break;
    }
    default:
      break;
  }
  return rows;
}

/** The one-step part of an ft_on_transfer message (oneclick.rs): present when it has `swap` or `add_liquidity`. */
function parseOneStep(text: string): { owner?: string; swap?: Json; add_liquidity?: Json } | null {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    return null;
  }
  if (v === null || typeof v !== "object" || Array.isArray(v)) return null;
  const o = v as Json;
  if (!("swap" in o) && !("add_liquidity" in o)) return null;
  const out: { owner?: string; swap?: Json; add_liquidity?: Json } = {};
  if (typeof o.owner === "string") out.owner = o.owner;
  if (o.swap !== undefined) out.swap = asObject(o.swap, "swap");
  if (o.add_liquidity !== undefined) out.add_liquidity = asObject(o.add_liquidity, "add_liquidity");
  return out;
}

function parseMsg(text: string): { owner?: string; lock?: { duration: unknown } } | null {
  try {
    const v = JSON.parse(text) as Record<string, unknown>;
    if (v === null || typeof v !== "object") return null;
    const out: { owner?: string; lock?: { duration: unknown } } = {};
    if (typeof v.owner === "string") out.owner = v.owner;
    if (v.lock && typeof v.lock === "object") out.lock = { duration: (v.lock as Record<string, unknown>).duration };
    return out;
  } catch {
    return null;
  }
}

function normalizeTxHash(raw: string): string {
  const hex = raw.replace(/^0x/i, "");
  if (!/^[0-9a-fA-F]{64}$/.test(hex)) throw new DecodeError("tx_hash is not 32 bytes");
  return `0x${hex.toLowerCase()}`;
}

/** Owner of a direct call: the predecessor account (contract uses owner_of_account(predecessor)). */
function directOwner(receipt: ReceiptExec): OwnerRef {
  return { account: receipt.predecessorId };
}

/** Decodes all calls and rows of one contract receipt. Receipts for other receivers decode to nothing. */
export function decodeReceipt(receipt: ReceiptExec, contractId: string): Decoded {
  const calls: CallRow[] = [];
  const rows: Row[] = [];
  if (receipt.receiverId !== contractId) return { calls, rows };

  const functionCalls = receipt.actions
    .map((action, actionIndex) => ({ action, actionIndex }))
    .filter((x): x is { action: Extract<ReceiptAction, { kind: "FunctionCall" }>; actionIndex: number } => x.action.kind === "FunctionCall");
  const lastIndex = functionCalls.length > 0 ? functionCalls[functionCalls.length - 1]?.actionIndex : undefined;

  for (const { action, actionIndex } of functionCalls) {
    const status = receipt.status;
    const ok = status.ok === null ? null : status.ok;
    const retText = status.ok === true && status.value !== null && actionIndex === lastIndex ? status.value : null;
    const call: CallRow = {
      kind: "call",
      receiptId: receipt.receiptId,
      actionIndex,
      txHash: receipt.txHash,
      height: receipt.height,
      timestampMs: receipt.timestampMs,
      method: action.methodName,
      decodedAs: null,
      predecessor: receipt.predecessorId,
      signer: receipt.signerId,
      args: null,
      ok,
      returnValue: null,
      error: status.ok === false ? status.error : null,
      logs: receipt.logs,
      deposit: action.deposit,
      decodeError: null,
    };
    calls.push(call);

    try {
      const a = parseArgs(action.argsText);
      call.args = a;
      // The handler name is recorded even for failed calls, so the audit table says what was attempted.
      call.decodedAs = handlerName(action.methodName, a);
      if (status.ok !== true) continue;

      const ret = parseReturn(retText);
      call.returnValue = ret === undefined ? null : ret;
      const ctx: Ctx = { receipt, actionIndex, owner: directOwner(receipt), via: "direct" };
      const m = makeMeta(ctx);
      const method = action.methodName;
      if (method === "execute") {
        rows.push(...runExecute(a, ret, ctx, m));
      } else if (DIRECT_TO_ACTION[method] !== undefined) {
        const viaRoute = method === "swap_exact_input" && Array.isArray(a.path) && a.path.length > 1;
        rows.push(...runAction(DIRECT_TO_ACTION[method] as string, a, ret, { ...ctx, via: viaRoute ? "route" : "direct" }, m));
      } else {
        rows.push(...runDirectOnly(method, a, ret, ctx, m));
      }
    } catch (err) {
      call.decodeError = err instanceof Error ? err.message : String(err);
    }
  }

  if (functionCalls.length > 0) {
    rows.push(...eventRows(receipt.logs, {
      receiptId: receipt.receiptId,
      actionIndex: 0,
      txHash: receipt.txHash,
      height: receipt.height,
      timestampMs: receipt.timestampMs,
      seq: 0,
    }));
  }
  return { calls, rows };
}

/** Name of the handler a call maps to: the signed action for `execute`, else the method's own handler. */
function handlerName(method: string, a: Json): string | null {
  if (method === "execute") {
    const inner = executeInnerName(a);
    return inner === null ? "execute" : `execute:${inner}`;
  }
  if (DIRECT_TO_ACTION[method] !== undefined) return DIRECT_TO_ACTION[method] as string;
  return DIRECT_ONLY.has(method) ? method : null;
}

function executeInnerName(a: Json): string | null {
  const action = a.action;
  if (typeof action === "string") return action;
  if (action && typeof action === "object" && !Array.isArray(action)) {
    const keys = Object.keys(action as Json);
    return keys.length === 1 ? (keys[0] as string) : null;
  }
  return null;
}

/** Methods handled by runDirectOnly (not signed actions). Other methods are recorded as calls only. */
const DIRECT_ONLY = new Set<string>([
  "create_pool",
  "add_token",
  "remove_token",
  "register_evm_asset",
  "set_wind_token",
  "create_gauge",
  "swap_exact_output",
  "report_evm_deposit",
  "on_withdrawal_signed",
  "mark_withdrawal_broadcast",
  "link_wallet",
  "unlink_wallet",
  "ft_on_transfer",
  "on_distribute_minted",
  "execute",
  "submit_intent",
  "execute_intent",
  "cancel_intent",
  "cancel_intent_near",
]);

export const SUPPORTED_DIRECT_METHODS = [
  ...Object.keys(DIRECT_TO_ACTION),
  "swap_exact_output",
  "create_pool",
  "add_token",
  "remove_token",
  "register_evm_asset",
  "set_wind_token",
  "create_gauge",
  "report_evm_deposit",
  "on_withdrawal_signed",
  "mark_withdrawal_broadcast",
  "link_wallet",
  "unlink_wallet",
  "ft_on_transfer",
  "on_distribute_minted",
  "execute",
] as const;

