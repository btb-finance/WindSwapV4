#!/usr/bin/env bash
# Usage: ./sign.sh evm|sol RUN_INDEX
# Sends one MPC sign request as windswapv4-user.testnet and records wall-clock latency.
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
HERE="$(cd "$(dirname "$0")" && pwd)"
CASE="$1"; RUN="$2"
PAYLOAD=$(node -e 'import("./common.mjs").then(m=>console.log(m.PAYLOAD_HEX))' 2>/dev/null)
case "$CASE" in
  evm) ARGS="{\"request\":{\"path\":\"ethereum-1\",\"payload_v2\":{\"Ecdsa\":\"$PAYLOAD\"},\"domain_id\":0}}" ;;
  sol) ARGS="{\"request\":{\"path\":\"solana-1\",\"payload_v2\":{\"Eddsa\":\"$PAYLOAD\"},\"domain_id\":1}}" ;;
  *) echo "unknown case $CASE" >&2; exit 1 ;;
esac
OUT="$HERE/out/sign_${CASE}_${RUN}.txt"
START=$(python3 -c 'import time;print(time.time())')
near contract call-function as-transaction v1.signer-prod.testnet sign \
  json-args "$ARGS" prepaid-gas '100.0 Tgas' attached-deposit '1 yoctoNEAR' \
  sign-as windswapv4-user.testnet network-config testnet sign-with-legacy-keychain send \
  2>&1 | sed 's/\x1b\[[0-9;]*m//g' > "$OUT"
END=$(python3 -c 'import time;print(time.time())')
WALL=$(python3 -c "print(round($END-$START,3))")
echo "$CASE run=$RUN wall_s=$WALL"
