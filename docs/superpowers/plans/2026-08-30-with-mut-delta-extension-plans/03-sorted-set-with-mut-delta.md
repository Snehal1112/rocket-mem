# SortedSet `with_mut_delta` Conversion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** convert `sorted_set.rs`'s three mutating functions (`zadd`, `zrem`, `zincrby`) from `Engine::with_mut` to `Engine::with_mut_delta`, so a growing `SortedSet` no longer pays an O(current member count) `bytes_used` rescan on every `ZADD`/`ZREM`/`ZINCRBY` call.

**Architecture:** pure refactor — `bytes_used` accounting only, no change to any function's return value, error behavior, or the `WRONGTYPE`/missing-key semantics `CLAUDE.md` documents. `Engine::with_mut_delta` already exists (built for the equivalent `list.rs` fix, unrelated to this plan). This plan touches only `crates/engine/src/commands/sorted_set.rs`.

**Tech Stack:** Rust, existing `Engine`/`Value`/`SortedSet` types, no new dependencies.

**Spec:** `../../specs/2026-08-30-with-mut-delta-extension-spec.md` — read its "Decision: exact per-type delta formulas" section for `SortedSet` before starting; every delta computation below is copied from it.

## Global Constraints

- `Value::approx_size`'s `SortedSet` formula (unchanged, ground truth): `member.len() + 24` per member — a member's **score is never part of the size formula**, so any mutation that only updates an existing member's score (not its presence) has delta `0`. Every delta below must reproduce this exactly.
- No behavior changes: return values, `WRONGTYPE` errors, and the missing-key convention must be byte-for-byte identical to today.
- This is a refactor, not a bug fix: the correctness test in Step 1 must **pass against today's unmodified code** before any conversion happens.

---

### Task 1: Convert `zadd`/`zrem`/`zincrby` to `with_mut_delta`

**Files:**
- Modify: `crates/engine/src/commands/sorted_set.rs`

**Interfaces:**
- Consumes: `Engine::with_mut_delta<F, R>(&self, key: &[u8], f: F) -> R where F: FnOnce(Option<&mut Value>) -> (R, isize)` (already exists in `crates/engine/src/engine.rs`, unchanged by this plan).
- Consumes: `Engine::memory_used(&self) -> usize` (already exists).
- Consumes: `Value::approx_size(&self) -> usize` (already exists on `crate::Value`, already `pub`).
- Consumes: `SortedSet::score(&self, member: &[u8]) -> Option<f64>` (already exists on `crate::SortedSet`, used by the current code to compute `is_new` — reused as-is).
- Produces: no new public functions — `zadd`/`zrem`/`zincrby` keep their existing signatures exactly.

- [ ] **Step 1: Write the `bytes_used`-correctness test**

Add this to `crates/engine/src/commands/sorted_set.rs`'s existing `#[cfg(test)] mod tests` block (it already has `use super::*; use crate::Engine; use bytes::Bytes;` at the top — don't duplicate those imports):

```rust
    /// `engine.memory_used()` after each mutation must equal independently recomputing the
    /// entry's true size from scratch (`key.len() + Value::approx_size()`) -- proving the
    /// delta each function reports (once converted to `with_mut_delta`) is exactly right.
    /// Written to pass against today's `with_mut`-based code too, since this is a refactor,
    /// not a bug fix -- it stays green through every step below.
    fn assert_memory_used_matches_recomputed_size(engine: &Engine, key: &[u8]) {
        let value = engine.get(key).expect("key must exist");
        let expected = key.len() + value.approx_size();
        assert_eq!(engine.memory_used(), expected);
    }

    #[test]
    fn sorted_set_mutations_keep_memory_used_exactly_in_sync() {
        let engine = Engine::new();
        let key = Bytes::from_static(b"z");

        // zadd: new member
        zadd(&engine, key.clone(), 5.0, Bytes::from_static(b"alice")).unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // zadd: existing member, score-only update -- must not change the size
        zadd(&engine, key.clone(), 9.0, Bytes::from_static(b"alice")).unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // zincrby: new member
        zincrby(&engine, key.clone(), 2.0, Bytes::from_static(b"bob")).unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // zincrby: existing member, score-only update
        zincrby(&engine, key.clone(), 3.0, Bytes::from_static(b"bob")).unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // zrem: removes an existing member
        zrem(&engine, &key, b"alice").unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // zrem: member already absent, no-op
        zrem(&engine, &key, b"alice").unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);
    }
```

- [ ] **Step 2: Run the test and confirm it PASSES against today's code**

Run: `cargo test -p engine commands::sorted_set::tests::sorted_set_mutations_keep_memory_used_exactly_in_sync -- --nocapture`
Expected: `test result: ok. 1 passed`. If this fails, stop and re-read the spec's formula section before proceeding — do not convert anything against a test you haven't confirmed is already green.

- [ ] **Step 3: Convert `zadd`**

Replace:

```rust
pub fn zadd(
    engine: &Engine,
    key: Bytes,
    score: f64,
    member: Bytes,
) -> Result<bool, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<bool>, common::EngineError> {
            match existing {
                Some(Value::SortedSet(zset)) => {
                    let is_new = zset.score(&member).is_none();
                    zset.insert(member.clone(), score);
                    Ok(Some(is_new))
                }
                Some(_) => Err(common::EngineError::WrongType),
                None => Ok(None),
            }
        },
    )?;
```

with:

```rust
pub fn zadd(
    engine: &Engine,
    key: Bytes,
    score: f64,
    member: Bytes,
) -> Result<bool, common::EngineError> {
    let existed = engine.with_mut_delta(
        &key,
        |existing| -> (Result<Option<bool>, common::EngineError>, isize) {
            match existing {
                Some(Value::SortedSet(zset)) => {
                    let is_new = zset.score(&member).is_none();
                    zset.insert(member.clone(), score);
                    // A member's score is never part of `approx_size` -- only a brand-new
                    // member changes the total size; updating an existing member's score
                    // leaves it unchanged.
                    let size_delta = if is_new {
                        member.len() as isize + 24
                    } else {
                        0
                    };
                    (Ok(Some(is_new)), size_delta)
                }
                Some(_) => (Err(common::EngineError::WrongType), 0),
                None => (Ok(None), 0),
            }
        },
    )?;
```

Leave the rest of `zadd` (the `match existed { ... }` block below) untouched.

- [ ] **Step 4: Run tests, confirm still green**

Run: `cargo test -p engine commands::sorted_set::tests`
Expected: every test in the module passes, including `sorted_set_mutations_keep_memory_used_exactly_in_sync` from Step 1.

- [ ] **Step 5: Convert `zrem`**

Replace:

```rust
pub fn zrem(engine: &Engine, key: &[u8], member: &[u8]) -> Result<bool, common::EngineError> {
    engine.with_mut(key, |existing| match existing {
        None => Ok(false),
        Some(Value::SortedSet(zset)) => Ok(zset.remove(member)),
        Some(_) => Err(common::EngineError::WrongType),
    })
}
```

with:

```rust
pub fn zrem(engine: &Engine, key: &[u8], member: &[u8]) -> Result<bool, common::EngineError> {
    engine.with_mut_delta(key, |existing| match existing {
        None => (Ok(false), 0),
        Some(Value::SortedSet(zset)) => {
            let removed = zset.remove(member);
            let size_delta = if removed {
                -(member.len() as isize + 24)
            } else {
                0
            };
            (Ok(removed), size_delta)
        }
        Some(_) => (Err(common::EngineError::WrongType), 0),
    })
}
```

- [ ] **Step 6: Run tests, confirm still green**

Run: `cargo test -p engine commands::sorted_set::tests`
Expected: all pass.

- [ ] **Step 7: Convert `zincrby`**

Replace:

```rust
pub fn zincrby(
    engine: &Engine,
    key: Bytes,
    delta: f64,
    member: Bytes,
) -> Result<f64, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<f64>, common::EngineError> {
            match existing {
                Some(Value::SortedSet(zset)) => {
                    let new_score = zset.score(&member).unwrap_or(0.0) + delta;
                    zset.insert(member.clone(), new_score);
                    Ok(Some(new_score))
                }
                Some(_) => Err(common::EngineError::WrongType),
                None => Ok(None),
            }
        },
    )?;
```

with:

```rust
pub fn zincrby(
    engine: &Engine,
    key: Bytes,
    delta: f64,
    member: Bytes,
) -> Result<f64, common::EngineError> {
    let existed = engine.with_mut_delta(
        &key,
        |existing| -> (Result<Option<f64>, common::EngineError>, isize) {
            match existing {
                Some(Value::SortedSet(zset)) => {
                    let is_new = zset.score(&member).is_none();
                    let new_score = zset.score(&member).unwrap_or(0.0) + delta;
                    zset.insert(member.clone(), new_score);
                    let size_delta = if is_new {
                        member.len() as isize + 24
                    } else {
                        0
                    };
                    (Ok(Some(new_score)), size_delta)
                }
                Some(_) => (Err(common::EngineError::WrongType), 0),
                None => (Ok(None), 0),
            }
        },
    )?;
```

Note the parameter is still named `delta` (an `f64` score delta, unrelated to the new `isize` byte `size_delta` computed inside the closure) — kept as-is to avoid changing this function's public signature; `size_delta` is used for the new byte-count variable specifically to avoid confusion with it.

- [ ] **Step 8: Run tests, confirm still green**

Run: `cargo test -p engine commands::sorted_set::tests`
Expected: all pass.

- [ ] **Step 9: Full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all three clean — this is the project's CI gate (`CLAUDE.md`).

- [ ] **Step 10: Manual benchmark verification**

`redis-benchmark` has no built-in `ZADD` test, so this uses a small shell loop against `redis-cli` instead:

```bash
ROCKET_MEM_ADDR=127.0.0.1:16403 ROCKET_MEM_AOF_PATH=/tmp/zset-bench.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/zset-bench.snapshot cargo run --release --bin rocket-mem &
sleep 3
time (for i in $(seq 1 20000); do redis-cli -p 16403 zadd myzset "$i" "member-$i" > /dev/null; done)
redis-cli -p 16403 zcard myzset
kill %1
rm -f /tmp/zset-bench.aof /tmp/zset-bench.snapshot*
```

(20,000 not 100,000: each iteration pays a fresh `redis-cli` process-spawn cost that dwarfs the actual `ZADD` round-trip, so this loop measures relative shape — whether the last 5,000 iterations take noticeably longer than the first 5,000 — not absolute throughput. Compare against running the same loop on the pre-conversion binary if you want a concrete before/after number; the correctness test from Step 1 is what actually proves the fix, this step is a sanity check.) Expected: no visible slowdown as `myzset` grows. Record what you observe in the commit message body in Step 11.

- [ ] **Step 11: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit `crates/engine/src/commands/sorted_set.rs` — do not compose the commit message freeform. Suggested subject: `perf(engine): make SortedSet bytes_used accounting O(1) per mutation`. Include what you observed in Step 10 in the body.
