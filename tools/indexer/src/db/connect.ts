import pg from "pg";

/**
 * Connection pool from DATABASE_URL (a standard Postgres URL, e.g. Supabase's session or direct string).
 * DATABASE_SSL=true enables TLS without certificate verification (Supabase pooler certificates).
 */
export function createPool(databaseUrl: string, env: NodeJS.ProcessEnv = process.env): pg.Pool {
  const ssl = env.DATABASE_SSL === "true" ? { rejectUnauthorized: false } : undefined;
  return new pg.Pool({ connectionString: databaseUrl, max: 8, ssl });
}
