/**
 * Computes what each Wind Swap v4 pool (and its gauge and NFT manager) holds per token on NEAR
 * testnet, and the `seed_pool_reserves` entries that make the contract's solvency totals match.
 *
 * Read-only: NEAR view calls, NEP-141 `ft_balance_of`, and Base Sepolia `eth_getBalance` /
 * `eth_call`. It sends no transaction.
 *
 * Method (see README.md for the reasoning):
 *   1. Per pool: every owner position (direct), and the NFT manager's pool position for each
 *      staked range, valued at the current price with the engine's rounding (round up).
 *      Plus the owed amounts (tokens_owed) of all of those positions.
 *   2. Cross-check on the liquidity: active liquidity of the direct positions vs `pool.liquidity`,
 *      active staked liquidity of the NFT manager vs `pool.staked_liquidity`.
 *   3. Cross-check on the tokens (NEP-141 only, and the vault for EVM assets): held balance minus
 *      liabilities minus locked minus pending minus the pool-side total from step 1 = residual.
 *      The residual is the gauge fees, unpoked LP fees, and rounding dust that step 1 cannot see.
 *      For NEP-141 tokens it is allocated to the pools that hold the token, in proportion to their
 *      step-1 amounts. For EVM assets it is reported and NOT allocated (see README.md).
 *   4. The seed amount is `holdings - recorded`, where `recorded` is what the contract already
 *      recorded for the pool since the v7 migration (`get_pool_reserves`). It is zero today.
 */
import { createHash } from "node:crypto";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  amount0Delta,
  getTickAtSqrtRatio,
  positionAmounts,
} from "./math.ts";
import {
  BASE_RPC,
  NEAR_RPC,
  erc20Balance,
  ftBalanceOf,
  ftDecimals,
  nativeBalance,
  view,
  ViewError,
} from "./rpc.ts";

// ---- configuration (public testnet identifiers, no secrets) ----

const CONTRACT = "windswapv4-btb.testnet";
/** The WIND token. Its gauge reward balances are the only WIND the pools hold. */
const WIND = "windswapv4-wind.testnet";
/** Contract-derived Base Sepolia vault (`ethereum-1`), from docs/testnet.md v3. */
const VAULT = (process.env.VAULT ?? "0xaa817ca355475d22d5a410f401dc877ec401259e").toLowerCase();
const BASE_CHAIN_ID = 84532;

/** NEAR accounts whose owner id is sha256(account). `btb` is also the team owner. */
const NEAR_OWNERS = [
  { label: "user (windswapv4-user.testnet)", account: "windswapv4-user.testnet" },
  { label: "btb (windswapv4-btb.testnet)", account: "windswapv4-btb.testnet" },
  { label: "relayer (windswapv4-relayer.testnet)", account: "windswapv4-relayer.testnet" },
];
/** Wallets with no NEAR account: owner id is sha256(canonical wallet string). Testnet keys only. */
const WALLET_OWNERS = [
  { label: "hardhat#1 EVM", wallet: "evm:0x70997970c51812dc3a010c7d01b50e0d17dc79c8" },
  { label: "fresh EVM", wallet: "evm:0x0d53794e20e0e4f96e07bcd8487ce557c6d9a4ac" },
];

const sha256Hex = (s: string) => createHash("sha256").update(s).digest("hex");

/** Engine `system_id`: `sha256("windswap-system:" + parts.join(":"))`. */
const systemId = (...parts: (string | number)[]) =>
  sha256Hex(`windswap-system:${parts.join(":")}`);

const NFT_MANAGER = systemId("nft-manager");

// ---- helpers ----

const WEI_PER_TOKEN = (dec: number) => 10n ** BigInt(dec);

function human(wei: bigint, dec: number): string {
  const neg = wei < 0n;
  const abs = neg ? -wei : wei;
  const base = WEI_PER_TOKEN(dec);
  const whole = abs / base;
  const frac = (abs % base).toString().padStart(dec, "0").slice(0, 8).replace(/0+$/, "");
  return `${neg ? "-" : ""}${whole}${frac ? "." + frac : ""}`;
}

function allocate(total: bigint, weights: bigint[]): bigint[] {
  const sum = weights.reduce((a, b) => a + b, 0n);
  if (weights.length === 0) return [];
  if (sum === 0n) throw new Error("allocate: zero total weight");
  const shares = weights.map((w) => (total * w) / sum);
  const assigned = shares.reduce((a, b) => a + b, 0n);
  // The rounding remainder (a few wei) goes to the heaviest entry, so the shares sum to total exactly.
  let heaviest = 0;
  weights.forEach((w, i) => {
    if (w > weights[heaviest]) heaviest = i;
  });
  shares[heaviest] += total - assigned;
  return shares;
}

function table(headers: string[], rows: string[][]): string {
  const widths = headers.map((h, i) => Math.max(h.length, ...rows.map((r) => (r[i] ?? "").length)));
  const line = (cells: string[]) => cells.map((c, i) => c.padEnd(widths[i])).join("  ");
  return [line(headers), line(widths.map((w) => "-".repeat(w))), ...rows.map(line)].join("\n");
}

function section(title: string): void {
  console.log(`\n== ${title} ==`);
}

// ---- data types ----

interface PoolState {
  id: number;
  token0: string;
  token1: string;
  tick: number;
  sqrt: bigint;
  liquidity: bigint;
  stakedLiquidity: bigint;
}

interface OwnerPosition {
  owner: string;
  ownerLabel: string;
  pool: number;
  lo: number;
  hi: number;
  staked: boolean;
  liquidity: bigint;
  owed0: bigint;
  owed1: bigint;
}

/** A pool position held by the protocol: the gauge (staked NFTs) or the NFT manager. */
interface HolderPosition {
  label: string;
  liquidity: bigint;
  owed0: bigint;
  owed1: bigint;
}

interface RangeHoldings {
  pool: number;
  lo: number;
  hi: number;
  /** Direct positions of the owners in this range (non-staked). */
  direct: OwnerPosition[];
  /** Pool positions of the gauge (staked NFTs, `holder_of` in gauge.rs) and of the NFT manager. */
  holders: HolderPosition[];
  /** Sum of the staked NFT liquidity the owners report for this range (get_positions_of_owner). */
  stakedByOwners: bigint;
}

interface PoolTokenRow {
  pool: number;
  token: string;
  positions: bigint; // amounts the positions hold (direct + NFT manager), rounded up
  owed: bigint; // tokens_owed of all those positions
  terms: number; // number of rounded-up amounts summed (bounds the rounding dust)
  residualShare: bigint;
  holdings: bigint; // positions + owed + residualShare
  recorded: bigint; // get_pool_reserves before seeding (post-migration deltas)
  seed: bigint; // holdings - recorded: the amount seed_pool_reserves will add
  allocated: boolean;
}

// ---- owner set ----

interface Owner {
  label: string;
  hex: string;
  /** true for positions owners (users and wallets), false for protocol-internal system owners. */
  user: boolean;
}

// ---- main ----

async function main(): Promise<number> {
  const errors: string[] = [];
  const warnings: string[] = [];
  console.log(`NEAR RPC ${NEAR_RPC}`);
  console.log(`Base RPC ${BASE_RPC}`);
  console.log(`contract ${CONTRACT}   NFT manager owner ${NFT_MANAGER}`);

  // -- pools and gauges --
  const pools: PoolState[] = [];
  const tokensOf = new Map<number, [string, string]>();
  for (let id = 0; ; id++) {
    let p: any;
    try {
      p = await view(CONTRACT, "get_pool", { pool_id: id });
    } catch (e) {
      if (e instanceof ViewError) break; // first missing pool id ends the list (ids are contiguous)
      throw e;
    }
    const state: PoolState = {
      id,
      token0: p.token0,
      token1: p.token1,
      tick: p.slot0.tick,
      sqrt: BigInt(p.slot0.sqrt_price_x96),
      liquidity: BigInt(p.liquidity),
      stakedLiquidity: BigInt(p.staked_liquidity),
    };
    // Sanity: our tick math agrees with the engine's current tick.
    const recomputed = getTickAtSqrtRatio(state.sqrt);
    if (recomputed !== state.tick) {
      errors.push(`pool ${id}: tick math mismatch (slot0.tick ${state.tick}, recomputed ${recomputed})`);
    }
    pools.push(state);
    tokensOf.set(id, [state.token0, state.token1]);
  }
  if (pools.length === 0) throw new Error("no pools found");

  const gauges: any[] = await view(CONTRACT, "get_gauges", {});
  const evmAssets: any[] = await view(CONTRACT, "get_evm_assets", {});
  const evmByToken = new Map<string, any>(evmAssets.map((a) => [a.token_id, a]));

  // -- owners --
  const owners: Owner[] = [
    ...NEAR_OWNERS.map((o) => ({ label: o.label, hex: sha256Hex(o.account), user: true })),
    ...WALLET_OWNERS.map((o) => ({ label: o.label, hex: sha256Hex(o.wallet), user: true })),
  ];
  const system: Owner[] = [
    { label: "voter", hex: systemId("voter"), user: false },
    { label: "minter", hex: systemId("minter"), user: false },
    { label: "distributor", hex: systemId("distributor"), user: false },
    { label: "nft-manager", hex: NFT_MANAGER, user: false },
  ];
  for (const g of gauges) {
    system.push({ label: `gauge account pool ${g.pool_id}`, hex: systemId("gauge", g.pool_id), user: false });
    system.push({ label: `fees voting reward gauge ${g.gauge_id}`, hex: systemId("fees", g.gauge_id), user: false });
    system.push({ label: `bribe gauge ${g.gauge_id}`, hex: systemId("bribe", g.gauge_id), user: false });
  }
  const allOwners = [...owners, ...system];

  // -- positions of user owners --
  const direct: OwnerPosition[] = [];
  const stakedRanges = new Map<string, { pool: number; lo: number; hi: number }>();
  // Owners' staked NFT liquidity, summed per range (their staked views report the NFT liquidity).
  const stakedNft = new Map<string, bigint>();
  for (const o of owners) {
    for (let from = 0; ; ) {
      const page: any[] = await view(CONTRACT, "get_positions_of_owner", {
        owner: o.hex,
        from_index: from,
        limit: 100,
      });
      for (const p of page) {
        const pos: OwnerPosition = {
          owner: o.hex,
          ownerLabel: o.label,
          pool: p.pool_id,
          lo: p.tick_lower,
          hi: p.tick_upper,
          staked: p.staked,
          liquidity: BigInt(p.liquidity),
          owed0: BigInt(p.tokens_owed0),
          owed1: BigInt(p.tokens_owed1),
        };
        if (p.staked) {
          const k = `${p.pool_id}:${p.tick_lower}:${p.tick_upper}`;
          stakedRanges.set(k, { pool: p.pool_id, lo: p.tick_lower, hi: p.tick_upper });
          stakedNft.set(k, (stakedNft.get(k) ?? 0n) + BigInt(p.liquidity));
        } else {
          direct.push(pos);
        }
      }
      if (page.length < 100) break;
      from += page.length;
    }
  }

  // -- per-range holdings: direct positions and the NFT manager's pool position --
  // Staked NFT liquidity: the gauge's pool position (systemId("gauge", pool), the gauge account).
  // Unstaked NFT liquidity would sit in the NFT manager's pool position; none is expected.
  const ranges: RangeHoldings[] = [];
  for (const pool of pools) {
    const inPool = direct.filter((d) => d.pool === pool.id);
    const keys = new Set(inPool.map((d) => `${d.lo}:${d.hi}`));
    const staked = [...stakedRanges.values()].filter((r) => r.pool === pool.id);
    for (const r of staked) keys.add(`${r.lo}:${r.hi}`);
    const gaugeAccount = systemId("gauge", pool.id);
    const gaugeView = gauges.find((g) => g.pool_id === pool.id);
    if (gaugeView && gaugeView.gauge_account !== gaugeAccount) {
      errors.push(`pool ${pool.id}: gauge account ${gaugeView.gauge_account} is not systemId(gauge, pool)`);
    }
    for (const key of keys) {
      const [lo, hi] = key.split(":").map(Number);
      const holders: HolderPosition[] = [];
      for (const [label, owner] of [["gauge", gaugeAccount], ["nft-manager", NFT_MANAGER]] as const) {
        const p: any = await view(CONTRACT, "get_position_of_owner", {
          owner,
          pool_id: pool.id,
          tick_lower: lo,
          tick_upper: hi,
        });
        holders.push({
          label,
          liquidity: BigInt(p.liquidity),
          owed0: BigInt(p.tokens_owed0),
          owed1: BigInt(p.tokens_owed1),
        });
      }
      const stakedByOwners = staked
        .filter((s) => s.lo === lo && s.hi === hi)
        .reduce((a, s) => a + stakedNft.get(`${s.pool}:${s.lo}:${s.hi}`)!, 0n);
      ranges.push({ pool: pool.id, lo, hi, direct: inPool.filter((d) => d.lo === lo && d.hi === hi), holders, stakedByOwners });
    }
  }

  // -- liquidity cross-checks per pool --
  section("Pools: state and liquidity cross-check");
  const poolRows: string[][] = [];
  for (const pool of pools) {
    const active = (lo: number, hi: number) => pool.tick >= lo && pool.tick < hi;
    const inRange = ranges.filter((r) => r.pool === pool.id && active(r.lo, r.hi));
    const directActive = inRange.reduce((a, r) => a + r.direct.reduce((b, d) => b + d.liquidity, 0n), 0n);
    const holderActive = inRange.reduce((a, r) => a + r.holders.reduce((b, h) => b + h.liquidity, 0n), 0n);
    const stakedByOwners = inRange.reduce((a, r) => a + r.stakedByOwners, 0n);
    // pool.liquidity counts all in-range liquidity, staked or not (pool 4 is all staked).
    const liqOk = directActive + holderActive === pool.liquidity;
    const stakedOk = holderActive === pool.stakedLiquidity && stakedByOwners === holderActive;
    if (!liqOk) errors.push(`pool ${pool.id}: active liquidity ${directActive + holderActive} != pool.liquidity ${pool.liquidity}`);
    if (!stakedOk) errors.push(`pool ${pool.id}: gauge/NFT liquidity ${holderActive} (owners report ${stakedByOwners}) != pool.staked_liquidity ${pool.stakedLiquidity}`);
    poolRows.push([
      String(pool.id),
      `${pool.token0} / ${pool.token1}`,
      String(pool.tick),
      pool.sqrt.toString(),
      pool.liquidity.toString(),
      directActive.toString(),
      liqOk ? "ok" : "MISMATCH",
      pool.stakedLiquidity.toString(),
      holderActive.toString(),
      stakedOk ? "ok" : "MISMATCH",
    ]);
  }
  console.log(
    table(
      ["pool", "tokens", "tick", "sqrtPriceX96", "pool.liquidity", "Σ direct active", "check", "pool.staked", "Σ gauge+NFT active", "check"],
      poolRows,
    ),
  );

  // -- pool-side holdings (step 1) --
  // Per pool and token: positions (rounded up), owed, and the term count for the dust bound.
  const rows = new Map<string, PoolTokenRow>();
  const key = (pool: number, token: string) => `${pool}|${token}`;
  for (const pool of pools) {
    for (const token of [pool.token0, pool.token1]) {
      rows.set(key(pool.id, token), {
        pool: pool.id,
        token,
        positions: 0n,
        owed: 0n,
        terms: 0,
        residualShare: 0n,
        holdings: 0n,
        recorded: 0n,
        seed: 0n,
        allocated: false,
      });
    }
  }
  for (const r of ranges) {
    const pool = pools.find((p) => p.id === r.pool)!;
    const add = (token: string, amount: bigint, isOwed: boolean) => {
      const row = rows.get(key(pool.id, token))!;
      if (isOwed) row.owed += amount;
      else {
        row.positions += amount;
        row.terms += 1;
      }
    };
    // Direct positions: each owner's position is valued on its own (its own rounding).
    for (const d of r.direct) {
      const a = positionAmounts(d.liquidity, r.lo, r.hi, pool.tick, pool.sqrt, true);
      add(pool.token0, a.amount0, false);
      add(pool.token1, a.amount1, false);
      add(pool.token0, d.owed0, true);
      add(pool.token1, d.owed1, true);
    }
    // Gauge and NFT manager pool positions: one position each, valued once.
    for (const h of r.holders) {
      const a = positionAmounts(h.liquidity, r.lo, r.hi, pool.tick, pool.sqrt, true);
      add(pool.token0, a.amount0, false);
      add(pool.token1, a.amount1, false);
      add(pool.token0, h.owed0, true);
      add(pool.token1, h.owed1, true);
    }
  }

  // -- recorded post-migration deltas (what the contract already holds in pool_reserves) --
  for (const pool of pools) {
    const rec: [string, string][] = await view(CONTRACT, "get_pool_reserves", { pool_id: pool.id });
    for (const [token, amount] of rec) {
      const row = rows.get(key(pool.id, token));
      if (!row) errors.push(`pool ${pool.id}: recorded reserve for unknown token ${token}`);
      else row.recorded = BigInt(amount);
    }
  }

  // -- contract accounting per token --
  const accounting: any[] = await view(CONTRACT, "get_all_tokens_accounting", {});
  const acc = new Map<string, any>(accounting.map((a) => [a.token, a]));

  // -- tokens to reconcile --
  const tokenSet = new Set<string>([
    ...acc.keys(),
    ...[...rows.values()].map((r) => r.token),
    WIND,
    ...evmAssets.map((a) => a.token_id),
  ]);

  // -- balances (liabilities): every known owner x every known token --
  // get_balances_of_owner is not used: its token list is an index that starts empty at v7, so the
  // balances credited before v7 (for example the bribe pool, credited by bribe_for) are missing from it.
  const liabilities = new Map<string, bigint>();
  const balanceRows: { owner: string; label: string; token: string; amount: bigint }[] = [];
  for (const o of allOwners) {
    for (const token of tokenSet) {
      const amount = BigInt(await view<string>(CONTRACT, "get_balance_of_owner", { owner: o.hex, token }));
      if (amount === 0n) continue;
      liabilities.set(token, (liabilities.get(token) ?? 0n) + amount);
      balanceRows.push({ owner: o.hex, label: o.label, token, amount });
    }
  }
  // Tokens held as balances that no pool or accounting view names (none expected).
  for (const token of liabilities.keys()) tokenSet.add(token);

  // -- step 2: token cross-check --
  section("Tokens: held vs owed (cross-check)");
  const tokenRows: string[][] = [];
  const reconcile: {
    token: string;
    kind: string;
    held: bigint | null;
    liab: bigint;
    locked: bigint;
    pending: bigint;
    poolHeld: bigint;
    residual: bigint | null;
    bound: bigint;
    decimals: number;
    status: string;
  }[] = [];

  for (const token of tokenSet) {
    const tokenRowsForPools = [...rows.values()].filter((r) => r.token === token);
    const poolHeld = tokenRowsForPools.reduce((a, r) => a + r.positions + r.owed, 0n);
    const termCount = tokenRowsForPools.reduce((a, r) => a + r.terms, 0);
    const bound = BigInt(2 * termCount + 2);
    const a = acc.get(token);
    const locked = a ? BigInt(a.locked) : 0n;
    const pending = a ? BigInt(a.pending_withdrawals) : 0n;
    const liab = liabilities.get(token) ?? 0n;

    const evm = evmByToken.get(token);
    let held: bigint | null = null;
    let decimals = 18;
    let kind: string;
    if (evm) {
      kind = "EVM vault";
      decimals = evm.decimals;
      if (evm.chain_id !== BASE_CHAIN_ID) {
        errors.push(`${token}: chain ${evm.chain_id} is not checked by this tool`);
      }
      try {
        held = evm.asset === "native" ? await nativeBalance(VAULT) : await erc20Balance(evm.asset, VAULT);
      } catch (e) {
        errors.push(`${token}: vault balance read failed: ${String(e)}`);
      }
    } else if (token === WIND) {
      kind = "NEP-141 (WIND)";
      held = await ftBalanceOf(token, CONTRACT);
      decimals = (await ftDecimals(token)) ?? 18;
    } else {
      kind = "NEP-141";
      held = await ftBalanceOf(token, CONTRACT);
      if (held === null) kind = "no token contract (internal id)";
      decimals = (await ftDecimals(token)) ?? 18;
    }

    const residual = held === null ? null : held - liab - locked - pending - poolHeld;
    reconcile.push({ token, kind, held, liab, locked, pending, poolHeld, residual, bound, decimals, status: "" });
    tokenRows.push([
      token,
      kind,
      held === null ? "n/a" : human(held, decimals),
      human(liab, decimals),
      human(a ? BigInt(a.liabilities) : 0n, decimals),
      human(locked, decimals),
      human(pending, decimals),
      human(poolHeld, decimals),
      residual === null ? "n/a" : `${human(residual, decimals)} (${residual} wei)`,
    ]);
  }
  console.log(
    table(["token", "kind", "held", "liabilities (sum of balances)", "liabilities (on-chain view)", "locked", "pending", "pool side (positions+owed)", "residual = pool-side gap"], tokenRows),
  );

  // -- step 3: residual allocation --
  const gaugeWind = gauges.reduce((a, g) => a + BigInt(g.reward_balance), 0n);
  for (const rc of reconcile) {
    const isEvm = evmByToken.has(rc.token);
    const poolTokenRows = [...rows.values()].filter((r) => r.token === rc.token);
    if (rc.residual === null) {
      rc.status = "not checked (no token contract): entries = positions only";
      poolTokenRows.forEach((r) => (r.holdings = r.positions + r.owed));
      continue;
    }
    if (isEvm) {
      rc.status = "EVM: residual NOT allocated; see README (gas and untracked funding)";
      poolTokenRows.forEach((r) => (r.holdings = r.positions + r.owed));
      if (rc.residual < -rc.bound) warnings.push(`${rc.token}: vault holds ${human(-rc.residual, rc.decimals)} less than the pool-side plus liabilities; owed > held`);
      if (rc.residual > rc.bound) warnings.push(`${rc.token}: vault holds ${human(rc.residual, rc.decimals)} more than the contract tracks (untracked funding or unbooked deposit)`);
      continue;
    }
    if (rc.token === WIND && poolTokenRows.length === 0) {
      // WIND reaches pools only as gauge reward balances, which seed_pool_reserves cannot name.
      if (rc.residual !== gaugeWind && (rc.residual > gaugeWind + rc.bound || rc.residual < gaugeWind - rc.bound)) {
        errors.push(`WIND residual ${rc.residual} does not equal the gauge reward balances ${gaugeWind}`);
        rc.status = "ERROR: WIND residual unexplained";
      } else {
        rc.status = `WIND held by gauges (${gaugeWind} wei): not seedable, no pool holds WIND as a token`;
      }
      continue;
    }
    if (rc.residual < -rc.bound) {
      errors.push(`${rc.token}: held is short of the owed side by ${-rc.residual} wei (beyond the ${rc.bound} wei dust bound)`);
      rc.status = "ERROR: shortfall";
      poolTokenRows.forEach((r) => (r.holdings = r.positions + r.owed));
      continue;
    }
    if (poolTokenRows.length === 0) {
      rc.status = `surplus of ${rc.residual} wei with no pool holding ${rc.token}: not seeded`;
      warnings.push(rc.status);
      continue;
    }
    // Positive residual (or small negative dust): split across the pools that hold the token.
    // Weight by the pool-side amount; if every pool is zero, split equally.
    const weights = poolTokenRows.map((r) => (poolHeldOf(r) > 0n ? poolHeldOf(r) : 1n));
    const shares = allocate(rc.residual, weights);
    poolTokenRows.forEach((r, i) => {
      r.residualShare = shares[i];
      r.holdings = r.positions + r.owed + shares[i];
      r.allocated = true;
    });
    rc.status = `residual allocated to ${poolTokenRows.length} pool(s)`;
  }
  function poolHeldOf(r: PoolTokenRow): bigint {
    return r.positions + r.owed;
  }

  // -- step 4: seed amounts and the final table --
  section("Pool reserves: holdings and seed amounts");
  const finalRows: string[][] = [];
  const entries: [number, string, string][] = [];
  for (const pool of pools) {
    for (const token of tokensOf.get(pool.id)!) {
      const r = rows.get(key(pool.id, token))!;
      const rc = reconcile.find((x) => x.token === token)!;
      r.seed = r.holdings - r.recorded;
      if (r.seed < 0n) errors.push(`pool ${pool.id} ${token}: seed amount would be negative (${r.seed})`);
      finalRows.push([
        String(pool.id),
        token,
        human(r.positions, rc.decimals),
        human(r.owed, rc.decimals),
        "n/a (not exposed)",
        r.allocated ? human(r.residualShare, rc.decimals) : "0",
        human(r.holdings, rc.decimals),
        human(r.recorded, rc.decimals),
        r.seed.toString(),
      ]);
      entries.push([pool.id, token, r.seed.toString()]);
    }
  }
  console.log(
    table(
      ["pool", "token", "positions", "owed", "gauge fees", "residual share", "holdings", "recorded", "seed (wei)"],
      finalRows,
    ),
  );
  for (const rc of reconcile) {
    if (rc.status) console.log(`  ${rc.token}: ${rc.status}`);
  }

  // -- outputs --
  const accountingEntries = balanceRows.map((b) => [b.owner, b.token]);
  const outDir = join(dirname(fileURLToPath(import.meta.url)), "..", "out");
  section("Results");
  for (const w of warnings) console.log(`WARNING: ${w}`);
  for (const e of errors) console.log(`ERROR: ${e}`);
  if (errors.length > 0) {
    console.log("\nNo entries written: fix the errors above first.");
    return 1;
  }
  writeFileSync(join(outDir, "seed-entries.json"), JSON.stringify(entries, null, 2) + "\n");
  writeFileSync(join(outDir, "accounting-entries.json"), JSON.stringify(accountingEntries, null, 2) + "\n");
  console.log(`wrote ${join(outDir, "seed-entries.json")}`);
  console.log(`wrote ${join(outDir, "accounting-entries.json")}`);
  console.log("\nseed_pool_reserves entries (wei):");
  console.log(JSON.stringify(entries));
  console.log("\nseed_accounting entries (owner, token) with nonzero balance:");
  console.log(JSON.stringify(accountingEntries));
  return 0;
}

main().then(
  (code) => process.exit(code),
  (e) => {
    console.error(e);
    process.exit(2);
  },
);
