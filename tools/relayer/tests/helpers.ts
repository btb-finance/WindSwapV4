import { parseRelay, type EvmChainConfig, type RelayerConfig } from "../src/config.js";
import { txHashOf } from "../src/evm.js";
import type {
  AssetInfo,
  BlockTx,
  EvmChainInfo,
  EvmReader,
  Hex,
  IntentRecord,
  Logger,
  NearReader,
  NearWriter,
  RawLog,
  WithdrawalRecord,
} from "../src/types.js";

export const VAULT: Hex = "0xaa817ca355475d22d5a410f401dc877ec401259e";
export const OTHER: Hex = "0x1111111111111111111111111111111111111111";
export const SENDER: Hex = "0x2222222222222222222222222222222222222222";
export const USDC: Hex = "0x036cbd53842c5426634e7929541ec2318f3dcf7e";
export const UNREGISTERED: Hex = "0x3333333333333333333333333333333333333333";
export const CHAIN_ID = 84532;

export const silentLog: Logger = { info() {}, warn() {}, error() {} };

export function recordingLog(): Logger & { errors: string[]; warns: string[] } {
  const errors: string[] = [];
  const warns: string[] = [];
  return {
    errors,
    warns,
    info() {},
    warn(msg) {
      warns.push(msg);
    },
    error(msg) {
      errors.push(msg);
    },
  };
}

export function chainCfg(overrides: Partial<EvmChainConfig> = {}): EvmChainConfig {
  return {
    name: "base-sepolia-test",
    chainId: CHAIN_ID,
    rpcUrl: "http://unused",
    vault: VAULT,
    confirmations: 2,
    startBlock: 100,
    scanBatchSize: 5,
    requestsPerSecond: 1000,
    ...overrides,
  };
}

export function relayerCfg(stateDir: string, chain: EvmChainConfig, fallbackAssets: AssetInfo[] = []): RelayerConfig {
  return {
    near: {
      rpcUrl: "http://unused",
      contractId: "windswapv4-btb.testnet",
      relayerAccountId: "relayer.testnet",
      credentialsPath: "/unused",
      gasTgas: { report: 50, process: 200, mark: 30, execute: 300, submitIntent: 300, cancelIntent: 300 },
    },
    evm: [chain],
    pollIntervalMs: 1,
    stateDir,
    withdrawalLimit: 20,
    fallbackAssets,
    relay: parseRelay(undefined, {}),
  };
}

export const NATIVE_ASSET_INFO: AssetInfo = {
  chain_id: CHAIN_ID,
  asset: "native",
  token_id: "eth.testnet",
  decimals: 18,
  per_deposit_cap: "1000000000000000000",
  daily_withdraw_cap: "10000000000000000000",
};

export const USDC_ASSET_INFO: AssetInfo = {
  chain_id: CHAIN_ID,
  asset: USDC,
  token_id: "usdc.testnet",
  decimals: 6,
  per_deposit_cap: "1000000000",
  daily_withdraw_cap: "10000000000",
};

export function block(n: bigint, txs: Array<Partial<BlockTx> & { hash: Hex }>): BlockTx[] {
  return txs.map((t, i) => ({
    from: SENDER,
    to: VAULT,
    value: 0n,
    transactionIndex: i,
    blockNumber: n,
    ...t,
  }));
}

export function transferLog(args: {
  token: Hex;
  to?: Hex;
  from?: Hex;
  value: bigint;
  blockNumber: bigint;
  txHash: Hex;
  logIndex: number;
}): RawLog {
  const pad = (a: string) => ("0x" + "0".repeat(24) + a.slice(2).toLowerCase()) as Hex;
  return {
    address: args.token,
    topics: [
      "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
      pad(args.from ?? SENDER),
      pad(args.to ?? VAULT),
    ],
    data: ("0x" + args.value.toString(16).padStart(64, "0")) as Hex,
    transactionHash: args.txHash,
    transactionIndex: 0,
    logIndex: args.logIndex,
    blockNumber: args.blockNumber,
  };
}

export interface MockReaderOpts {
  head: bigint;
  blocks?: Record<string, BlockTx[]>;
  receipts?: Record<string, "success" | "reverted">;
  logs?: RawLog[];
  pendingNonce?: number;
  knownHashes?: Set<string>;
  sendError?: (raw: Hex) => Error | null;
  blockError?: Error;
  baseFee?: bigint;
  priorityFee?: bigint;
}

export interface MockReader extends EvmReader {
  sent: Hex[];
  blocksFetched: bigint[];
}

export function mockReader(opts: MockReaderOpts): MockReader {
  const sent: Hex[] = [];
  const blocksFetched: bigint[] = [];
  const known = opts.knownHashes ?? new Set<string>();
  return {
    sent,
    blocksFetched,
    async blockNumber() {
      return opts.head;
    },
    async blockTransactions(n) {
      blocksFetched.push(n);
      if (opts.blockError) throw opts.blockError;
      return opts.blocks?.[n.toString()] ?? [];
    },
    async receiptStatus(hash) {
      return opts.receipts?.[hash] ?? "success";
    },
    async transferLogs(from, to) {
      return (opts.logs ?? []).filter((l) => l.blockNumber >= from && l.blockNumber <= to);
    },
    async nonce(_addr, tag) {
      if (tag === "pending") return opts.pendingNonce ?? 0;
      return opts.pendingNonce ?? 0;
    },
    async feeData() {
      return { baseFeePerGas: opts.baseFee ?? 1_000_000_000n, priorityFeePerGas: opts.priorityFee ?? 1_000_000_000n };
    },
    async hasTransaction(hash) {
      return known.has(hash);
    },
    async sendRawTransaction(raw) {
      const err = opts.sendError?.(raw);
      if (err) throw err;
      const hash = txHashOf(raw);
      sent.push(raw);
      known.add(hash);
      return hash;
    },
  };
}

export interface MockNear extends NearReader {
  assets: AssetInfo[] | Error;
  chainInfo: EvmChainInfo | null | Error;
  pending: WithdrawalRecord[];
  signed: WithdrawalRecord[];
  statuses: Record<number, WithdrawalRecord["status"]>;
  /** Intents returned by get_intents_of_owner for any owner. */
  intents: IntentRecord[];
  /** Owner id per wallet id (`evm:0x...`); unknown wallets resolve to OWNER_HEX. */
  owners: Record<string, string>;
  resolvedWallets: string[];
}

export const OWNER_HEX = "cd".repeat(32);

export function mockNear(overrides: Partial<MockNear> = {}): MockNear {
  const near: MockNear = {
    assets: [NATIVE_ASSET_INFO, USDC_ASSET_INFO],
    chainInfo: { chain_id: CHAIN_ID, path: "ethereum-1", next_nonce: 0, max_fee_per_gas_cap: "100000000000" },
    pending: [],
    signed: [],
    statuses: {},
    intents: [],
    owners: {},
    resolvedWallets: [],
    ...overrides,
    async getEvmAssets() {
      if (near.assets instanceof Error) throw near.assets;
      return near.assets;
    },
    async getEvmChain() {
      if (near.chainInfo instanceof Error) throw near.chainInfo;
      return near.chainInfo;
    },
    async getPendingWithdrawals() {
      return near.pending;
    },
    async getSignedWithdrawals() {
      return near.signed;
    },
    async getWithdrawal(id) {
      const found = [...near.pending, ...near.signed].find((w) => w.id === id);
      if (!found) return null;
      return { ...found, status: near.statuses[id] ?? found.status };
    },
    async resolveWallet(wallet) {
      near.resolvedWallets.push(wallet);
      return near.owners[wallet] ?? OWNER_HEX;
    },
    async getIntent(id) {
      return near.intents.find((i) => i.id === id) ?? null;
    },
    async getIntentsOfOwner() {
      return near.intents;
    },
  };
  return near;
}

export interface MockWriter extends NearWriter {
  reports: Array<Parameters<NearWriter["reportEvmDeposit"]>[0]>;
  processed: Array<{ id: number; max_fee_per_gas: string; max_priority_fee_per_gas: string }>;
  marked: Array<{ id: number; tx_hash: Hex }>;
}

export function mockWriter(opts: {
  reportResult?: (args: Parameters<NearWriter["reportEvmDeposit"]>[0]) => boolean | Error;
  processError?: (id: number) => Error | null;
  markError?: Error;
} = {}): MockWriter {
  const w: MockWriter = {
    reports: [],
    processed: [],
    marked: [],
    async reportEvmDeposit(args) {
      w.reports.push(args);
      const r = opts.reportResult?.(args) ?? true;
      if (r instanceof Error) throw r;
      return r;
    },
    async processWithdrawal(args) {
      const err = opts.processError?.(args.id);
      if (err) throw err;
      w.processed.push(args);
    },
    async markWithdrawalBroadcast(args) {
      if (opts.markError) throw opts.markError;
      w.marked.push(args);
    },
    async executeAction() {
      throw new Error("executeAction is not used by the watcher tests");
    },
    async submitIntent() {
      throw new Error("submitIntent is not used by the watcher tests");
    },
    async cancelIntent() {
      throw new Error("cancelIntent is not used by the watcher tests");
    },
  };
  return w;
}

export function withdrawal(overrides: Partial<WithdrawalRecord> & { id: number }): WithdrawalRecord {
  return {
    owner_hex: "0x" + "ab".repeat(20),
    token_id: "eth.testnet",
    amount: "1000",
    to_chain_id: CHAIN_ID,
    to_address: OTHER,
    status: "Pending",
    ...overrides,
  };
}
