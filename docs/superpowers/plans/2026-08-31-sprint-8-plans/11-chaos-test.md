# Chaos Test Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** an overnight `kill -9` chaos loop against a real leader+follower pair, verified against an independent write log — not a CI-gated test (an overnight run can't fit CI's time budget), a committed script plus a committed log from one real run, matching Sprint 6's `redis-benchmark` report precedent.

**Architecture:** a load-generator example binary (`chaos_load`) that writes sequential keys against a real server over a real TCP connection and logs every write it *successfully* issues to its own file — an independent record of "what should be there" that survives the server being killed mid-connection. `scripts/chaos.sh` starts a leader+follower, runs the load generator, randomly `kill -9`s one side or the other on a timer, restarts it, and — after the load generator finishes — diffs the load log against both nodes' actual keyspace.

**Tech Stack:** the existing `redis` crate (already a `server` dev-dependency), exposed to a Cargo `[[example]]` target (examples can use dev-dependencies; a `[[bin]]` cannot without also adding `redis` to the shipped binary's real dependencies) — no new dependency. `redis-cli` (already assumed present, same as Sprint 6's benchmark script assumes `redis-benchmark`).

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: chaos test is a committed script + committed log, not a CI job" section.

## Global Constraints

- Verification compares the **actual recovered/resynced keyspace** against the load generator's own independent record of what it wrote — not "the process didn't crash." A mismatch is a hard failure (`scripts/chaos.sh` exits non-zero and prints the exact key/expected/actual/node).
- The load generator reconnects per write rather than pooling a connection — deliberately simple and robust against a server that can disappear mid-write at any moment, which is the entire premise of this test; throughput is not the point.
- This is never added to `.github/workflows/ci.yml` — it is a manual/scheduled operational script, run and its log committed by a human (or a separate, non-blocking scheduled job), exactly like `scripts/benchmark.sh` from Sprint 6.

---

### Task 1: Load-generator example (`chaos_load`)

**Files:**
- Create: `crates/server/examples/chaos_load.rs`

**Interfaces:**
- Consumes: the `redis` crate (already a `server` dev-dependency, usable from an `[[example]]` target).
- Produces: a binary invoked as `chaos_load <redis-url> <log-path> <duration-secs>`, writing `<key> <value>\n` lines to `<log-path>` for every write it confirms succeeded — the "expected state" record `scripts/chaos.sh` (Task 2) verifies against.

- [ ] **Step 1: Write `chaos_load.rs`**

```rust
// crates/server/examples/chaos_load.rs
//! Chaos-test load generator: writes sequential keys against a live server over real TCP,
//! reconnecting on every write so a server that dies mid-connection (the whole point of the
//! chaos loop in scripts/chaos.sh) never wedges this loop. Logs one line per *confirmed*
//! successful write -- this log is the independent "what should be there" record chaos.sh
//! verifies the post-chaos keyspace against. See
//! ../../docs/superpowers/plans/2026-08-31-sprint-8-plans/11-chaos-test.md.

use redis::Commands;
use std::io::Write;

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .expect("usage: chaos_load <redis-url> <log-path> <duration-secs>");
    let log_path = args
        .next()
        .expect("usage: chaos_load <redis-url> <log-path> <duration-secs>");
    let duration_secs: u64 = args
        .next()
        .expect("usage: chaos_load <redis-url> <log-path> <duration-secs>")
        .parse()
        .expect("duration-secs must be a number");

    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("failed to open the write log");

    let start = std::time::Instant::now();
    let mut written: u64 = 0;
    let mut i: u64 = 0;
    while start.elapsed().as_secs() < duration_secs {
        let key = format!("chaos:{i}");
        let value = format!("v{i}");
        i += 1;

        let outcome = redis::Client::open(url.as_str())
            .and_then(|client| client.get_connection())
            .and_then(|mut con| con.set::<_, _, ()>(&key, &value));

        match outcome {
            Ok(()) => {
                writeln!(log, "{key} {value}").expect("failed to append to the write log");
                log.flush().expect("failed to flush the write log");
                written += 1;
            }
            // A connection error (the target was just kill -9'd, or hasn't restarted yet) is
            // expected and swallowed -- the loop just tries again on the next key. A write
            // whose connection died before its reply arrived is NOT logged, so it is correctly
            // absent from the expected-state record even if it landed on the server before the
            // reply was lost -- this makes the log a conservative (never over-claiming) record.
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    println!("chaos_load: confirmed {written} writes over {duration_secs}s");
}
```

- [ ] **Step 2: Verify it builds and runs against a real (manually started) server**

Run, in one terminal:
```bash
cargo run -p rocket-mem --bin rocket-mem &
```
In another terminal:
```bash
cargo run -p rocket-mem --example chaos_load -- redis://127.0.0.1:6379 /tmp/chaos-smoke.log 5
redis-cli GET chaos:0
```
Expected: `chaos_load` prints `chaos_load: confirmed N writes over 5s` with `N > 0`; `/tmp/chaos-smoke.log` has `N` lines of `chaos:<i> v<i>`; `redis-cli GET chaos:0` returns `v0`. Stop the manually-started server afterward (`kill %1` or `Ctrl-C` its terminal).

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/examples/chaos_load.rs`.

---

### Task 2: `scripts/chaos.sh`

**Files:**
- Create: `scripts/chaos.sh`

**Interfaces:**
- Consumes: the `rocket-mem` release binary, the `chaos_load` example (Task 1), `redis-cli`.
- Produces: an executable script, `scripts/chaos.sh [iterations] [duration_secs]` (defaults `200` / `1800`), exit code `0` on a clean run or `1` on any detected mismatch or node that failed to restart.

- [ ] **Step 1: Write the script**

```bash
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
```

- [ ] **Step 2: Make it executable and smoke-test it with a short duration**

Run:
```bash
chmod +x scripts/chaos.sh
scripts/chaos.sh 5 30
```
Expected: exits `0`, prints `chaos.sh: done. N keys checked against 2 nodes, 0 mismatches.` with `N > 0`. This 30-second, 5-iteration run is a smoke test proving the script's mechanics (start/kill/restart/verify) work at all — Task 3 runs the real overnight configuration.

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `scripts/chaos.sh`.

---

### Task 3: Run the full overnight chaos loop and commit its log

**Files:**
- Create: `docs/chaos/<today's date, YYYY-MM-DD>-chaos-log.md`

**Interfaces:**
- Consumes: `scripts/chaos.sh` (Task 2).
- Produces: a committed record of one real, full-length run — the sprint's actual DoD evidence, not a smoke test.

- [ ] **Step 1: Run the full chaos loop**

Run (this genuinely takes on the order of an hour or more — the whole point):
```bash
scripts/chaos.sh 200 3600 2>&1 | tee /tmp/chaos-full-run.log
```

- [ ] **Step 2: Write `docs/chaos/<date>-chaos-log.md`**

Using today's actual date and the real output from Step 1, write a report in this shape:

```markdown
# Chaos Test — <YYYY-MM-DD>

Full run of `scripts/chaos.sh 200 3600`: a leader and follower under continuous write load from
`chaos_load`, with a random `kill -9` (leader or follower, chosen with equal probability) every
0–30 seconds, for 200 iterations or 1 hour of load-generator runtime, whichever came first.

**Result:** <PASS -- 0 mismatches | FAIL -- see below>

**Summary** (fill in from the real run's final output line and iteration count):
- Iterations completed: <N> of 200 (early stop only if the load generator's 1-hour duration elapsed first)
- Total writes confirmed by the load generator: <N>
- Mismatches found verifying against the leader: <N>
- Mismatches found verifying against the follower: <N>

**Full script output:** attached below, unedited.

<details>
<summary>scripts/chaos.sh output</summary>

​```
<paste the real, complete output captured in /tmp/chaos-full-run.log>
​```

</details>
```

(If the run found any mismatches, do not soften or omit them — record the exact `MISMATCH key=... expected=... actual=... port=...` lines from the log and treat this as a real finding to investigate and fix before Sprint 8 can be called done, per the spec's own "zero corruption incidents" DoD line — reopen a task rather than committing a FAIL report as if it were acceptable.)

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `docs/chaos/<date>-chaos-log.md`.
