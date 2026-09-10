#!/usr/bin/env bash
# replication-health.sh -- alerting-only replication health probe for rocket-mem.
#
# Polls INFO REPLICATION across a configured list of nodes and reports:
#   - a follower whose master_link_status is down
#   - a replica whose lag exceeds --max-lag-secs, or is -1 (never acknowledged)
#   - a leader whose count of "good" replicas has dropped below --min-good-replicas
#   - a node that does not answer at all
#
# THIS SCRIPT NEVER WRITES TO ANY NODE. The only commands it ever sends are PING and
# INFO REPLICATION. It never sends REPLICAOF, REPLICAOF NO ONE, or any other write command,
# and it makes no decision about promoting or re-pointing anything. See
# docs/superpowers/specs/2026-09-09-sentinel-failover-spec.md: a single, unelected observer
# has no quorum and cannot tell "the leader is dead" from "I can't reach the leader," so
# acting automatically on what this script sees would risk exactly the split-brain the spec
# warns about. If you are about to add a write here, stop -- the human procedure this script
# exists to trigger lives in .claude/runbook-failover.md.
#
# Usage: scripts/replication-health.sh --nodes <path> [options]
#
# Nodes file: one node per line, blank lines and lines starting with '#' ignored.
#   <name> <host:port> <role:leader|follower> <tls:yes|no>
# `role` is descriptive only, used in output labels -- every node gets the same
# INFO REPLICATION parse regardless of what the file says it should be, and this script
# acts on what the node actually reports.
#
# Options:
#   --nodes <path>            required. Nodes file, format above.
#   --max-lag-secs <n>        default 10. A replica lag over this many seconds alerts.
#   --min-good-replicas <n>   default 1. A leader with fewer good replicas than this alerts.
#   --timeout-secs <n>        default 3. Per-node round-trip timeout.
#   --tls-ca <path>           CA cert for TLS nodes (redis-cli --cacert).
#   --user <name>             ACL username, if the deployment has ACL users configured.
#   --pass <password>         ACL password. Prefer ROCKET_MEM_HEALTH_PASS over this flag --
#                             a flag value is visible to anyone who can run `ps`.
#   -h, --help                print usage and exit 0.
#
# Env vars, used only when the matching flag is absent:
#   ROCKET_MEM_HEALTH_USER, ROCKET_MEM_HEALTH_PASS
#
# Exit codes:
#   0  every node healthy
#   1  one or more alert conditions found
#   2  usage error (bad flags, missing/unreadable nodes file, missing dependency)
set -uo pipefail

usage() {
  cat <<'USAGE'
Usage: replication-health.sh --nodes <path> [options]

Polls INFO REPLICATION across the nodes listed in <path> and reports alert conditions: a
follower with master_link_status down, a replica whose lag exceeds --max-lag-secs or is -1
(never acknowledged), a leader whose good-replica count is below --min-good-replicas, or a
node that does not answer at all. Never writes to any node -- see the header comment in
this file and .claude/runbook-failover.md for the human procedure this script exists to
trigger.

Nodes file: one node per line, blank/'#'-comment lines ignored:
  <name> <host:port> <role:leader|follower> <tls:yes|no>

Options:
  --nodes <path>            required.
  --max-lag-secs <n>        default 10.
  --min-good-replicas <n>   default 1.
  --timeout-secs <n>        default 3.
  --tls-ca <path>           CA cert for TLS nodes.
  --user <name>             ACL username.
  --pass <password>         ACL password (prefer ROCKET_MEM_HEALTH_PASS).
  -h, --help                print this message and exit 0.

Exit codes: 0 healthy, 1 alert(s) found, 2 usage error.
USAGE
}

MAX_LAG_SECS=10
MIN_GOOD_REPLICAS=1
TIMEOUT_SECS=3
NODES_FILE=""
TLS_CA=""
CLI_USER="${ROCKET_MEM_HEALTH_USER:-}"
CLI_PASS="${ROCKET_MEM_HEALTH_PASS:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --nodes) NODES_FILE="$2"; shift 2 ;;
    --max-lag-secs) MAX_LAG_SECS="$2"; shift 2 ;;
    --min-good-replicas) MIN_GOOD_REPLICAS="$2"; shift 2 ;;
    --timeout-secs) TIMEOUT_SECS="$2"; shift 2 ;;
    --tls-ca) TLS_CA="$2"; shift 2 ;;
    --user) CLI_USER="$2"; shift 2 ;;
    --pass) CLI_PASS="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "replication-health.sh: unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$NODES_FILE" ]; then
  echo "replication-health.sh: --nodes <path> is required" >&2
  exit 2
fi
if [ ! -r "$NODES_FILE" ]; then
  echo "replication-health.sh: cannot read nodes file: $NODES_FILE" >&2
  exit 2
fi
case "$MAX_LAG_SECS$MIN_GOOD_REPLICAS$TIMEOUT_SECS" in
  *[!0-9]*)
    echo "replication-health.sh: --max-lag-secs/--min-good-replicas/--timeout-secs must be non-negative integers" >&2
    exit 2
    ;;
esac
for bin in redis-cli timeout; do
  command -v "$bin" >/dev/null 2>&1 || { echo "replication-health.sh: '$bin' is not on PATH" >&2; exit 2; }
done

ALERTS=0
CHECKED=0

# Runs a single command against one node and prints the raw reply on stdout, or nothing on
# failure/timeout. This is the ONLY place this script talks to a node's command port, and
# $cmd is always literally "ping" or "info" -- never anything built from node-file input or
# a CLI flag.
query_node() {
  local host="$1" port="$2" tls="$3" cmd="$4"
  local -a args=(-h "$host" -p "$port" --no-raw --no-auth-warning)
  [ "$tls" = "yes" ] && args+=(--tls)
  [ -n "$TLS_CA" ] && args+=(--cacert "$TLS_CA")
  [ -n "$CLI_USER" ] && args+=(--user "$CLI_USER")
  [ -n "$CLI_PASS" ] && args+=(--pass "$CLI_PASS")
  if [ "$cmd" = "ping" ]; then
    timeout "$TIMEOUT_SECS" redis-cli "${args[@]}" ping 2>/dev/null
  else
    timeout "$TIMEOUT_SECS" redis-cli "${args[@]}" info replication 2>/dev/null
  fi
}

alert() { echo "ALERT $*"; ALERTS=$((ALERTS + 1)); }
ok()    { echo "OK    $*"; }

check_node() {
  local name="$1" hostport="$2" role="$3" tls="$4"
  local host="${hostport%:*}" port="${hostport##*:}"
  CHECKED=$((CHECKED + 1))

  local pong
  pong=$(query_node "$host" "$port" "$tls" ping | tr -d '\r')
  if [ "$pong" != "PONG" ]; then
    alert "$name ($hostport): unreachable -- no PONG within ${TIMEOUT_SECS}s"
    return
  fi

  local info
  info=$(query_node "$host" "$port" "$tls" info | tr -d '\r')
  if [ -z "$info" ]; then
    alert "$name ($hostport): PING answered but INFO REPLICATION returned nothing"
    return
  fi

  local actual_role
  actual_role=$(printf '%s\n' "$info" | sed -n 's/^role:\(.*\)$/\1/p')

  if [ "$actual_role" = "slave" ]; then
    local link
    link=$(printf '%s\n' "$info" | sed -n 's/^master_link_status:\(.*\)$/\1/p')
    if [ "$link" = "up" ]; then
      ok "$name ($hostport): role=slave master_link_status=up"
    else
      alert "$name ($hostport): role=slave master_link_status=${link:-unknown}"
    fi
    return
  fi

  if [ "$actual_role" != "master" ]; then
    alert "$name ($hostport): INFO REPLICATION had no role: line (unexpected reply)"
    return
  fi

  local good=0 total=0 line ip port_ lag offset
  while IFS= read -r line; do
    [[ "$line" =~ ^slave[0-9]+: ]] || continue
    total=$((total + 1))
    ip=$(printf '%s' "$line" | sed -n 's/.*ip=\([^,]*\).*/\1/p')
    port_=$(printf '%s' "$line" | sed -n 's/.*port=\([^,]*\).*/\1/p')
    offset=$(printf '%s' "$line" | sed -n 's/.*offset=\([^,]*\).*/\1/p')
    lag=$(printf '%s' "$line" | sed -n 's/.*lag=\(-\?[0-9]*\).*/\1/p')
    if [ "$lag" = "-1" ]; then
      alert "$name ($hostport): replica $ip:$port_ has never acknowledged (offset=$offset, lag=-1)"
    elif [ "$lag" -gt "$MAX_LAG_SECS" ]; then
      alert "$name ($hostport): replica $ip:$port_ lag=${lag}s exceeds max ${MAX_LAG_SECS}s (offset=$offset)"
    else
      good=$((good + 1))
    fi
  done <<<"$info"

  if [ "$good" -lt "$MIN_GOOD_REPLICAS" ]; then
    alert "$name ($hostport): role=master good_replicas=$good/$total below configured minimum $MIN_GOOD_REPLICAS"
  else
    ok "$name ($hostport): role=master good_replicas=$good/$total"
  fi
}

while IFS= read -r raw_line || [ -n "$raw_line" ]; do
  line="${raw_line%%#*}"
  line=$(printf '%s' "$line" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
  [ -z "$line" ] && continue
  read -r n hp r t <<<"$line"
  check_node "$n" "$hp" "$r" "$t"
done < "$NODES_FILE"

echo "---"
echo "replication-health: $CHECKED node(s) checked, $ALERTS alert(s)"
[ "$ALERTS" -eq 0 ]
