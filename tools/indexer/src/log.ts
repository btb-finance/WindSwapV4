import type { Logger } from "./indexer.js";

export const consoleLogger: Logger = {
  info: (msg) => console.log(`${new Date().toISOString()} ${msg}`),
  warn: (msg) => console.warn(`${new Date().toISOString()} WARN ${msg}`),
};
