import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import type pg from "pg";

const here = dirname(fileURLToPath(import.meta.url));
/** `db/` sits next to `src/` in the package (and next to `dist/` once built). */
export const SQL_DIR = join(here, "..", "..", "db");

/** Applies schema.sql then views.sql. Both are idempotent. */
export async function migrate(client: pg.Client | pg.PoolClient): Promise<void> {
  for (const file of ["schema.sql", "views.sql"]) {
    const sql = await readFile(join(SQL_DIR, file), "utf8");
    await client.query(sql);
  }
}
