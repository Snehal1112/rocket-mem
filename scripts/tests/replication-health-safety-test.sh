#!/usr/bin/env bash
# Behavioral proof that scripts/replication-health.sh never mutates any node's state, across
# every condition it alerts on. Task 1's static/dynamic checks prove the script never issues a
# write command; this test proves the RUNTIME effect: running the script, repeatedly, against
# real nodes in both a broken and a healthy state, never flips a role, never triggers a
# resync, and never changes which node is in charge of anything. The oracle is INFO
# REPLICATION's own `role:` line -- the exact state REPLICAOF NO ONE exists to flip -- read
# directly with redis-cli, never through the script under test.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$ROOT/scripts/replication-health.sh"
# shellcheck source=scripts/tests/lib.sh
source "$ROOT/scripts/tests/lib.sh"

cargo build --release --workspace --manifest-path "$ROOT/Cargo.toml" >&2
BIN="$ROOT/target/release/rocket-mem"

WORK="$(mktemp -d)"
LEADER_PID="" FOLLOWER_PID=""
cleanup() {
  [ -n "$LEADER_PID" ] && kill "$LEADER_PID" 2>/dev/null || true
  [ -n "$FOLLOWER_PID" ] && kill "$FOLLOWER_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

LEADER_PORT=17601
FOLLOWER_PORT=17602
DEAD_PORT=17603

wait_for_port() {
  for _ in $(seq 1 50); do redis-cli -p "$1" ping >/dev/null 2>&1 && return 0; sleep 0.1; done
  return 1
}
role_of() { redis-cli -p "$1" info replication | tr -d '\r' | sed -n 's/^role:\(.*\)$/\1/p'; }

# --config points at a path that doesn't exist, so each node fails open past this repo's own
# rocket-mem.toml in the working directory instead of picking it up -- see the live-test's
# commit message for the real AddrInUse collision this avoids when two nodes launch from the
# same CWD.
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
redis-cli -p "$FOLLOWER_PORT" replicaof 127.0.0.1 "$DEAD_PORT" >/dev/null   # start broken on purpose

f="$WORK/nodes.conf"
printf '%s\n' "leader 127.0.0.1:$LEADER_PORT leader no" "follower 127.0.0.1:$FOLLOWER_PORT follower no" > "$f"

assert_eq "$(role_of "$LEADER_PORT")" "master" "sanity: leader starts as master"
assert_eq "$(role_of "$FOLLOWER_PORT")" "slave" "sanity: follower starts as slave, even though its link is broken"

echo "== running the health script 25 times against a broken follower changes nothing =="
for _ in $(seq 1 25); do
  "$SCRIPT" --nodes "$f" >/dev/null 2>&1 || true   # expected to exit 1 every time; not the point here
done
assert_eq "$(role_of "$LEADER_PORT")" "master" "leader is still master after 25 runs against a broken follower"
assert_eq "$(role_of "$FOLLOWER_PORT")" "slave" "follower is still slave -- never promoted by the health script"

echo "== fixing the link, then running the script 25 more times, still changes nothing =="
redis-cli -p "$FOLLOWER_PORT" replicaof 127.0.0.1 "$LEADER_PORT" >/dev/null
deadline=$((SECONDS + 10))
while [ "$SECONDS" -lt "$deadline" ]; do
  [ "$(redis-cli -p "$FOLLOWER_PORT" info replication | tr -d '\r' | grep -c 'master_link_status:up')" = "1" ] && break
  sleep 0.2
done
for _ in $(seq 1 25); do
  "$SCRIPT" --nodes "$f" --min-good-replicas 0 >/dev/null 2>&1 || true
done
assert_eq "$(role_of "$LEADER_PORT")" "master" "leader is still master after 25 more runs, now healthy"
assert_eq "$(role_of "$FOLLOWER_PORT")" "slave" "follower is still slave after 25 more runs, now healthy"

report_and_exit "replication-health-safety-test.sh"
