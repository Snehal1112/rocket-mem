#!/usr/bin/env bash
# Sprint 8 chaos test: random kill -9 loop against a leader+follower, verified against an
# independent load-generator write log. Not a CI test -- see
# ../docs/superpowers/specs/2026-08-31-sprint-8-spec.md's chaos-test decision for why.
set -uo pipefail

ITERATIONS="${1:-200}"
DURATION_SECS="${2:-1800}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKDIR="$(mktemp -d)"
LEADER_DIR="$WORKDIR/leader"
FOLLOWER_DIR="$WORKDIR/follower"
mkdir -p "$LEADER_DIR" "$FOLLOWER_DIR"

LEADER_PORT=17001
FOLLOWER_PORT=17002
LOAD_LOG="$WORKDIR/load.log"
CHAOS_LOG="$WORKDIR/chaos-events.log"

# LEADER_PORT/FOLLOWER_PORT are fixed, so two chaos.sh runs started at once would silently
# fight over the same ports: each run's kill/restart loop can hand the other run's load
# generator a write that lands in *this* run's AOF (or vice versa), producing spurious
# MISMATCH reports at verification time that look like a replication bug but are really just
# two harnesses stepping on each other. Take a flock on a fixed file before touching any
# ports, so a second concurrent invocation fails fast instead of corrupting both runs. The
# lock is tied to fd 9 and releases automatically when this process exits or is killed, so it
# can't go stale like a manual pidfile.
LOCK_FILE="/tmp/rocket-mem-chaos.lock"
exec 9>"$LOCK_FILE"
if ! flock -n 9; then
  echo "chaos.sh: another chaos.sh run already holds $LOCK_FILE (ports $LEADER_PORT/$FOLLOWER_PORT are fixed, so only one run at a time). Aborting." >&2
  exit 1
fi

echo "chaos.sh: workdir=$WORKDIR iterations=$ITERATIONS duration=${DURATION_SECS}s"

cargo build --release --bin rocket-mem -p rocket-mem --manifest-path "$ROOT/Cargo.toml"
cargo build --release --example chaos_load -p rocket-mem --manifest-path "$ROOT/Cargo.toml"

BIN="$ROOT/target/release/rocket-mem"
LOAD_BIN="$ROOT/target/release/examples/chaos_load"

start_leader() {
  ROCKET_MEM_ADDR="127.0.0.1:$LEADER_PORT" \
  ROCKET_MEM_RMP_ADDR="127.0.0.1:0" \
  ROCKET_MEM_METRICS_ADDR="127.0.0.1:0" \
  ROCKET_MEM_AOF_PATH="$LEADER_DIR/appendonly.aof" \
  ROCKET_MEM_SNAPSHOT_PATH="$LEADER_DIR/dump.snapshot" \
  "$BIN" >>"$WORKDIR/leader.out" 2>&1 &
  echo $!
}

start_follower() {
  ROCKET_MEM_ADDR="127.0.0.1:$FOLLOWER_PORT" \
  ROCKET_MEM_RMP_ADDR="127.0.0.1:0" \
  ROCKET_MEM_METRICS_ADDR="127.0.0.1:0" \
  ROCKET_MEM_AOF_PATH="$FOLLOWER_DIR/appendonly.aof" \
  ROCKET_MEM_SNAPSHOT_PATH="$FOLLOWER_DIR/dump.snapshot" \
  "$BIN" >>"$WORKDIR/follower.out" 2>&1 &
  echo $!
}

wait_for_port() {
  local port="$1"
  for _ in $(seq 1 50); do
    redis-cli -p "$port" PING >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  return 1
}

LEADER_PID=$(start_leader)
wait_for_port "$LEADER_PORT" || { echo "chaos.sh: leader failed to start"; exit 1; }

FOLLOWER_PID=$(start_follower)
wait_for_port "$FOLLOWER_PORT" || { echo "chaos.sh: follower failed to start"; exit 1; }
redis-cli -p "$FOLLOWER_PORT" REPLICAOF 127.0.0.1 "$LEADER_PORT" >/dev/null

echo "chaos.sh: leader pid=$LEADER_PID follower pid=$FOLLOWER_PID"

"$LOAD_BIN" "redis://127.0.0.1:$LEADER_PORT" "$LOAD_LOG" "$DURATION_SECS" &
LOAD_PID=$!

END_TIME=$(( $(date +%s) + DURATION_SECS ))
for i in $(seq 1 "$ITERATIONS"); do
  if [ "$(date +%s)" -ge "$END_TIME" ]; then
    echo "chaos.sh: load generator's duration elapsed, stopping the kill loop early at iteration $i"
    break
  fi
  SLEEP=$(( RANDOM % 30 ))
  sleep "$SLEEP"

  if [ $((RANDOM % 2)) -eq 0 ]; then
    TARGET_PID=$LEADER_PID; TARGET_PORT=$LEADER_PORT; TARGET_NAME="leader"
  else
    TARGET_PID=$FOLLOWER_PID; TARGET_PORT=$FOLLOWER_PORT; TARGET_NAME="follower"
  fi

  echo "chaos.sh: iteration $i: kill -9 $TARGET_NAME (pid $TARGET_PID, slept ${SLEEP}s)" | tee -a "$CHAOS_LOG"
  kill -9 "$TARGET_PID" 2>/dev/null || true
  sleep 0.2

  if [ "$TARGET_NAME" = "leader" ]; then
    LEADER_PID=$(start_leader)
    NEW_PORT=$LEADER_PORT
  else
    FOLLOWER_PID=$(start_follower)
    NEW_PORT=$FOLLOWER_PORT
  fi
  if ! wait_for_port "$NEW_PORT"; then
    echo "chaos.sh: FAIL iteration $i: $TARGET_NAME did not come back up" | tee -a "$CHAOS_LOG"
    exit 1
  fi
  if [ "$TARGET_NAME" = "follower" ]; then
    redis-cli -p "$FOLLOWER_PORT" REPLICAOF 127.0.0.1 "$LEADER_PORT" >/dev/null
  fi
done

wait "$LOAD_PID" 2>/dev/null || true
echo "chaos.sh: load generator finished, giving the follower a moment to catch up"
sleep 5

TOTAL_WRITES=$(wc -l < "$LOAD_LOG" | tr -d ' ')
echo "chaos.sh: verifying $TOTAL_WRITES writes against both leader and follower"
MISMATCHES=0
while read -r key value; do
  for PORT in "$LEADER_PORT" "$FOLLOWER_PORT"; do
    actual=$(redis-cli -p "$PORT" GET "$key")
    if [ "$actual" != "$value" ]; then
      echo "chaos.sh: MISMATCH key=$key expected=$value actual=$actual port=$PORT" | tee -a "$CHAOS_LOG"
      MISMATCHES=$((MISMATCHES + 1))
    fi
  done
done < "$LOAD_LOG"

kill -9 "$LEADER_PID" "$FOLLOWER_PID" 2>/dev/null || true

echo "chaos.sh: done. $TOTAL_WRITES keys checked against 2 nodes, $MISMATCHES mismatches."
echo "chaos.sh: workdir preserved at $WORKDIR for inspection"
if [ "$MISMATCHES" -ne 0 ]; then
  exit 1
fi
