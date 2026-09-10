#!/usr/bin/env bash
# Stub-based test suite for scripts/replication-health.sh. No test framework is installed in
# this repo (no bats) -- this is a plain-shell assertion harness, matching this project's
# existing scripts/chaos.sh and scripts/benchmark.sh style (real assertions, echoed PASS/FAIL,
# nonzero exit on any failure). It never talks to a real network: a fake redis-cli answers from
# canned fixture files, so every scenario below (including ones that would otherwise require
# racing real replication timing, like a never-acked replica) is deterministic and fast.
# scripts/tests/replication-health-live-test.sh separately proves the real server's wire format
# matches what these fixtures assume.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$ROOT/scripts/replication-health.sh"
# shellcheck source=scripts/tests/lib.sh
source "$ROOT/scripts/tests/lib.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
STUBDIR="$WORK/stub"
FIXTURES="$WORK/fixtures"
mkdir -p "$STUBDIR" "$FIXTURES"

cat > "$STUBDIR/redis-cli" <<'STUB'
#!/usr/bin/env bash
# Test double for redis-cli -- see replication-health-test.sh for why this exists. Records
# every invocation to $REDIS_CLI_LOG and answers from a canned fixture file, or fails outright
# (exit 1) to simulate a node that never answers.
set -uo pipefail
host="" port=""
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  case "${args[$i]}" in
    -h) host="${args[$((i + 1))]}" ;;
    -p) port="${args[$((i + 1))]}" ;;
  esac
done
printf '%s\n' "${args[*]}" >> "$REDIS_CLI_LOG"
last="${args[-1]:-}"
second_last="${args[-2]:-}"
if [ "$second_last" = "info" ] && [ "$last" = "replication" ]; then
  kind="info"
elif [ "$last" = "ping" ]; then
  kind="ping"
else
  kind="unknown"
fi
fixture="$REDIS_CLI_FIXTURE_DIR/${host}_${port}.${kind}"
[ -f "$fixture" ] || exit 1
cat "$fixture"
STUB
chmod +x "$STUBDIR/redis-cli"

export REDIS_CLI_FIXTURE_DIR="$FIXTURES"
export PATH="$STUBDIR:$PATH"

write_fixture() { printf '%s' "$3" > "$FIXTURES/$1.$2"; } # $1=host_port $2=ping|info $3=content

# 127.0.0.1:6400 -- healthy leader, one good replica.
write_fixture 127.0.0.1_6400 ping "PONG"
write_fixture 127.0.0.1_6400 info $'# Replication\r\nrole:master\r\nconnected_slaves:1\r\nmaster_repl_offset:1500\r\nslave0:ip=127.0.0.1,port=6401,state=online,offset=1500,lag=0\r\n'

# 127.0.0.1:6401 -- follower, link down.
write_fixture 127.0.0.1_6401 ping "PONG"
write_fixture 127.0.0.1_6401 info $'# Replication\r\nrole:slave\r\nmaster_host:127.0.0.1\r\nmaster_port:6400\r\nmaster_link_status:down\r\nslave_repl_offset:1200\r\nmaster_repl_offset:1200\r\n'

# 127.0.0.1:6402 -- leader with one never-acked replica (offset=0, lag=-1).
write_fixture 127.0.0.1_6402 ping "PONG"
write_fixture 127.0.0.1_6402 info $'# Replication\r\nrole:master\r\nconnected_slaves:1\r\nmaster_repl_offset:1500\r\nslave0:ip=127.0.0.1,port=6402,state=online,offset=0,lag=-1\r\n'

# 127.0.0.1:6403 -- leader with one replica whose lag exceeds the default threshold.
write_fixture 127.0.0.1_6403 ping "PONG"
write_fixture 127.0.0.1_6403 info $'# Replication\r\nrole:master\r\nconnected_slaves:1\r\nmaster_repl_offset:1500\r\nslave0:ip=127.0.0.1,port=6403,state=online,offset=1400,lag=45\r\n'

# 127.0.0.1:6404 -- leader with zero replicas.
write_fixture 127.0.0.1_6404 ping "PONG"
write_fixture 127.0.0.1_6404 info $'# Replication\r\nrole:master\r\nconnected_slaves:0\r\nmaster_repl_offset:0\r\n'

# 127.0.0.1:6405 -- healthy follower.
write_fixture 127.0.0.1_6405 ping "PONG"
write_fixture 127.0.0.1_6405 info $'# Replication\r\nrole:slave\r\nmaster_host:127.0.0.1\r\nmaster_port:6400\r\nmaster_link_status:up\r\nslave_repl_offset:1500\r\nmaster_repl_offset:1500\r\n'

# 127.0.0.1:6499 -- no fixture at all: the stub exits 1, simulating an unreachable node.

nodes_file() { local f="$WORK/nodes-$RANDOM.conf"; printf '%s\n' "$@" > "$f"; echo "$f"; }

echo "== usage errors =="
REDIS_CLI_LOG="$WORK/log-usage" "$SCRIPT" >/tmp/out 2>/tmp/err; rc=$?
assert_eq "$rc" "2" "no --nodes exits 2"

REDIS_CLI_LOG="$WORK/log-usage2" "$SCRIPT" --nodes /no/such/file >/tmp/out 2>/tmp/err; rc=$?
assert_eq "$rc" "2" "unreadable nodes file exits 2"

echo "== --help =="
out=$("$SCRIPT" --help); rc=$?
assert_eq "$rc" "0" "--help exits 0"
assert_contains "$out" "Exit codes" "--help prints the exit-code contract"

echo "== unreachable node =="
f=$(nodes_file "dead 127.0.0.1:6499 leader no")
: > "$WORK/log-unreachable"
out=$(REDIS_CLI_LOG="$WORK/log-unreachable" "$SCRIPT" --nodes "$f"); rc=$?
assert_eq "$rc" "1" "unreachable node exits 1"
assert_contains "$out" "unreachable" "reports the node as unreachable"

echo "== down-link follower =="
f=$(nodes_file "follower 127.0.0.1:6401 follower no")
: > "$WORK/log-downlink"
out=$(REDIS_CLI_LOG="$WORK/log-downlink" "$SCRIPT" --nodes "$f"); rc=$?
assert_eq "$rc" "1" "down-link follower exits 1"
assert_contains "$out" "master_link_status=down" "reports the down link"

echo "== healthy leader and follower =="
f=$(nodes_file "leader 127.0.0.1:6400 leader no" "follower 127.0.0.1:6405 follower no")
: > "$WORK/log-healthy"
out=$(REDIS_CLI_LOG="$WORK/log-healthy" "$SCRIPT" --nodes "$f"); rc=$?
assert_eq "$rc" "0" "two healthy nodes exit 0"
assert_contains "$out" "2 node(s) checked, 0 alert(s)" "summary line matches"

echo "== never-acked replica (lag=-1) =="
f=$(nodes_file "leader 127.0.0.1:6402 leader no")
: > "$WORK/log-neveracked"
out=$(REDIS_CLI_LOG="$WORK/log-neveracked" "$SCRIPT" --nodes "$f" --min-good-replicas 0); rc=$?
assert_eq "$rc" "1" "a never-acked replica exits 1 even with min-good-replicas disabled"
assert_contains "$out" "never acknowledged" "reports the never-acked replica by name"

echo "== exceeded lag =="
f=$(nodes_file "leader 127.0.0.1:6403 leader no")
: > "$WORK/log-exceeded"
out=$(REDIS_CLI_LOG="$WORK/log-exceeded" "$SCRIPT" --nodes "$f"); rc=$?
assert_eq "$rc" "1" "exceeded lag exits 1"
assert_contains "$out" "lag=45s exceeds max 10s" "reports the exceeded lag with both numbers"

echo "== good-replicas below minimum =="
f=$(nodes_file "leader 127.0.0.1:6404 leader no")
: > "$WORK/log-mincount"
out=$(REDIS_CLI_LOG="$WORK/log-mincount" "$SCRIPT" --nodes "$f"); rc=$?
assert_eq "$rc" "1" "zero replicas below the default minimum of 1 exits 1"
assert_contains "$out" "good_replicas=0/0 below configured minimum 1" "reports the threshold breach"

out=$(REDIS_CLI_LOG="$WORK/log-mincount" "$SCRIPT" --nodes "$f" --min-good-replicas 0); rc=$?
assert_eq "$rc" "0" "the same leader is fine once the minimum is disabled"

echo "== multi-node combined report =="
f=$(nodes_file "leader 127.0.0.1:6400 leader no" "follower 127.0.0.1:6401 follower no" "dead 127.0.0.1:6499 leader no")
: > "$WORK/log-combined"
out=$(REDIS_CLI_LOG="$WORK/log-combined" "$SCRIPT" --nodes "$f"); rc=$?
assert_eq "$rc" "1" "one bad node among three exits 1"
assert_contains "$out" "3 node(s) checked, 2 alert(s)" "summary counts both the down link and the unreachable node"

echo "== flag plumbing (tls/user/pass reach redis-cli) =="
f=$(nodes_file "leader 127.0.0.1:6400 leader yes")
: > "$WORK/log-flags"
REDIS_CLI_LOG="$WORK/log-flags" "$SCRIPT" --nodes "$f" --tls-ca /fake/ca.pem --user app --pass changeme >/dev/null
assert_contains "$(cat "$WORK/log-flags")" "--tls" "the tls flag reaches redis-cli"
assert_contains "$(cat "$WORK/log-flags")" "--cacert /fake/ca.pem" "the CA path reaches redis-cli"
assert_contains "$(cat "$WORK/log-flags")" "--user app" "the ACL username reaches redis-cli"
assert_contains "$(cat "$WORK/log-flags")" "--pass changeme" "the ACL password reaches redis-cli"

echo "== static safety check: no write/topology command appears outside a comment =="
code_only=$(grep -vE '^[[:space:]]*#' "$SCRIPT" | tr '[:lower:]' '[:upper:]')
for token in REPLICAOF SLAVEOF FLUSHALL FLUSHDB SHUTDOWN BGSAVE "CONFIG SET" "ACL SETUSER" "CLUSTER SETSLOT" "CLUSTER MEET" "CLUSTER FORGET"; do
  assert_not_contains "$code_only" "$token" "script source (comments excluded) never mentions $token"
done

echo "== dynamic safety check: every redis-cli invocation across this whole run was ping or info replication =="
bad=$(cat "$WORK"/log-* 2>/dev/null | grep -vE ' (ping|info replication)$' || true)
assert_eq "$bad" "" "every recorded redis-cli invocation ended in 'ping' or 'info replication'"

report_and_exit "replication-health-test.sh"
