/**
 * NEP-297 event logs: `EVENT_JSON:{"standard":..,"version":..,"event":..,"data":..}`.
 *
 * The contract does not emit these yet. When it does, each parsed event becomes an `event` row (table
 * `chain_events`) and `EVENT_HANDLERS` can map `standard/event` to typed rows, which is the only place
 * that needs to change.
 */
import type { ChainEventRow, Meta, Row } from "./rows.js";

export const EVENT_PREFIX = "EVENT_JSON:";

export interface ParsedEvent {
  logIndex: number;
  standard: string;
  version: string;
  event: string;
  data: unknown;
}

export function parseEventLogs(logs: string[]): ParsedEvent[] {
  const out: ParsedEvent[] = [];
  logs.forEach((line, logIndex) => {
    if (!line.startsWith(EVENT_PREFIX)) return;
    try {
      const body = JSON.parse(line.slice(EVENT_PREFIX.length)) as Record<string, unknown>;
      if (typeof body.standard !== "string" || typeof body.event !== "string") return;
      out.push({
        logIndex,
        standard: body.standard,
        version: typeof body.version === "string" ? body.version : "",
        event: body.event,
        data: body.data ?? null,
      });
    } catch {
      // Malformed event JSON is kept out of the typed tables; the raw log is still on the call row.
    }
  });
  return out;
}

/** Typed-row handlers keyed by `standard/event`. Empty until the contract emits NEP-297 events. */
export const EVENT_HANDLERS: Record<string, (ev: ParsedEvent, meta: Meta) => Row[]> = {};

export function eventRows(logs: string[], meta: Meta): Row[] {
  const rows: Row[] = [];
  for (const ev of parseEventLogs(logs)) {
    const row: ChainEventRow = {
      kind: "event",
      ...meta,
      logIndex: ev.logIndex,
      standard: ev.standard,
      version: ev.version,
      event: ev.event,
      data: ev.data,
    };
    rows.push(row);
    const handler = EVENT_HANDLERS[`${ev.standard}/${ev.event}`];
    if (handler) rows.push(...handler(ev, meta));
  }
  return rows;
}
