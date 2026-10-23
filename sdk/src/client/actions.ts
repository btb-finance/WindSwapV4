/**
 * Builders for the contract's change methods. Each returns a plain object that any NEAR wallet
 * library can sign as a FunctionCall action (near-api-js `functionCall(methodName, args, gas,
 * deposit)`, wallet-selector, MyNearWallet, etc.). The SDK never signs or sends anything.
 *
 * `args` is the exact JSON object the contract deserializes (snake_case keys, decimal strings
 * for U128/U256/I256). `gas` and `deposit` are decimal strings in gas units and yoctoNEAR.
 */
import { toBigInt } from "../util.js";

/** 300 TGas. Unused gas is refunded. */
export const DEFAULT_GAS = "300000000000000";
/** 1 yoctoNEAR. Required by NEP-141 `ft_transfer_call` and by `withdraw`. */
export const ONE_YOCTO = "1";
/** Zero deposit. */
export const ZERO_DEPOSIT = "0";

export interface FunctionCallAction {
  /** Account that receives the call. The DEX for most methods, the token contract for `deposit`. */
  receiverId: string;
  methodName: string;
  /** JSON-serializable arguments, exactly as the contract expects them. */
  args: Record<string, unknown>;
  /** Gas in gas units, decimal string. */
  gas: string;
  /** yoctoNEAR, decimal string. */
  deposit: string;
}

export interface CallOptions {
  /** Override the gas limit (bigint, or decimal string in gas units). Default: 300 TGas. */
  gas?: bigint | string;
}

const U128_MAX = (1n << 128n) - 1n;
const I256_LIMIT = 1n << 255n;

function gasArg(opts?: CallOptions): string {
  if (opts?.gas === undefined) return DEFAULT_GAS;
  const gas = toBigInt(opts.gas, "gas");
  if (gas <= 0n) throw new RangeError("gas must be positive");
  return gas.toString();
}

/** Non-negative U128 as a decimal string. */
function u128Arg(value: bigint | string | number, name: string): string {
  const v = toBigInt(value, name);
  if (v < 0n || v > U128_MAX) throw new RangeError(`${name} must fit in u128, got ${v}`);
  return v.toString();
}

/** Signed I256 as a decimal string. */
function i256Arg(value: bigint | string | number, name: string): string {
  const v = toBigInt(value, name);
  if (v <= -I256_LIMIT || v >= I256_LIMIT) throw new RangeError(`${name} must fit in i256`);
  return v.toString();
}

/** Non-negative U256 as a decimal string. */
function u256Arg(value: bigint | string | number, name: string): string {
  const v = toBigInt(value, name);
  if (v < 0n || v >= 1n << 256n) throw new RangeError(`${name} must fit in u256`);
  return v.toString();
}

function i32Arg(value: number, name: string): number {
  if (!Number.isInteger(value) || value < -(2 ** 31) || value >= 2 ** 31) {
    throw new RangeError(`${name} must be an i32, got ${value}`);
  }
  return value;
}

function u64Arg(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new RangeError(`${name} must be a non-negative safe integer, got ${value}`);
  }
  return value;
}

function u32Arg(value: number, name: string): number {
  if (!Number.isInteger(value) || value < 0 || value > 0xffffffff) {
    throw new RangeError(`${name} must be a u32, got ${value}`);
  }
  return value;
}

export interface HopArg {
  poolId: number;
  tokenIn: string;
}

function pathArg(path: HopArg[]): { pool_id: number; token_in: string }[] {
  if (path.length === 0) throw new RangeError("path must not be empty");
  return path.map((hop) => ({ pool_id: u64Arg(hop.poolId, "poolId"), token_in: hop.tokenIn }));
}

/**
 * Builds the change-method actions for a deployed Wind Swap v4 contract.
 * `contractId` is the DEX account, for example `windswapv4-btb.testnet`.
 */
export function buildActions(contractId: string) {
  const call = (
    methodName: string,
    args: Record<string, unknown>,
    opts?: CallOptions,
    receiverId: string = contractId,
    deposit: string = ZERO_DEPOSIT,
  ): FunctionCallAction => ({
    receiverId,
    methodName,
    args,
    gas: gasArg(opts),
    deposit,
  });

  return {
    /** Owner only. Creates a pool. `tickSpacing` must be enabled; `sqrtPriceX96` is the initial price. */
    createPool(
      p: { token0: string; token1: string; tickSpacing: number; sqrtPriceX96: bigint | string },
      opts?: CallOptions,
    ): FunctionCallAction {
      return call(
        "create_pool",
        {
          token0: p.token0,
          token1: p.token1,
          tick_spacing: i32Arg(p.tickSpacing, "tickSpacing"),
          sqrt_price_x96: u256Arg(p.sqrtPriceX96, "sqrtPriceX96"),
        },
        opts,
      );
    },

    /** Owner only. Whitelists a token for deposits. */
    addToken(p: { token: string }, opts?: CallOptions): FunctionCallAction {
      return call("add_token", { token: p.token }, opts);
    },

    /** Owner only. Removes a token from the deposit whitelist. */
    removeToken(p: { token: string }, opts?: CallOptions): FunctionCallAction {
      return call("remove_token", { token: p.token }, opts);
    },

    /**
     * Deposit: `ft_transfer_call` on the token contract, to the DEX, with 1 yocto. The token
     * credits the DEX internal balance through `ft_on_transfer`. `msg` is empty for a plain deposit.
     */
    deposit(
      p: { token: string; amount: bigint | string; msg?: string },
      opts?: CallOptions,
    ): FunctionCallAction {
      return call(
        "ft_transfer_call",
        {
          receiver_id: contractId,
          amount: u128Arg(p.amount, "amount"),
          msg: p.msg ?? "",
        },
        opts,
        p.token,
        ONE_YOCTO,
      );
    },

    /** Withdraw from the internal balance to the caller's token account. Requires 1 yocto. */
    withdraw(p: { token: string; amount: bigint | string }, opts?: CallOptions): FunctionCallAction {
      return call(
        "withdraw",
        { token: p.token, amount: u128Arg(p.amount, "amount") },
        opts,
        contractId,
        ONE_YOCTO,
      );
    },

    /** Add liquidity for the caller. Debits internal balances. */
    mintPosition(
      p: { poolId: number; tickLower: number; tickUpper: number; liquidity: bigint | string },
      opts?: CallOptions,
    ): FunctionCallAction {
      return call(
        "mint_position",
        {
          pool_id: u64Arg(p.poolId, "poolId"),
          tick_lower: i32Arg(p.tickLower, "tickLower"),
          tick_upper: i32Arg(p.tickUpper, "tickUpper"),
          liquidity: u128Arg(p.liquidity, "liquidity"),
        },
        opts,
      );
    },

    /** Remove liquidity. The amounts become owed to the position; call `collect` to pay them out. */
    burnPosition(
      p: { poolId: number; tickLower: number; tickUpper: number; liquidity: bigint | string },
      opts?: CallOptions,
    ): FunctionCallAction {
      return call(
        "burn_position",
        {
          pool_id: u64Arg(p.poolId, "poolId"),
          tick_lower: i32Arg(p.tickLower, "tickLower"),
          tick_upper: i32Arg(p.tickUpper, "tickUpper"),
          liquidity: u128Arg(p.liquidity, "liquidity"),
        },
        opts,
      );
    },

    /** Collect everything owed to the caller's position into internal balances. */
    collect(p: { poolId: number; tickLower: number; tickUpper: number }, opts?: CallOptions): FunctionCallAction {
      return call(
        "collect",
        {
          pool_id: u64Arg(p.poolId, "poolId"),
          tick_lower: i32Arg(p.tickLower, "tickLower"),
          tick_upper: i32Arg(p.tickUpper, "tickUpper"),
        },
        opts,
      );
    },

    /**
     * Single-pool swap at the engine level. `amountSpecified > 0` is exact input, `< 0` is exact
     * output. `sqrtPriceLimitX96` bounds the price move (see `defaultSqrtPriceLimit`).
     */
    swap(
      p: {
        poolId: number;
        zeroForOne: boolean;
        amountSpecified: bigint | string;
        sqrtPriceLimitX96: bigint | string;
      },
      opts?: CallOptions,
    ): FunctionCallAction {
      return call(
        "swap",
        {
          pool_id: u64Arg(p.poolId, "poolId"),
          zero_for_one: p.zeroForOne,
          amount_specified: i256Arg(p.amountSpecified, "amountSpecified"),
          sqrt_price_limit_x96: u256Arg(p.sqrtPriceLimitX96, "sqrtPriceLimitX96"),
        },
        opts,
      );
    },

    /**
     * Multi-hop exact-input swap. Spends exactly `amountIn` of the first hop's input token and
     * panics if the output is below `minAmountOut`.
     */
    swapExactInput(
      p: { path: HopArg[]; amountIn: bigint | string; minAmountOut: bigint | string },
      opts?: CallOptions,
    ): FunctionCallAction {
      return call(
        "swap_exact_input",
        {
          path: pathArg(p.path),
          amount_in: u128Arg(p.amountIn, "amountIn"),
          min_amount_out: u128Arg(p.minAmountOut, "minAmountOut"),
        },
        opts,
      );
    },

    /**
     * Multi-hop exact-output swap. Credits exactly `amountOut` and panics if the input needed
     * is above `maxAmountIn`.
     */
    swapExactOutput(
      p: { path: HopArg[]; amountOut: bigint | string; maxAmountIn: bigint | string },
      opts?: CallOptions,
    ): FunctionCallAction {
      return call(
        "swap_exact_output",
        {
          path: pathArg(p.path),
          amount_out: u128Arg(p.amountOut, "amountOut"),
          max_amount_in: u128Arg(p.maxAmountIn, "maxAmountIn"),
        },
        opts,
      );
    },

    /** Owner only. Enables a tick spacing with its swap fee (hundredths of a bip). */
    enableTickSpacing(p: { tickSpacing: number; fee: number }, opts?: CallOptions): FunctionCallAction {
      return call(
        "enable_tick_spacing",
        { tick_spacing: i32Arg(p.tickSpacing, "tickSpacing"), fee: u32Arg(p.fee, "fee") },
        opts,
      );
    },

    /** Owner only. Sets a pool's swap fee. `0` clears it, `420` means 0%. */
    setCustomSwapFee(p: { poolId: number; fee: number }, opts?: CallOptions): FunctionCallAction {
      return call(
        "set_custom_swap_fee",
        { pool_id: u64Arg(p.poolId, "poolId"), fee: u32Arg(p.fee, "fee") },
        opts,
      );
    },

    /** Owner only. Sets a pool's unstaked fee. No effect on swaps yet (no gauges). */
    setCustomUnstakedFee(p: { poolId: number; fee: number }, opts?: CallOptions): FunctionCallAction {
      return call(
        "set_custom_unstaked_fee",
        { pool_id: u64Arg(p.poolId, "poolId"), fee: u32Arg(p.fee, "fee") },
        opts,
      );
    },

    /** Owner only. Adds or removes an account from the zero-swap-fee whitelist. */
    setFeeWhitelist(p: { accountId: string; whitelisted: boolean }, opts?: CallOptions): FunctionCallAction {
      return call("set_fee_whitelist", { account_id: p.accountId, whitelisted: p.whitelisted }, opts);
    },

    /** Owner only, testnet builds only. Credits an internal balance directly (faucet). */
    testCredit(
      p: { accountId: string; token: string; amount: bigint | string },
      opts?: CallOptions,
    ): FunctionCallAction {
      return call(
        "test_credit",
        { account_id: p.accountId, token: p.token, amount: u128Arg(p.amount, "amount") },
        opts,
      );
    },
  };
}

export type WindSwapActions = ReturnType<typeof buildActions>;
