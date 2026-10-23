import type { Logger } from "./types.js";

function fmt(level: string, msg: string, fields?: Record<string, unknown>): string {
  const extra = fields
    ? " " + JSON.stringify(fields, (_k, v) => (typeof v === "bigint" ? v.toString() : v))
    : "";
  return `${new Date().toISOString()} ${level.padEnd(5)} ${msg}${extra}`;
}

export const consoleLogger: Logger = {
  info: (msg, fields) => console.log(fmt("info", msg, fields)),
  warn: (msg, fields) => console.warn(fmt("warn", msg, fields)),
  error: (msg, fields) => console.error(fmt("error", msg, fields)),
};

export const silentLogger: Logger = { info() {}, warn() {}, error() {} };
