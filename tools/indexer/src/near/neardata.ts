import { getJson } from "../http.js";
import { normalizeNeardataBlock } from "./normalize.js";
import type { BlockSnapshot } from "./types.js";

/**
 * neardata.xyz (FastNEAR) block API: `/v0/block/{height}` returns a full block with every shard's
 * receipt execution outcomes. `/v0/last_block/final` redirects to the latest final block.
 */
export class NeardataSource {
  constructor(
    private readonly baseUrl: string,
    private readonly receivers: ReadonlySet<string>,
  ) {}

  /** Latest final block height. */
  async finalHeight(): Promise<number> {
    const json = await getJson<{ block: { header: { height: number } } }>(
      `${this.baseUrl}/v0/last_block/final`,
      { label: "neardata last_block/final" },
    );
    return json.block.header.height;
  }

  /** Block at `height`, restricted to receipts executed on the receivers. */
  async block(height: number): Promise<BlockSnapshot> {
    const json = await getJson<unknown>(`${this.baseUrl}/v0/block/${height}`, {
      label: `neardata block ${height}`,
      attempts: 10,
    });
    return normalizeNeardataBlock(json, this.receivers);
  }
}
