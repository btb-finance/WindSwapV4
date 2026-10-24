/** Small, strict helpers for reading JSON args and return values. Throw on shape mismatch. */

export class DecodeError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DecodeError";
  }
}

export type Json = Record<string, unknown>;

export function asObject(v: unknown, what: string): Json {
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    throw new DecodeError(`${what} is not an object`);
  }
  return v as Json;
}

export function str(v: unknown, what: string): string {
  if (typeof v !== "string") throw new DecodeError(`${what} is not a string`);
  return v;
}

/** Integers arrive as JSON numbers or decimal strings; the result is validated decimal text. */
export function decimal(v: unknown, what: string): string {
  if (typeof v === "number" && Number.isSafeInteger(v)) return String(v);
  if (typeof v === "string" && /^-?\d+$/.test(v)) return v;
  throw new DecodeError(`${what} is not an integer`);
}

export function int(v: unknown, what: string): number {
  if (typeof v === "number" && Number.isSafeInteger(v)) return v;
  if (typeof v === "string" && /^-?\d+$/.test(v) && Number.isSafeInteger(Number(v))) return Number(v);
  throw new DecodeError(`${what} is not a safe integer`);
}

export function bool(v: unknown, what: string): boolean {
  if (typeof v !== "boolean") throw new DecodeError(`${what} is not a boolean`);
  return v;
}

export function arr(v: unknown, what: string): unknown[] {
  if (!Array.isArray(v)) throw new DecodeError(`${what} is not an array`);
  return v;
}

/** Parses receipt args text as JSON (empty text is an empty object). */
export function parseArgs(text: string): Json {
  if (text.trim() === "") return {};
  try {
    return asObject(JSON.parse(text), "args");
  } catch (err) {
    if (err instanceof DecodeError) throw err;
    throw new DecodeError(`args are not JSON: ${(err as Error).message}`);
  }
}

/** Return value text: JSON when it parses, otherwise the raw text. Null for an empty return. */
export function parseReturn(text: string | null): unknown {
  if (text === null) return undefined;
  if (text === "") return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

/** Absolute value of a decimal string, and its sign. */
export function splitSign(v: string): { negative: boolean; magnitude: string } {
  return v.startsWith("-") ? { negative: true, magnitude: v.slice(1) } : { negative: false, magnitude: v };
}
