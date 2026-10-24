/**
 * Writes decoded rows. Every function here runs inside the caller's transaction, so a block's rows and
 * the cursor move together. Idempotent: each event table has a unique key and inserts ignore duplicates.
 */
import type pg from "pg";
import { accountOwnerId, sha256Hex, type OwnerRef } from "../decode/owner.js";
import type { CallRow, Decoded, LiquidityRow, Meta, Row, SwapRow } from "../decode/rows.js";

type Client = pg.PoolClient | pg.Client;

export interface Resolved {
  owner: string;
  account: string | null;
  wallet: string | null;
}

export function tsOf(ms: number): string {
  return new Date(ms).toISOString();
}

/** Owner of a reference, looking up wallet links in the same transaction. */
export async function resolveOwner(client: Client, ref: OwnerRef): Promise<Resolved> {
  if ("account" in ref) {
    return { owner: accountOwnerId(ref.account), account: ref.account, wallet: null };
  }
  const { wallet } = ref;
  if (wallet.startsWith("near:")) {
    const account = wallet.slice(5);
    return { owner: accountOwnerId(account), account, wallet };
  }
  const link = await client.query<{ account: string; owner: string }>(
    "select account, owner from wallet_links where wallet = $1",
    [wallet],
  );
  const row = link.rows[0];
  if (row) return { owner: row.owner, account: row.account, wallet };
  return { owner: sha256Hex(wallet), account: null, wallet };
}

function negate(decimal: string): string {
  if (decimal === "0") return "0";
  return decimal.startsWith("-") ? decimal.slice(1) : `-${decimal}`;
}

async function insertNew(client: Client, sql: string, params: unknown[]): Promise<boolean> {
  const res = await client.query(sql, params);
  return (res.rowCount ?? 0) > 0;
}

async function poolTokens(client: Client, poolId: number): Promise<{ token0: string; token1: string } | null> {
  const res = await client.query<{ token0: string; token1: string }>(
    "select token0, token1 from pools where pool_id = $1",
    [poolId],
  );
  return res.rows[0] ?? null;
}

async function writeSwap(client: Client, r: SwapRow): Promise<void> {
  const owner = await resolveOwner(client, r.owner);
  // Token in: the hop's explicit token, or for a direct single-pool swap the side zero_for_one selects.
  // Token out: the other token of the last pool.
  const hops = await Promise.all(r.path.map(async (hop) => ({ hop, tokens: await poolTokens(client, hop.poolId) })));
  const first = hops[0];
  const last = hops[hops.length - 1];
  const inputOf = (hop: { poolId: number; tokenIn: string | null }, t: { token0: string; token1: string }) =>
    hop.tokenIn ?? (r.zeroForOne ? t.token0 : t.token1);
  const tokenIn = first?.tokens ? inputOf(first.hop, first.tokens) : null;
  let tokenOut: string | null = null;
  if (last?.tokens) {
    const lastIn = inputOf(last.hop, last.tokens);
    tokenOut = lastIn === last.tokens.token0 ? last.tokens.token1 : last.tokens.token0;
  }
  await client.query(
    `insert into swaps (receipt_id, action_index, tx_hash, block_height, block_ts, pool_id, route, token_in, token_out,
        exact, via, zero_for_one, amount0, amount1, amount_in, amount_out, trader_owner, trader_account, wallet)
     values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)
     on conflict (receipt_id, action_index) do nothing`,
    [
      r.receiptId,
      r.actionIndex,
      r.txHash,
      r.height,
      tsOf(r.timestampMs),
      r.path.length === 1 ? r.path[0]?.poolId ?? null : null,
      r.path.length > 1 ? r.path.map((h) => h.poolId) : null,
      tokenIn,
      tokenOut,
      r.exact,
      r.via,
      r.zeroForOne,
      r.amount0,
      r.amount1,
      r.amountIn,
      r.amountOut,
      owner.owner,
      owner.account,
      owner.wallet,
    ],
  );
}

async function writeLiquidity(client: Client, r: LiquidityRow): Promise<void> {
  const owner = await resolveOwner(client, r.owner);
  const inserted = await insertNew(
    client,
    `insert into liquidity_events (receipt_id, action_index, seq, kind, via, owner, account, wallet, pool_id,
        tick_lower, tick_upper, liquidity, amount0, amount1, tx_hash, block_height, block_ts)
     values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)
     on conflict (receipt_id, action_index, seq) do nothing`,
    [
      r.receiptId, r.actionIndex, r.seq, r.subkind, r.via, owner.owner, owner.account, owner.wallet, r.poolId,
      r.tickLower, r.tickUpper, r.liquidity, r.amount0, r.amount1, r.txHash, r.height, tsOf(r.timestampMs),
    ],
  );
  if (!inserted || r.subkind === "collect" || r.liquidity === null) return;
  const delta = r.subkind === "mint" ? r.liquidity : negate(r.liquidity);
  await client.query(
    `insert into positions (owner, pool_id, tick_lower, tick_upper, account, wallet, liquidity, updated_height)
     values ($1,$2,$3,$4,$5,$6,$7,$8)
     on conflict (owner, pool_id, tick_lower, tick_upper) do update set
       liquidity = positions.liquidity + excluded.liquidity,
       updated_height = excluded.updated_height,
       account = coalesce(excluded.account, positions.account),
       wallet = coalesce(excluded.wallet, positions.wallet)`,
    [owner.owner, r.poolId, r.tickLower, r.tickUpper, owner.account, owner.wallet, delta, r.height],
  );
}

async function writeCall(client: Client, c: CallRow): Promise<void> {
  await client.query(
    `insert into calls (receipt_id, action_index, tx_hash, block_height, block_ts, method, decoded_as, predecessor,
        signer, args, ok, return_value, error, logs, deposit_yocto, decode_error)
     values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::jsonb,$11,$12::jsonb,$13,$14::jsonb,$15,$16)
     on conflict (receipt_id, action_index) do nothing`,
    [
      c.receiptId, c.actionIndex, c.txHash, c.height, tsOf(c.timestampMs), c.method, c.decodedAs, c.predecessor,
      c.signer, c.args === null ? null : JSON.stringify(c.args), c.ok,
      c.returnValue === null || c.returnValue === undefined ? null : JSON.stringify(c.returnValue),
      c.error, JSON.stringify(c.logs), c.deposit, c.decodeError,
    ],
  );
}

async function writeOne(client: Client, row: Row): Promise<void> {
  switch (row.kind) {
    case "event":
      await client.query(
        `insert into chain_events (receipt_id, log_index, tx_hash, block_height, block_ts, standard, version, event, data)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9::jsonb) on conflict (receipt_id, log_index) do nothing`,
        [row.receiptId, row.logIndex, row.txHash, row.height, tsOf(row.timestampMs), row.standard, row.version,
          row.event, JSON.stringify(row.data ?? null)],
      );
      return;

    case "pool": {
      await client.query(
        `insert into pools (pool_id, token0, token1, tick_spacing, sqrt_price_x96_init, created_at, created_tx, created_height)
         values ($1,$2,$3,$4,$5,$6,$7,$8) on conflict (pool_id) do nothing`,
        [row.poolId, row.token0, row.token1, row.tickSpacing, row.sqrtPriceX96, tsOf(row.timestampMs), row.txHash, row.height],
      );
      return;
    }

    case "swap":
      return writeSwap(client, row);

    case "liquidity":
      return writeLiquidity(client, row);

    case "deposit": {
      const owner = await resolveOwner(client, row.owner);
      let tokenId = row.tokenId;
      if (row.subkind === "evm") {
        const t = await client.query<{ token_id: string }>(
          "select token_id from tokens where chain_id = $1 and asset = $2",
          [row.chainId, row.asset],
        );
        tokenId = t.rows[0]?.token_id ?? null;
      }
      const inserted = await insertNew(
        client,
        `insert into deposits (receipt_id, action_index, seq, kind, token_id, chain_id, asset, amount, owner, account,
            wallet, sender, tx_hash_evm, log_index, from_address, tx_hash, block_height, block_ts)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)
         on conflict (receipt_id, action_index, seq) do nothing`,
        [
          row.receiptId, row.actionIndex, row.seq, row.subkind, tokenId, row.chainId, row.asset, row.amount,
          owner.owner, owner.account, owner.wallet, row.sender, row.txHashEvm, row.logIndex, row.fromAddress,
          row.txHash, row.height, tsOf(row.timestampMs),
        ],
      );
      if (!inserted || row.lockDuration === null || tokenId === null) return;
      // A deposit with a `lock` message becomes a veNFT only for the WIND token (lib.rs ft_on_transfer).
      const wind = await client.query<{ value: string }>("select value from settings where key = 'wind_token'");
      if (wind.rows[0]?.value !== tokenId) return;
      await client.query(
        `insert into lock_events (receipt_id, action_index, seq, kind, via, owner, account, wallet, token_id, amount,
            duration, permanent, extra, tx_hash, block_height, block_ts)
         values ($1,$2,$3,'create_from_deposit','direct',$4,$5,$6,null,$7,$8,false,'{}'::jsonb,$9,$10,$11)
         on conflict (receipt_id, action_index, seq) do nothing`,
        [row.receiptId, row.actionIndex, row.seq + 1000, owner.owner, owner.account, owner.wallet, row.amount,
          row.lockDuration, row.txHash, row.height, tsOf(row.timestampMs)],
      );
      return;
    }

    case "withdrawal_request": {
      const owner = await resolveOwner(client, row.owner);
      await client.query(
        `insert into withdrawals (id, owner, account, wallet, token_id, amount, to_chain_id, to_address, status,
            requested_tx_hash, requested_height, requested_ts, updated_height, updated_ts, via)
         values ($1,$2,$3,$4,$5,$6,coalesce($7, (select chain_id from tokens where token_id = $5)),$8,'pending',$9,$10,$11,$10,$11,$12)
         on conflict (id) do nothing`,
        [row.id, owner.owner, owner.account, owner.wallet, row.tokenId, row.amount, row.toChainId, row.toAddress,
          row.txHash, row.height, tsOf(row.timestampMs), row.via],
      );
      return;
    }

    case "withdrawal_update":
      await client.query(
        `update withdrawals set
           status = case when status = 'broadcast' then status else $2 end,
           nonce = coalesce($3, nonce),
           evm_tx_hash = coalesce($4, evm_tx_hash),
           updated_height = $5, updated_ts = $6
         where id = $1`,
        [row.id, row.status, row.nonce, row.evmTxHash, row.height, tsOf(row.timestampMs)],
      );
      return;

    case "token":
      await client.query(
        `insert into tokens (token_id, kind, chain_id, asset, decimals, listed, first_height, updated_height)
         values ($1,$2,$3,$4,$5,$6,$7,$7)
         on conflict (token_id) do update set
           chain_id = coalesce(excluded.chain_id, tokens.chain_id),
           asset = coalesce(excluded.asset, tokens.asset),
           decimals = coalesce(excluded.decimals, tokens.decimals),
           listed = excluded.listed,
           updated_height = excluded.updated_height`,
        [row.tokenId, row.tokenKind, row.chainId, row.asset, row.decimals, row.listed, row.height],
      );
      return;

    case "setting":
      await client.query(
        `insert into settings (key, value) values ($1, $2)
         on conflict (key) do update set value = excluded.value, updated_at = now()`,
        [row.key, row.value],
      );
      return;

    case "lock_event": {
      const owner = await resolveOwner(client, row.owner);
      await client.query(
        `insert into lock_events (receipt_id, action_index, seq, kind, via, owner, account, wallet, token_id, amount,
            duration, permanent, extra, tx_hash, block_height, block_ts)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13::jsonb,$14,$15,$16)
         on conflict (receipt_id, action_index, seq) do nothing`,
        [row.receiptId, row.actionIndex, row.seq, row.subkind, row.via, owner.owner, owner.account, owner.wallet,
          row.tokenId, row.amount, row.duration, row.permanent, JSON.stringify(row.extra), row.txHash, row.height,
          tsOf(row.timestampMs)],
      );
      return;
    }

    case "vote": {
      const owner = await resolveOwner(client, row.owner);
      await client.query(
        `insert into votes (receipt_id, action_index, seq, kind, via, owner, account, wallet, token_id, pool_id, weight,
            tx_hash, block_height, block_ts)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
         on conflict (receipt_id, action_index, seq) do nothing`,
        [row.receiptId, row.actionIndex, row.seq, row.subkind, row.via, owner.owner, owner.account, owner.wallet,
          row.tokenId, row.poolId, row.weight, row.txHash, row.height, tsOf(row.timestampMs)],
      );
      return;
    }

    case "bribe": {
      const owner = await resolveOwner(client, row.owner);
      await client.query(
        `insert into bribes (receipt_id, action_index, seq, via, owner, account, wallet, pool_id, token, amount,
            tx_hash, block_height, block_ts)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
         on conflict (receipt_id, action_index, seq) do nothing`,
        [row.receiptId, row.actionIndex, row.seq, row.via, owner.owner, owner.account, owner.wallet, row.poolId,
          row.token, row.amount, row.txHash, row.height, tsOf(row.timestampMs)],
      );
      return;
    }

    case "claim": {
      const owner = await resolveOwner(client, row.owner);
      await client.query(
        `insert into claims (receipt_id, action_index, seq, kind, via, owner, account, wallet, token_id, pool_id, token,
            amount, extra, tx_hash, block_height, block_ts)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13::jsonb,$14,$15,$16)
         on conflict (receipt_id, action_index, seq) do nothing`,
        [row.receiptId, row.actionIndex, row.seq, row.subkind, row.via, owner.owner, owner.account, owner.wallet,
          row.tokenId, row.poolId, row.token, row.amount, JSON.stringify(row.extra), row.txHash, row.height,
          tsOf(row.timestampMs)],
      );
      return;
    }

    case "gauge": {
      if (row.subkind === "create") {
        await client.query(
          `insert into gauges (pool_id, gauge_id, created_height, created_ts) values ($1,$2,$3,$4)
           on conflict (pool_id) do nothing`,
          [row.poolId, row.gaugeId, row.height, tsOf(row.timestampMs)],
        );
      }
      const owner = row.owner ? await resolveOwner(client, row.owner) : { owner: null, account: null, wallet: null };
      await client.query(
        `insert into gauge_events (receipt_id, action_index, seq, kind, via, owner, account, wallet, pool_id, gauge_id,
            tick_lower, tick_upper, tx_hash, block_height, block_ts)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
         on conflict (receipt_id, action_index, seq) do nothing`,
        [row.receiptId, row.actionIndex, row.seq, row.subkind, row.via, owner.owner, owner.account, owner.wallet,
          row.poolId, row.gaugeId, row.tickLower, row.tickUpper, row.txHash, row.height, tsOf(row.timestampMs)],
      );
      if (row.subkind !== "create" && row.tickLower !== null && row.tickUpper !== null && owner.owner) {
        // Staking flips the position's flag; the position row is created if it is not known yet.
        await client.query(
          `insert into positions (owner, pool_id, tick_lower, tick_upper, account, wallet, staked, updated_height)
           values ($1,$2,$3,$4,$5,$6,$7,$8)
           on conflict (owner, pool_id, tick_lower, tick_upper) do update set
             staked = excluded.staked, updated_height = excluded.updated_height`,
          [owner.owner, row.poolId, row.tickLower, row.tickUpper, owner.account, owner.wallet, row.subkind === "stake", row.height],
        );
      }
      return;
    }

    case "epoch":
      await client.query(
        `insert into epochs (receipt_id, amount, block_height, block_ts) values ($1,$2,$3,$4)
         on conflict (receipt_id) do nothing`,
        [row.receiptId, row.amount, row.height, tsOf(row.timestampMs)],
      );
      return;

    case "wallet":
      if (row.account === null) {
        await client.query("delete from wallet_links where wallet = $1", [row.wallet]);
      } else {
        await client.query(
          `insert into wallet_links (wallet, account, owner, linked_height, linked_ts) values ($1,$2,$3,$4,$5)
           on conflict (wallet) do update set account = excluded.account, owner = excluded.owner,
             linked_height = excluded.linked_height, linked_ts = excluded.linked_ts`,
          [row.wallet, row.account, accountOwnerId(row.account), row.height, tsOf(row.timestampMs)],
        );
      }
      return;

    case "intent": {
      if (row.subkind === "submit") {
        const owner = row.owner ? await resolveOwner(client, row.owner) : { owner: null, account: null, wallet: null };
        await client.query(
          `insert into intents (id, owner, account, wallet, kind, token_in, amount_in, spec, nonce, expires, status,
              submitted_tx_hash, submitted_height, submitted_ts, updated_height, updated_ts)
           values ($1,$2,$3,$4,$5,$6,$7,$8::jsonb,$9,$10,'open',$11,$12,$13,$12,$13)
           on conflict (id) do nothing`,
          [row.id, owner.owner, owner.account, owner.wallet, row.intentKind, row.tokenIn, row.amountIn,
            JSON.stringify(row.spec), row.nonce, row.expires, row.txHash, row.height, tsOf(row.timestampMs)],
        );
      } else {
        await client.query(
          `update intents set status = $2, result = $3::jsonb, reason = $4, updated_height = $5, updated_ts = $6
           where id = $1`,
          [row.id, row.status, row.result === null ? null : JSON.stringify(row.result), row.reason, row.height,
            tsOf(row.timestampMs)],
        );
      }
      return;
    }

    case "one_step": {
      const owner = await resolveOwner(client, row.owner);
      await client.query(
        `insert into one_step_transfers (receipt_id, kind, owner, account, wallet, sender, token_id, amount, spec,
            tx_hash, block_height, block_ts)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9::jsonb,$10,$11,$12)
         on conflict (receipt_id) do nothing`,
        [row.receiptId, row.subkind, owner.owner, owner.account, owner.wallet, row.sender, row.tokenId, row.amount,
          JSON.stringify(row.spec), row.txHash, row.height, tsOf(row.timestampMs)],
      );
      return;
    }

    case "token_meta":
      await client.query(
        `update tokens set decimals = coalesce($3, decimals), symbol = coalesce($4, symbol)
         where kind = 'evm' and chain_id = $1 and asset = $2`,
        [row.chainId, row.asset, row.decimals, row.symbol],
      );
      return;

    default: {
      const unhandled: never = row;
      throw new Error(`unhandled row kind ${JSON.stringify(unhandled)}`);
    }
  }
}

/** Writes all decoded rows of one receipt batch (already in chain order). */
export async function writeDecoded(client: Client, decoded: Decoded): Promise<void> {
  for (const call of decoded.calls) await writeCall(client, call);
  for (const row of decoded.rows) {
    await writeOne(client, row);
  }
}

export async function setCursor(client: Client, height: number): Promise<void> {
  await client.query(
    `insert into blocks_cursor (id, last_height, updated_at) values (1, $1, now())
     on conflict (id) do update set last_height = excluded.last_height, updated_at = now()`,
    [height],
  );
}

export async function getCursor(client: Client): Promise<number | null> {
  const res = await client.query<{ last_height: string }>("select last_height from blocks_cursor where id = 1");
  const v = res.rows[0]?.last_height;
  return v === undefined ? null : Number(v);
}

