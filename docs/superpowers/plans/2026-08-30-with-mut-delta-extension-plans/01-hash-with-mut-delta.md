# Hash `with_mut_delta` Conversion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** convert `hash.rs`'s four mutating functions (`hset`, `hdel`, `hincrby`, `hsetnx`) from `Engine::with_mut` to `Engine::with_mut_delta`, so a growing `Hash` no longer pays an O(current field count) `bytes_used` rescan on every `HSET`/`HDEL`/`HINCRBY`/`HSETNX` call.

**Architecture:** pure refactor — `bytes_used` accounting only, no change to any function's return value, error behavior, or the `WRONGTYPE`/missing-key semantics `CLAUDE.md` documents. `Engine::with_mut_delta` already exists (built for the equivalent `list.rs` fix, unrelated to this plan). This plan touches only `crates/engine/src/commands/hash.rs`.

**Tech Stack:** Rust, existing `Engine`/`Value` types, no new dependencies.

**Spec:** `../../specs/2026-08-30-with-mut-delta-extension-spec.md` — read its "Decision: exact per-type delta formulas" section for `Hash` before starting; every delta computation below is copied from it.

## Global Constraints

- `Value::approx_size`'s `Hash` formula (unchanged, ground truth): `field.len() + value.len() + 16` per field/value pair — every delta below must reproduce this exactly.
- No behavior changes: return values, `WRONGTYPE` errors, and the missing-key convention (a read on a missing key is `None`/empty, not an error) must be byte-for-byte identical to today.
- This is a refactor, not a bug fix: the correctness test in Step 1 must **pass against today's unmodified code** before any conversion happens — that's what proves it's a valid safety net, not a trivially-true assertion.

---

### Task 1: Convert `hset`/`hdel`/`hincrby`/`hsetnx` to `with_mut_delta`

**Files:**
- Modify: `crates/engine/src/commands/hash.rs`

**Interfaces:**
- Consumes: `Engine::with_mut_delta<F, R>(&self, key: &[u8], f: F) -> R where F: FnOnce(Option<&mut Value>) -> (R, isize)` (already exists in `crates/engine/src/engine.rs`, unchanged by this plan).
- Consumes: `Engine::memory_used(&self) -> usize` (already exists).
- Consumes: `Value::approx_size(&self) -> usize` (already exists on `crate::Value`, already `pub`).
- Produces: no new public functions — `hset`/`hdel`/`hincrby`/`hsetnx` keep their existing signatures exactly.

- [ ] **Step 1: Write the `bytes_used`-correctness test**

Add this to `crates/engine/src/commands/hash.rs`'s existing `#[cfg(test)] mod tests` block (it already has `use super::*; use crate::{Engine, Value}; use bytes::Bytes;` at the top — don't duplicate those imports):

```rust
    /// `engine.memory_used()` after each mutation must equal independently recomputing the
    /// entry's true size from scratch (`key.len() + Value::approx_size()`) -- proving the
    /// delta each function reports (once converted to `with_mut_delta`) is exactly right, not
    /// just "close enough." Written to pass against today's `with_mut`-based code too, since
    /// this is a refactor, not a bug fix -- it stays green through every step below.
    fn assert_memory_used_matches_recomputed_size(engine: &Engine, key: &[u8]) {
        let value = engine.get(key).expect("key must exist");
        let expected = key.len() + value.approx_size();
        assert_eq!(engine.memory_used(), expected);
    }

    #[test]
    fn hash_mutations_keep_memory_used_exactly_in_sync() {
        let engine = Engine::new();
        let key = Bytes::from_static(b"h");

        // hset: new field
        hset(
            &engine,
            key.clone(),
            Bytes::from_static(b"f1"),
            Bytes::from_static(b"v1"),
        )
        .unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // hset: overwrite an existing field with a longer value
        hset(
            &engine,
            key.clone(),
            Bytes::from_static(b"f1"),
            Bytes::from_static(b"a-much-longer-value"),
        )
        .unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // hincrby: new field
        hincrby(&engine, key.clone(), Bytes::from_static(b"counter"), 5).unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // hincrby: existing field, value grows from "5" to "15" (still 2 chars, but exercises
        // the existing-field branch)
        hincrby(&engine, key.clone(), Bytes::from_static(b"counter"), 10).unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // hsetnx: field absent, inserts
        hsetnx(
            &engine,
            key.clone(),
            Bytes::from_static(b"f2"),
            Bytes::from_static(b"v2"),
        )
        .unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // hsetnx: field present, no-op
        hsetnx(
            &engine,
            key.clone(),
            Bytes::from_static(b"f2"),
            Bytes::from_static(b"ignored"),
        )
        .unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // hdel: removes a field
        hdel(&engine, &key, b"f2").unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // hdel: field already absent, no-op
        hdel(&engine, &key, b"f2").unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);
    }
```

- [ ] **Step 2: Run the test and confirm it PASSES against today's code**

Run: `cargo test -p engine commands::hash::tests::hash_mutations_keep_memory_used_exactly_in_sync -- --nocapture`
Expected: `test result: ok. 1 passed`. This confirms the test is a valid characterization of correct behavior *before* touching `hset`/`hdel`/`hincrby`/`hsetnx` — today's `with_mut`-based code already computes `bytes_used` correctly, just expensively. If this fails, stop and re-read the spec's formula section; do not proceed to convert anything against a test you haven't confirmed is already green.

- [ ] **Step 3: Convert `hset`**

Replace:

```rust
pub fn hset(
    engine: &Engine,
    key: Bytes,
    field: Bytes,
    val: Bytes,
) -> Result<bool, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<bool>, common::EngineError> {
            match existing {
                Some(Value::Hash(map)) => {
                    let is_new = !map.contains_key(&field);
                    map.insert(field.clone(), val.clone());
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
pub fn hset(
    engine: &Engine,
    key: Bytes,
    field: Bytes,
    val: Bytes,
) -> Result<bool, common::EngineError> {
    let existed = engine.with_mut_delta(
        &key,
        |existing| -> (Result<Option<bool>, common::EngineError>, isize) {
            match existing {
                Some(Value::Hash(map)) => {
                    // `HashMap::insert` returns the field's previous value for free, so no
                    // separate `contains_key` probe is needed -- and its length is exactly
                    // what the delta needs when overwriting.
                    let old = map.insert(field.clone(), val.clone());
                    let size_delta = match &old {
                        None => field.len() as isize + val.len() as isize + 16,
                        Some(old_val) => val.len() as isize - old_val.len() as isize,
                    };
                    (Ok(Some(old.is_none())), size_delta)
                }
                Some(_) => (Err(common::EngineError::WrongType), 0),
                None => (Ok(None), 0),
            }
        },
    )?;
```

Leave the rest of `hset` (the `match existed { ... }` block below) untouched — it's unchanged by this conversion.

- [ ] **Step 4: Run tests, confirm still green**

Run: `cargo test -p engine commands::hash::tests`
Expected: every test in the module passes, including `hash_mutations_keep_memory_used_exactly_in_sync` from Step 1.

- [ ] **Step 5: Convert `hdel`**

Replace:

```rust
pub fn hdel(engine: &Engine, key: &[u8], field: &[u8]) -> Result<bool, common::EngineError> {
    engine.with_mut(key, |existing| match existing {
        None => Ok(false),
        Some(Value::Hash(map)) => Ok(map.remove(field).is_some()),
        Some(_) => Err(common::EngineError::WrongType),
    })
}
```

with:

```rust
pub fn hdel(engine: &Engine, key: &[u8], field: &[u8]) -> Result<bool, common::EngineError> {
    engine.with_mut_delta(key, |existing| match existing {
        None => (Ok(false), 0),
        Some(Value::Hash(map)) => {
            let removed = map.remove(field);
            let size_delta = match &removed {
                Some(old_val) => -(field.len() as isize + old_val.len() as isize + 16),
                None => 0,
            };
            (Ok(removed.is_some()), size_delta)
        }
        Some(_) => (Err(common::EngineError::WrongType), 0),
    })
}
```

- [ ] **Step 6: Run tests, confirm still green**

Run: `cargo test -p engine commands::hash::tests`
Expected: all pass.

- [ ] **Step 7: Convert `hincrby`**

Replace:

```rust
pub fn hincrby(
    engine: &Engine,
    key: Bytes,
    field: Bytes,
    delta: i64,
) -> Result<i64, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<i64>, common::EngineError> {
            match existing {
                Some(Value::Hash(map)) => {
                    let current: i64 = match map.get(&field) {
                        Some(b) => std::str::from_utf8(b)
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .ok_or(common::EngineError::NotAnInteger)?,
                        None => 0,
                    };
                    let next = current + delta;
                    map.insert(field.clone(), Bytes::from(next.to_string()));
                    Ok(Some(next))
                }
                Some(_) => Err(common::EngineError::WrongType),
                None => Ok(None),
            }
        },
    )?;
```

with:

```rust
pub fn hincrby(
    engine: &Engine,
    key: Bytes,
    field: Bytes,
    delta: i64,
) -> Result<i64, common::EngineError> {
    let existed = engine.with_mut_delta(
        &key,
        |existing| -> (Result<Option<i64>, common::EngineError>, isize) {
            match existing {
                Some(Value::Hash(map)) => {
                    let old = map.get(&field);
                    let current: i64 = match old {
                        Some(b) => match std::str::from_utf8(b).ok().and_then(|s| s.parse().ok())
                        {
                            Some(n) => n,
                            None => return (Err(common::EngineError::NotAnInteger), 0),
                        },
                        None => 0,
                    };
                    let old_len = old.map(|b| b.len());
                    let next = current + delta;
                    let next_bytes = Bytes::from(next.to_string());
                    let size_delta = match old_len {
                        None => field.len() as isize + next_bytes.len() as isize + 16,
                        Some(len) => next_bytes.len() as isize - len as isize,
                    };
                    map.insert(field.clone(), next_bytes);
                    (Ok(Some(next)), size_delta)
                }
                Some(_) => (Err(common::EngineError::WrongType), 0),
                None => (Ok(None), 0),
            }
        },
    )?;
```

Note the `NotAnInteger` branch changed shape: the original used `.ok_or(...)?` inside a closure returning `Result<Option<i64>, EngineError>`, which the outer `?` propagated directly. `with_mut_delta`'s closure returns `(R, isize)`, not a bare `Result`, so `?` can no longer be used inside it — the `match ... { Some(n) => n, None => return (Err(...), 0) }` above is the direct equivalent, returning early from the closure with a `0` delta (no mutation happened on that path).

- [ ] **Step 8: Run tests, confirm still green**

Run: `cargo test -p engine commands::hash::tests`
Expected: all pass, including `hincrby_on_non_integer_field_returns_not_an_integer_error`.

- [ ] **Step 9: Convert `hsetnx`**

Replace:

```rust
pub fn hsetnx(
    engine: &Engine,
    key: Bytes,
    field: Bytes,
    val: Bytes,
) -> Result<bool, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<bool>, common::EngineError> {
            match existing {
                Some(Value::Hash(map)) => {
                    if map.contains_key(&field) {
                        Ok(Some(false))
                    } else {
                        map.insert(field.clone(), val.clone());
                        Ok(Some(true))
                    }
                }
                Some(_) => Err(common::EngineError::WrongType),
                None => Ok(None),
            }
        },
    )?;
```

with:

```rust
pub fn hsetnx(
    engine: &Engine,
    key: Bytes,
    field: Bytes,
    val: Bytes,
) -> Result<bool, common::EngineError> {
    let existed = engine.with_mut_delta(
        &key,
        |existing| -> (Result<Option<bool>, common::EngineError>, isize) {
            match existing {
                Some(Value::Hash(map)) => {
                    if map.contains_key(&field) {
                        (Ok(Some(false)), 0)
                    } else {
                        let size_delta = field.len() as isize + val.len() as isize + 16;
                        map.insert(field.clone(), val.clone());
                        (Ok(Some(true)), size_delta)
                    }
                }
                Some(_) => (Err(common::EngineError::WrongType), 0),
                None => (Ok(None), 0),
            }
        },
    )?;
```

- [ ] **Step 10: Run tests, confirm still green**

Run: `cargo test -p engine commands::hash::tests`
Expected: all pass.

- [ ] **Step 11: Full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all three clean — this is the project's CI gate (`CLAUDE.md`).

- [ ] **Step 12: Manual benchmark verification**

Start the server against scratch paths so nothing pollutes the repo root (per `CLAUDE.md`'s AOF/snapshot-path convention):

```bash
ROCKET_MEM_ADDR=127.0.0.1:16401 ROCKET_MEM_AOF_PATH=/tmp/hash-bench.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/hash-bench.snapshot cargo run --release --bin rocket-mem &
sleep 3
redis-benchmark -p 16401 -t HSET -n 100000 -r 100000 -q
redis-cli -p 16401 hlen myhash
kill %1
rm -f /tmp/hash-bench.aof /tmp/hash-bench.snapshot*
```

`-r 100000` forces unique fields (`redis-benchmark`'s default reuses one literal field, which would never exercise the O(n) path at all — see the spec's "Why" section). Expected: `HSET` throughput stays roughly flat across the run (comparable to `SET`'s own req/s), not degrading as `hlen myhash` grows toward 100000 — contrast this against the `LPUSH` reproduction that motivated this whole fix (throughput dropping from ~18.7k to ~9.5k req/s as the list grew). Record the actual numbers you see in the commit message body in Step 13.

- [ ] **Step 13: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit `crates/engine/src/commands/hash.rs` — do not compose the commit message freeform. Suggested subject: `perf(engine): make Hash bytes_used accounting O(1) per mutation`. Include the benchmark numbers from Step 12 in the body.
