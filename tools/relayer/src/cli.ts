#!/usr/bin/env node
import { formatUnits } from "viem";
import { loadConfig } from "./config.js";
import { consoleLogger } from "./log.js";
import { createNearReader, createNearWriter } from "./near.js";
import { buildReaders, runOnce } from "./runner.js";
import type { AssetInfo } from "./types.js";

const USAGE = `Usage: relayer <run|once> [--config <path>] [--dry-run] [--from-block <n>] [--to-block <n>]

  run        loop forever: deposits + withdrawals every pollIntervalMs
  once       single pass, then exit (code 1 if any stage failed)
  --dry-run  no NEAR or EVM writes, no state writes; prints what would happen
  --from-block / --to-block   override the scan range for this run
  --config   default: config/relayer.json (see config/relayer.example.json)
`;

interface Args {
  command: "run" | "once";
  configPath: string;
  dryRun: boolean;
  fromBlock?: bigint;
  toBlock?: bigint;
}

function parseArgs(argv: string[]): Args {
  const [command, ...rest] = argv;
  if (command !== "run" && command !== "once") {
    throw new Error(USAGE);
  }
  const args: Args = { command, configPath: "config/relayer.json", dryRun: false };
  for (let i = 0; i < rest.length; i++) {
    const flag = rest[i];
    const value = () => {
      const v = rest[++i];
      if (v === undefined) throw new Error(`${flag} needs a value`);
      return v;
    };
    switch (flag) {
      case "--config":
        args.configPath = value();
        break;
      case "--dry-run":
        args.dryRun = true;
        break;
      case "--from-block":
        args.fromBlock = BigInt(value());
        break;
      case "--to-block":
        args.toBlock = BigInt(value());
        break;
      default:
        throw new Error(`unknown argument ${flag}\n${USAGE}`);
    }
  }
  return args;
}

async function main(): Promise<number> {
  const args = parseArgs(process.argv.slice(2));
  const config = loadConfig(args.configPath);
  const log = consoleLogger;
  const near = createNearReader(config.near);
  const writer = args.dryRun ? null : createNearWriter(config.near);
  const readers = buildReaders(config);

  if (args.dryRun) log.warn("DRY RUN: no NEAR or EVM writes, no state writes");

  let stopping = false;
  const shouldStop = () => stopping;
  for (const sig of ["SIGINT", "SIGTERM"] as const) {
    process.on(sig, () => {
      log.info(`${sig} received, stopping after the current batch`);
      stopping = true;
    });
  }

  const deps = { config, near, writer, readers, dryRun: args.dryRun, log, shouldStop };
  const opts = { fromBlock: args.fromBlock, toBlock: args.toBlock };

  if (args.command === "once") {
    const summary = await runOnce(deps, opts);
    printSummary(summary.deposits.flatMap((d) => d.found.map((f) => ({ ...f, chain: d.chain }))), config.fallbackAssets, args.dryRun);
    if (summary.errors.length > 0) {
      for (const e of summary.errors) log.error(e);
      return 1;
    }
    return 0;
  }

  while (!stopping) {
    const summary = await runOnce(deps, opts);
    for (const e of summary.errors) log.error(e);
    if (stopping) break;
    await new Promise((r) => setTimeout(r, config.pollIntervalMs));
  }
  return 0;
}

function printSummary(
  found: Array<{ chain: string; txHash: string; logIndex: number; from: string; asset: string; amount: bigint; blockNumber: bigint }>,
  fallback: AssetInfo[],
  dryRun: boolean,
): void {
  if (!dryRun) return;
  console.log(`\nDetected ${found.length} deposit(s) to the vault:`);
  for (const f of found) {
    const decimals = fallback.find((a) => a.asset.toLowerCase() === f.asset)?.decimals ?? 18;
    console.log(
      `  block ${f.blockNumber} ${f.chain} tx ${f.txHash} logIndex ${f.logIndex} from ${f.from} ${f.asset} ${formatUnits(f.amount, decimals)}`,
    );
  }
}

main().then(
  (code) => process.exit(code),
  (err) => {
    consoleLogger.error("fatal", { error: String(err) });
    process.exit(1);
  },
);
