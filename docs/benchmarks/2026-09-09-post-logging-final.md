# Post-Logging Throughput — Final Cumulative Result

**Date:** 2026-09-09 (benchmark captured 18:57–18:59 UTC)
**Commit:** `35bb708`
**Purpose:** the series-final measurement against
[the pre-logging baseline](2026-09-09-pre-logging-baseline.md), per
[the verbose logging spec](../superpowers/specs/2026-09-09-verbose-logging-design.md)'s
benchmark gate. Every plan from 01 to 22 gated its *own* throughput delta; this is the
cumulative number across all of them, since twenty-odd individually-passing deltas can still
compound past the gate.

---

## ⚠️ Contamination notice — read before using any number in this document

**These are NOT clean numbers.** Six hand-started `rocket-mem` processes (3 cluster shards + 3
replicas) were serving on this machine, on `192.168.1.12` ports 6379/7379/9121/16379/17379,
throughout every one of the seven benchmark iterations below. They were left running
deliberately: the user was told the measurement would be contaminated and asked for the run
anyway. They were never killed, signalled, or restarted, and all six were still alive and
untouched at the end of this task.

What that means for a reader:

- The benchmark server and the six foreign nodes shared CPU, memory bandwidth, page cache, and
  the loopback/network stack.
- Measured at capture time, each of the six sat at **0.1% CPU** (idle replicas exchanging
  replication heartbeats), so the CPU contention they contribute is small — but "small" is not
  "zero", and their memory and cache footprint is not measured at all.
- **Do not quote these figures as rocket-mem's clean throughput, and do not compare them
  against any other document's numbers as though both were captured on a quiet machine.**

A second, larger contamination source is documented under
[Interpretation](#interpretation--honest-reading-of-the-result): the unchanged `redis-server`
control in this same harness also measured well below *its* own baseline, which says the
machine itself was slower during this capture than during the 2026-09-09 morning baseline.

---

## Deviation from the plan's template

Three deliberate departures from plan 21, Task 2's template, each recorded here rather than
made silently:

1. **The plan's Step 2 says `cd /home/numericlabs/data/rocket/rocket-mem`. That instruction is
   wrong and was not followed.** That path is the main repo checkout, which does not contain
   this logging series at all; benchmarking it would have measured pre-series code. The harness
   was instead invoked as
   `/home/numericlabs/data/rocket/rocket-mem/.claude/worktrees/verbose-logging/scripts/benchmark.sh`,
   from this worktree. No edit to the script was needed: `scripts/benchmark.sh` derives its
   `ROOT` from its own location (`ROOT="$(cd "$(dirname "$0")/.." && pwd)"`), so invoking this
   worktree's copy builds and benchmarks this worktree's binary.

2. **Seven iterations, not three, and spread is reported alongside every mean.** Three runs
   cannot separate a 2% regression from this harness's noise, which the series measured at 6–9%
   typical on the gated rows and which once produced a 5.91-point swing between two triplets at
   the *same commit* — larger than the 5.97-point effect it was being used to judge. Every table
   below therefore carries Min / Max / Spread columns, and no mean is reported alone.

3. **The harness was verified safe before running, not after.** `scripts/benchmark.sh` binds
   `127.0.0.1` on ports 7777 (its own `redis-server`), 7778 (rocket-mem), 9178 (metrics) and
   9179 (RMP), and writes the AOF, snapshot and Redis dir into a `mktemp -d` scratch directory
   it removes on exit. All four ports were confirmed free before the first run, and none of them
   overlaps the six live nodes' `192.168.1.12` ports. Its `cleanup` trap kills only the two PIDs
   it started itself. No `systemctl` was run and no pre-existing process was signalled.

## The gate this document is measured against

Per the baseline document's own Gate section, **the ≤2% gate applies to exactly two rows** —
`SET, 3B, no pipeline` (baseline mean **89,484.60** rps) and `GET, 3B, no pipeline` (baseline
mean **100,235.04** rps). The other six rows swing 8.8–22.2% run-to-run *within the baseline
capture itself*, an order of magnitude wider than the threshold, and are recorded as context
only.

Per the gate revision established during plans 08 and 12 and recorded in the baseline document,
the gate has two parts:

- **(a) Mechanistic** — read the code and macro expansion to establish what the instrumentation
  costs at `info`. **This decides PASS/FAIL.**
- **(b) Empirical** — run the harness and report honestly, treating the result as a **>10%
  gross-regression tripwire**, not as a 2% pass/fail, because the instrument cannot resolve 2%.

## Harness

`scripts/benchmark.sh` (this worktree's copy), **seven** consecutive runs, matched durability
(`--appendonly yes --appendfsync everysec` on both servers), default `info` log level (no
`RUST_LOG` set). `redis-benchmark -t set,get -n 100000 -c 50 -r 100000` at 3B and 1024B
payloads, unpipelined and `-P 16`.

**Machine state.** 16-core box. Load average immediately before the runs: `2.41, 3.26, 4.60`;
immediately after: `2.05, 2.68, 3.87`. Top CPU consumers were steady-state desktop background
processes (`herdr`, `ghostty`, Brave, RedisInsight, other `claude` sessions) — no `cargo
build`, no `cargo test`, no other benchmark. Plus, of course, the six contaminating
`rocket-mem` nodes described above, at 0.1% CPU each.

---

## Gated rows — the two the ≤2% gate applies to

| Workload | Run 1 | Run 2 | Run 3 | Run 4 | Run 5 | Run 6 | Run 7 | Mean | Min | Max | Spread | Baseline mean | Δ |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| SET, 3B, no pipeline | 82,850.04 | 83,612.04 | 84,745.77 | 87,336.24 | 73,046.02 | 85,763.29 | 87,489.06 | **83,548.92** | 73,046.02 | 87,489.06 | **17.29%** | 89,484.60 | **−6.63%** |
| GET, 3B, no pipeline | 93,109.87 | 87,719.30 | 92,421.44 | 95,969.28 | 92,592.59 | 93,632.96 | 98,039.22 | **93,354.95** | 87,719.30 | 98,039.22 | **11.05%** | 100,235.04 | **−6.86%** |

Robustness checks on the same data, since the SET row contains one clear low outlier
(run 5, 73,046.02, against 82.9k–87.5k in the other six):

| Workload | Median | Δ from median | Mean excluding the single lowest run | Δ from that mean |
|---|---|---|---|---|
| SET, 3B, no pipeline | 84,745.77 | −5.30% | 85,299.41 | −4.68% |
| GET, 3B, no pipeline | 93,109.87 | −7.11% | 94,293.23 | −5.93% |

Those trimmed figures are shown to characterise the outlier's influence, **not** to replace the
headline number. The reported result is the full seven-run mean: −6.63% and −6.86%.

### Both gated rows exceed this series' own spread ceiling

The 17.29% and 11.05% spreads are both **above the 10% ceiling** this series established (in the
plan 08 section of the baseline document) as the line past which a triplet is "inconclusive on
its own" rather than gate-eligible. By the project's own stated rule, **this measurement is not
gate-eligible on either gated row.** Seven iterations narrowed the confidence interval compared
with three, but did not narrow it enough to resolve a 2% effect — if anything they revealed the
noise more fully than a triplet would have.

## Context-only rows — recorded, not gated

| Workload | Mean | Min | Max | Spread | Baseline mean | Δ |
|---|---|---|---|---|---|---|
| SET, 3B, pipeline=16 | 690,208.95 | 662,251.69 | 729,927.06 | 9.81% | 700,242.04 | −1.43% |
| GET, 3B, pipeline=16 | 1,154,088.62 | 1,075,268.75 | 1,219,512.12 | 12.50% | 1,135,299.50 | +1.65% |
| SET, 1KB, no pipeline | 82,579.32 | 80,064.05 | 84,104.29 | 4.89% | 81,774.68 | +0.98% |
| GET, 1KB, no pipeline | 92,312.40 | 88,339.23 | 96,618.36 | 8.97% | 89,748.65 | +2.86% |
| SET, 1KB, pipeline=16 | 476,879.54 | 432,900.41 | 552,486.19 | 25.08% | 530,293.04 | −10.07% |
| GET, 1KB, pipeline=16 | 675,663.35 | 641,025.62 | 719,424.44 | 11.60% | 683,440.31 | −1.14% |

Per-run values for these six rows are in the [Raw data](#raw-data) section.

## The `redis-server` control — the most informative number in this document

`scripts/benchmark.sh` benchmarks a real `redis-server` in the same invocation, with matched
durability. **Redis's code did not change between the baseline capture and this one.** Any
movement in its numbers is pure machine/environment drift, and it is the only clean measure of
that drift available.

| Workload (redis-server) | Baseline mean | This capture's mean | Δ |
|---|---|---|---|
| SET, 3B, no pipeline | 78,453.98 | 77,102.10 | −1.72% |
| GET, 3B, no pipeline | 101,392.28 | 97,110.73 | −4.22% |
| SET, 3B, pipeline=16 | 824,634.64 | 746,531.79 | −9.47% |
| GET, 3B, pipeline=16 | 1,435,667.13 | 1,211,642.48 | −15.60% |
| SET, 1KB, no pipeline | 97,471.90 | 90,476.35 | −7.18% |
| GET, 1KB, no pipeline | 99,622.27 | 95,207.39 | −4.43% |
| SET, 1KB, pipeline=16 | 403,431.88 | 395,043.41 | −2.08% |
| GET, 1KB, pipeline=16 | 764,938.29 | 760,132.21 | −0.63% |

**The unchanged control measured below its own baseline on all eight rows, averaging −5.67%.**
Across the same eight rows, rocket-mem — carrying the entire logging series — averaged
**−2.58%**, i.e. *less* drift than the control.

This does not prove the logging series costs nothing. It does mean this capture cannot
attribute rocket-mem's shortfall on the two gated rows to the instrumentation: the machine was
demonstrably slower during this capture than during the morning baseline, by an amount of the
same order as the effect being measured, and the code that provably did not change moved down
further than the code that did.

---

## Interpretation — honest reading of the result

**(a) Mechanistic verdict: PASS, with one genuinely eager cost identified and bounded.**

Plans 01–21 added instrumentation that sits entirely behind a level check. As the baseline
document's plan 12 and plan 15 sections established from the vendored `tracing 0.1.44` /
`tracing-core 0.1.36` sources, a disabled `trace!`/`debug!`/`debug_span!` at `info` costs one
relaxed atomic load and a comparison; field expressions are never expanded into executable code
on that path (`tracing`'s `log` feature is off in this build, confirmed in `Cargo.lock`), so no
allocation, lock, or dispatcher call is reachable.

**Plan 22 is the exception, and it is the first eager per-command cost in the series.**
`logged_key` in `crates/server/src/dispatcher.rs:3020` runs on every command at `info`, outside
any level check — deliberately, because the slow log's `warn!` fires at the production default
and needs the key after `frame` has been moved into `dispatch_and_log_inner`. Its cost, read
from the code:

- A `key_spec` match (`dispatcher.rs:1459`). For `SET`/`GET` this falls through the four
  literal-list arms to `KNOWN_COMMANDS.binary_search(&name)` — a binary search over a 91-entry
  `&[&str]`, so roughly 7 string comparisons. `metric_label` already pays the same search, so
  this is a *duplicate* of work the command path already did once, not new work of a new kind.
- At most one `Bytes` clone (`dispatcher.rs:3031`) — one atomic refcount increment, plus the
  matching decrement on drop. No data copy.

Order-of-magnitude: ~100–200 ns against an ~11.4 µs per-request budget at the ~87k rps these
rows measure, i.e. roughly **1–2%**. That is a real cost, plausibly at or just under the 2%
line, and it is emphatically **not** 6.6%. Nothing in the code accounts for a 6.6% regression.

**(b) Empirical verdict: the >10% tripwire did NOT trip. Neither gated row is a pass or a fail.**

Both gated rows measured about 6.6–6.9% below baseline. Per this document's contamination
notice, the machine drift shown by the unchanged `redis-server` control, and this harness's
demonstrated 6–9% run-to-run noise on exactly these two rows:

> **A result of this magnitude is neither a pass nor a fail.** It sits inside the band where
> this instrument cannot distinguish a real 2% regression from measurement noise plus
> environmental drift plus six foreign server processes. Declaring victory here would be
> unjustified; declaring a regression would be equally unjustified.

What *can* be stated with confidence:

- The **>10% gross-regression tripwire did not trip** on either gated row (−6.63%, −6.86%).
  Neither is close to it.
- The unchanged control drifted *further* down (−5.67% average) than rocket-mem did (−2.58%
  average) across all eight rows, so the environment explains at least as much of the shortfall
  as any code change could.
- The mechanistic reading, which is what decides this gate, identifies one eager per-command
  cost bounded at roughly 1–2% and nothing else.
- Both gated rows' spreads (17.29%, 11.05%) exceed this series' own 10% gate-eligibility
  ceiling, so by the project's own rule this capture is not gate-eligible regardless of which
  side of 2% its mean landed on.

**Verdict: PASS on the mechanistic gate that decides it. INCONCLUSIVE on the empirical
measurement, which is contaminated by design and too noisy to resolve a 2% effect.**

No run was tuned, retried, discarded, or cherry-picked to improve this number. All seven
iterations are reported; the run-5 SET outlier is included in the headline mean.

### If someone later wants a clean cumulative number

This document is not it, and cannot be made into it retroactively. A clean capture needs the
six foreign nodes stopped, a quiet machine, and enough iterations to resolve 2% against a 6–9%
noise floor — on the order of 30+ runs per arm, or a lower-variance instrument than
`redis-benchmark` wall-clock throughput. The mechanistic analysis above is the stronger evidence
either way, which is exactly why the gate was restructured to let it decide.

---

## Manual verification at info / debug / trace

Run against this worktree's release binary on spare ports (16399/16400/16401) with scratch
AOF/snapshot paths, so the six live nodes were never involved.

### Two corrections to the plan's Step 3 script

Both were bugs in the plan's verification script, not in the implementation:

1. **The ACL config used the wrong field names.** The plan's snippet writes
   `commands = "allcommands"` / `keys = "allkeys"`. The real schema
   ([`docs/config-reference.md`](../config-reference.md#the-aclusers-array),
   `crates/server/src/config.rs:72`) is a single `rules` array. With the plan's spelling, `alice`
   authenticated but had no permissions and every command returned `NOPERM`. Corrected to
   `rules = ["allcommands", "allkeys"]`.

2. **Each `redis-cli` invocation is its own connection.** The plan issues `AUTH` as one
   `redis-cli` process and `SET`/`GET`/`LPUSH` as separate ones, so the `AUTH` authenticated
   only the connection that carried it and every other command came back `NOAUTH` — the
   `WRONGTYPE` and expiry paths were never actually exercised. Corrected by piping the whole
   sequence over a single `redis-cli` connection.

### Results

| Check | `info` | `debug` | `trace` |
|---|---|---|---|
| Per-command dispatch lines (`command dispatched`) | **0** ✅ | 7 ✅ | 7 ✅ |
| Password `s3cret` anywhere in the log | **0** ✅ | **0** ✅ | **0** ✅ |
| `<redacted>` rendered for `AUTH` | 0 (no argument line at this level) | 0 (no argument line at this level) | **1** ✅ |
| Value contents (`bar`) rendered | 0 ✅ | 0 ✅ | **1** ✅ |
| Per-key TTL expiry event | 0 | 0 | **1** ✅ |
| Total log lines for the same workload | 10 | 21 | 44 |

Representative lines:

```
# info -- milestones only, no per-command line
2026-09-09T19:01:19Z  INFO rocket_mem: resolved config summary addr=127.0.0.1:16399 ... log_filter=info log_value_max_bytes=128 ...
2026-09-09T19:01:20Z  INFO conn{conn_id=1 peer=127.0.0.1:38110 protocol=RESP tls=false}: rocket_mem::connection: connection accepted

# debug -- one line per command, error reported as a reply kind
DEBUG conn{conn_id=1 ...}:cmd{cmd=SET key=foo argc=2}: rocket_mem::dispatcher: command dispatched elapsed_us=42 reply="ok"
DEBUG conn{conn_id=1 ...}:cmd{cmd=GET key=mylist argc=1}: rocket_mem::dispatcher: command dispatched elapsed_us=32 reply="error"

# trace -- argument contents, with credentials redacted unconditionally
TRACE conn{conn_id=1 ...}:cmd{cmd=SET key=foo argc=2}: rocket_mem::dispatcher: command arguments args=foo bar
TRACE conn{conn_id=1 ...}:cmd{cmd=AUTH key= argc=2}: rocket_mem::dispatcher: command arguments args=<redacted>
DEBUG engine::engine: active expire cycle shard=16 removed=1
```

### Three expectations in the plan that the implementation does not meet — all by design

None of these is a bug; each is a case where the plan's Step 3 predicted a behaviour the code
deliberately does not have. Recorded so a later reader does not "fix" the code to match a stale
expectation.

1. **`grep WRONGTYPE` at `debug` finds nothing.** The per-command line reports `reply="error"`,
   not the error text — `reply_kind` (`dispatcher.rs:3046`) returns a `&'static str` of `"ok"` or
   `"error"` on purpose, reusing the exact split that drives
   `rocket_mem_command_errors_total` so the log and the error-rate metric can never disagree,
   and returning a static string rather than a formatted one because it is evaluated once per
   command whenever `debug` is enabled. The `WRONGTYPE` string reaches the client, as it should;
   it is not a log field.

2. **`grep redacted` at `debug` finds nothing.** Redaction is applied by `redact_args` inside the
   *argument* line, and that line is `trace`-only. At `debug` there is no argument line to
   redact. The security invariant that actually matters — the password never appears at any
   level — holds: `s3cret` has zero occurrences at `info`, `debug` *and* `trace`.

3. **`grep -c 'cmd=' == 0` at `info` is not quite right.** A first pass found exactly one `cmd=`
   hit at `info`, and it is correct behaviour: the slow log's `warn!` sits *above* `info` and
   fired because `AUTH`'s Argon2 password verification legitimately took 43,795 µs, over the
   10,000 µs slowlog threshold:

   ```
   WARN conn{conn_id=3 ...}: rocket_mem::slowlog: slow command recorded cmd=AUTH key= elapsed_us=43795
   ```

   Note `key=` is **empty**. That is plan 22's `logged_key` fix demonstrated live at the
   production default level: `key_spec` maps `AUTH` to `KeySpec::None`, so the password that the
   old "first argument" rendering would have printed as a key name is not printed at all.

---

## Series-wide test-suite stability

Not required by the plan, but the acceptance record for a series that ended with ~19
log-capture tests sharing one test binary. `tracing` caches per-callsite `Interest`
process-globally, and a callsite first reached with no subscriber installed can be cached
`never` for the life of the process — a failure mode this series hit and fixed once (commit
`4e646d2`) by consolidating capture assertions into `crates/server/tests/logging.rs`. That
remedy had never been verified at the final test count.

| Sweep | Iterations | Result |
|---|---|---|
| `cargo test --workspace` | **12** | 12/12 pass, **945 passed / 0 failed / 0 ignored** every time |
| `cargo test -p rocket-mem --test logging` (the capture binary alone) | **30** | 30/30 pass |
| `cargo test -p rocket-mem --test logging -- --test-threads=1` | 3 | 3/3 pass, 19/19 tests |

**No flakes were observed in 45 runs.** The remedy holds at this test count, and there is a
structural reason it should keep holding:

- `crates/server/tests/logging.rs` is the **only** test file in the workspace that installs a
  subscriber (verified by grepping `set_default`/`with_default`/`set_global_default`/
  `tracing_subscriber` across all crates' `tests/` and `src/`), so no second test binary can
  race it.
- Every installation there is **thread-local** (`tracing::subscriber::with_default` /
  `set_default`); `set_global_default` appears nowhere in test code.
- In `tracing-core 0.1.36`, `Dispatch::new` calls `callsite::register_dispatch`
  (`dispatcher.rs:479`), which triggers `Callsites::rebuild_interest` across the *entire*
  callsite registry. `rebuild_callsite_interest` (`callsite.rs:490`) folds every registered
  dispatcher's answer together with `Interest::and`, so a callsite that cached `never` before any
  subscriber existed is re-evaluated — and re-enabled — the moment the first capture subscriber
  is constructed. The `interest.unwrap_or_else(Interest::never)` fallback at `callsite.rs:505`
  only applies when *no* dispatcher is registered at all.

The contention therefore does **not** scale with the number of capture tests in the way the
concern anticipated: each new capture test registers another permissive dispatcher, which
widens the union rather than narrowing it.

---

## Global constraints checked

No Rust source was changed by this task; this document is the only file it adds.

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean, no warnings.
- `cargo test --workspace` — 945 passed, 0 failed (12 consecutive runs, see above).
- `git status --porcelain` — clean before the run; `Cargo.lock` untouched by the release build.
- All six pre-existing `rocket-mem` processes confirmed alive and untouched afterwards.

---

## Raw data

Full unedited capture of all seven runs, including the `redis-benchmark -q` interim progress
lines and the `redis-server` control figures, preserved outside the repo at
`/tmp/claude-1000/-home-numericlabs-data-rocket-rocket-mem/850c0577-d0e8-4d03-9147-6aec1a827079/scratchpad/post-logging-final.txt`
(raw) and `.../post-logging-clean.txt` (carriage returns expanded to newlines, which is
necessary before parsing — `redis-benchmark`'s progress output otherwise collapses each case
onto a single physical line and any naive `awk`/`grep` reads a progress sample instead of the
final result).

Per-run values for every workload, both servers, are below.

### rocket-mem, per run

| Workload | Run 1 | Run 2 | Run 3 | Run 4 | Run 5 | Run 6 | Run 7 |
|---|---|---|---|---|---|---|---|
| SET, 3B, no pipeline | 82,850.04 | 83,612.04 | 84,745.77 | 87,336.24 | 73,046.02 | 85,763.29 | 87,489.06 |
| GET, 3B, no pipeline | 93,109.87 | 87,719.30 | 92,421.44 | 95,969.28 | 92,592.59 | 93,632.96 | 98,039.22 |
| SET, 3B, pipeline=16 | 699,300.69 | 684,931.50 | 662,251.69 | 729,927.06 | 684,931.50 | 675,675.69 | 694,444.50 |
| GET, 3B, pipeline=16 | 1,086,956.50 | 1,075,268.75 | 1,190,476.25 | 1,219,512.12 | 1,204,819.38 | 1,111,111.12 | 1,190,476.25 |
| SET, 1KB, no pipeline | 80,064.05 | 81,766.15 | 81,833.06 | 82,850.04 | 83,333.33 | 84,104.29 | 84,104.29 |
| GET, 1KB, no pipeline | 91,659.03 | 88,339.23 | 89,605.73 | 92,336.11 | 91,659.03 | 96,618.36 | 95,969.28 |
| SET, 1KB, pipeline=16 | 529,100.56 | 432,900.41 | 473,933.66 | 460,829.50 | 446,428.56 | 442,477.88 | 552,486.19 |
| GET, 1KB, pipeline=16 | 641,025.62 | 662,251.69 | 662,251.69 | 709,219.88 | 641,025.62 | 719,424.44 | 694,444.50 |

### redis-server control, per run

| Workload | Run 1 | Run 2 | Run 3 | Run 4 | Run 5 | Run 6 | Run 7 |
|---|---|---|---|---|---|---|---|
| SET, 3B, no pipeline | 76,161.46 | 75,018.76 | 81,632.65 | 73,583.52 | 75,642.96 | 78,247.26 | 79,428.12 |
| GET, 3B, no pipeline | 98,039.22 | 99,009.90 | 99,206.34 | 98,328.42 | 83,963.05 | 98,135.42 | 103,092.78 |
| SET, 3B, pipeline=16 | 740,740.69 | 729,927.06 | 740,740.69 | 757,575.75 | 617,283.94 | 813,008.12 | 826,446.31 |
| GET, 3B, pipeline=16 | 1,176,470.62 | 1,204,819.38 | 1,176,470.62 | 1,176,470.62 | 1,149,425.38 | 1,282,051.25 | 1,315,789.50 |
| SET, 1KB, no pipeline | 87,336.24 | 91,743.12 | 88,967.98 | 91,659.03 | 89,206.06 | 90,171.33 | 94,250.71 |
| GET, 1KB, no pipeline | 93,370.68 | 96,246.39 | 94,607.38 | 97,943.19 | 92,250.92 | 93,023.25 | 99,009.90 |
| SET, 1KB, pipeline=16 | 371,747.22 | 469,483.56 | 361,010.81 | 348,432.06 | 309,597.50 | 421,940.94 | 483,091.78 |
| GET, 1KB, pipeline=16 | 735,294.06 | 729,927.06 | 769,230.81 | 751,879.69 | 740,740.69 | 787,401.56 | 806,451.62 |
