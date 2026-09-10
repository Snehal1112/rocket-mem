# Replication Health Probe Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `scripts/replication-health.sh`, a standalone alerting script that polls `INFO REPLICATION` across a configured list of rocket-mem nodes and reports four conditions: a follower whose `master_link_status` is `down`, a replica whose `lag` exceeds a threshold (or is `-1`, meaning it has never acknowledged), a leader whose count of "good" replicas has dropped below a configured minimum, and a node that does not answer at all. It never writes to any node it checks — no `REPLICAOF`, no `REPLICAOF NO ONE`, nothing — and that constraint is enforced in three independent ways: a static source check, a dynamic recording of every command the script issues, and a live behavioral proof against real running nodes.

**Architecture:** one dependency-free bash script (matching `scripts/chaos.sh`/`scripts/benchmark.sh`'s existing style: `set -uo pipefail`, no external test framework, `redis-cli` and `timeout` as the only runtime dependencies). It reads a plain-text nodes file (one line per node: name, `host:port`, declared role, TLS yes/no), and for each node runs exactly two commands — `PING` then `INFO REPLICATION` — parsing the reply with `sed`/`grep`, never anything constructed from node-file or CLI input. There is no framework installed in this repo for testing shell scripts (no `bats`), so the test suite is plain shell: a small shared assertion library (`scripts/tests/lib.sh`) plus three test scripts, one per task, escalating from a fast fixture-driven unit suite (a fake `redis-cli` in `$PATH` answering from canned `INFO` text — this is what makes the `-1`/exceeded-lag/good-replicas-threshold scenarios deterministic, since they'd otherwise require racing real replication-ack timing) to a live integration suite against two real `rocket-mem` processes, to a live behavioral safety proof that repeatedly running the script under broken conditions never changes any node's `role:`.

**Tech Stack:** bash, `redis-cli`, `sed`, `grep`, `timeout` — all already assumed by this repo's existing `scripts/*.sh`. The live test tasks also build and run the real `rocket-mem` release binary, following `scripts/chaos.sh`'s spawn/cleanup conventions.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md)

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) first, in full.** It is normative; where it and this plan disagree, it wins and the disagreement is a bug worth reporting before writing code. This plan assumes chains A (01-06) and B (07-09) have already landed: `INFO REPLICATION` on a leader emits `master_repl_offset:<n>` and one `slave{i}:ip=..,port=..,state=online,offset=<ack>,lag=<secs>` line per replica (`lag=-1` for a replica that has never acked); on a follower it emits `slave_repl_offset:<n>`, `master_repl_offset:<n>`, and `master_link_status:up|down`.
- **This script is alerting-only, permanently.** It must never issue `REPLICAOF`, `REPLICAOF NO ONE`, or any other write command, and it must never decide anything about promotion — see the spec's "Decision: v1 is not failover" section. If a step here seems to call for the script to *act* on what it finds, stop; that is out of scope for this entire folder, not just this plan. The human procedure this script exists to trigger is `11-manual-promotion-runbook.md`'s `.claude/runbook-failover.md`.
- **No test framework is installed in this repo (no `bats`).** `scripts/tests/lib.sh` (Task 1) is the plain-shell assertion harness every task's test script sources: `assert_eq`, `assert_contains`, `assert_not_contains`, and `report_and_exit`, echoing `PASS`/`FAIL` per assertion and exiting nonzero on any failure — the same shape `scripts/chaos.sh` already uses informally (`tee -a "$CHAOS_LOG"`, an explicit mismatch counter, `exit 1` on failure), formalized into three reusable functions.
- **The "never writes" constraint gets three independent proofs, not one.** (1) Task 1's static check: strip full-line `#` comments from the script source and assert none of a fixed list of write/topology-mutating command tokens remain. (2) Task 1's dynamic check: the fixture-suite's fake `redis-cli` records every invocation it receives, and the suite asserts every single recorded line ended in `ping` or `info replication`. (3) Task 3's live check: run the real script dozens of times against a real leader/follower pair in both a healthy and a deliberately-broken state, and assert via direct `redis-cli INFO REPLICATION` (never through the script under test) that neither node's `role:` line ever changes. Do not treat any one of these as sufficient on its own — a plan that only adds the static check has not verified the runtime behavior, and vice versa.
- **This plan's own script will contain a real, deliberately-narrated bug in Task 1** (a `sed` capture group that doesn't handle the minus sign in `lag=-1`), caught by the failing test for the never-acked scenario and fixed within the same task. This is not a placeholder or a fake TDD cycle — it is the actual, plausible mistake an implementer writing this regex by hand is likely to make, and the plan is written to have you write it, watch the specific test that catches it fail for exactly that reason, and fix it, rather than skip straight to the correct regex.
- This plan touches no Rust — every deliverable is a shell script. The three CI gates below are still run once per task as a repo-wide safety net (a stray filename collision or similar), but no task here is expected to change their output.
- **The three CI gates must be clean before every commit:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
- **Comment style** (project `CLAUDE.md`): short, easy, full sentences ending in a punctuation mark. No emojis.
- Commit through the `1-git-commit` skill, this project's standing convention for Superpowers-driven commits — not a freeform `git commit -m`.

---

### Task 1: The script itself, plus a fixture-driven unit suite

**Files:**
- Create: `scripts/replication-health.sh`
- Create: `scripts/tests/lib.sh`
- Create: `scripts/tests/replication-health-test.sh`

**Interfaces:**
- Produces: `scripts/replication-health.sh`'s CLI contract — `--nodes <path>` (required), `--max-lag-secs` (default 10), `--min-good-replicas` (default 1), `--timeout-secs` (default 3), `--tls-ca`, `--user`, `--pass` (or `ROCKET_MEM_HEALTH_USER`/`ROCKET_MEM_HEALTH_PASS`), `-h`/`--help`; exit 0 (healthy), 1 (alert found), 2 (usage error). Internal functions `usage`, `query_node`, `alert`, `ok`, `check_node` — not a public interface, but Task 2 and Task 3's tests rely on the script's stdout format (`ALERT ...`, `OK ...`, and the trailing `replication-health: N node(s) checked, M alert(s)` summary line) and its exit codes.
- Produces: `scripts/tests/lib.sh`'s `assert_eq`, `assert_contains`, `assert_not_contains`, `report_and_exit` — sourced by all three tasks' test scripts in this plan.
- Consumes: nothing from earlier plans in this folder directly (it talks to `INFO REPLICATION` over the wire, not to any Rust type), but its correctness depends on chains A/B's `INFO REPLICATION` format holding exactly as stated in Global Constraints.

- [ ] **Step 1: Write the failing tests**

Create `scripts/tests/lib.sh`:

```bash
#!/usr/bin/env bash
# Shared plain-shell assertion helpers for scripts/tests/replication-health-*.sh. No test
# framework is installed in this repo (no bats) -- these few functions are the harness: print
# PASS/FAIL per assertion, track a running failure count, and let the caller decide the
# process exit code from that count at the end. Source this, don't execute it.
TESTS_RUN=0
TESTS_FAILED=0

assert_eq() { # $1=actual $2=expected $3=description
  TESTS_RUN=$((TESTS_RUN + 1))
  if [ "$1" = "$2" ]; then
    echo "  PASS: $3"
  else
    echo "  FAIL: $3 -- expected [$2], got [$1]"
    TESTS_FAILED=$((TESTS_FAILED + 1))
  fi
}

assert_contains() { # $1=haystack $2=needle $3=description
  TESTS_RUN=$((TESTS_RUN + 1))
  if [[ "$1" == *"$2"* ]]; then
    echo "  PASS: $3"
  else
    echo "  FAIL: $3 -- expected to find [$2] in:"
    echo "        $1"
    TESTS_FAILED=$((TESTS_FAILED + 1))
  fi
}

assert_not_contains() { # $1=haystack $2=needle $3=description
  TESTS_RUN=$((TESTS_RUN + 1))
  if [[ "$1" != *"$2"* ]]; then
    echo "  PASS: $3"
  else
    echo "  FAIL: $3 -- did not expect to find [$2] in:"
    echo "        $1"
    TESTS_FAILED=$((TESTS_FAILED + 1))
  fi
}

report_and_exit() { # $1=suite name
  echo "--- $1: $TESTS_RUN assertion(s), $TESTS_FAILED failure(s) ---"
  [ "$TESTS_FAILED" -eq 0 ]
}
```

Create `scripts/tests/replication-health-test.sh`:

```bash
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `chmod +x scripts/tests/replication-health-test.sh && bash scripts/tests/replication-health-test.sh`
Expected: FAIL, broadly — `scripts/replication-health.sh` does not exist yet, so every invocation reports `No such file or directory` (bash exit 127), every assertion built on its output fails, and `report_and_exit` exits nonzero.

- [ ] **Step 3: Write the implementation (v1 — contains one deliberate bug, caught in Step 4)**

Create `scripts/replication-health.sh`:

```bash
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
    lag=$(printf '%s' "$line" | sed -n 's/.*lag=\([0-9]*\).*/\1/p')
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
```

```bash
chmod +x scripts/replication-health.sh
```

- [ ] **Step 4: Run the tests to verify they mostly pass — and watch the deliberate bug fail for a specific, informative reason**

Run: `bash scripts/tests/replication-health-test.sh`
Expected: every assertion passes **except** `"a never-acked replica exits 1 even with min-good-replicas disabled"` and `"reports the never-acked replica by name"`, which FAIL. The `lag=-1` fixture instead reports `OK ... good_replicas=1/1` and the script exits `0`.

Root cause: `sed -n 's/.*lag=\([0-9]*\).*/\1/p'` against `...,lag=-1` matches zero digits at the position right after `lag=` (the next character is `-`, not a digit), so `\1` captures the empty string, not `-1`. `lag` then holds `""`. The check `[ "$lag" = "-1" ]` is false, so control falls to `elif [ "$lag" -gt "$MAX_LAG_SECS" ]`, which errors (`bash: [: : integer expression expected`, a non-fatal message under `set -uo pipefail` with no `-e`) and evaluates false, so control falls through to `else good=$((good + 1))` — a replica that has never acknowledged anything gets silently counted as good. The exceeded-lag fixture (`lag=45`, no minus sign) is unaffected by this bug, which is why only the never-acked assertions fail.

- [ ] **Step 5: Fix the regex**

In `scripts/replication-health.sh`, the lag-extraction line currently reads:

```bash
    lag=$(printf '%s' "$line" | sed -n 's/.*lag=\([0-9]*\).*/\1/p')
```

Change it to also capture an optional leading minus sign:

```bash
    lag=$(printf '%s' "$line" | sed -n 's/.*lag=\(-\?[0-9]*\).*/\1/p')
```

- [ ] **Step 6: Run the tests to verify they all pass**

Run: `bash scripts/tests/replication-health-test.sh`
Expected: every assertion PASSes, including both previously-failing ones. Final line: `--- replication-health-test.sh: <N> assertion(s), 0 failure(s) ---`, exit 0.

- [ ] **Step 7: Full-repo check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green (unaffected by this task's shell-only changes).

```bash
git add scripts/replication-health.sh scripts/tests/lib.sh scripts/tests/replication-health-test.sh
```
Commit through the `1-git-commit` skill. Suggested subject: `Add replication-health.sh, an alerting-only replication probe`.

---

### Task 2: Live integration test against real leader/follower processes

**Files:**
- Create: `scripts/tests/replication-health-live-test.sh`

**Interfaces:**
- Consumes: `scripts/replication-health.sh` (Task 1) and `scripts/tests/lib.sh` (Task 1); the real `rocket-mem` binary, built via `cargo build --release --workspace`, driven the way `scripts/chaos.sh` already drives it (env-var config, `redis-cli replicaof` for live reconfiguration, bounded polling instead of fixed sleeps).
- Produces: nothing new for later tasks — this is a standalone confidence check that Task 1's fixture text actually matches what a real node sends on the wire.

- [ ] **Step 1: Write the failing test**

Create `scripts/tests/replication-health-live-test.sh`:

```bash
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
ROCKET_MEM_SNAPSHOT_PATH="$WORK/leader.snapshot" "$BIN" >"$WORK/leader.log" 2>&1 &
LEADER_PID=$!
wait_for_port "$LEADER_PORT" || { echo "leader failed to start"; exit 1; }

ROCKET_MEM_ADDR="127.0.0.1:$FOLLOWER_PORT" ROCKET_MEM_RMP_ADDR="127.0.0.1:0" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:0" ROCKET_MEM_AOF_PATH="$WORK/follower.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/follower.snapshot" "$BIN" >"$WORK/follower.log" 2>&1 &
FOLLOWER_PID=$!
wait_for_port "$FOLLOWER_PORT" || { echo "follower failed to start"; exit 1; }
redis-cli -p "$FOLLOWER_PORT" replicaof 127.0.0.1 "$LEADER_PORT" >/dev/null

ROCKET_MEM_ADDR="127.0.0.1:$BADFOLLOWER_PORT" ROCKET_MEM_RMP_ADDR="127.0.0.1:0" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:0" ROCKET_MEM_AOF_PATH="$WORK/badfollower.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/badfollower.snapshot" "$BIN" >"$WORK/badfollower.log" 2>&1 &
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
```

```bash
chmod +x scripts/tests/replication-health-live-test.sh
```

- [ ] **Step 2: Run the test to verify it fails for the right reason if the script were broken**

Run: `bash scripts/tests/replication-health-live-test.sh`
Expected: PASS. (This test exercises Task 1's already-correct script against real processes — there is no new implementation in this task. To confirm the test itself is a real check and not a tautology, temporarily revert Step 5 of Task 1's regex fix, rerun, confirm the never-acked-style assertions here are unaffected since this task never exercises that fixture-only scenario, then restore the fix. This is a sanity check on the test, not a required step to leave in the repeatable suite.)

- [ ] **Step 3: (No implementation change — Task 1's script already satisfies this suite.) Run to confirm.**

Run: `bash scripts/tests/replication-health-live-test.sh`
Expected: `--- replication-health-live-test.sh: 5 assertion(s), 0 failure(s) ---`, exit 0.

- [ ] **Step 4: Full-repo check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

```bash
git add scripts/tests/replication-health-live-test.sh
```
Commit through the `1-git-commit` skill. Suggested subject: `Add a live integration test for replication-health.sh`.

---

### Task 3: Live non-mutation safety proof, and the real deployment's nodes file

**Files:**
- Create: `scripts/tests/replication-health-safety-test.sh`
- Create: `scripts/replication-health.nodes.example`

**Interfaces:**
- Consumes: `scripts/replication-health.sh` (Task 1), `scripts/tests/lib.sh` (Task 1), the real `rocket-mem` binary (as Task 2).
- Produces: `scripts/replication-health.nodes.example`, the nodes file `11-manual-promotion-runbook.md`'s runbook references directly for the real 3-shard + 3-replica TLS deployment.

- [ ] **Step 1: Write the failing test**

Create `scripts/tests/replication-health-safety-test.sh`:

```bash
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

ROCKET_MEM_ADDR="127.0.0.1:$LEADER_PORT" ROCKET_MEM_RMP_ADDR="127.0.0.1:0" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:0" ROCKET_MEM_AOF_PATH="$WORK/leader.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/leader.snapshot" "$BIN" >"$WORK/leader.log" 2>&1 &
LEADER_PID=$!
wait_for_port "$LEADER_PORT" || { echo "leader failed to start"; exit 1; }

ROCKET_MEM_ADDR="127.0.0.1:$FOLLOWER_PORT" ROCKET_MEM_RMP_ADDR="127.0.0.1:0" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:0" ROCKET_MEM_AOF_PATH="$WORK/follower.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/follower.snapshot" "$BIN" >"$WORK/follower.log" 2>&1 &
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
```

```bash
chmod +x scripts/tests/replication-health-safety-test.sh
```

Create `scripts/replication-health.nodes.example`:

```
# rocket-mem 3-shard + 3-replica deployment (see cluster.conf and rocket-mem.toml /
# rocket-mem-shard-{b,c}.toml / rocket-mem-shard-{a,b,c}-replica.toml at the repo root).
# All addresses below are TLS RESP listeners, and every node in this deployment has ACL
# users configured, so pass --tls-ca/--user/--pass (or ROCKET_MEM_HEALTH_PASS) when using
# this file, e.g.:
#
#   scripts/replication-health.sh --nodes scripts/replication-health.nodes.example \
#     --tls-ca /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app
#
# <name> <host:port> <role> <tls>
shard-a          numericlabs.lxd:16379 leader   yes
shard-a-replica  numericlabs.lxd:16479 follower yes
shard-b          numericlabs.lxd:16380 leader   yes
shard-b-replica  numericlabs.lxd:16480 follower yes
shard-c          numericlabs.lxd:16381 leader   yes
shard-c-replica  numericlabs.lxd:16481 follower yes
```

- [ ] **Step 2: Run the test to verify it fails if it would have caught a regression**

Run: `bash scripts/tests/replication-health-safety-test.sh`
Expected: PASS against Task 1's script as written. (To confirm this test is a real check: temporarily add a line to `check_node` that calls `redis-cli ... replicaof no one` on any `slave` node reporting `master_link_status=down`, rerun, and confirm the follower's role flips to `master` and the "follower is still slave" assertions FAIL. Then remove that line — it was only to prove the test has teeth.)

- [ ] **Step 3: Confirm the suite passes as shipped**

Run: `bash scripts/tests/replication-health-safety-test.sh`
Expected: `--- replication-health-safety-test.sh: 6 assertion(s), 0 failure(s) ---`, exit 0.

- [ ] **Step 4: Full-repo check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

```bash
git add scripts/tests/replication-health-safety-test.sh scripts/replication-health.nodes.example
```
Commit through the `1-git-commit` skill. Suggested subject: `Prove replication-health.sh never mutates node state, live`.

---

## Next plan

[`11-manual-promotion-runbook.md`](11-manual-promotion-runbook.md) — the operator runbook (`.claude/runbook-failover.md`) that uses this plan's offsets-aware alerting script and chain A/B's offset/fencing data to walk a human through confirming a dead leader, picking the most-caught-up replica, promoting it, re-pointing survivors, and the cluster-mode topology surgery and hazards this deployment's clustered shape requires.
