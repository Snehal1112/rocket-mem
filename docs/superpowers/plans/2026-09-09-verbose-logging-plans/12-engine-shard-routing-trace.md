# Verbose Logging Plan 12: Engine Shard Routing & Byte-Delta Trace

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `trace`-level visibility into which shard a key routes to and how many bytes an in-place mutation grew or shrank a value by — the two `engine` events the spec catalogue lists as "shard routing, `key` → `shard` (trace); mutation byte delta (trace)" — without touching the `#[inline]` hot function every single read and write already funnels through.

**Architecture:** `Store::shard_index` (`crates/engine/src/store.rs:42`) is marked `#[inline]` and sits on the hot path of *every* `get`, `set`, `del`, `exists`, `with_ref`, `with_mut`, and `with_mut_delta` call — `Store::shard_for` (line 48) calls it on every one of those. A log call inside it, even a disabled one, adds a relaxed atomic load and a branch to the single most frequently executed function in the whole engine crate. That makes it the riskiest possible placement in this entire logging series, worse than anything already landed in `crates/server`.

This plan instruments `Engine::shard_index` (`crates/engine/src/engine.rs:99-101`) instead — the public facade method, not `Store`'s internal one. Concretely: `Store::shard_index` stays untouched, byte-for-byte, forever a plain hash-and-modulo with no tracing call anywhere near it. `Engine::shard_index` is a separate, deliberately-exposed method that most read/write traffic never calls at all — `Engine::get`/`set`/`with_ref`/`with_mut`/`with_mut_delta` all route through `Store`'s *methods* (`store.get`, `store.set`, ...), which resolve their own shard internally via `shard_for`/`shard_index` without ever going through `Engine::shard_index`. The only callers of `Engine::shard_index` today are in `crates/server` — the AOF ordering guard (`aof.rs:1322`) and cluster-slot routing in `dispatcher.rs:3134` — both already off the per-key mutation hot path (AOF ordering guards are taken once per batched operation, not per shard access; the cluster router runs once per command, not once per shard touch). Instrumenting there gives an operator the "which shard does this key route to" answer the spec asks for, exactly where callers already ask that question explicitly, while the actual read/write hot path inside `Store` never executes a tracing macro at all.

This is a deliberate scope narrowing from a literal reading of "shard routing ... (trace)" as "log every `shard_for` call inside `Store`" — that reading is exactly what plan 01's spec-writing round warned against wiring in without a dedicated benchmark gate, and Task 3 below is that gate.

The mutation byte delta trace goes into `Engine::with_mut_delta` (`crates/engine/src/engine.rs:78-85`), which is what `RPUSH`/`HSET`/`SADD`/`ZADD` actually call to grow or shrink a value in place (see the doc comment above it in `engine.rs`). It already receives the caller-reported `isize` delta; this plan surfaces it, without changing `Store::with_mut_delta` or `Shard::with_mut_delta` (`crates/engine/src/shard.rs:287-319`), which stay exactly as they are.

**Tech Stack:** Rust 2021, `tracing 0.1` (already added to `crates/engine`'s `Cargo.toml` by plan 01 Task 2).

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the Event catalogue's `Engine` row.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting. The load-bearing ones for this plan specifically:

- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must both pass.
- Every pre-existing test must pass **unchanged**.
- No `format!` outside a log macro's argument list; every field uses `%`/`?` so formatting is lazy.
- `Bytes` is never logged via `Debug` on the hot path — keys are logged as `%String::from_utf8_lossy(key)`, values are never logged from `engine` at all (byte lengths only, and here not even that — only routing and a signed delta).
- Redaction policy lives only in `crates/server`. `engine` logs key names and byte figures, never value contents — there is no `fmt_value`/`redact_args` call anywhere in this plan, and there must never be one: `crates/server/src/logging.rs` is not importable from `engine` (that would be a circular dependency: `server` depends on `engine`, not the other way around).
- Throughput at the default `info` level must stay within **2%** of the baseline recorded in `docs/benchmarks/2026-09-09-pre-logging-baseline.md`. Both new call sites here are `trace!`, which is disabled at `info`, but `Engine::shard_index` and `Engine::with_mut_delta` are still real functions on paths `dispatcher.rs` calls per-command — Task 3 is the gate that proves the disabled-check cost is not measurable.

---

### Task 1: Trace shard routing in `Engine::shard_index`

**Files:**
- Modify: `crates/engine/src/engine.rs` (the `shard_index` method, lines 98-101)
- Test: `crates/engine/src/engine.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Store::shard_index` (`crates/engine/src/store.rs:42`), unchanged and untouched by this plan.
- Produces: the same `pub fn shard_index(&self, key: &[u8]) -> usize` signature. `crates/server/src/aof.rs:1322` and `crates/server/src/dispatcher.rs:3134` keep calling it exactly as before — no signature change, so nothing downstream needs updating.

There is no new pure logic in this task — `Engine::shard_index` already delegates correctly to `Store::shard_index` and is already covered indirectly by every test that exercises sharding through `Store`. Per the plan's testing ground rules, a log line's mere presence cannot be unit-asserted without a capture subscriber this series deliberately avoids adding. What *is* worth a new test is that `Engine::shard_index`'s existing behavior — the exact usize it hands back — never changes as a side effect of adding instrumentation, since that method has no test of its own today.

- [ ] **Step 1: Add a regression test for the untouched behavior**

Add to the `#[cfg(test)] mod tests` block in `crates/engine/src/engine.rs`:

```rust
    #[test]
    fn shard_index_is_deterministic_and_within_bounds() {
        let engine = Engine::new();
        let a = engine.shard_index(b"some-key");
        let b = engine.shard_index(b"some-key");
        assert_eq!(a, b, "the same key must always route to the same shard");
        assert!(a < crate::SHARD_COUNT);
    }

    #[test]
    fn shard_index_matches_the_underlying_store() {
        // Engine::shard_index is a thin facade -- this pins that it never diverges from
        // what Store computes, which is the only contract callers like the AOF ordering
        // guard (crates/server/src/aof.rs) actually depend on.
        let engine = Engine::new();
        for key in [&b"a"[..], b"bb", b"ccc", b"dddd"] {
            assert_eq!(engine.shard_index(key), engine.store.shard_index(key));
        }
    }
```

The second test needs `store` to be reachable from the test module; it already is, since `mod tests` is declared inside `engine.rs` and `Engine::store` is a private field of the same module's `struct Engine`.

- [ ] **Step 2: Run the tests**

```bash
cargo test -p engine engine::tests::shard_index
```

Expected: PASS immediately. There is no new behavior here to fail against first — `Engine::shard_index` already forwards correctly today. This step exists to lock in the contract *before* Step 3 touches the method body, so that if instrumentation is ever added carelessly (e.g. computing the shard a second time, or via a different path than `Store::shard_index`) these tests catch the divergence.

- [ ] **Step 3: Add the trace call**

Replace the method in `crates/engine/src/engine.rs`:

```rust
    /// Which shard a key routes to — see `Store::shard_index`. Traced at the facade level, not
    /// inside `Store::shard_index` itself: that method is `#[inline]` and sits on the hot path
    /// of every single `get`/`set`/`with_ref`/`with_mut`/`with_mut_delta` call, so a log call
    /// there — even a disabled one — is the single riskiest placement in this series. This
    /// method is a separate, explicitly-called facade that only the AOF ordering guard and the
    /// cluster router use today; instrumenting here answers "which shard does this key route
    /// to" exactly where a caller already asks that question, without adding a branch to the
    /// per-key read/write path inside `Store`.
    pub fn shard_index(&self, key: &[u8]) -> usize {
        let shard = self.store.shard_index(key);
        tracing::trace!(key = %String::from_utf8_lossy(key), shard, "shard routing");
        shard
    }
```

- [ ] **Step 4: Verify**

```bash
cargo test -p engine
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all `engine` tests pass (including the two added in Step 1), fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/engine.rs
git commit -m "feat(engine): trace shard routing at the Engine facade"
```

---

### Task 2: Trace the mutation byte delta in `with_mut_delta`

**Files:**
- Modify: `crates/engine/src/engine.rs` (`with_mut_delta`, lines 78-85)
- Test: `crates/engine/src/engine.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Store::with_mut_delta` (`crates/engine/src/store.rs:88-93`), unchanged. That in turn calls `Shard::with_mut_delta` (`crates/engine/src/shard.rs:287-319`), also unchanged — the delta accounting logic (the `match delta.cmp(&0)` block) stays exactly as it is; only `Engine`'s facade gains a way to observe the delta its own caller reported.
- Produces: the same `pub fn with_mut_delta<F, R>(&self, key: &[u8], f: F) -> R where F: FnOnce(Option<&mut Value>) -> (R, isize)` signature. `commands/{hash,list,set,sorted_set}.rs` (per `engine.rs`'s own doc comment: "RPUSH/HSET/SADD/ZADD... actually calls to grow a value in place") keep calling it unchanged.

Same TDD carve-out as Task 1: no new pure logic is added, so there is nothing to drive with a failing test. `Shard::with_mut_delta`'s accounting is already covered by `with_mut_re_accounts_bytes_used_after_growing_a_value_in_place` (`shard.rs`) and by `with_maxmemory_also_bounds_memory_grown_in_place_not_only_through_set` (`engine.rs`), both of which exercise `with_mut_delta` end-to-end through real command paths. This task adds a direct, facade-level regression test of `Engine::with_mut_delta` itself (there isn't one today — the existing coverage is all indirect, through `commands::list::rpush`), which doubles as the guard that observing the delta does not change what gets returned or accounted.

- [ ] **Step 1: Add a direct regression test for `Engine::with_mut_delta`**

Add to `crates/engine/src/engine.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn with_mut_delta_returns_the_closures_result_and_still_accounts_bytes() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"abc")),
        );
        let before = engine.memory_used();
        let returned = engine.with_mut_delta(b"k", |v| {
            if let Some(Value::String(s)) = v {
                *s = Bytes::from_static(b"a much longer replacement value");
                (42, 29) // 29 == "a much longer replacement value".len() - "abc".len()
            } else {
                (0, 0)
            }
        });
        assert_eq!(returned, 42, "the closure's own result must still come back unchanged");
        assert!(engine.memory_used() > before, "the reported delta must still be accounted");
    }

    #[test]
    fn with_mut_delta_on_a_missing_key_reports_no_delta_and_creates_nothing() {
        let engine = Engine::new();
        let saw_none = engine.with_mut_delta(b"missing", |v| (v.is_none(), 0));
        assert!(saw_none);
        assert!(!engine.exists(b"missing"));
    }
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p engine engine::tests::with_mut_delta
```

Expected: PASS immediately, for the same reason as Task 1 Step 2 — `with_mut_delta`'s accounting behavior is pre-existing and correct; these tests pin it down before Step 3 touches the method body.

- [ ] **Step 3: Add the trace call**

Replace the method in `crates/engine/src/engine.rs`:

```rust
    /// Like `with_mut`, but for callers that can report their mutation's byte delta directly
    /// instead of paying `Shard::with_mut`'s O(current collection size) before/after diff --
    /// see `Shard::with_mut_delta`'s doc comment for why that scan matters.
    ///
    /// Traces the reported delta at the point it becomes known to `Engine` — after `Store`
    /// has already applied it to `bytes_used`, so the traced number and the accounted number
    /// can never disagree. The delta itself may be negative (a shrinking mutation, e.g. `LPOP`),
    /// which is why the field carries an `isize`, not a `usize`.
    pub fn with_mut_delta<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&mut Value>) -> (R, isize),
    {
        let mut observed_delta: isize = 0;
        let result = self.store.with_mut_delta(key, |v| {
            let (r, delta) = f(v);
            observed_delta = delta;
            (r, delta)
        });
        tracing::trace!(
            key = %String::from_utf8_lossy(key),
            bytes = observed_delta,
            "mutation byte delta"
        );
        self.maybe_evict();
        result
    }
```

- [ ] **Step 4: Verify**

```bash
cargo test -p engine
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all `engine` tests pass (including the two added in Step 1), the full workspace suite stays green, fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/engine.rs
git commit -m "feat(engine): trace mutation byte delta in with_mut_delta"
```

---

### Task 3: Benchmark gate — Engine instrumentation is the highest-risk placement in this series

**Files:**
- Create: `docs/benchmarks/2026-09-09-plan-12-engine-trace-benchmark.md`

**Interfaces:**
- Consumes: `docs/benchmarks/2026-09-09-pre-logging-baseline.md` (plan 01 Task 1's recorded Mean SET/GET requests/sec).
- Produces: a committed comparison document. This is not optional for this plan specifically: Task 1 instrumented `Engine::shard_index`, called from `crates/server`'s per-command AOF ordering guard and cluster router, and Task 2 instrumented `with_mut_delta`, called by every `RPUSH`/`HSET`/`SADD`/`ZADD`. Both are real functions `dispatcher.rs` reaches on a meaningful fraction of commands, making this plan's disabled-tracing cost the one most likely to be measurable of anything landed so far.

- [ ] **Step 1: Build the release binary with both tasks' changes included**

```bash
cd /home/numericlabs/data/rocket/rocket-mem
cargo build --workspace --release
```

- [ ] **Step 2: Run the benchmark three times at the default `info` level**

```bash
for i in 1 2 3; do
  echo "=== run $i ==="
  ./scripts/benchmark.sh
done 2>&1 | tee /tmp/rocket-mem-plan12-benchmark.txt
```

Do not set `RUST_LOG` — the gate is specifically about the default `info` level, where every `trace!` call added in Tasks 1 and 2 is disabled and should cost only a relaxed atomic load and a branch.

- [ ] **Step 3: Write the comparison document**

Create `docs/benchmarks/2026-09-09-plan-12-engine-trace-benchmark.md`:

```markdown
# Plan 12 Benchmark: Engine Shard Routing & Byte-Delta Trace

**Date:** 2026-09-09
**Commit:** <output of `git rev-parse --short HEAD`>
**Baseline:** [`2026-09-09-pre-logging-baseline.md`](2026-09-09-pre-logging-baseline.md)

Compares throughput at the default `info` level (both new `trace!` call sites disabled)
against the pre-instrumentation baseline, per the gate in
[the verbose logging spec](../superpowers/specs/2026-09-09-verbose-logging-design.md).

## rocket-mem requests/sec (info level, RUST_LOG unset)

| Workload | Run 1 | Run 2 | Run 3 | Mean | Baseline Mean | Delta |
|---|---|---|---|---|---|---|
| SET | | | | | | |
| GET | | | | | | |

## Gate result

<PASS if both Delta rows are within -2% of baseline; FAIL otherwise, with next steps>

## Raw output

<paste the full tee'd output of the three runs here>
```

Fill every blank from the actual Step 2 output — the table above is a shape, not a value.

- [ ] **Step 4: Judge the gate**

If both SET and GET means are within 2% of baseline: the gate passes, this plan is done. If either regresses more than 2%: this plan is **not done** — do not proceed to plan 13. Instead, profile with `docs/benchmarks/2026-09-07-flamegraph-notes.md`'s method to confirm whether Task 1's or Task 2's call site is responsible (comment one out, rebuild, re-run), and report the finding rather than guessing at a fix.

- [ ] **Step 5: Commit**

```bash
git add docs/benchmarks/2026-09-09-plan-12-engine-trace-benchmark.md
git commit -m "docs: record plan 12 engine-trace benchmark against the pre-logging baseline"
```

---

## Next plan

[`13-engine-expiry-and-eviction.md`](13-engine-expiry-and-eviction.md) — adds `debug`-level active-expire-cycle counts and `warn`-level eviction events to the engine, on top of the shard-routing and byte-delta trace this plan added.
