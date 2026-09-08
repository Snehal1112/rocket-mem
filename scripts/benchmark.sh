#!/usr/bin/env bash
# Head-to-head redis-benchmark run: real Redis vs rocket-mem, identical workloads and
# identical durability settings. Writes a plain-text summary to stdout; the committed report
# in docs/benchmarks/ is assembled from this output.
set -euo pipefail

for bin in redis-server redis-benchmark; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "error: '$bin' is not on PATH. Install a Redis distribution first" >&2
    echo "       (Debian/Ubuntu: apt install redis-server redis-tools)" >&2
    exit 1
  fi
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REDIS_PORT=7777
ROCKET_PORT=7778

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

# Same durability on both sides: rocket-mem always runs an AOF with an everysec fsync policy,
# so benchmarking against a Redis with persistence off would flatter it.
redis-server --port "$REDIS_PORT" --save '' --appendonly yes --appendfsync everysec \
  --dir "$WORK" >"$WORK/redis.log" 2>&1 &
REDIS_PID=$!

ROCKET_MEM_ADDR="127.0.0.1:$ROCKET_PORT" \
ROCKET_MEM_AOF_PATH="$WORK/rocket.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/rocket.snapshot" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:9178" \
ROCKET_MEM_RMP_ADDR="127.0.0.1:9179" \
  "$ROOT/target/release/rocket-mem" --config "$WORK/unused.toml" >"$WORK/rocket.log" 2>&1 &
ROCKET_PID=$!

sleep 1
redis-cli -p "$REDIS_PORT" ping >/dev/null
redis-cli -p "$ROCKET_PORT" ping >/dev/null

echo "redis-server:  $(redis-cli -p "$REDIS_PORT" info server | grep -i '^redis_version' | tr -d '\r')"
echo "rocket-mem:    $(redis-cli -p "$ROCKET_PORT" info server | grep -i '^redis_version' | tr -d '\r')"
echo "host:          $(uname -srm)"
echo "date:          $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo

# -q prints one "COMMAND: N requests per second" line per tested command.
run_case() { # $1=label $2=port $3=payload-bytes $4=pipeline-depth
  echo "--- $1 (payload=${3}B, pipeline=${4}) ---"
  # -r spreads writes over a keyspace instead of hammering one key. Without it every command
  # targeted a single key, which is a degenerate case: no shard parallelism on rocket-mem's side,
  # one permanently hot cache line, and a keyspace small enough to sit entirely in L1. 100k is
  # matched to -n so GET has a realistic hit rate rather than measuring mostly-misses.
  redis-benchmark -h 127.0.0.1 -p "$2" -t set,get -n 100000 -c 50 -r 100000 -d "$3" -P "$4" -q
  # Deliberately no `flushall` between cases. rocket-mem implements no FLUSHALL, so the call
  # only ever succeeded against redis-server -- harmless while both sides used one key, but with
  # -r it would leave redis-server serving GET against an emptied keyspace (all misses, no value
  # copy) while rocket-mem served hits. Letting both keyspaces persist keeps the two comparable.
  # Bounded: every case draws from the same `key:__rand_int__` namespace, so this tops out at
  # ~100k keys at the largest payload rather than growing per case.
}

for payload in 3 1024; do
  for pipeline in 1 16; do
    run_case "redis-server" "$REDIS_PORT" "$payload" "$pipeline"
    run_case "rocket-mem" "$ROCKET_PORT" "$payload" "$pipeline"
    echo
  done
done

echo "--- rocket-mem /metrics sample after the run ---"
curl -s "http://127.0.0.1:9178/metrics" | grep -E '^rocket_mem_(commands_total|keys|memory_used_bytes)' || true
