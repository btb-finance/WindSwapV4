#!/usr/bin/env bash
# Prints a per-table fingerprint (row count + md5 of the sorted rows without surrogate ids) for two databases.
# Usage: scripts/compare-dbs.sh <url-a> <url-b>. Used to cross-check the block and account read paths.
set -euo pipefail
# gauges and pool_state are view-backed (refreshed from contract views), so they are not compared.
TABLES="calls pools swaps liquidity_events positions deposits withdrawals tokens lock_events votes bribes claims gauge_events epochs wallet_links chain_events settings"
fp() {
  for t in $TABLES; do
    local extra=""
    [ "$t" = "pools" ] && extra="- 'fee'"
    local h
    h=$(psql "$1" -X -tAc "select coalesce(md5(string_agg(j, '|' order by j)), 'empty'), count(*) from (select (to_jsonb(x) - 'id' $extra)::text as j from $t x) s" | tr '|' ' ')
    printf "%-16s %s\n" "$t" "$h"
  done
}
diff <(fp "$1") <(fp "$2") && echo "IDENTICAL"
