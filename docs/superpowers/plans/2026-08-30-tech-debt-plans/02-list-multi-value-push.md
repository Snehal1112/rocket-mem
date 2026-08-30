# RPUSH/LPUSH Multi-Value Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `engine::commands::list::rpush`/`lpush` accept all of a multi-value push's values in one shard-lock acquisition and return the resulting list length directly, removing the dispatcher's per-value loop and compensating `llen` call.

**Architecture:** Change both functions' signature from `(engine: &Engine, key: Bytes, val: Bytes) -> Result<(), EngineError>` to `(engine: &Engine, key: Bytes, values: Vec<Bytes>) -> Result<usize, EngineError>`. When the key already holds a `List`, push every value inside a single `with_mut` closure and return `list.len()` from inside it. When the key doesn't exist, build the whole list from `values` and `Engine::set` it once — this is the same two-step "probe, then set" shape the missing-key case already uses today, just extended to a batch of values instead of one.

**Tech Stack:** Rust, existing `engine` and `server` crates, no new dependencies.

**Spec:** `../../specs/2026-08-30-tech-debt-cleanup-spec.md` (Item 1)

## Global Constraints

- No other call site in the codebase calls `commands::list::rpush`/`lpush` besides `crates/server/src/dispatcher.rs` and `list.rs`'s own unit tests (confirmed by repo-wide grep) — this plan's tasks are the complete set of call sites to update.
- `RPUSH`/`LPUSH`'s user-visible RESP behavior (return value, insertion order, WRONGTYPE, missing-key semantics) must not change — this is an internal efficiency fix, not a behavior change. Every existing dispatcher-level test for these two commands must keep passing unmodified.
- `cargo fmt --all -- --check` and `cargo clippy --workspace -- -D warnings` must both pass before any commit in this plan.

---

### Task 1: `rpush`/`lpush` accept `Vec<Bytes>` and return the length

**Files:**
- Modify: `crates/engine/src/commands/list.rs:12-29` (`rpush`), `:31-48` (`lpush`)
- Test: `crates/engine/src/commands/list.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub fn rpush(engine: &Engine, key: Bytes, values: Vec<Bytes>) -> Result<usize, common::EngineError>` and `pub fn lpush(engine: &Engine, key: Bytes, values: Vec<Bytes>) -> Result<usize, common::EngineError>`. Both return the list's length *after* the push, matching real Redis `RPUSH`/`LPUSH` reply semantics.
- Consumed by: Task 2 (dispatcher.rs).

- [ ] **Step 1: Update the existing unit tests to the new signature**

`list.rs`'s tests currently call `rpush(&engine, key, Bytes::from_static(b"a"))` (a single `Bytes`). Update every call site in the `#[cfg(test)] mod tests` block of `crates/engine/src/commands/list.rs` to pass a `vec![...]` instead. For example, change:

```rust
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b")).unwrap();
```

to:

```rust
        rpush(&engine, Bytes::from_static(b"l"), vec![Bytes::from_static(b"a")]).unwrap();
        rpush(&engine, Bytes::from_static(b"l"), vec![Bytes::from_static(b"b")]).unwrap();
```

Apply this same single-value-wrapped-in-`vec![...]` change to every `rpush(&engine, ...)` and `lpush(&engine, ...)` call in that test module — there are 19 such call sites across `rpush_then_lrange_returns_in_insertion_order`, `lpush_prepends`, `rpop_returns_and_removes_last_element`, `rpush_on_string_key_returns_wrongtype`, `lpush_on_string_key_returns_wrongtype`, `lindex_returns_the_element_at_a_positive_index`, `lindex_supports_negative_indices`, `lindex_out_of_range_returns_none_not_an_error`, `lset_replaces_the_element_at_index_and_reports_success`, `lset_out_of_range_returns_false_not_an_error`, `ltrim_keeps_only_the_requested_range`, `lrem_positive_count_removes_from_head_up_to_count`, `lrem_negative_count_removes_from_tail_up_to_count`, `lrem_zero_count_removes_every_occurrence`, `linsert_before_pivot_shifts_the_pivot_right`, `linsert_pivot_not_found_returns_negative_one`, `lindex_on_string_key_returns_wrongtype`. Also add two new tests, after `lpush_prepends`:

```rust
    #[test]
    fn rpush_with_multiple_values_pushes_all_in_one_call_in_order() {
        let engine = Engine::new();
        let len = rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"c"),
            ],
        )
        .unwrap();
        assert_eq!(len, 3);
        assert_eq!(
            lrange(&engine, b"l", 0, -1).unwrap(),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"c"),
            ]
        );
    }

    #[test]
    fn lpush_with_multiple_values_prepends_each_so_the_last_argument_ends_up_first() {
        let engine = Engine::new();
        let len = lpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"c"),
            ],
        )
        .unwrap();
        assert_eq!(len, 3);
        assert_eq!(
            lrange(&engine, b"l", 0, -1).unwrap(),
            vec![
                Bytes::from_static(b"c"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"a"),
            ]
        );
    }

    #[test]
    fn rpush_multiple_values_onto_an_existing_list_appends_after_the_existing_tail() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), vec![Bytes::from_static(b"a")]).unwrap();
        let len = rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"b"), Bytes::from_static(b"c")],
        )
        .unwrap();
        assert_eq!(len, 3);
        assert_eq!(
            lrange(&engine, b"l", 0, -1).unwrap(),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"c"),
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cargo test -p engine commands::list::tests`
Expected: compile FAILURE — `rpush`/`lpush` still take a single `Bytes`, not a `Vec<Bytes>`, so the updated test calls don't type-check yet.

- [ ] **Step 3: Implement the new signatures**

Replace both functions in `crates/engine/src/commands/list.rs`:

```rust
pub fn rpush(
    engine: &Engine,
    key: Bytes,
    values: Vec<Bytes>,
) -> Result<usize, common::EngineError> {
    let existed = engine.with_mut(&key, |existing| -> Result<Option<usize>, common::EngineError> {
        match existing {
            Some(Value::List(list)) => {
                for val in &values {
                    list.push_back(val.clone()); // Bytes clone is O(1), not a deep copy
                }
                Ok(Some(list.len()))
            }
            Some(_) => Err(common::EngineError::WrongType),
            None => Ok(None),
        }
    })?;
    match existed {
        Some(len) => Ok(len),
        None => {
            let list: VecDeque<Bytes> = values.into_iter().collect();
            let len = list.len();
            engine.set(key, Value::List(list));
            Ok(len)
        }
    }
}

pub fn lpush(
    engine: &Engine,
    key: Bytes,
    values: Vec<Bytes>,
) -> Result<usize, common::EngineError> {
    let existed = engine.with_mut(&key, |existing| -> Result<Option<usize>, common::EngineError> {
        match existing {
            Some(Value::List(list)) => {
                for val in &values {
                    list.push_front(val.clone());
                }
                Ok(Some(list.len()))
            }
            Some(_) => Err(common::EngineError::WrongType),
            None => Ok(None),
        }
    })?;
    match existed {
        Some(len) => Ok(len),
        None => {
            // LPUSH with multiple values prepends each in argument order, so the *last*
            // argument ends up first (matches the dispatcher-level test and real Redis) --
            // pushing onto a fresh VecDeque front-to-back in argument order achieves that
            // directly, so no reversal is needed here.
            let mut list = VecDeque::new();
            for val in values {
                list.push_front(val);
            }
            let len = list.len();
            engine.set(key, Value::List(list));
            Ok(len)
        }
    }
}
```

Note the asymmetry between `rpush`'s missing-key branch (`values.into_iter().collect()`, a straight append) and `lpush`'s (an explicit `push_front` loop) — `RPUSH a b c` on an empty list must yield `[a, b, c]` (collect preserves order), while `LPUSH a b c` must yield `[c, b, a]` (each value prepended in turn puts the last argument at the front).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p engine commands::list::tests`
Expected: PASS, including the three new multi-value tests from Step 1.

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/commands/list.rs
git commit -m "feat(engine): push all RPUSH/LPUSH values in one shard-lock acquisition"
```

---

### Task 2: Wire the dispatcher to the new signature

**Files:**
- Modify: `crates/server/src/dispatcher.rs:372-393`

**Interfaces:**
- Consumes: `commands::list::rpush(engine: &Engine, key: Bytes, values: Vec<Bytes>) -> Result<usize, common::EngineError>` and the `lpush` equivalent, from Task 1.
- Produces: no change to `RPUSH`/`LPUSH`'s RESP-visible behavior — this task only changes how the dispatcher arm is implemented internally.

- [ ] **Step 1: Confirm the existing dispatcher-level tests already specify the desired external behavior**

`crates/server/src/dispatcher.rs` already has `rpush_with_multiple_values_pushes_all_in_order_and_returns_final_length` and `lpush_with_multiple_values_prepends_each_so_the_last_argument_ends_up_first` (around line 1770-1820) — these pass today against the old loop-plus-`llen` implementation and must keep passing unchanged after this task, proving the refactor is behavior-preserving. No new dispatcher-level test is needed for this task; run the existing suite before and after Step 3 to confirm.

Run (before making any change, to establish the baseline): `cargo test -p rocket-mem dispatcher::tests::rpush_with_multiple_values dispatcher::tests::lpush_with_multiple_values`
Expected: PASS (this is the pre-change baseline, not a new failing test — this task is a pure refactor).

- [ ] **Step 2: Replace the `RPUSH` and `LPUSH` dispatcher arms**

In `crates/server/src/dispatcher.rs`, replace:

```rust
        "RPUSH" => {
            require_args!(rest, 2, "rpush");
            for val in &rest[1..] {
                if let Err(e) = commands::list::rpush(engine, rest[0].clone(), val.clone()) {
                    return engine_error_to_frame(e);
                }
            }
            match commands::list::llen(engine, &rest[0]) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LPUSH" => {
            require_args!(rest, 2, "lpush");
            for val in &rest[1..] {
                if let Err(e) = commands::list::lpush(engine, rest[0].clone(), val.clone()) {
                    return engine_error_to_frame(e);
                }
            }
            match commands::list::llen(engine, &rest[0]) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
```

with:

```rust
        "RPUSH" => {
            require_args!(rest, 2, "rpush");
            match commands::list::rpush(engine, rest[0].clone(), rest[1..].to_vec()) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LPUSH" => {
            require_args!(rest, 2, "lpush");
            match commands::list::lpush(engine, rest[0].clone(), rest[1..].to_vec()) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
```

- [ ] **Step 3: Run the full server test suite**

Run: `cargo test -p rocket-mem`
Expected: PASS, including both multi-value tests from Step 1 and every other existing `RPUSH`/`LPUSH`-touching test (the WRONGTYPE matrix, missing-key semantics tests, and the AOF-related tests that log `RPUSH`/`LPUSH` — none of these depend on the internal implementation, only on the RESP-visible reply).

- [ ] **Step 4: Run the full workspace test suite and lints**

Run: `cargo test --workspace && cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "refactor(server): drop RPUSH/LPUSH's per-value loop now the engine pushes in one call"
```
