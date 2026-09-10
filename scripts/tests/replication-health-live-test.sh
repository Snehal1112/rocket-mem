#!/usr/bin/env bash
# Live integration test for scripts/replication-health.sh: proves the parsing logic that
# replication-health-test.sh already verified against canned fixtures also works against a
# REAL rocket-mem leader and follower's actual INFO REPLICATION bytes -- whitespace, \r\n, and
# field ordering included. Plaintext, loopback only: TLS/ACL plumbing is already covered by
# the stub-based flag-plumbing test in Task 1, and exercising it live here would only add
# certificate setup, not new coverage of this script's own parsing logic.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$ROOT/scripts/replication-health.sh"
# shellcheck source=scripts/tests/lib.sh
source "$ROOT/scripts/tests/lib.sh"

echo "Building rocket-mem in release mode..." >&2
cargo build --release --workspace --manifest-path "$ROOT/Cargo.toml" >&2
BIN="$ROOT/target/release/rocket-mem"

WORK="$(mktemp -d)"
LEADER_PID="" FOLLOWER_PID="" BADFOLLOWER_PID=""
cleanup() {
  for pid in "$LEADER_PID" "$FOLLOWER_PID" "$BADFOLLOWER_PID"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
  rm -rf "$WORK"
}
trap cleanup EXIT

LEADER_PORT=17501
FOLLOWER_PORT=17502
BADFOLLOWER_PORT=17503
DEAD_PORT=17504   # nothing ever listens here

wait_for_port() {
  for _ in $(seq 1 50); do redis-cli -p "$1" ping >/dev/null 2>&1 && return 0; sleep 0.1; done
  return 1
}

ROCKET_MEM_ADDR="127.0.0.1:$LEADER_PORT" ROCKET_MEM_RMP_ADDR="127.0.0.1:0" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:0" ROCKET_MEM_AOF_PATH="$WORK/leader.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/leader.snapshot" "$BIN" --config "$WORK/no-such.toml" >"$WORK/leader.log" 2>&1 &
LEADER_PID=$!
wait_for_port "$LEADER_PORT" || { echo "leader failed to start"; exit 1; }

ROCKET_MEM_ADDR="127.0.0.1:$FOLLOWER_PORT" ROCKET_MEM_RMP_ADDR="127.0.0.1:0" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:0" ROCKET_MEM_AOF_PATH="$WORK/follower.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/follower.snapshot" "$BIN" --config "$WORK/no-such.toml" >"$WORK/follower.log" 2>&1 &
FOLLOWER_PID=$!
wait_for_port "$FOLLOWER_PORT" || { echo "follower failed to start"; exit 1; }
redis-cli -p "$FOLLOWER_PORT" replicaof 127.0.0.1 "$LEADER_PORT" >/dev/null

ROCKET_MEM_ADDR="127.0.0.1:$BADFOLLOWER_PORT" ROCKET_MEM_RMP_ADDR="127.0.0.1:0" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:0" ROCKET_MEM_AOF_PATH="$WORK/badfollower.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/badfollower.snapshot" "$BIN" --config "$WORK/no-such.toml" >"$WORK/badfollower.log" 2>&1 &
BADFOLLOWER_PID=$!
wait_for_port "$BADFOLLOWER_PORT" || { echo "bad-follower failed to start"; exit 1; }
redis-cli -p "$BADFOLLOWER_PORT" replicaof 127.0.0.1 "$DEAD_PORT" >/dev/null

# Bounded poll: wait until the leader/follower pair has actually linked up and the bad
# follower has actually noticed its target doesn't exist, instead of a fixed sleep.
deadline=$((SECONDS + 10))
while [ "$SECONDS" -lt "$deadline" ]; do
  up=$(redis-cli -p "$FOLLOWER_PORT" info replication | tr -d '\r' | grep -c 'master_link_status:up' || true)
  down=$(redis-cli -p "$BADFOLLOWER_PORT" info replication | tr -d '\r' | grep -c 'master_link_status:down' || true)
  [ "$up" = "1" ] && [ "$down" = "1" ] && break
  sleep 0.2
done

nodes_file() { local f="$WORK/nodes-$RANDOM.conf"; printf '%s\n' "$@" > "$f"; echo "$f"; }

echo "== real healthy leader+follower =="
f=$(nodes_file "leader 127.0.0.1:$LEADER_PORT leader no" "follower 127.0.0.1:$FOLLOWER_PORT follower no")
out=$("$SCRIPT" --nodes "$f" --min-good-replicas 0); rc=$?
assert_eq "$rc" "0" "a real healthy leader+follower pair exits 0"
assert_contains "$out" "role=slave master_link_status=up" "the real follower's INFO parses as up"

echo "== real down-link follower =="
f=$(nodes_file "badfollower 127.0.0.1:$BADFOLLOWER_PORT follower no")
out=$("$SCRIPT" --nodes "$f"); rc=$?
assert_eq "$rc" "1" "a real follower pointed at a dead leader exits 1"
assert_contains "$out" "master_link_status=down" "the real down-link follower is reported"

echo "== real unreachable node (process killed) =="
kill -9 "$FOLLOWER_PID"; FOLLOWER_PID=""
deadline=$((SECONDS + 5))
while [ "$SECONDS" -lt "$deadline" ] && redis-cli -p "$FOLLOWER_PORT" ping >/dev/null 2>&1; do
  sleep 0.1
done
f=$(nodes_file "gone 127.0.0.1:$FOLLOWER_PORT follower no")
out=$("$SCRIPT" --nodes "$f"); rc=$?
assert_eq "$rc" "1" "a killed node exits 1"
assert_contains "$out" "unreachable" "the killed node is reported unreachable"

report_and_exit "replication-health-live-test.sh"
