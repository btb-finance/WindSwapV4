import { bad, RelayError } from "./errors.js";

/**
 * Field order of each `SignedAction` variant in crates/near-contract/src/lib.rs. A client must send
 * the fields in exactly this order, because the wallet signs `JSON.stringify(action)` and the
 * contract re-serialises the parsed action in declaration order.
 */
type FieldType =
  | "u64" | "i32" | "bool" | "string"
  | "u128" | "u256" | "i256"
  | "u64[]" | "u128[]" | "string[]" | "hop[]";

export const SIGNED_ACTION_FIELDS: Readonly<Record<string, ReadonlyArray<readonly [string, FieldType]>>> = {
  Swap: [["pool_id", "u64"], ["zero_for_one", "bool"], ["amount_specified", "i256"], ["sqrt_price_limit_x96", "u256"]],
  SwapExactInput: [["path", "hop[]"], ["amount_in", "u128"], ["min_amount_out", "u128"]],
  MintPosition: [["pool_id", "u64"], ["tick_lower", "i32"], ["tick_upper", "i32"], ["liquidity", "u128"]],
  BurnPosition: [["pool_id", "u64"], ["tick_lower", "i32"], ["tick_upper", "i32"], ["liquidity", "u128"]],
  Collect: [["pool_id", "u64"], ["tick_lower", "i32"], ["tick_upper", "i32"]],
  WithdrawEvm: [["token", "string"], ["amount", "u128"], ["to_chain_id", "u64"], ["to_address", "string"]],
  CreateLock: [["amount", "u128"], ["duration", "u64"]],
  IncreaseAmount: [["token_id", "u64"], ["amount", "u128"]],
  IncreaseUnlockTime: [["token_id", "u64"], ["duration", "u64"]],
  WithdrawLock: [["token_id", "u64"]],
  Merge: [["from", "u64"], ["to", "u64"]],
  Split: [["token_id", "u64"], ["amount", "u128"]],
  LockPermanent: [["token_id", "u64"]],
  UnlockPermanent: [["token_id", "u64"]],
  Delegate: [["delegator", "u64"], ["delegatee", "u64"]],
  Vote: [["token_id", "u64"], ["pools", "u64[]"], ["weights", "u128[]"]],
  Reset: [["token_id", "u64"]],
  Poke: [["token_id", "u64"]],
  StakePosition: [["pool_id", "u64"], ["tick_lower", "i32"], ["tick_upper", "i32"]],
  UnstakePosition: [["pool_id", "u64"], ["tick_lower", "i32"], ["tick_upper", "i32"]],
  ClaimGaugeRewards: [["pool_id", "u64"], ["tick_lower", "i32"], ["tick_upper", "i32"]],
  ClaimFees: [["token_id", "u64"], ["gauges", "u64[]"]],
  ClaimBribes: [["token_id", "u64"], ["gauges", "u64[]"], ["tokens", "string[]"]],
  Bribe: [["pool_id", "u64"], ["token", "string"], ["amount", "u128"]],
  ClaimRebase: [["token_id", "u64"]],
};

const HOP_FIELDS: ReadonlyArray<readonly [string, FieldType]> = [["pool_id", "u64"], ["token_in", "string"]];

/** Upper bounds that keep a single call within the gas the relayer reserves. */
export const MAX_LIST_LENGTH = 32;
export const MAX_STRING_LENGTH = 256;

const U128_MAX = (1n << 128n) - 1n;
const U256_MAX = (1n << 256n) - 1n;
const I256_MIN = -(1n << 255n);
const I256_MAX = (1n << 255n) - 1n;
const DECIMAL = /^(0|[1-9][0-9]*)$/;
const SIGNED_DECIMAL = /^-?(0|[1-9][0-9]*)$/;

export function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Checks an action against the variant table and returns its variant name. Rejects unknown
 * variants, variants not in `allowed`, and any object whose keys are not exactly the variant's
 * fields in declaration order.
 */
export function checkAction(action: unknown, allowed: readonly string[] | "*"): string {
  if (!isPlainObject(action)) throw bad("action must be an object with exactly one variant key");
  const keys = Object.keys(action);
  if (keys.length !== 1) throw bad("action must be an object with exactly one variant key");
  const variant = keys[0] as string;
  const fields = SIGNED_ACTION_FIELDS[variant];
  if (!fields || !Object.prototype.hasOwnProperty.call(SIGNED_ACTION_FIELDS, variant)) {
    throw bad(`unknown action variant "${variant}"`);
  }
  if (allowed !== "*" && !allowed.includes(variant)) {
    throw new RelayError(403, `action variant "${variant}" is not accepted by this relayer`);
  }
  const body = action[variant];
  if (!isPlainObject(body)) throw bad(`${variant} must be an object`);
  checkFields(body, fields, variant);
  return variant;
}

function checkFields(body: Record<string, unknown>, fields: ReadonlyArray<readonly [string, FieldType]>, path: string): void {
  const want = fields.map(([name]) => name);
  const got = Object.keys(body);
  if (got.length !== want.length || got.some((k, i) => k !== want[i])) {
    throw bad(`${path} fields must be exactly [${want.join(", ")}] in that order`);
  }
  for (const [name, type] of fields) {
    checkValue(body[name], type, `${path}.${name}`);
  }
}

function checkValue(v: unknown, type: FieldType, path: string): void {
  switch (type) {
    case "u64":
      if (typeof v !== "number" || !Number.isSafeInteger(v) || v < 0) throw bad(`${path} must be a non-negative integer`);
      return;
    case "i32":
      if (typeof v !== "number" || !Number.isSafeInteger(v) || v < -2147483648 || v > 2147483647) {
        throw bad(`${path} must be a 32-bit integer`);
      }
      return;
    case "bool":
      if (typeof v !== "boolean") throw bad(`${path} must be a boolean`);
      return;
    case "string":
      checkString(v, path);
      return;
    case "u128":
      checkBig(v, DECIMAL, 0n, U128_MAX, path, "an unsigned 128-bit decimal string");
      return;
    case "u256":
      checkBig(v, DECIMAL, 0n, U256_MAX, path, "an unsigned 256-bit decimal string");
      return;
    case "i256":
      checkBig(v, SIGNED_DECIMAL, I256_MIN, I256_MAX, path, "a signed 256-bit decimal string");
      return;
    case "u64[]":
      checkList(v, path, (item, p) => checkValue(item, "u64", p));
      return;
    case "u128[]":
      checkList(v, path, (item, p) => checkValue(item, "u128", p));
      return;
    case "string[]":
      checkList(v, path, (item, p) => checkValue(item, "string", p));
      return;
    case "hop[]":
      checkList(v, path, (item, p) => {
        if (!isPlainObject(item)) throw bad(`${p} must be an object {pool_id, token_in}`);
        checkFields(item, HOP_FIELDS, p);
      });
      return;
  }
}

function checkString(v: unknown, path: string): void {
  if (typeof v !== "string" || v.length === 0 || v.length > MAX_STRING_LENGTH) {
    throw bad(`${path} must be a non-empty string of at most ${MAX_STRING_LENGTH} characters`);
  }
}

function checkBig(v: unknown, re: RegExp, min: bigint, max: bigint, path: string, what: string): void {
  if (typeof v !== "string" || !re.test(v)) throw bad(`${path} must be ${what} (a JSON string, no spaces)`);
  const n = BigInt(v);
  if (n < min || n > max) throw bad(`${path} is out of range`);
}

function checkList(v: unknown, path: string, item: (x: unknown, p: string) => void): void {
  if (!Array.isArray(v) || v.length > MAX_LIST_LENGTH) throw bad(`${path} must be an array of at most ${MAX_LIST_LENGTH} items`);
  v.forEach((x, i) => item(x, `${path}[${i}]`));
}
