#!/usr/bin/env bash
# Starts a throwaway PostgreSQL cluster in .local-pg/ (Homebrew binaries), creates windswap_test,
# runs the given command with TEST_DATABASE_URL set, then stops the cluster.
#   PG_PORT (default 55432) must be free. Data is removed with: rm -rf .local-pg
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$ROOT/.local-pg"
PORT="${PG_PORT:-55432}"
mkdir -p "$DIR"
if [ ! -f "$DIR/data/PG_VERSION" ]; then
  initdb -D "$DIR/data" -U postgres --auth=trust >/dev/null
fi
pg_ctl -D "$DIR/data" -o "-p $PORT -k $DIR -c listen_addresses=127.0.0.1" -l "$DIR/server.log" -w start >/dev/null
trap 'pg_ctl -D "$DIR/data" -m fast stop >/dev/null 2>&1 || true' EXIT
psql -h 127.0.0.1 -p "$PORT" -U postgres -qc "select 1" >/dev/null
psql -h 127.0.0.1 -p "$PORT" -U postgres -tAc "select 1 from pg_database where datname = 'windswap_test'" | grep -q 1 \
  || psql -h 127.0.0.1 -p "$PORT" -U postgres -qc "create database windswap_test"
export TEST_DATABASE_URL="postgres://postgres@127.0.0.1:$PORT/windswap_test"
cd "$ROOT"
"$@"
