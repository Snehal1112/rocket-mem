# Verbose Logging Plan 01: Benchmark Baseline & Crate Dependencies

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record the pre-instrumentation throughput baseline every later plan is measured against, and add the `tracing` dependency to `engine` and `protocol` without yet adding a single call site.

**Architecture:** This plan changes no behavior. It captures a number and adds two `Cargo.toml` lines, so that the first plan which actually instruments code has something to compare against and somewhere to emit from. Splitting the baseline out is deliberate: once instrumentation lands, the un-instrumented number can never be re-measured.

**Tech Stack:** Rust 2021, cargo workspace, `tracing 0.1`, `scripts/benchmark.sh` (wraps `redis-benchmark`).

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md)

## Global Constraints

These apply to **every task in every plan in this series**:

- `cargo fmt --all -- --check` must pass.
- `cargo clippy --workspace --all-targets -- -D warnings` must pass. This is strict: no warnings at all, including dead-code, and it lints test code too.
- `cargo test --workspace` must pass. Every pre-existing test must keep passing **unchanged** — if a test needs editing to accommodate a log line, that is a signal the change altered behavior, which this series must not do.
- Throughput at the default `info` level must stay within **2%** of the baseline recorded in Task 1 of this plan.
- No `format!` outside a log macro's argument list.
- All log fields use the `%` (Display) or `?` (Debug) sigil so formatting is lazy and never runs when the level is disabled.
- `Bytes` is never logged via `Debug` on the hot path — it renders byte-by-byte.
- No new atomic counters on the hot path.
- Redaction policy lives only in `crates/server`. `engine` and `protocol` log key names and byte lengths, never value contents.

---

### Task 1: Capture the pre-instrumentation benchmark baseline

**Files:**
- Create: `docs/benchmarks/2026-09-09-pre-logging-baseline.md`

**Interfaces:**
- Consumes: nothing.
- Produces: a committed baseline file. Every later plan's benchmark step compares against the `SET`/`GET` requests-per-second figures recorded here.

- [ ] **Step 1: Confirm the benchmark harness can run**

`scripts/benchmark.sh` requires `redis-server` and `redis-benchmark` on `PATH`. Check first:

```bash
command -v redis-server && command -v redis-benchmark
```

If either is missing, install them (`apt install redis-server redis-tools` on Debian/Ubuntu) before continuing. Do not substitute a hand-rolled `redis-benchmark` invocation: `scripts/benchmark.sh` deliberately runs both servers with matched durability (`--appendonly yes --appendfsync everysec`), because rocket-mem cannot disable its AOF and an unmatched comparison flatters Redis.

- [ ] **Step 2: Run the benchmark three times and record every run**

Three runs, not one — `redis-benchmark` has real run-to-run jitter, and a 2% gate cannot be judged against a single sample.

```bash
cd /home/numericlabs/data/rocket/rocket-mem
for i in 1 2 3; do
  echo "=== run $i ==="
  ./scripts/benchmark.sh
done 2>&1 | tee /tmp/rocket-mem-baseline.txt
```

- [ ] **Step 3: Write the baseline document**

Create `docs/benchmarks/2026-09-09-pre-logging-baseline.md`. Fill the table from the actual output of Step 2 — do not copy the illustrative numbers below, they are placeholders showing the shape only:

```markdown
# Pre-Logging Throughput Baseline

**Date:** 2026-09-09
**Commit:** <output of `git rev-parse --short HEAD`>
**Purpose:** the reference point for the <=2% regression gate defined in
[the verbose logging spec](../superpowers/specs/2026-09-09-verbose-logging-design.md).
Captured before any instrumentation landed, because this number cannot be re-measured
afterwards.

**Harness:** `scripts/benchmark.sh`, three consecutive runs, matched durability
(`--appendonly yes --appendfsync everysec` on both servers).

**Log level during capture:** default `info` (no `RUST_LOG` set).

## rocket-mem requests/sec

| Workload | Run 1 | Run 2 | Run 3 | Mean |
|---|---|---|---|---|
| SET | | | | |
| GET | | | | |

## Gate

A later plan passes its benchmark step when its measured mean, at default `info`,
is within 2% of the Mean column above. Below-baseline results inside that band are
noise, not regression; anything worse is a blocker for that plan.

## Raw output

<paste the full tee'd output of the three runs here>
```

- [ ] **Step 4: Commit**

```bash
git add docs/benchmarks/2026-09-09-pre-logging-baseline.md
git commit -m "docs: record pre-logging throughput baseline"
```

---

### Task 2: Add `tracing` to the `engine` and `protocol` crates

**Files:**
- Modify: `crates/engine/Cargo.toml`
- Modify: `crates/protocol/Cargo.toml`

**Interfaces:**
- Consumes: `tracing = "0.1"`, already declared in the workspace `Cargo.toml`'s `[workspace.dependencies]`.
- Produces: `tracing::` macros usable inside `crates/engine` and `crates/protocol`. Plans 10 onward depend on this.

- [ ] **Step 1: Add the dependency to `engine`**

In `crates/engine/Cargo.toml`, add one line to the end of the existing `[dependencies]` block:

```toml
[dependencies]
common = { path = "../common" }
bytes.workspace = true
parking_lot.workspace = true
rand.workspace = true
ordered-float.workspace = true
serde.workspace = true
bincode.workspace = true
thiserror.workspace = true
tracing.workspace = true
```

- [ ] **Step 2: Add the dependency to `protocol`**

In `crates/protocol/Cargo.toml`:

```toml
[dependencies]
bytes.workspace = true
tokio-util.workspace = true
tracing.workspace = true
```

- [ ] **Step 3: Verify the workspace still builds clean**

An unused dependency is not a clippy error by default, so this should pass with zero call sites present:

```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three succeed. If clippy reports an unused-crate warning, do **not** add a placeholder log call to silence it — instead add `#[allow(unused_crate_dependencies)]` is *not* wanted either; report the issue, as it means the workspace has a lint configuration this plan did not anticipate.

- [ ] **Step 4: Commit**

```bash
git add crates/engine/Cargo.toml crates/protocol/Cargo.toml
git commit -m "chore: add tracing dependency to engine and protocol"
```

---

### Task 3: Record the dependency decision in `CLAUDE.md`

**Files:**
- Modify: `CLAUDE.md` (the "Workspace layout" section)

**Interfaces:**
- Consumes: nothing.
- Produces: nothing consumed by later plans. This exists so a future reader does not treat Task 2 as an erosion of the engine's dependency boundary and revert it.

- [ ] **Step 1: Amend the workspace layout section**

`CLAUDE.md`'s "Workspace layout" section currently describes `common` as having "Zero dependencies on other crates" and frames `engine` as protocol-agnostic. Add this paragraph directly beneath the five-crate bullet list:

```markdown
**Logging is the one permitted cross-cutting dependency.** `engine` and `protocol` both
depend on `tracing` (since 2026-09-09). This does not weaken the protocol-agnostic rule:
`tracing` is a facade crate with no runtime of its own, its macros compile to a level check
that is never true when no subscriber is installed, and an instrumented engine still knows
nothing about RESP or RMP. Redaction policy deliberately does *not* live here — `engine` and
`protocol` log key names and byte lengths only, never value contents, so
`crates/server/src/logging.rs` stays the single auditable place a secret could reach a log.
See [the verbose logging spec](docs/superpowers/specs/2026-09-09-verbose-logging-design.md).
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: record tracing as engine/protocol's permitted cross-cutting dep"
```

---

## Next plan

[`02-logging-module-fmt-value.md`](02-logging-module-fmt-value.md) — creates `crates/server/src/logging.rs` and its `fmt_value` function, the value renderer `redact_args` and every trace-level log line build on.
