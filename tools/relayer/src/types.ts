export type Hex = `0x${string}`;

/** Row returned by the contract view `get_evm_assets`. */
export interface AssetInfo {
  chain_id: number;
  /** "native" or a lowercase 0x ERC-20 address. */
  asset: string;
  token_id: string;
  decimals: number;
  per_deposit_cap: string;
  daily_withdraw_cap: string;
}

/** Result of the contract view `get_evm_chain`. */
export interface EvmChainInfo {
  chain_id: number;
  path: string;
  next_nonce: number | string;
  max_fee_per_gas_cap: string;
}

/** Result of the contract views `get_pending_withdrawals` / `get_signed_withdrawals` / `get_withdrawal`. */
export interface WithdrawalRecord {
  id: number;
  owner_hex: string;
  token_id: string;
  amount: string;
  to_chain_id: number;
  to_address: string;
  status: "Pending" | "Signed" | "Broadcast" | "Failed";
  nonce?: number | string | null;
  raw_tx?: string | null;
  tx_hash?: string | null;
}

export interface BlockTx {
  hash: Hex;
  from: Hex;
  to: Hex | null;
  value: bigint;
  transactionIndex: number;
  blockNumber: bigint;
}

export interface RawLog {
  address: Hex;
  topics: Hex[];
  data: Hex;
  transactionHash: Hex;
  transactionIndex: number;
  logIndex: number;
  blockNumber: bigint;
}

export interface DepositCandidate {
  chainId: number;
  txHash: Hex;
  /** 0 for native deposits, the block log index for ERC-20 Transfer logs. */
  logIndex: number;
  from: Hex;
  /** "native" or lowercase ERC-20 address. */
  asset: string;
  amount: bigint;
  blockNumber: bigint;
  transactionIndex: number;
}

export interface TransferEvent {
  token: Hex;
  from: Hex;
  to: Hex;
  value: bigint;
  txHash: Hex;
  logIndex: number;
  blockNumber: bigint;
  transactionIndex: number;
}

/** Read-only EVM access used by the watcher and the broadcaster. Mocked in tests. */
export interface EvmReader {
  blockNumber(): Promise<bigint>;
  blockTransactions(blockNumber: bigint): Promise<BlockTx[]>;
  receiptStatus(txHash: Hex): Promise<"success" | "reverted" | null>;
  /** All ERC-20-shaped Transfer logs with `to == vault` in the inclusive range. */
  transferLogs(fromBlock: bigint, toBlock: bigint): Promise<RawLog[]>;
  nonce(address: Hex, blockTag: "latest" | "pending"): Promise<number>;
  feeData(): Promise<{ baseFeePerGas: bigint; priorityFeePerGas: bigint }>;
  /** True if the node knows the transaction (mempool or mined). */
  hasTransaction(txHash: Hex): Promise<boolean>;
  sendRawTransaction(raw: Hex): Promise<Hex>;
}

export interface NearReader {
  getEvmAssets(): Promise<AssetInfo[]>;
  getEvmChain(chainId: number): Promise<EvmChainInfo | null>;
  getPendingWithdrawals(fromId: number, limit: number): Promise<WithdrawalRecord[]>;
  getSignedWithdrawals(fromId: number, limit: number): Promise<WithdrawalRecord[]>;
  getWithdrawal(id: number): Promise<WithdrawalRecord | null>;
}

export interface ReportDepositArgs {
  chain_id: number;
  tx_hash: Hex;
  log_index: number;
  from: Hex;
  asset: string;
  amount: string;
}

/** Arguments of the contract's `execute`, forwarded as received after relay-side checks. */
export interface ExecuteArgs {
  wallet: string;
  action: unknown;
  nonce: number;
  expires: number;
  signature: string;
}

export interface ExecuteOutcome {
  txHash: string;
  /** The contract's `ActionResult`, decoded from the success value. */
  result?: unknown;
}

/** Change calls. Only constructed outside dry-run. */
export interface NearWriter {
  /** Relays a signed action: functionCall `execute`, 0 deposit. Panics reject with the contract message. */
  executeAction(args: ExecuteArgs): Promise<ExecuteOutcome>;
  reportEvmDeposit(args: ReportDepositArgs): Promise<boolean>;
  processWithdrawal(args: { id: number; max_fee_per_gas: string; max_priority_fee_per_gas: string }): Promise<void>;
  markWithdrawalBroadcast(args: { id: number; tx_hash: Hex }): Promise<void>;
}

export interface Logger {
  info(msg: string, fields?: Record<string, unknown>): void;
  warn(msg: string, fields?: Record<string, unknown>): void;
  error(msg: string, fields?: Record<string, unknown>): void;
}
