# Remaining List Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `LINDEX`/`LSET`/`LTRIM`/`LREM`/`LINSERT`, rounding out List command coverage per `../../rocket-mem-production-plan.md`'s Week 6 detail for the "Remaining List/Hash/Set commands" backlog item.

**Architecture:** all five extend `crates/engine/src/commands/list.rs`, reusing the existing private `get_list` helper and the existing negative-index normalization pattern from `lrange`.

**Tech Stack:** no new dependencies.

**Spec:** `../../specs/2026-08-29-sprint-3-spec.md` — no list-specific decisions; this plan follows Sprint 1's established `list.rs` conventions directly.

**Depends on:** nothing this sprint. Independent of every other Sprint 3 plan.

---

### Task 1: `LINDEX`/`LSET`/`LTRIM`/`LREM`/`LINSERT` (engine level)

**Files:**
- Modify: `crates/engine/src/commands/list.rs`
- Modify: `crates/engine/src/commands/wrongtype_matrix_tests.rs`

**Interfaces:**
- Consumes: the existing private `get_list` helper and `lrange` (both in `list.rs`).
- Produces: `pub fn lindex(engine: &Engine, key: &[u8], index: i64) -> Result<Option<Bytes>, common::EngineError>`, `pub fn lset(engine: &Engine, key: Bytes, index: i64, val: Bytes) -> Result<bool, common::EngineError>`, `pub fn ltrim(engine: &Engine, key: Bytes, start: i64, stop: i64) -> Result<(), common::EngineError>`, `pub fn lrem(engine: &Engine, key: Bytes, count: i64, val: &[u8]) -> Result<usize, common::EngineError>`, `pub fn linsert(engine: &Engine, key: Bytes, before: bool, pivot: &[u8], val: Bytes) -> Result<i64, common::EngineError>`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/commands/list.rs — add to the existing tests module
#[test]
fn lindex_returns_the_element_at_a_positive_index() {
    let engine = Engine::new();
    rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
    rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b")).unwrap();
    assert_eq!(lindex(&engine, b"l", 1).unwrap(), Some(Bytes::from_static(b"b")));
}

#[test]
fn lindex_supports_negative_indices() {
    let engine = Engine::new();
    rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
    rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b")).unwrap();
    assert_eq!(lindex(&engine, b"l", -1).unwrap(), Some(Bytes::from_static(b"b")));
}

#[test]
fn lindex_out_of_range_returns_none_not_an_error() {
    let engine = Engine::new();
    rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
    assert_eq!(lindex(&engine, b"l", 5).unwrap(), None);
}

#[test]
fn lset_replaces_the_element_at_index_and_reports_success() {
    let engine = Engine::new();
    rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
    assert!(lset(&engine, Bytes::from_static(b"l"), 0, Bytes::from_static(b"z")).unwrap());
    assert_eq!(lindex(&engine, b"l", 0).unwrap(), Some(Bytes::from_static(b"z")));
}

#[test]
fn lset_out_of_range_returns_false_not_an_error() {
    let engine = Engine::new();
    rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
    assert!(!lset(&engine, Bytes::from_static(b"l"), 5, Bytes::from_static(b"z")).unwrap());
}

#[test]
fn ltrim_keeps_only_the_requested_range() {
    let engine = Engine::new();
    for v in [b"a", b"b", b"c", b"d"] {
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(v)).unwrap();
    }
    ltrim(&engine, Bytes::from_static(b"l"), 1, 2).unwrap();
    assert_eq!(
        lrange(&engine, b"l", 0, -1).unwrap(),
        vec![Bytes::from_static(b"b"), Bytes::from_static(b"c")]
    );
}

#[test]
fn lrem_positive_count_removes_from_head_up_to_count() {
    let engine = Engine::new();
    for v in [b"a", b"x", b"b", b"x", b"c"] {
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(v)).unwrap();
    }
    let removed = lrem(&engine, Bytes::from_static(b"l"), 1, b"x").unwrap();
    assert_eq!(removed, 1);
    assert_eq!(
        lrange(&engine, b"l", 0, -1).unwrap(),
        vec![
            Bytes::from_static(b"a"),
            Bytes::from_static(b"b"),
            Bytes::from_static(b"x"),
            Bytes::from_static(b"c"),
        ]
    );
}

#[test]
fn lrem_negative_count_removes_from_tail_up_to_count() {
    let engine = Engine::new();
    for v in [b"a", b"x", b"b", b"x", b"c"] {
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(v)).unwrap();
    }
    let removed = lrem(&engine, Bytes::from_static(b"l"), -1, b"x").unwrap();
    assert_eq!(removed, 1);
    assert_eq!(
        lrange(&engine, b"l", 0, -1).unwrap(),
        vec![
            Bytes::from_static(b"a"),
            Bytes::from_static(b"x"),
            Bytes::from_static(b"b"),
            Bytes::from_static(b"c"),
        ]
    );
}

#[test]
fn lrem_zero_count_removes_every_occurrence() {
    let engine = Engine::new();
    for v in [b"a", b"x", b"b", b"x", b"c"] {
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(v)).unwrap();
    }
    let removed = lrem(&engine, Bytes::from_static(b"l"), 0, b"x").unwrap();
    assert_eq!(removed, 2);
    assert_eq!(
        lrange(&engine, b"l", 0, -1).unwrap(),
        vec![Bytes::from_static(b"a"), Bytes::from_static(b"b"), Bytes::from_static(b"c")]
    );
}

#[test]
fn linsert_before_pivot_shifts_the_pivot_right() {
    let engine = Engine::new();
    rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
    rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"c")).unwrap();
    let len = linsert(&engine, Bytes::from_static(b"l"), true, b"c", Bytes::from_static(b"b")).unwrap();
    assert_eq!(len, 3);
    assert_eq!(
        lrange(&engine, b"l", 0, -1).unwrap(),
        vec![Bytes::from_static(b"a"), Bytes::from_static(b"b"), Bytes::from_static(b"c")]
    );
}

#[test]
fn linsert_pivot_not_found_returns_negative_one() {
    let engine = Engine::new();
    rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
    assert_eq!(linsert(&engine, Bytes::from_static(b"l"), true, b"missing", Bytes::from_static(b"x")).unwrap(), -1);
}

#[test]
fn linsert_on_missing_key_returns_zero() {
    let engine = Engine::new();
    assert_eq!(linsert(&engine, Bytes::from_static(b"missing"), true, b"pivot", Bytes::from_static(b"x")).unwrap(), 0);
}

#[test]
fn lindex_on_string_key_returns_wrongtype() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    assert_eq!(lindex(&engine, b"k", 0).unwrap_err(), common::EngineError::WrongType);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine commands::list`
Expected: FAIL — the five new functions don't exist yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/commands/list.rs — add below the existing `lrange`
pub fn lindex(engine: &Engine, key: &[u8], index: i64) -> Result<Option<Bytes>, common::EngineError> {
    let list = get_list(engine, key)?;
    let len = list.len() as i64;
    let idx = if index < 0 { len + index } else { index };
    if idx < 0 || idx >= len {
        return Ok(None);
    }
    Ok(list.get(idx as usize).cloned())
}

pub fn lset(engine: &Engine, key: Bytes, index: i64, val: Bytes) -> Result<bool, common::EngineError> {
    let mut list = get_list(engine, &key)?;
    let len = list.len() as i64;
    let idx = if index < 0 { len + index } else { index };
    if idx < 0 || idx >= len {
        return Ok(false);
    }
    list[idx as usize] = val;
    engine.set(key, Value::List(list));
    Ok(true)
}

pub fn ltrim(engine: &Engine, key: Bytes, start: i64, stop: i64) -> Result<(), common::EngineError> {
    let trimmed = lrange(engine, &key, start, stop)?;
    engine.set(key, Value::List(trimmed.into_iter().collect()));
    Ok(())
}

/// Removes occurrences of `val`: `count > 0` removes up to `count` from the head,
/// `count < 0` removes up to `-count` from the tail, `count == 0` removes every occurrence.
/// Returns the number actually removed.
pub fn lrem(engine: &Engine, key: Bytes, count: i64, val: &[u8]) -> Result<usize, common::EngineError> {
    let list = get_list(engine, &key)?;
    let mut removed = 0usize;
    let new_list: VecDeque<Bytes> = if count >= 0 {
        let mut remaining = if count == 0 { usize::MAX } else { count as usize };
        let mut items: Vec<Bytes> = list.into_iter().collect();
        items.retain(|item| {
            if remaining > 0 && item.as_ref() == val {
                remaining -= 1;
                removed += 1;
                false
            } else {
                true
            }
        });
        items.into_iter().collect()
    } else {
        let mut remaining = (-count) as usize;
        let mut items: Vec<Bytes> = list.into_iter().rev().collect();
        items.retain(|item| {
            if remaining > 0 && item.as_ref() == val {
                remaining -= 1;
                removed += 1;
                false
            } else {
                true
            }
        });
        items.into_iter().rev().collect()
    };
    engine.set(key, Value::List(new_list));
    Ok(removed)
}

pub fn linsert(
    engine: &Engine,
    key: Bytes,
    before: bool,
    pivot: &[u8],
    val: Bytes,
) -> Result<i64, common::EngineError> {
    if !engine.exists(&key) {
        return Ok(0);
    }
    let mut list = get_list(engine, &key)?;
    let Some(pos) = list.iter().position(|item| item.as_ref() == pivot) else {
        return Ok(-1);
    };
    let insert_at = if before { pos } else { pos + 1 };
    list.insert(insert_at, val);
    let len = list.len() as i64;
    engine.set(key, Value::List(list));
    Ok(len)
}
```

- [ ] **Step 4: Add wrongtype coverage**

```rust
// crates/engine/src/commands/wrongtype_matrix_tests.rs — extend list_commands_reject_non_list_keys
assert_wrongtype!(list::lindex(&engine_with_string_key(), b"k", 0));
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p engine commands::list`
Expected: PASS, all tests including the 13 new ones

- [ ] **Step 6: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 7: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/commands/list.rs` and `crates/engine/src/commands/wrongtype_matrix_tests.rs`
— do not compose the commit message freeform. Suggested subject:
`feat(engine): add lindex/lset/ltrim/lrem/linsert list commands`.

---

### Task 2: Wire the five commands into the dispatcher

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `commands::list::{lindex, lset, ltrim, lrem, linsert}` (Task 1).
- Produces: five new `match` arms.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn lindex_lset_round_trip_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"RPUSH", b"l", b"a"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"LINDEX", b"l", b"0"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"a"))
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"LSET", b"l", b"0", b"z"]), &mut Protocol::default(), 1),
        Frame::Simple("OK".into())
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"LINDEX", b"l", b"0"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"z"))
    );
}

#[test]
fn lset_out_of_range_is_a_resp_error() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"RPUSH", b"l", b"a"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"LSET", b"l", b"5", b"z"]), &mut Protocol::default(), 1),
        Frame::Error("ERR index out of range".into())
    );
}

#[test]
fn ltrim_then_lrange_round_trip_through_dispatch() {
    let engine = Engine::new();
    for v in [b"a" as &[u8], b"b", b"c"] {
        dispatch(&engine, cmd(&[b"RPUSH", b"l", v]), &mut Protocol::default(), 1);
    }
    assert_eq!(
        dispatch(&engine, cmd(&[b"LTRIM", b"l", b"0", b"1"]), &mut Protocol::default(), 1),
        Frame::Simple("OK".into())
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"LRANGE", b"l", b"0", b"-1"]), &mut Protocol::default(), 1),
        Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"a")), Frame::Bulk(Bytes::from_static(b"b"))])
    );
}

#[test]
fn lrem_returns_the_count_removed_through_dispatch() {
    let engine = Engine::new();
    for v in [b"a" as &[u8], b"x", b"x"] {
        dispatch(&engine, cmd(&[b"RPUSH", b"l", v]), &mut Protocol::default(), 1);
    }
    assert_eq!(
        dispatch(&engine, cmd(&[b"LREM", b"l", b"0", b"x"]), &mut Protocol::default(), 1),
        Frame::Integer(2)
    );
}

#[test]
fn linsert_before_and_after_work_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"RPUSH", b"l", b"a"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"RPUSH", b"l", b"c"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"LINSERT", b"l", b"BEFORE", b"c", b"b"]), &mut Protocol::default(), 1),
        Frame::Integer(3)
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"LRANGE", b"l", b"0", b"-1"]), &mut Protocol::default(), 1),
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"a")),
            Frame::Bulk(Bytes::from_static(b"b")),
            Frame::Bulk(Bytes::from_static(b"c")),
        ])
    );
}

#[test]
fn linsert_with_an_invalid_direction_is_a_resp_error() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"RPUSH", b"l", b"a"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"LINSERT", b"l", b"SIDEWAYS", b"a", b"b"]), &mut Protocol::default(), 1),
        Frame::Error("ERR syntax error".into())
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — none of the five commands are wired yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — add near the other List commands
"LINDEX" => {
    require_args!(rest, 2, "lindex");
    let index: i64 = match std::str::from_utf8(&rest[1]).ok().and_then(|s| s.parse().ok()) {
        Some(n) => n,
        None => return Frame::Error("ERR value is not an integer or out of range".into()),
    };
    match commands::list::lindex(engine, &rest[0], index) {
        Ok(Some(b)) => Frame::Bulk(b),
        Ok(None) => Frame::Null,
        Err(e) => engine_error_to_frame(e),
    }
}
"LSET" => {
    require_args!(rest, 3, "lset");
    let index: i64 = match std::str::from_utf8(&rest[1]).ok().and_then(|s| s.parse().ok()) {
        Some(n) => n,
        None => return Frame::Error("ERR value is not an integer or out of range".into()),
    };
    match commands::list::lset(engine, rest[0].clone(), index, rest[2].clone()) {
        Ok(true) => Frame::Simple("OK".into()),
        Ok(false) => Frame::Error("ERR index out of range".into()),
        Err(e) => engine_error_to_frame(e),
    }
}
"LTRIM" => {
    require_args!(rest, 3, "ltrim");
    let (start, stop) = match (
        std::str::from_utf8(&rest[1]).ok().and_then(|s| s.parse::<i64>().ok()),
        std::str::from_utf8(&rest[2]).ok().and_then(|s| s.parse::<i64>().ok()),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => return Frame::Error("ERR value is not an integer or out of range".into()),
    };
    match commands::list::ltrim(engine, rest[0].clone(), start, stop) {
        Ok(()) => Frame::Simple("OK".into()),
        Err(e) => engine_error_to_frame(e),
    }
}
"LREM" => {
    require_args!(rest, 3, "lrem");
    let count: i64 = match std::str::from_utf8(&rest[1]).ok().and_then(|s| s.parse().ok()) {
        Some(n) => n,
        None => return Frame::Error("ERR value is not an integer or out of range".into()),
    };
    match commands::list::lrem(engine, rest[0].clone(), count, &rest[2]) {
        Ok(n) => Frame::Integer(n as i64),
        Err(e) => engine_error_to_frame(e),
    }
}
"LINSERT" => {
    require_args!(rest, 4, "linsert");
    let before = match String::from_utf8_lossy(&rest[1]).to_ascii_uppercase().as_str() {
        "BEFORE" => true,
        "AFTER" => false,
        _ => return Frame::Error("ERR syntax error".into()),
    };
    match commands::list::linsert(engine, rest[0].clone(), before, &rest[2], rest[3].clone()) {
        Ok(n) => Frame::Integer(n),
        Err(e) => engine_error_to_frame(e),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 6 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): wire lindex/lset/ltrim/lrem/linsert list commands`.
