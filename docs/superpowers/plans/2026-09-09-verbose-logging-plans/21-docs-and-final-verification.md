# Verbose Logging Plan 21: Documentation and Final Verification

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the verbose logging series: document the logging capability for operators,
run the full verification sweep (fmt/clippy/test/benchmark/manual) against the whole
series' cumulative cost, and audit the spec's Event catalogue row by row to confirm every
promised event actually landed.

**Architecture:** This plan changes no runtime behavior — it is documentation, verification,
and audit. Task 1 writes the operator-facing "how do I turn this on" story that plans 01-20
never had a natural home for, building on top of the `log_value_max_bytes` rows and warning
plan 04 already added to `README.md`'s config table and `docs/config-reference.md` rather
than repeating them. Task 2 is the acceptance gate the whole series has been building
toward: every prior plan's Global Constraints section gates that *one plan's* throughput at
≤2% against the baseline, but twenty individually-passing 2% results can still compound past
2% cumulatively — that compounded number, not any single plan's, is what Task 2 measures
against [`docs/benchmarks/2026-09-09-pre-logging-baseline.md`](../../benchmarks/2026-09-09-pre-logging-baseline.md)
(plan 01, Task 1). Task 3 is a mechanical audit: grep every subsystem the spec's Event
catalogue names and confirm the promised event actually exists in code, rather than trusting
that twenty plans executed over time did not quietly drop one.

**Tech Stack:** Rust 2021, `cargo fmt`/`clippy`/`test`, `scripts/benchmark.sh`, `redis-cli`.

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md)

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting.

---

### Task 1: Document the logging capability

**Files:**
- Modify: `README.md` — the "Project status" blockquote (~line 26-28) and a new `## Logging`
  section inserted after the "On-disk files after a `BGREWRITEAOF`" subsection and before
  `## Deployment` (currently ~line 294-296)

**Interfaces:**
- Consumes: the level taxonomy, the three-span structure, and the fixed field vocabulary the
  spec defines (implemented across plans 02-20); the `log_value_max_bytes` config rows and
  trace-level warning plan 04 already added to `README.md`'s config table and
  `docs/config-reference.md` — this task links to that warning rather than repeating it.
- Produces: nothing consumed by other code. This is the operator-facing documentation
  deliverable the series has been building toward.

- [ ] **Step 1: Confirm plan 04's docs already landed**

```bash
grep -n "log_value_max_bytes" README.md docs/config-reference.md
```

Expected: exactly one hit in each file. If either comes back empty, stop — plan 04 has not
actually been executed yet, and this task's instruction to build on it rather than duplicate
it has nothing to build on.

- [ ] **Step 2: Compute the current test count for the status line**

```bash
cargo test --workspace 2>&1 | tee /tmp/rocket-mem-final-test-run.txt | \
  grep -E "^test result:" | sed -E 's/.*; ([0-9]+) passed.*/\1/' | \
  awk '{sum += $1} END {print sum}'
```

This sums the "N passed" figure across every `test result:` line printed workspace-wide —
one per test binary. Use the printed number in place of `<N>` in Step 3. (The README
currently says "829 tests"; that number predates this entire series and every prior plan's
new tests, so it is stale by now regardless of this series' outcome.)

- [ ] **Step 3: Update the Project status blockquote**

In `README.md`, replace:

```markdown
> **Project status.** rocket-mem is complete and tested — 829 tests, durability verified under a
> `kill -9` chaos loop — but it is not yet production-hardened: there is no failover and no live
> resharding. Read [Limitations](#limitations) before deploying it.
```

with (substituting the real number from Step 2 for `<N>`):

```markdown
> **Project status.** rocket-mem is complete and tested — <N> tests, durability verified under a
> `kill -9` chaos loop — but it is not yet production-hardened: there is no failover and no live
> resharding. Read [Limitations](#limitations) before deploying it. Every subsystem — dispatch,
> engine, protocol codecs, AOF, replication, cluster routing — emits leveled activity logs from
> `info` milestones down to `trace`-level command and value contents; see [Logging](#logging).
```

- [ ] **Step 4: Insert the Logging section**

In `README.md`, directly beneath the "On-disk files after a `BGREWRITEAOF`" subsection's
last paragraph (ending "...unreferenced and safe to delete.") and above the `## Deployment`
heading, insert:

```markdown
## Logging

rocket-mem logs through [`tracing`](https://docs.rs/tracing), writing structured, leveled
lines to stderr. Three levels matter:

| Level | Meaning | What you get |
|---|---|---|
| `info` | Milestones | Startup, resolved config, listener bound, connection accept/close, AOF rewrite, snapshot save/load, replica register/prune — safe to leave on in production. |
| `debug` | What happened | One line per dispatched command, AOF offsets, active-expire cycle results, cluster routing decisions, PSYNC handshake steps. |
| `trace` | The bytes | Argument and value contents (capped by `log_value_max_bytes`), shard routing, codec frame decode, replication stream offsets. |

`error` and `warn` sit above `info` as usual, for durability/correctness failures and for
recovered or client-caused anomalies respectively.

### Setting the level

Four ways, in precedence order — the first one set wins:

1. **`RUST_LOG`** — standard `tracing_subscriber::EnvFilter` syntax, e.g. `RUST_LOG=debug`.
   Always wins over every other source below.
2. **`ROCKET_MEM_LOG_LEVEL`** — same syntax, layered like every other `ROCKET_MEM_*`
   environment variable.
3. **`--log-level`** — the CLI flag, e.g. `--log-level debug`.
4. **`log_level`** in `rocket-mem.toml` — the lowest-precedence source; `"info"` if nothing
   above overrides it.

Per-module targeting uses the same `EnvFilter` directive syntax in any of the four sources
above — turn up one subsystem without paying for a firehose everywhere else:

```bash
# trace everything replication-related, info for the rest of the server
RUST_LOG=rocket_mem::replication=trace,info ./target/release/rocket-mem

# debug the dispatcher and the engine, leave protocol codecs at info
RUST_LOG=rocket_mem::dispatcher=debug,engine=debug,info ./target/release/rocket-mem
```

### Spans and fields

Three spans carry correlation through nested log lines — everything logged inside one
inherits its fields for free, so a single `grep` on a `conn_id` or `cmd` follows an activity
from accept to reply:

| Span | Where it opens | Fields |
|---|---|---|
| `conn` | Once per connection, in `connection.rs`/`rmp_connection.rs`'s `handle_connection` | `conn_id`, `peer`, `protocol`, `tls` |
| `cmd` | Once per dispatched command, in `dispatcher.rs`'s `dispatch_and_log` | `cmd`, `key`, `argc` |
| `repl` | Once per replication session, in the replication client loop and `serve_replica` | `host_port` |

One fixed field vocabulary is used across every crate and subsystem, rather than each module
inventing its own names, specifically so a single `grep` follows an activity end to end:

`conn_id`, `peer`, `cmd`, `key`, `argc`, `user`, `error`, `elapsed_us`, `shard`, `offset`,
`bytes`.

### `trace` is a plaintext copy of your data

At `trace`, rocket-mem logs command arguments and value contents (truncated by
`log_value_max_bytes` — see the [Configuration](#configuration) table). Credentials are
always redacted (`AUTH`, `HELLO ... AUTH`, `ACL SETUSER`, `ACL GETUSER`, `REPLICAOF ... AUTH`
render `<redacted>` at every level, not just `trace`), but ordinary keys and values are not.
Treat a `trace`-level log file with the same retention and access controls as the dataset
itself — the full warning and the `log_value_max_bytes` field are documented in
[`docs/config-reference.md`](docs/config-reference.md#fields).
```

- [ ] **Step 5: Verify**

```bash
grep -n "^## Logging" README.md
grep -n "conn_id" README.md
grep -c "^## " README.md
```

Expected: one hit for the section heading, at least one hit for `conn_id`, and the heading
count one higher than before this task (a new top-level section was added, nothing else was
restructured).

- [ ] **Step 6: Commit**

```bash
git add README.md
git commit -m "docs: document the logging capability - levels, spans, and field vocabulary"
```

---

### Task 2: Full verification sweep

**Files:**
- Create: `docs/benchmarks/2026-09-09-post-logging-final.md`

**Interfaces:**
- Consumes: `docs/benchmarks/2026-09-09-pre-logging-baseline.md`'s Mean column (plan 01,
  Task 1) as the comparison point.
- Produces: the series' final acceptance record — the one benchmark result that reflects the
  whole series' cumulative cost, not any single plan's.

- [ ] **Step 1: fmt, clippy, full test suite**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three exit `0` with no warnings and no failures — the same three commands
every prior plan's Global Constraints required individually, run once more across the
entire accumulated diff of all 20 preceding plans.

- [ ] **Step 2: Benchmark three times at default `info` level, compute the cumulative regression**

```bash
cd /home/numericlabs/data/rocket/rocket-mem
for i in 1 2 3; do
  echo "=== run $i ==="
  ./scripts/benchmark.sh
done 2>&1 | tee /tmp/rocket-mem-post-logging.txt
```

Read the baseline's Mean `SET`/`GET` requests/sec from
`docs/benchmarks/2026-09-09-pre-logging-baseline.md`, compute this run's mean from the three
new runs, and compute `(baseline_mean - new_mean) / baseline_mean * 100` for each workload.
Write `docs/benchmarks/2026-09-09-post-logging-final.md`:

```markdown
# Post-Logging Throughput — Final Cumulative Result

**Date:** 2026-09-09
**Commit:** <output of `git rev-parse --short HEAD`>
**Purpose:** the series-final measurement against
[the pre-logging baseline](2026-09-09-pre-logging-baseline.md), per
[the verbose logging spec](../superpowers/specs/2026-09-09-verbose-logging-design.md)'s
benchmark gate. Every plan from 01 to 20 gated its *own* throughput delta at ≤2%; this is
the cumulative number across all of them, since twenty individually-passing deltas can still
compound past the gate.

**Harness:** `scripts/benchmark.sh`, three consecutive runs, matched durability
(`--appendonly yes --appendfsync everysec` on both servers), default `info` log level (no
`RUST_LOG` set).

## rocket-mem requests/sec

| Workload | Run 1 | Run 2 | Run 3 | Mean | Baseline mean | Δ |
|---|---|---|---|---|---|---|
| SET | | | | | | |
| GET | | | | | | |

## Gate

**Cumulative regression must be ≤2% against the pre-logging baseline.** If either workload's
Δ exceeds 2%, this task is not done: do not write a passing verdict here. Instead, `git log
--oneline` the 20 plans' commits, re-run `scripts/benchmark.sh` at a few commits along that
range (e.g. via `git stash`/`git checkout <sha> -- .` on a scratch worktree, never on the
working tree this plan is executing in) to localize which plan's instrumentation the
regression tracks back to, and report that plan and the measured delta instead of silently
accepting a number over the line.

**Verdict:** <PASS — within 2% | FAIL — see localization above>

## Manual verification at info / debug / trace

<filled in by Step 3 below>

## Raw output

<paste the full tee'd output of the three runs here>
```

- [ ] **Step 3: Live manual run at `info`, `debug`, and `trace`**

Exercise a normal `SET`/`GET`, an `AUTH` (verify redaction), a `WRONGTYPE` error, and a key
expiry, once per level, checking what each level does and does not show. Use a throwaway ACL
config so `AUTH` has a real user to authenticate against:

```bash
cat > /tmp/rocket-mem-verify-acl.toml <<'EOF'
[[acl.users]]
username = "alice"
password = "s3cret"
commands = "allcommands"
keys = "allkeys"
EOF
```

**At `info`** (expect no per-command lines, and the password never appears):

```bash
RUST_LOG=info ./target/release/rocket-mem --config /tmp/rocket-mem-verify-acl.toml \
  --addr 127.0.0.1:16399 --rmp-addr 127.0.0.1:16400 --metrics-addr 127.0.0.1:16401 \
  --aof-path /tmp/rocket-mem-verify-info.aof --snapshot-path /tmp/rocket-mem-verify-info.snapshot \
  2>/tmp/rocket-mem-info.log &
SERVER_PID=$!
sleep 1
redis-cli -p 16399 SET foo bar
redis-cli -p 16399 GET foo
redis-cli -p 16399 AUTH alice s3cret
redis-cli -p 16399 LPUSH mylist a
redis-cli -p 16399 GET mylist                 # WRONGTYPE
redis-cli -p 16399 SET expkey v PX 100
sleep 0.3
redis-cli -p 16399 GET expkey                 # gone
kill "$SERVER_PID"; wait "$SERVER_PID" 2>/dev/null

grep -c 'cmd=' /tmp/rocket-mem-info.log       # expect 0 -- info emits no per-command lines
grep -n 's3cret' /tmp/rocket-mem-info.log     # expect no hits -- redaction is unconditional
```

**At `debug`** (expect per-command lines and the `WRONGTYPE` error, still no password):

```bash
RUST_LOG=debug ./target/release/rocket-mem --config /tmp/rocket-mem-verify-acl.toml \
  --addr 127.0.0.1:16399 --rmp-addr 127.0.0.1:16400 --metrics-addr 127.0.0.1:16401 \
  --aof-path /tmp/rocket-mem-verify-debug.aof --snapshot-path /tmp/rocket-mem-verify-debug.snapshot \
  2>/tmp/rocket-mem-debug.log &
SERVER_PID=$!
sleep 1
redis-cli -p 16399 SET foo bar
redis-cli -p 16399 GET foo
redis-cli -p 16399 AUTH alice s3cret
redis-cli -p 16399 LPUSH mylist a
redis-cli -p 16399 GET mylist
kill "$SERVER_PID"; wait "$SERVER_PID" 2>/dev/null

grep -n 'cmd=.*SET' /tmp/rocket-mem-debug.log     # expect a hit -- per-command line now shows
grep -n 'WRONGTYPE' /tmp/rocket-mem-debug.log     # expect a hit
grep -n 'redacted' /tmp/rocket-mem-debug.log      # expect a hit for the AUTH line
grep -n 's3cret' /tmp/rocket-mem-debug.log        # expect no hits -- still redacted
```

**At `trace`** (expect value contents and the expiry event, password still never appears):

```bash
RUST_LOG=trace ./target/release/rocket-mem --config /tmp/rocket-mem-verify-acl.toml \
  --addr 127.0.0.1:16399 --rmp-addr 127.0.0.1:16400 --metrics-addr 127.0.0.1:16401 \
  --aof-path /tmp/rocket-mem-verify-trace.aof --snapshot-path /tmp/rocket-mem-verify-trace.snapshot \
  2>/tmp/rocket-mem-trace.log &
SERVER_PID=$!
sleep 1
redis-cli -p 16399 SET foo bar
redis-cli -p 16399 GET foo
redis-cli -p 16399 AUTH alice s3cret
redis-cli -p 16399 SET expkey v PX 100
sleep 0.3
redis-cli -p 16399 GET expkey
kill "$SERVER_PID"; wait "$SERVER_PID" 2>/dev/null

grep -n '\bbar\b' /tmp/rocket-mem-trace.log     # expect a hit -- trace renders value contents
grep -n 'expir' /tmp/rocket-mem-trace.log       # expect a hit for the per-key TTL expiry event
grep -n 's3cret' /tmp/rocket-mem-trace.log      # expect no hits -- redaction holds even at trace
```

Record the actual grep results (hit / no hit, and the matched line where useful) under the
"Manual verification" heading of `docs/benchmarks/2026-09-09-post-logging-final.md` from
Step 2. Any unexpected result (a per-command line at `info`, a password anywhere, no value
content at `trace`) is a bug in an earlier plan's implementation — not something to fix
silently inside this verification task; report it against the plan that introduced it.

- [ ] **Step 4: Commit**

```bash
git add docs/benchmarks/2026-09-09-post-logging-final.md
git commit -m "docs: record final cumulative benchmark and manual verification"
```

---

### Task 3: Spec-coverage audit

**Files:** none by default. If the audit finds a gap and it is a trivial one-liner, the fix
lands in whichever file the corresponding grep below names, following that file's own
existing call-site pattern for events at the same level.

**Interfaces:**
- Consumes: every source file the spec's Event catalogue table names (the table at
  `../../specs/2026-09-09-verbose-logging-design.md#event-catalogue`).
- Produces: a pass/fail per catalogue row. This is the series' closing correctness check —
  it exists so "twenty plans executed in sequence" is verified, not assumed.

- [ ] **Step 1: List every tracing call site the catalogue's files actually contain**

```bash
grep -n 'tracing::\(error\|warn\|info\|debug\|trace\)!' \
  crates/server/src/main.rs \
  crates/server/src/connection.rs crates/server/src/rmp_connection.rs \
  crates/server/src/dispatcher.rs \
  crates/server/src/acl.rs \
  crates/engine/src/engine.rs crates/engine/src/shard.rs crates/engine/src/store.rs \
  crates/protocol/src/codec.rs crates/protocol/src/rmp.rs \
  crates/server/src/aof.rs \
  crates/engine/src/snapshot.rs \
  crates/server/src/replication.rs \
  crates/server/src/cluster.rs \
  crates/server/src/slowlog.rs \
  crates/server/src/metrics.rs
```

```bash
# the three spans -- opened via span!/#[instrument], not a plain log macro
grep -rn 'span!(\|#\[instrument' \
  crates/server/src/connection.rs crates/server/src/rmp_connection.rs \
  crates/server/src/dispatcher.rs crates/server/src/replication.rs
```

```bash
# redact_args/fmt_value must be *called*, not just defined in logging.rs
grep -n 'redact_args(\|fmt_value(' crates/server/src/dispatcher.rs
```

- [ ] **Step 2: Cross-reference each catalogue row against the grep output**

Go row by row through the spec's Event catalogue table and confirm each promised event has
a corresponding hit above. Use this checklist (file → what to look for):

| Row | File(s) | Look for in Step 1's output |
|---|---|---|
| Startup | `main.rs` | `resolved config summary`; `listener bound` (multiple hits); no `shutdown` hit — expected, see plan 20 Task 3's deferral comment |
| Connection | `connection.rs`, `rmp_connection.rs` | a `debug!` for the `HELLO`/RESP3 upgrade; an `info!` for clean close/EOF; a `conn` span in the span grep |
| Dispatch | `dispatcher.rs` | `debug!` per command; `trace!` for full arguments; a `cmd` span; `redact_args`/`fmt_value` called |
| ACL | `acl.rs` | `info!` for auth success and `SETUSER`/`DELUSER`; `warn!` for auth failure and permission denied |
| Engine | `engine.rs`, `shard.rs`, `store.rs` | `trace!` for shard routing, byte delta, per-key TTL expiry; `debug!` for active-expire cycle count; `warn!` for eviction |
| Protocol | `codec.rs`, `rmp.rs` | `trace!` for frame decode and split-read reassembly; `warn!` for protocol error |
| AOF | `aof.rs` | `trace!` for offset/bytes; `debug!` for fsync; `info!` for rewrite start/finish and recovery replay summary |
| Snapshot | `snapshot.rs`, `aof.rs` | `info!` for save/load start and finish |
| Replication | `replication.rs` | `debug!` for PSYNC steps and apply; `info!` for register/prune; `trace!` for offset progress; a `repl` span |
| Cluster | `cluster.rs`, `dispatcher.rs` | `info!` for topology loaded; `debug!` for MOVED redirect |
| Slowlog | `slowlog.rs` | `warn!` for entry recorded |
| Metrics | `metrics.rs` | `trace!` for scrape served |

A row with zero matching hits is a gap.

- [ ] **Step 3: Resolve or report every gap found**

For each gap: if it is a single missing `tracing::<level>!` call whose neighboring code in
that same file already shows the pattern to follow (an adjacent event at a related level in
the same function), add it, matching the existing sigil (`%`/`?`) and field-name conventions
in that file, then re-run the relevant grep from Step 1 to confirm it now shows up.

If a gap is not a trivial one-liner — an entire missing subsystem's worth of instrumentation,
not one overlooked call site — do not implement it inside this task. Report it plainly
instead: which row, which file, what's missing. Silently expanding this closing-verification
task into a full implementation task for a dropped plan is the same scope-creep failure mode
Task 3 of plan 20 was written to avoid for `shutdown` — the fix belongs in a proper follow-up
plan, not folded invisibly into the audit that discovered it.

- [ ] **Step 4: Commit, if and only if a gap was fixed**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add <files touched in Step 3>
git commit -m "fix(logging): add missing <event> found by the spec-coverage audit"
```

If the audit found no gaps, there is nothing to commit for this task — that is the expected,
successful outcome. Do not create a no-op commit to satisfy a checklist item; instead record
the clean audit result (which rows were checked, all matched) in the PR description or
session notes.

---

## Next plan

None — this is the final plan in the series.

Deliberate follow-ups remain, all named in the spec's "Out of scope" section rather than
overlooked: a JSON log output format (the single text format is retained for now), log file
rotation and alternate destinations (stderr redirection stays the operator's job),
runtime-reloadable level / `CONFIG SET loglevel` (deferred pending its own spec once this
series' real cost is known from Task 2's numbers), OpenTelemetry / distributed trace export,
and sampling (rejected outright unless the benchmark gate later proves unmeetable, since
sampling would make the `debug` log lie about what happened). None of these are implied by
anything this plan closes out — they are separate specs, if and when they are wanted.
