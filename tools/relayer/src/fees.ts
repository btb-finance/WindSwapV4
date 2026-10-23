export class FeeCapError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "FeeCapError";
  }
}

export interface FeeInputs {
  baseFeePerGas: bigint;
  priorityFeePerGas: bigint;
  /** Contract cap for max_fee_per_gas (from get_evm_chain). */
  capPerGas: bigint;
}

/**
 * EIP-1559 fee policy:
 *   maxFee      = min(2 * baseFee + priority, cap)
 *   maxPriority = min(priority, maxFee)
 * Throws if the cap is below the current base fee, since such a transaction could never be included.
 */
export function computeFees({ baseFeePerGas, priorityFeePerGas, capPerGas }: FeeInputs): {
  maxFeePerGas: bigint;
  maxPriorityFeePerGas: bigint;
} {
  if (baseFeePerGas < 0n || priorityFeePerGas < 0n || capPerGas <= 0n) {
    throw new FeeCapError("fee inputs must be non-negative and cap must be positive");
  }
  const uncapped = 2n * baseFeePerGas + priorityFeePerGas;
  const maxFeePerGas = uncapped < capPerGas ? uncapped : capPerGas;
  if (maxFeePerGas < baseFeePerGas) {
    throw new FeeCapError(`fee cap ${capPerGas} is below base fee ${baseFeePerGas}`);
  }
  const maxPriorityFeePerGas = priorityFeePerGas < maxFeePerGas ? priorityFeePerGas : maxFeePerGas;
  return { maxFeePerGas, maxPriorityFeePerGas };
}
