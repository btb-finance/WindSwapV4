/**
 * Typed rows produced by the decoder. The writer maps each kind to a table. Amounts are decimal
 * strings (never JS numbers), so U128/U256 values survive intact.
 */
import type { OwnerRef } from "./owner.js";

export type Via = "direct" | "route" | "signed";

export interface Meta {
  receiptId: string;
  actionIndex: number;
  txHash: string | null;
  height: number;
  timestampMs: number;
  seq: number;
}

export interface CallRow {
  kind: "call";
  receiptId: string;
  actionIndex: number;
  txHash: string | null;
  height: number;
  timestampMs: number;
  method: string;
  decodedAs: string | null;
  predecessor: string;
  signer: string | null;
  args: unknown;
  ok: boolean | null;
  returnValue: unknown;
  error: string | null;
  logs: string[];
  deposit: string;
  decodeError: string | null;
}

export interface PoolRow extends Meta {
  kind: "pool";
  poolId: number;
  token0: string;
  token1: string;
  tickSpacing: number;
  sqrtPriceX96: string;
}

export interface SwapRow extends Meta {
  kind: "swap";
  exact: "input" | "output";
  via: Via;
  owner: OwnerRef;
  /** Pools in order. `tokenIn` is null when the pool's zero_for_one flag decides it (direct swaps). */
  path: Array<{ poolId: number; tokenIn: string | null }>;
  zeroForOne: boolean | null;
  /** Signed pool deltas: positive = paid to the pool. Single-pool swaps only. */
  amount0: string | null;
  amount1: string | null;
  amountIn: string;
  amountOut: string;
}

export interface LiquidityRow extends Meta {
  kind: "liquidity";
  subkind: "mint" | "burn" | "collect";
  via: Via;
  owner: OwnerRef;
  poolId: number;
  tickLower: number;
  tickUpper: number;
  liquidity: string | null;
  amount0: string;
  amount1: string;
}

export interface DepositRow extends Meta {
  kind: "deposit";
  subkind: "ft" | "evm";
  owner: OwnerRef;
  tokenId: string | null;
  chainId: number | null;
  asset: string | null;
  amount: string;
  sender: string | null;
  txHashEvm: string | null;
  logIndex: number | null;
  fromAddress: string | null;
  /** Set when the deposit carried a `lock` message (applied only for the WIND token). */
  lockDuration: number | null;
}

export interface WithdrawalRequestRow extends Meta {
  kind: "withdrawal_request";
  id: number;
  owner: OwnerRef;
  tokenId: string;
  amount: string;
  toChainId: number | null;
  toAddress: string;
  via: Via;
}

export interface WithdrawalUpdateRow extends Meta {
  kind: "withdrawal_update";
  id: number;
  status: "signed" | "broadcast";
  nonce: number | null;
  evmTxHash: string | null;
}

export interface TokenRow extends Meta {
  kind: "token";
  tokenId: string;
  tokenKind: "nep141" | "evm";
  chainId: number | null;
  asset: string | null;
  decimals: number | null;
  listed: boolean;
}

export interface SettingRow extends Meta {
  kind: "setting";
  key: string;
  value: string;
}

export interface LockEventRow extends Meta {
  kind: "lock_event";
  subkind: "create" | "create_from_deposit" | "increase_amount" | "increase_unlock_time" | "withdraw" | "merge" | "split" | "permanent" | "unpermanent" | "delegate";
  via: Via;
  owner: OwnerRef;
  tokenId: number | null;
  amount: string | null;
  duration: number | null;
  permanent: boolean | null;
  extra: Record<string, unknown>;
}

export interface VoteRow extends Meta {
  kind: "vote";
  subkind: "vote" | "reset" | "poke";
  via: Via;
  owner: OwnerRef;
  tokenId: number;
  poolId: number | null;
  weight: string | null;
}

export interface BribeRow extends Meta {
  kind: "bribe";
  via: Via;
  owner: OwnerRef;
  poolId: number;
  token: string;
  amount: string;
}

export interface ClaimRow extends Meta {
  kind: "claim";
  subkind: "fees" | "bribes" | "rebase" | "gauge_rewards";
  via: Via;
  owner: OwnerRef;
  tokenId: number | null;
  poolId: number | null;
  token: string | null;
  amount: string;
  extra: Record<string, unknown>;
}

export interface GaugeRow extends Meta {
  kind: "gauge";
  subkind: "create" | "stake" | "unstake";
  via: Via;
  owner: OwnerRef | null;
  poolId: number;
  gaugeId: number | null;
  tickLower: number | null;
  tickUpper: number | null;
}

export interface IntentRow extends Meta {
  kind: "intent";
  subkind: "submit" | "execute" | "cancel";
  id: number;
  owner: OwnerRef | null;
  intentKind: "swap" | "add_liquidity" | null;
  tokenIn: string | null;
  amountIn: string | null;
  spec: Record<string, unknown> | null;
  nonce: number | null;
  expires: number | null;
  status: string | null;
  result: unknown;
  reason: string | null;
}

/** A one-step NEP-141 transfer (`ft_on_transfer` with a `swap` or `add_liquidity` message) that succeeded. */
export interface OneStepRow extends Meta {
  kind: "one_step";
  subkind: "swap" | "add_liquidity";
  owner: OwnerRef;
  sender: string;
  tokenId: string;
  amount: string;
  spec: Record<string, unknown>;
}

/** Decimals and symbol reported with an EVM deposit (optional arguments of report_evm_deposit). */
export interface TokenMetaRow extends Meta {
  kind: "token_meta";
  chainId: number;
  asset: string;
  decimals: number | null;
  symbol: string | null;
}

export interface EpochRow extends Meta {
  kind: "epoch";
  amount: string;
}

/** Owner-independent calls worth keeping as typed rows: a withdrawal that was broadcast, etc. */

export interface WalletRow extends Meta {
  kind: "wallet";
  wallet: string;
  account: string | null;
}

export interface ChainEventRow extends Meta {
  kind: "event";
  logIndex: number;
  standard: string;
  version: string;
  event: string;
  data: unknown;
}

export type Row =
  | PoolRow
  | SwapRow
  | LiquidityRow
  | DepositRow
  | WithdrawalRequestRow
  | WithdrawalUpdateRow
  | TokenRow
  | SettingRow
  | LockEventRow
  | VoteRow
  | BribeRow
  | ClaimRow
  | GaugeRow
  | EpochRow
  | IntentRow
  | OneStepRow
  | TokenMetaRow
  | WalletRow
  | ChainEventRow;

export interface Decoded {
  calls: CallRow[];
  rows: Row[];
}
