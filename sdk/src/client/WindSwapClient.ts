import type { HopInput, PoolView, PositionView } from "../types.js";
import { toBigInt } from "../util.js";
import { buildActions } from "./actions.js";
import { callView, DEFAULT_CONTRACT_ID, DEFAULT_RPC_URL } from "./rpc.js";

export interface WindSwapClientOptions {
  /** The DEX account. Default: `windswapv4-btb.testnet`. */
  contractId?: string;
  /** NEAR JSON-RPC endpoint. Default: `https://rpc.testnet.near.org`. */
  rpcUrl?: string;
  /** Custom fetch, e.g. for tests or a proxy. Default: global `fetch`. */
  fetch?: typeof fetch;
}

/**
 * Read-only client for Wind Swap v4 (view methods over JSON-RPC) plus `actions`, the change
 * method builders. The client never signs or sends transactions.
 *
 * Views return the contract's JSON as-is (see `types.ts`). U128 values are decimal strings.
 * Convert them with `BigInt(x)` when you need arithmetic.
 */
export class WindSwapClient {
  readonly contractId: string;
  readonly rpcUrl: string;
  /** Builders for change methods (`ft_transfer_call` deposits, swaps, liquidity, admin). */
  readonly actions: ReturnType<typeof buildActions>;
  private readonly fetchImpl: typeof fetch;

  constructor(options: WindSwapClientOptions = {}) {
    this.contractId = options.contractId ?? DEFAULT_CONTRACT_ID;
    this.rpcUrl = options.rpcUrl ?? DEFAULT_RPC_URL;
    this.fetchImpl = options.fetch ?? ((...args) => globalThis.fetch(...args));
    this.actions = buildActions(this.contractId);
  }

  /** Calls any view method by name. Prefer the typed methods below. */
  view<T>(method: string, args: Record<string, unknown> = {}): Promise<T> {
    return callView<T>(this.rpcUrl, this.contractId, method, args, this.fetchImpl);
  }

  /** Pool state, including `slot0.sqrt_price_x96` and `slot0.tick`. */
  getPool(poolId: number): Promise<PoolView> {
    return this.view<PoolView>("get_pool", { pool_id: poolId });
  }

  /** Position of `accountId` in `[tickLower, tickUpper)`. */
  getPosition(poolId: number, accountId: string, tickLower: number, tickUpper: number): Promise<PositionView> {
    return this.view<PositionView>("get_position", {
      pool_id: poolId,
      account_id: accountId,
      tick_lower: tickLower,
      tick_upper: tickUpper,
    });
  }

  /** Internal DEX balance of `accountId` for `token`, as a decimal string. */
  getBalance(accountId: string, token: string): Promise<string> {
    return this.view<string>("get_balance", { account_id: accountId, token });
  }

  /** Tokens whitelisted for deposit. */
  getTokens(): Promise<string[]> {
    return this.view<string[]>("get_tokens");
  }

  /** Swap fee `accountId` pays on `poolId`, in hundredths of a bip (300 = 0.03%). */
  getFee(poolId: number, accountId: string): Promise<number> {
    return this.view<number>("get_fee", { pool_id: poolId, account_id: accountId });
  }

  /** Output of an exact-input route. Uses the non-whitelisted fee. Returns a decimal string. */
  quoteExactInput(path: HopInput[], amountIn: bigint | string | number): Promise<string> {
    return this.view<string>("quote_exact_input", {
      path,
      amount_in: toBigInt(amountIn, "amountIn").toString(),
    });
  }

  /** Input an exact-output route would cost. Uses the non-whitelisted fee. Returns a decimal string. */
  quoteExactOutput(path: HopInput[], amountOut: bigint | string | number): Promise<string> {
    return this.view<string>("quote_exact_output", {
      path,
      amount_out: toBigInt(amountOut, "amountOut").toString(),
    });
  }
}
