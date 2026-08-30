# Set `with_mut_delta` Conversion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** convert `set.rs`'s three mutating functions (`sadd`, `srem`, `spop`) from `Engine::with_mut` to `Engine::with_mut_delta`, so a growing `Set` no longer pays an O(current member count) `bytes_used` rescan on every `SADD`/`SREM`/`SPOP` call.

**Architecture:** pure refactor — `bytes_used` accounting only, no change to any function's return value, error behavior, or the `WRONGTYPE`/missing-key semantics `CLAUDE.md` documents. `Engine::with_mut_delta` already exists (built for the equivalent `list.rs` fix, unrelated to this plan). This plan touches only `crates/engine/src/commands/set.rs`. `sinterstore`/`sunionstore`/`sdiffstore` are explicitly **out of scope** — they build a whole new `Set` via `Engine::set`, not `with_mut`, and an O(result size) cost there is unavoidable and already correct (see the spec).

**Tech Stack:** Rust, existing `Engine`/`Value` types, no new dependencies.

**Spec:** `../../specs/2026-08-30-with-mut-delta-extension-spec.md` — read its "Decision: exact per-type delta formulas" section for `Set` before starting; every delta computation below is copied from it.

## Global Constraints

- `Value::approx_size`'s `Set` formula (unchanged, ground truth): `member.len() + 8` per member — every delta below must reproduce this exactly.
- No behavior changes: return values, `WRONGTYPE` errors, and the missing-key convention must be byte-for-byte identical to today.
- This is a refactor, not a bug fix: the correctness test in Step 1 must **pass against today's unmodified code** before any conversion happens.

---

### Task 1: Convert `sadd`/`srem`/`spop` to `with_mut_delta`

**Files:**
- Modify: `crates/engine/src/commands/set.rs`

**Interfaces:**
- Consumes: `Engine::with_mut_delta<F, R>(&self, key: &[u8], f: F) -> R where F: FnOnce(Option<&mut Value>) -> (R, isize)` (already exists in `crates/engine/src/engine.rs`, unchanged by this plan).
- Consumes: `Engine::memory_used(&self) -> usize` (already exists).
- Consumes: `Value::approx_size(&self) -> usize` (already exists on `crate::Value`, already `pub`).
- Produces: no new public functions — `sadd`/`srem`/`spop` keep their existing signatures exactly.

- [ ] **Step 1: Write the `bytes_used`-correctness test**

Add this to `crates/engine/src/commands/set.rs`'s existing `#[cfg(test)] mod tests` block (it already has `use super::*; use crate::Engine; use bytes::Bytes;` at the top — don't duplicate those imports):

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
    fn set_mutations_keep_memory_used_exactly_in_sync() {
        let engine = Engine::new();
        let key = Bytes::from_static(b"s");

        // sadd: two new members in one call, plus a duplicate within the same call that must
        // not be double-counted
        sadd(
            &engine,
            key.clone(),
            vec![
                Bytes::from_static(b"x"),
                Bytes::from_static(b"y"),
                Bytes::from_static(b"x"),
            ],
        )
        .unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // sadd: mix of an already-present member and a genuinely new one
        sadd(
            &engine,
            key.clone(),
            vec![Bytes::from_static(b"x"), Bytes::from_static(b"z")],
        )
        .unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // srem: mix of a present member and one that was never a member
        srem(
            &engine,
            &key,
            &[Bytes::from_static(b"y"), Bytes::from_static(b"never-there")],
        )
        .unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // spop: removes one remaining member (x or z)
        spop(&engine, &key).unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // spop: on the single remaining member
        spop(&engine, &key).unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);
    }
```

- [ ] **Step 2: Run the test and confirm it PASSES against today's code**

Run: `cargo test -p engine commands::set::tests::set_mutations_keep_memory_used_exactly_in_sync -- --nocapture`
Expected: `test result: ok. 1 passed`. If this fails, stop and re-read the spec's formula section before proceeding — do not convert anything against a test you haven't confirmed is already green. Note: after both `spop` calls the set is empty but the key still exists (an empty `Set` is a valid stored value, distinct from a missing key), so `engine.get(key)` in the helper still finds it — if a future edit to `spop` ever deleted an emptied set's key entirely, this test's `.expect("key must exist")` would need to move to `Option`-aware assertions, but that's not what this plan changes.

- [ ] **Step 3: Convert `sadd`**

Replace:

```rust
pub fn sadd(engine: &Engine, key: Bytes, members: Vec<Bytes>) -> Result<i64, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<i64>, common::EngineError> {
            match existing {
                Some(Value::Set(set)) => {
                    let mut added = 0i64;
                    for member in &members {
                        if set.insert(member.clone()) {
                            added += 1;
                        }
                    }
                    Ok(Some(added))
                }
                Some(_) => Err(common::EngineError::WrongType),
                None => Ok(None),
            }
        },
    )?;
```

with:

```rust
pub fn sadd(engine: &Engine, key: Bytes, members: Vec<Bytes>) -> Result<i64, common::EngineError> {
    let existed = engine.with_mut_delta(
        &key,
        |existing| -> (Result<Option<i64>, common::EngineError>, isize) {
            match existing {
                Some(Value::Set(set)) => {
                    let mut added = 0i64;
                    let mut size_delta = 0isize;
                    for member in &members {
                        if set.insert(member.clone()) {
                            added += 1;
                            size_delta += member.len() as isize + 8;
                        }
                    }
                    (Ok(Some(added)), size_delta)
                }
                Some(_) => (Err(common::EngineError::WrongType), 0),
                None => (Ok(None), 0),
            }
        },
    )?;
```

Leave the rest of `sadd` (the `match existed { ... }` block below) untouched.

- [ ] **Step 4: Run tests, confirm still green**

Run: `cargo test -p engine commands::set::tests`
Expected: every test in the module passes, including `set_mutations_keep_memory_used_exactly_in_sync` from Step 1.

- [ ] **Step 5: Convert `srem`**

Replace:

```rust
pub fn srem(engine: &Engine, key: &[u8], members: &[Bytes]) -> Result<i64, common::EngineError> {
    engine.with_mut(key, |existing| match existing {
        None => Ok(0),
        Some(Value::Set(set)) => {
            let mut removed = 0i64;
            for member in members {
                if set.remove(member.as_ref()) {
                    removed += 1;
                }
            }
            Ok(removed)
        }
        Some(_) => Err(common::EngineError::WrongType),
    })
}
```

with:

```rust
pub fn srem(engine: &Engine, key: &[u8], members: &[Bytes]) -> Result<i64, common::EngineError> {
    engine.with_mut_delta(key, |existing| match existing {
        None => (Ok(0), 0),
        Some(Value::Set(set)) => {
            let mut removed = 0i64;
            let mut size_delta = 0isize;
            for member in members {
                if set.remove(member.as_ref()) {
                    removed += 1;
                    size_delta -= member.len() as isize + 8;
                }
            }
            (Ok(removed), size_delta)
        }
        Some(_) => (Err(common::EngineError::WrongType), 0),
    })
}
```

- [ ] **Step 6: Run tests, confirm still green**

Run: `cargo test -p engine commands::set::tests`
Expected: all pass.

- [ ] **Step 7: Convert `spop`**

Replace:

```rust
pub fn spop(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    use rand::seq::IteratorRandom;
    engine.with_mut(key, |existing| {
        let set = match existing {
            None => return Ok(None),
            Some(Value::Set(set)) => set,
            Some(_) => return Err(common::EngineError::WrongType),
        };
        let Some(member) = set.iter().choose(&mut rand::thread_rng()).cloned() else {
            return Ok(None);
        };
        set.remove(&member);
        Ok(Some(member))
    })
}
```

with:

```rust
pub fn spop(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    use rand::seq::IteratorRandom;
    engine.with_mut_delta(key, |existing| {
        let set = match existing {
            None => return (Ok(None), 0),
            Some(Value::Set(set)) => set,
            Some(_) => return (Err(common::EngineError::WrongType), 0),
        };
        let Some(member) = set.iter().choose(&mut rand::thread_rng()).cloned() else {
            return (Ok(None), 0);
        };
        set.remove(&member);
        let size_delta = -(member.len() as isize + 8);
        (Ok(Some(member)), size_delta)
    })
}
```

- [ ] **Step 8: Run tests, confirm still green**

Run: `cargo test -p engine commands::set::tests`
Expected: all pass.

- [ ] **Step 9: Full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all three clean — this is the project's CI gate (`CLAUDE.md`).

- [ ] **Step 10: Manual benchmark verification**

```bash
ROCKET_MEM_ADDR=127.0.0.1:16402 ROCKET_MEM_AOF_PATH=/tmp/set-bench.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/set-bench.snapshot cargo run --release --bin rocket-mem &
sleep 3
redis-benchmark -p 16402 -t SADD -n 100000 -r 100000 -q
redis-cli -p 16402 scard myset
kill %1
rm -f /tmp/set-bench.aof /tmp/set-bench.snapshot*
```

`-r 100000` forces unique members (`redis-benchmark`'s default reuses one literal member, which would never exercise the O(n) path — see the spec's "Why" section). Expected: `SADD` throughput stays roughly flat across the run, not degrading as `scard myset` grows toward 100000. Record the actual numbers you see in the commit message body in Step 11.

- [ ] **Step 11: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit `crates/engine/src/commands/set.rs` — do not compose the commit message freeform. Suggested subject: `perf(engine): make Set bytes_used accounting O(1) per mutation`. Include the benchmark numbers from Step 10 in the body.
