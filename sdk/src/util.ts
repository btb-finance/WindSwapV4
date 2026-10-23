/** Integer helpers shared by the math and client modules. All values are exact BigInt. */

export function bitLength(x: bigint): number {
  return x === 0n ? 0 : x.toString(2).length;
}

/** Floor of the square root of a non-negative bigint (Newton iteration). */
export function isqrt(n: bigint): bigint {
  if (n < 0n) throw new RangeError("isqrt of negative number");
  if (n < 2n) return n;
  // Starts at or above sqrt(n), so the iteration decreases monotonically to floor(sqrt(n)).
  let x = 1n << BigInt(Math.ceil(bitLength(n) / 2));
  for (;;) {
    const y = (x + n / x) >> 1n;
    if (y >= x) return x;
    x = y;
  }
}

/** Accepts a bigint, a safe-integer number, or a decimal integer string. */
export function toBigInt(value: bigint | number | string, name: string): bigint {
  if (typeof value === "bigint") return value;
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value)) {
      throw new RangeError(`${name} must be a safe integer, got ${value}`);
    }
    return BigInt(value);
  }
  if (/^-?\d+$/.test(value)) return BigInt(value);
  throw new TypeError(`${name} must be an integer, got "${value}"`);
}

export function assertInteger(value: number, name: string): void {
  if (!Number.isSafeInteger(value)) {
    throw new RangeError(`${name} must be a safe integer, got ${value}`);
  }
}
