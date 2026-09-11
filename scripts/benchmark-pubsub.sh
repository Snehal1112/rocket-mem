#!/usr/bin/env bash
# PUBLISH throughput: redis-benchmark supports an arbitrary trailing command directly, unlike
# MULTI/EXEC (which needed scripts/benchmark-transactions.sh's custom RESP client because
# redis-benchmark's -t flag has no multi-command-transaction mode). No subscriber is attached
# here -- this measures PUBLISH's own dispatch overhead (registry lookup, zero matches, the
# replication broadcast), not delivery latency to a subscriber, which the two-connection
# integration test in this same plan already covers functionally.
set -euo pipefail

for bin in redis-server redis-benchmark; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "error: '$bin' is not on PATH. Install a Redis distribution first" >&2
    exit 1
  fi
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REDIS_PORT=7797
ROCKET_PORT=7798
N=100000

echo "Building rocket-mem in release mode..." >&2
cargo build --release --workspace --manifest-path "$ROOT/Cargo.toml" >&2

WORK="$(mktemp -d)"
REDIS_PID=""
ROCKET_PID=""
cleanup() {
  [ -n "$REDIS_PID" ] && kill "$REDIS_PID" 2>/dev/null || true
  [ -n "$ROCKET_PID" ] && kill "$ROCKET_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

redis-server --port "$REDIS_PORT" --save '' --appendonly yes --appendfsync everysec \
  --dir "$WORK" >"$WORK/redis.log" 2>&1 &
REDIS_PID=$!

ROCKET_MEM_ADDR="127.0.0.1:$ROCKET_PORT" \
ROCKET_MEM_AOF_PATH="$WORK/rocket.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/rocket.snapshot" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:9189" \
ROCKET_MEM_RMP_ADDR="127.0.0.1:9190" \
  "$ROOT/target/release/rocket-mem" --config "$WORK/unused.toml" >"$WORK/rocket.log" 2>&1 &
ROCKET_PID=$!

sleep 1
redis-cli -p "$REDIS_PORT" ping >/dev/null
redis-cli -p "$ROCKET_PORT" ping >/dev/null

echo "--- PUBLISH throughput ($N requests, no attached subscriber) ---"
echo -n "redis-server: "
redis-benchmark -p "$REDIS_PORT" -n "$N" -q PUBLISH news hello
echo -n "rocket-mem: "
redis-benchmark -p "$ROCKET_PORT" -n "$N" -q PUBLISH news hello
