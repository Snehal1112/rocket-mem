# MULTI/EXEC Transactions — Plan 04: Benchmark Verification and Docs

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the non-transaction hot path did not regress, add a throughput number for
`MULTI`/`EXEC` itself, and document the feature as released — the required closing work per the
spec's "Performance" section.

**Architecture:** Follows this repo's own established before/after benchmark convention exactly
(`docs/benchmarks/2026-09-09-pre-logging-baseline.md` /
`2026-09-09-post-logging-final.md`, from the verbose-logging series): three runs of
`scripts/benchmark.sh` before this series' first commit, three runs after it, gated at <=2% on
only the two tightest (lowest-jitter) rows — unpipelined 3-byte `SET` and `GET` — with every
other row recorded as context only. The "before" measurement runs from a temporary, nested `git
worktree` pinned to the commit immediately preceding this series, so it never touches the branch
this plan itself is executing on.

**Tech Stack:** Bash, `redis-benchmark`, `redis-cli --pipe`, `git worktree`.

**Spec:** [`../../specs/2026-09-10-multi-exec-transactions-spec.md`](../../specs/2026-09-10-multi-exec-transactions-spec.md)

**Global Constraints:** See
[`01-session-state-and-queuing.md`](01-session-state-and-queuing.md)'s "Global Constraints"
section — every rule there applies here too. Baseline test count entering this plan:
`BASELINE + 27` (from Plan 03). This plan adds no new `#[test]` functions — its verification is
benchmark output and documentation, not unit tests — so the workspace test count must be
unchanged, `BASELINE + 27`, at the end of every task below.

---

### Task 1: Pre-series benchmark baseline

**Files:**
- Create: `docs/benchmarks/<TODAY>-pre-transactions-baseline.md` (`<TODAY>` = the actual date
  this task runs, `date -u +%Y-%m-%d` — do not reuse `2026-09-10` unless that is genuinely today)

**Interfaces:**
- Consumes: `scripts/benchmark.sh` (pre-existing, unmodified).
- Produces: the baseline file, consumed by Task 2.

- [ ] **Step 1: Check for contaminating local processes**

```bash
systemctl --user status rocket-mem-shard-a rocket-mem-shard-b rocket-mem-shard-c \
  rocket-mem-shard-a-replica rocket-mem-shard-b-replica rocket-mem-shard-c-replica \
  2>&1 | grep -E "Active:|●"
```

If any are `active (running)`, this benchmark run will be contaminated exactly the way
`docs/benchmarks/2026-09-09-post-logging-final.md` documents for a prior series (six hand-started
nodes sharing the machine during that capture). Do not stop them yourself — they are unrelated,
independently-managed services. Instead, add a "Contamination notice" section to both this
task's and Task 2's output files, modeled on that file's own, naming exactly which services were
active. If none are active, skip the notice.

- [ ] **Step 2: Find the commit immediately before this series**

```bash
git log --oneline --all --grep="add TransactionState and Session queuing fields"
```

This is Plan 01 Task 1's commit (the series' first). Its parent is the pre-series commit:

```bash
PRE_SERIES_COMMIT=$(git rev-parse <that-commit-hash>^)
echo "$PRE_SERIES_COMMIT"
```

- [ ] **Step 3: Benchmark that commit from a temporary nested worktree**

A nested worktree, not a `stash`/`checkout` on the branch this plan is executing on — this must
never touch the current branch's checked-out state:

```bash
git worktree add /tmp/rocket-mem-pre-transactions-benchmark "$PRE_SERIES_COMMIT"
cd /tmp/rocket-mem-pre-transactions-benchmark
./scripts/benchmark.sh > /tmp/pre-transactions-run1.txt 2>&1
./scripts/benchmark.sh > /tmp/pre-transactions-run2.txt 2>&1
./scripts/benchmark.sh > /tmp/pre-transactions-run3.txt 2>&1
cd -
git worktree remove /tmp/rocket-mem-pre-transactions-benchmark
```

Each run takes a couple of minutes (it builds `--release` and runs eight
payload/pipeline/command combinations against both servers). If any run fails outright (a port
already bound, a missing `redis-server`/`redis-benchmark` on `PATH`), fix the *environment*
issue and re-run that one command — do not modify `scripts/benchmark.sh` itself as part of this
task.

- [ ] **Step 4: Write the baseline report**

Create `docs/benchmarks/<TODAY>-pre-transactions-baseline.md`, modeled section-for-section on
`docs/benchmarks/2026-09-09-pre-logging-baseline.md`:

```markdown
# Pre-Transactions Throughput Baseline

**Date:** <TODAY>
**Commit:** <PRE_SERIES_COMMIT, short form>
**Purpose:** the reference point for the <=2% regression gate this series' Plan 04 defines, per
[the MULTI/EXEC transactions spec](../superpowers/specs/2026-09-10-multi-exec-transactions-spec.md)'s
"Performance" section. Captured from the commit immediately before this series' first commit, via
a temporary nested `git worktree` — never on the branch this plan itself executes on.

**Harness:** `scripts/benchmark.sh`, three consecutive runs, matched durability
(`--appendonly yes --appendfsync everysec` on both servers).

## rocket-mem requests/sec

| Workload | Run 1 | Run 2 | Run 3 | Mean |
|---|---|---|---|---|
| SET, 3B, no pipeline | ... | ... | ... | ... |
| GET, 3B, no pipeline | ... | ... | ... | ... |
| SET, 3B, pipeline=16 | ... | ... | ... | ... |
| GET, 3B, pipeline=16 | ... | ... | ... | ... |
| SET, 1KB, no pipeline | ... | ... | ... | ... |
| GET, 1KB, no pipeline | ... | ... | ... | ... |
| SET, 1KB, pipeline=16 | ... | ... | ... | ... |
| GET, 1KB, pipeline=16 | ... | ... | ... | ... |

(fill every `...` from the three `/tmp/pre-transactions-run*.txt` files' `SET`/`GET`
`requests per second` lines — do not estimate or round beyond what the tool printed)

## Gate

**The <=2% gate applies ONLY to `SET, 3B, no pipeline` and `GET, 3B, no pipeline`** — the two
rows this repo's own prior benchmark series (`2026-09-09-pre-logging-baseline.md`) found have the
tightest run-to-run jitter (0.8%-7.6%, versus 8.8%-22.2% on every other row). Task 2's post-series
numbers for these two rows, measured the same way, must be within 2% of these means. The other
six rows are recorded as context only.
```

- [ ] **Step 5: Confirm the workspace still builds (nothing in Task 1 touches source)**

```bash
cargo test --workspace 2>&1 | tail -5
```

Expected: PASS, total unchanged at `BASELINE + 27`.

- [ ] **Step 6: Commit**

```bash
git add docs/benchmarks/*-pre-transactions-baseline.md
git commit -m "docs(benchmarks): capture the pre-transactions throughput baseline

From a temporary nested worktree pinned to the commit immediately
before this series' first commit -- the reference point for Task 2's
<=2% regression gate on unpipelined 3B SET/GET."
```

---

### Task 2: Post-series benchmark, MULTI/EXEC scenario, and the gate verdict

**Files:**
- Create: `docs/benchmarks/<TODAY>-post-transactions-final.md`,
  `scripts/benchmark-transactions.sh`

**Interfaces:**
- Consumes: `docs/benchmarks/<TODAY>-pre-transactions-baseline.md` from Task 1.
- Produces: the gate verdict Task 3's documentation update references.

- [ ] **Step 1: Run the same non-transaction benchmark against the current tip**

From the repo root (the actual branch this plan is executing on, now containing all of Plans
01-03):

```bash
./scripts/benchmark.sh > /tmp/post-transactions-run1.txt 2>&1
./scripts/benchmark.sh > /tmp/post-transactions-run2.txt 2>&1
./scripts/benchmark.sh > /tmp/post-transactions-run3.txt 2>&1
```

- [ ] **Step 2: Write a MULTI/EXEC-specific throughput scenario**

`redis-benchmark`'s `-t` flag has no multi-command-transaction mode, so this uses `redis-cli
--pipe`, which accepts a raw RESP byte stream on stdin and reports elapsed time. Create
`scripts/benchmark-transactions.sh`:

```bash
#!/usr/bin/env bash
# MULTI/EXEC throughput: N complete 2-command transactions, piped as one raw RESP stream via
# redis-cli --pipe (redis-benchmark's -t has no multi-command-transaction mode). Reports
# transactions/sec for both real Redis and rocket-mem, same matched-durability setup as
# scripts/benchmark.sh.
set -euo pipefail

for bin in redis-server redis-cli; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "error: '$bin' is not on PATH. Install a Redis distribution first" >&2
    exit 1
  fi
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REDIS_PORT=7787
ROCKET_PORT=7788
N=20000

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
ROCKET_MEM_METRICS_ADDR="127.0.0.1:9187" \
ROCKET_MEM_RMP_ADDR="127.0.0.1:9188" \
  "$ROOT/target/release/rocket-mem" --config "$WORK/unused.toml" >"$WORK/rocket.log" 2>&1 &
ROCKET_PID=$!

sleep 1
redis-cli -p "$REDIS_PORT" ping >/dev/null
redis-cli -p "$ROCKET_PORT" ping >/dev/null

# One complete transaction: MULTI, a SET against a per-iteration key, EXEC. Built once and
# repeated N times, rather than generated per-key, since a distinct key per transaction would
# make this file O(N) in size for no benchmark-relevant reason -- throughput here is about
# transaction dispatch overhead, not keyspace variety (scripts/benchmark.sh's -r flag already
# covers keyspace-size effects for ordinary commands).
STREAM="$WORK/transactions.resp"
: > "$STREAM"
one_tx=$'*1\r\n$5\r\nMULTI\r\n*3\r\n$3\r\nSET\r\n$3\r\ntxk\r\n$3\r\ntxv\r\n*1\r\n$4\r\nEXEC\r\n'
for _ in $(seq 1 "$N"); do
  printf '%s' "$one_tx" >> "$STREAM"
done

run_case() { # $1=label $2=port
  local start end elapsed
  start=$(date +%s.%N)
  redis-cli -p "$2" --pipe < "$STREAM" >/dev/null
  end=$(date +%s.%N)
  elapsed=$(echo "$end - $start" | bc)
  local per_sec
  per_sec=$(echo "$N / $elapsed" | bc)
  echo "$1: $N transactions in ${elapsed}s (${per_sec} transactions/sec)"
}

echo "--- MULTI/EXEC throughput ($N transactions, 2 commands each) ---"
run_case "redis-server" "$REDIS_PORT"
run_case "rocket-mem" "$ROCKET_PORT"
```

```bash
chmod +x scripts/benchmark-transactions.sh
./scripts/benchmark-transactions.sh | tee /tmp/transactions-benchmark.txt
```

If `bc` is not on `PATH`, replace the two `echo ... | bc` lines with
`awk "BEGIN {print $end - $start}"` / `awk "BEGIN {print $N / $elapsed}"` instead — check with
`command -v bc` before assuming it is available.

- [ ] **Step 3: Write the post-series report and gate verdict**

Create `docs/benchmarks/<TODAY>-post-transactions-final.md`:

```markdown
# Post-Transactions Throughput — Final Result

**Date:** <TODAY>
**Commit:** <current HEAD, short form, from `git rev-parse --short HEAD`>
**Purpose:** the series-final measurement against
[the pre-transactions baseline](<TODAY>-pre-transactions-baseline.md), gating the spec's
requirement that the non-transaction hot path not regress.

## rocket-mem requests/sec (unchanged commands)

| Workload | Run 1 | Run 2 | Run 3 | Mean | vs. baseline |
|---|---|---|---|---|---|
| SET, 3B, no pipeline | ... | ... | ... | ... | ...% |
| GET, 3B, no pipeline | ... | ... | ... | ... | ...% |
| SET, 3B, pipeline=16 | ... | ... | ... | ... | (context only) |
| GET, 3B, pipeline=16 | ... | ... | ... | ... | (context only) |
| SET, 1KB, no pipeline | ... | ... | ... | ... | (context only) |
| GET, 1KB, no pipeline | ... | ... | ... | ... | (context only) |
| SET, 1KB, pipeline=16 | ... | ... | ... | ... | (context only) |
| GET, 1KB, pipeline=16 | ... | ... | ... | ... | (context only) |

(fill from `/tmp/post-transactions-run*.txt`; `vs. baseline` = `(mean - baseline_mean) /
baseline_mean * 100`, computed only for the two gated rows)

## Gate verdict

**PASS** if both `SET, 3B, no pipeline` and `GET, 3B, no pipeline` are within 2% of
[the baseline](<TODAY>-pre-transactions-baseline.md)'s means (below-baseline inside that band is
noise, not regression). **FAIL** — stop and profile before continuing to Task 3 — if either
exceeds it. State the actual verdict and both percentages here; do not leave this section as a
template.

## MULTI/EXEC transaction throughput

From `scripts/benchmark-transactions.sh` (20,000 two-command transactions, matched durability):

- redis-server: `<transactions/sec from /tmp/transactions-benchmark.txt>`
- rocket-mem: `<transactions/sec from /tmp/transactions-benchmark.txt>`

Recorded as a first-time reference number, not gated against anything — there is no prior
rocket-mem `MULTI`/`EXEC` throughput to compare it to. A future series that touches this path
again should treat this file as its own "before" baseline.
```

- [ ] **Step 4: Act on the gate verdict**

If Step 3's verdict is **FAIL**, stop here — do not proceed to Task 3. Profile the regression
(`cargo flamegraph`, following the pattern in `docs/benchmarks/2026-08-30-flamegraph-notes.md`),
identify whether it traces to the new `session.in_transaction.load` check on every command
(Plan 01) or something else, fix it, and re-run this entire task from Step 1 before continuing.
If **PASS**, continue.

- [ ] **Step 5: Confirm the workspace still builds**

```bash
cargo test --workspace 2>&1 | tail -5
```

Expected: PASS, total unchanged at `BASELINE + 27`.

- [ ] **Step 6: Commit**

```bash
git add docs/benchmarks/*-post-transactions-final.md scripts/benchmark-transactions.sh
git commit -m "docs(benchmarks): verify no regression and add a MULTI/EXEC throughput number

Post-series SET/GET (3B, no pipeline) within the <=2% gate against
the pre-series baseline. New scripts/benchmark-transactions.sh gives
a first MULTI/EXEC transactions/sec reference number via redis-cli
--pipe, since redis-benchmark -t has no multi-command mode."
```

---

### Task 3: Documentation and series completion

**Files:**
- Modify: `README.md` ("Command coverage" table), `docs/command-compatibility.md` (coverage
  table + "Known divergences")

**Interfaces:**
- Consumes: nothing from earlier tasks besides their having landed.
- Produces: nothing — this is the series' last task.

- [ ] **Step 1: Confirm the workspace still builds**

```bash
cargo test --workspace 2>&1 | tail -5
```

Expected: PASS, total unchanged at `BASELINE + 27`.

- [ ] **Step 2: Add a Transactions row to `README.md`'s command coverage table**

In `README.md`'s `## Command coverage` table, add a new row directly after the `Auth/ACL` row:

```markdown
| Transactions | `MULTI`, `EXEC`, `DISCARD` (writers-only isolation — see [`docs/command-compatibility.md`](docs/command-compatibility.md) for what that means; no `WATCH`/`UNWATCH` yet) |
```

- [ ] **Step 3: Add the same row and a divergence note to `docs/command-compatibility.md`**

In its `## Command coverage` table, add, after the `Auth/ACL` row:

```markdown
| Transactions | `MULTI`, `EXEC`, `DISCARD` |
```

In its `## Known divergences from real Redis` list, add:

```markdown
- **`MULTI`/`EXEC` gives writers-only isolation, not full read isolation.** A transaction's
  queued commands block any other *write* to a shard they touch for the whole batch, but a
  concurrent *read* can observe intermediate state partway through the batch — impossible in
  real Redis, which is single-threaded. See
  [the transactions spec](superpowers/specs/2026-09-10-multi-exec-transactions-spec.md)'s
  "Rejected: full read+write isolation now" section for why, and what closing this gap would
  require.
- **No `WATCH`/`UNWATCH`.** Optimistic locking needs a per-key change-tracking primitive this
  engine does not have yet — a deferred follow-up, not an oversight.
```

- [ ] **Step 4: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, total unchanged at `BASELINE + 27` (docs-only task).

- [ ] **Step 5: Commit**

```bash
git add README.md docs/command-compatibility.md
git commit -m "docs: mark MULTI/EXEC/DISCARD as released, note the isolation gap

Adds a Transactions row to both command-coverage tables and documents
the writers-only-isolation and missing-WATCH gaps as deliberate, per
the transactions spec's explicit scope decisions."
```

---

## Next plan

None — this is the final plan in the series. `WATCH`/`UNWATCH` and true read isolation, both
explicitly deferred by the spec, are the natural next series if this feature sees real use;
neither has a spec yet.
