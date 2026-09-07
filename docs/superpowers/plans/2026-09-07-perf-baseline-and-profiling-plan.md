# Fair Baseline Benchmark and Profiling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** produce two things the fix-selection work in Phase 3 of the spec depends on: (1) a fair, matched-durability, post-stall-fix benchmark of the full 8-row matrix, replacing the stale AOF-off comparison; (2) three separate, kernel-symbol-resolved flamegraph profiles (one per workload shape) that can actually attribute the unidentified 1.96%-self-time `Mutex` contention and, if possible, the 58x pipelined-1KB-GET anomaly — both left unresolved by the prior single-continuous-recording profile.

**Architecture:** no code changes in this plan — it produces benchmark output and profiling data as committed docs. `scripts/benchmark.sh` already runs both servers with matched durability (`--appendonly yes --appendfsync everysec` on `redis-server`; rocket-mem's AOF is always on) across the exact 8-row matrix, so Task 1 is "run it and write up the result," not "build a new script." Task 2 captures three separate `cargo flamegraph` recordings instead of one continuous one, so each workload's samples can be attributed individually — the specific limitation the prior profiling notes flagged.

**Tech Stack:** `redis-benchmark`, `cargo-flamegraph`, `perf` (kernel symbol resolution now unlocked: `kptr_restrict=0`, `perf_event_paranoid=-1`, confirmed via `/proc/sys/kernel/*`).

**Spec:** `docs/superpowers/specs/2026-09-07-redis-parity-perf-design.md` (Phases 1-2)

## Global Constraints

- **Depends on the AOF blocking-I/O fix** (`docs/superpowers/plans/2026-09-07-aof-blocking-fix-plan.md`) **landing first** — Task 1's baseline is meant to measure the *post-fix* state; running it before that fix lands would just re-record the already-diagnosed stall bug instead of establishing the real baseline Phase 3's fixes get measured against.
- No code changes in this plan. If a step's output reveals something that needs a code change, record it as a finding for the next plan — do not fix it inline here.
- Every produced artifact (benchmark output, flamegraph SVGs, analysis write-up) gets committed, following the existing `docs/benchmarks/` convention (see `2026-08-30-redis-benchmark.md` and `2026-08-30-flamegraph-notes.md` for the established format).
- Use `2026-09-07` as this plan's date prefix for new files under `docs/benchmarks/`, adjusted to the actual date if this plan is executed later.

---

### Task 1: Fair baseline benchmark, full 8-row matrix

**Files:**
- Create: `docs/benchmarks/2026-09-07-redis-benchmark.md`
- Modify: `README.md`'s Performance table (currently lines 128-137, may have shifted — locate by the `## Performance` heading)

**Interfaces:**
- Consumes: `scripts/benchmark.sh` (unmodified), the fixed `AofWriter` from the prior plan.
- Produces: a committed benchmark report other work (including Task 2's profiling and any future Phase 3 fix) compares against as "current state."

- [ ] **Step 1: Run the benchmark script**

Run: `./scripts/benchmark.sh 2>&1 | tee /tmp/rocket-mem-baseline-2026-09-07.txt`
Expected: it builds rocket-mem in release mode, starts both servers with matched durability, and prints 8 `redis-benchmark -q` result blocks (SET/GET × 3B/1024B × pipeline 1/16) plus a version/host header and a final `/metrics` sample. Takes a few minutes (release build + 8 benchmark runs).

- [ ] **Step 2: Write the report**

Create `docs/benchmarks/2026-09-07-redis-benchmark.md`, following the structure of `docs/benchmarks/2026-08-30-redis-benchmark.md` (same section headings: `## Setup`, `## Results`, `## Where we are slower, and why`, `## Where we are faster, if anywhere`, `## What this does not measure`). Populate:
- The version/host/date header from Step 1's output.
- The 8-row results table (`req/s` per `redis-benchmark`'s `-q` output line, ratio = redis ÷ rocket).
- An explicit note that this run is **post-AOF-blocking-fix** and **matched-durability** (`appendonly yes`, `appendfsync everysec` on both), contrasting it with the manual session runs from 2026-09-07 that showed `redis-server` with `appendonly: no` and rocket-mem hitting 100-495ms max-latency stalls — state plainly whether those stalls are gone in this run (check the `-q` output doesn't surface max latency directly; if a closer look at tail latency is needed, note that as a gap for a follow-up run with `--latency-history` or the non-`-q` percentile output `scripts/benchmark.sh` doesn't currently capture, rather than silently skipping it).
- Which of the 8 rows still show a gap, and roughly how large, compared to `docs/benchmarks/2026-08-30-redis-benchmark.md`'s numbers (same shape of analysis: is the gap proportional across GET/SET like before, or has the picture changed now that the stall bug is fixed).

- [ ] **Step 3: Update README's Performance table**

Replace the table under `## Performance` in `README.md` with this run's numbers, and update the intro line above the table to read `redis-server 8.10.1` (or whatever version Step 1's header reports) with `appendonly yes, appendfsync everysec` **on both servers** — remove any wording that implies only rocket-mem had durability on, since this run fixes that asymmetry. Link to the new `docs/benchmarks/2026-09-07-redis-benchmark.md` report.

- [ ] **Step 4: Commit**

```bash
git add docs/benchmarks/2026-09-07-redis-benchmark.md README.md
git commit -m "$(cat <<'EOF'
docs(bench): fair post-fix baseline, matched durability both sides

Re-runs the full 8-row redis-benchmark matrix after the AOF
blocking-I/O fix, with redis-server's appendonly/appendfsync
matched to rocket-mem's always-on AOF for the first time -- the
prior recorded numbers had redis-server's durability off.
EOF
)"
```

---

### Task 2: Three separate kernel-symbol-resolved flamegraph captures

**Files:**
- Create: `docs/benchmarks/2026-09-07-flamegraph-unpipelined-3b.svg`, `docs/benchmarks/2026-09-07-flamegraph-pipelined-3b.svg`, `docs/benchmarks/2026-09-07-flamegraph-pipelined-1kb.svg`
- Create: `docs/benchmarks/2026-09-07-flamegraph-notes.md`

**Interfaces:**
- Consumes: `kernel.kptr_restrict=0` and `kernel.perf_event_paranoid=-1` (already set — confirmed via `cat /proc/sys/kernel/kptr_restrict` returning `0` and `cat /proc/sys/kernel/perf_event_paranoid` returning `-1`), the fixed `AofWriter` from the prior plan.
- Produces: the profiling data Phase 3's fix-selection work reads to decide what to fix and in what order — specifically, which of `AofWriter::lock_for_ordering`, `SlowLog`'s mutex, or `ReplicaRegistry::senders` the unattributed 1.96% `std::sync::Mutex::lock_contended` self-time belongs to, and whether the pipelined-1KB-GET recording shows a distinct hot path explaining the 58x anomaly.

- [ ] **Step 1: Capture the unpipelined 3B recording**

```bash
cd /home/numericlabs/data/rocket/rocket-mem
mkdir -p /tmp/rocket-mem-fg && rm -f /tmp/rocket-mem-fg/perf.data
ROCKET_MEM_ADDR=127.0.0.1:7778 \
ROCKET_MEM_AOF_PATH=/tmp/rocket-mem-fg/rocket.aof \
ROCKET_MEM_SNAPSHOT_PATH=/tmp/rocket-mem-fg/rocket.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9178 \
  cargo flamegraph --release --bin rocket-mem \
    --output docs/benchmarks/2026-09-07-flamegraph-unpipelined-3b.svg \
    --deterministic -- 2>&1 | tee /tmp/rocket-mem-fg/unpipelined-3b.log &
FLAME_PID=$!
sleep 3
redis-cli -p 7778 ping
redis-benchmark -h 127.0.0.1 -p 7778 -t set,get -n 200000 -c 50 -d 3 -q
kill -INT "$FLAME_PID"
wait "$FLAME_PID" || true
cp /tmp/rocket-mem-fg/perf.data /tmp/rocket-mem-fg/perf-unpipelined-3b.data
```

Expected: `docs/benchmarks/2026-09-07-flamegraph-unpipelined-3b.svg` exists and is non-trivially sized (several hundred KB to a few MB, consistent with the prior profile's SVG).

- [ ] **Step 2: Capture the pipelined 3B recording**

Same as Step 1 but with `-P 16` added to the `redis-benchmark` line, output SVG `docs/benchmarks/2026-09-07-flamegraph-pipelined-3b.svg`, and `cp ... perf-pipelined-3b.data` at the end. Kill and restart the server fresh between captures (new `FLAME_PID`, fresh `/tmp/rocket-mem-fg/perf.data`) so each recording is isolated — this is the specific change from the prior methodology, which combined all three phases into one continuous recording and couldn't attribute samples to any one of them afterward.

- [ ] **Step 3: Capture the pipelined 1KB recording (the anomaly reproduction)**

Same as Step 1 but with `-d 1024 -P 16` on the `redis-benchmark` line, output SVG `docs/benchmarks/2026-09-07-flamegraph-pipelined-1kb.svg`, `perf-pipelined-1kb.data`. Note the `redis-benchmark` summary line's GET req/s in the step log — expect it near the previously recorded ~19,440-19,543 req/s if the anomaly is still present (confirms reproduction before analyzing the profile; if the AOF blocking-I/O fix incidentally changed this number, that itself is a finding worth recording, not something to treat as noise).

- [ ] **Step 4: Analyze each recording with resolved kernel symbols**

For each of the three `perf-*.data` files:

```bash
perf report -i /tmp/rocket-mem-fg/perf-unpipelined-3b.data --stdio --sort=overhead,symbol -g none | head -60
perf report -i /tmp/rocket-mem-fg/perf-unpipelined-3b.data --stdio -g graph,0.5,caller | head -100
```

(repeat for `perf-pipelined-3b.data` and `perf-pipelined-1kb.data`). With `kptr_restrict=0`, kernel frames should now resolve to real symbol names instead of `[unknown]` — confirm this by checking the flat report no longer shows the long `0xffffffff...` chains the prior notes recorded. Specifically look for:
- Which of `AofWriter::lock_for_ordering`, `SlowLog::maybe_record`'s mutex, or `ReplicaRegistry::broadcast`'s mutex actually shows up as the caller of the contended `std::sync::Mutex` frame (the prior profile could only see the leaf, not the caller, due to the DWARF-unwind failure this unlock fixes).
- Whether `perf-pipelined-1kb.data` has a hot call path absent from the other two recordings — if the anomaly is TCP/socket-write-path related as the prior notes hypothesized, it should now be visible by name instead of as unresolved kernel time.

- [ ] **Step 5: Write the findings**

Create `docs/benchmarks/2026-09-07-flamegraph-notes.md`, structured like `docs/benchmarks/2026-08-30-flamegraph-notes.md` (same section headings). Cover: confirmation that kernel symbols resolved this time (contrast with the prior "badly degraded" caveat), the Mutex-contention attribution (name the actual struct/lock, or state plainly if it's still unattributable and why), and the 58x anomaly's call path (name the actual hot function(s), or state plainly that the cause lies outside rocket-mem's own code — e.g. in the kernel network stack — if that's what the resolved symbols show).

- [ ] **Step 6: Commit**

```bash
git add docs/benchmarks/2026-09-07-flamegraph-unpipelined-3b.svg \
        docs/benchmarks/2026-09-07-flamegraph-pipelined-3b.svg \
        docs/benchmarks/2026-09-07-flamegraph-pipelined-1kb.svg \
        docs/benchmarks/2026-09-07-flamegraph-notes.md
git commit -m "$(cat <<'EOF'
docs(bench): three isolated flamegraph captures with resolved kernel symbols

Replaces the prior single continuous recording (which couldn't
attribute samples to any one workload) with three separate captures,
now with kptr_restrict/perf_event_paranoid unlocked so kernel call
stacks resolve. Attributes the previously-unidentified 1.96%
std::sync::Mutex contention and investigates the 58x pipelined-1KB-
GET anomaly with real call-graph data for the first time.
EOF
)"
```

---

## Self-Review Notes

- **Spec coverage:** implements the spec's "Decision: profile with resolved kernel symbols, three separate recordings" section in full, and the fair-baseline half of "Decision: fair baseline and iteration loop." The rest of that decision (iterate after each Phase 3 fix) is out of this plan's scope by design — there's nothing to iterate against yet.
- **Placeholder scan:** none in the executable steps. Step 4/5's analysis steps necessarily describe *what to look for* rather than the exact finding (the finding doesn't exist until the command runs) — this is the accepted shape for an investigation task, not a code-implementation placeholder; every actual command is concrete and complete.
- **Explicitly not in this plan:** any fix for whatever Task 2 finds. Per the spec's own sequencing and the "No Placeholders" rule, a plan can't pre-write TDD steps for a code change whose target isn't known yet. Once Task 2's findings exist, write a new plan (or several, one per fix) the same way `2026-09-07-aof-blocking-fix-plan.md` was written for the already-diagnosed stall bug — from real file:line evidence, not speculation.
