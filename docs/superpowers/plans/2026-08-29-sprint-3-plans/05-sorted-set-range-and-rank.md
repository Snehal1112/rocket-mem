# Sorted Set Range & Rank Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ZRANGE`/`ZRANK`, completing the sorted-set command set `../../rocket-mem-sprint-plan.md`'s Sprint 3 backlog names (`ZADD`/`ZRANGE`/`ZSCORE`/`ZRANK`).

**Architecture:** two more free functions in `crates/engine/src/commands/sorted_set.rs` (from `04-sorted-set-core.md`), reusing its `get_zset` helper and `SortedSet::members_ascending`. Same negative-index normalization `list::lrange` already uses, reused here rather than re-derived.

**Tech Stack:** no new dependencies.

**Spec:** `../../specs/2026-08-29-sprint-3-spec.md` — the O(n) `ZRANK` simplification (real Redis's skip list gives O(log n); this project's `BTreeSet` gives a linear `position()` scan instead) is an accepted tradeoff at this project's scale, not a bug to fix here.

**Depends on:** `04-sorted-set-core.md` must be complete (`get_zset`, `SortedSet::members_ascending`).

---

### Task 1: `zrange`/`zrank` (engine level)

**Files:**
- Modify: `crates/engine/src/commands/sorted_set.rs`

**Interfaces:**
- Consumes: `get_zset` (from `04-sorted-set-core.md`), `SortedSet::members_ascending` (from `04-sorted-set-core.md`).
- Produces: `pub fn zrange(engine: &Engine, key: &[u8], start: i64, stop: i64) -> Result<Vec<Bytes>, common::EngineError>`, `pub fn zrank(engine: &Engine, key: &[u8], member: &[u8]) -> Result<Option<usize>, common::EngineError>`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/commands/sorted_set.rs — add to the existing tests module
#[test]
fn zrange_returns_members_ordered_by_score_ascending() {
    let engine = Engine::new();
    zadd(&engine, Bytes::from_static(b"z"), 5.0, Bytes::from_static(b"alice")).unwrap();
    zadd(&engine, Bytes::from_static(b"z"), 2.0, Bytes::from_static(b"bob")).unwrap();
    let result = zrange(&engine, b"z", 0, -1).unwrap();
    assert_eq!(result, vec![Bytes::from_static(b"bob"), Bytes::from_static(b"alice")]);
}

#[test]
fn zrange_with_tied_scores_breaks_ties_lexicographically_by_member() {
    let engine = Engine::new();
    zadd(&engine, Bytes::from_static(b"z"), 2.0, Bytes::from_static(b"carol")).unwrap();
    zadd(&engine, Bytes::from_static(b"z"), 2.0, Bytes::from_static(b"bob")).unwrap();
    let result = zrange(&engine, b"z", 0, -1).unwrap();
    assert_eq!(result, vec![Bytes::from_static(b"bob"), Bytes::from_static(b"carol")]);
}

#[test]
fn zrange_supports_a_partial_slice() {
    let engine = Engine::new();
    zadd(&engine, Bytes::from_static(b"z"), 1.0, Bytes::from_static(b"a")).unwrap();
    zadd(&engine, Bytes::from_static(b"z"), 2.0, Bytes::from_static(b"b")).unwrap();
    zadd(&engine, Bytes::from_static(b"z"), 3.0, Bytes::from_static(b"c")).unwrap();
    assert_eq!(zrange(&engine, b"z", 0, 0).unwrap(), vec![Bytes::from_static(b"a")]);
    assert_eq!(zrange(&engine, b"z", -2, -1).unwrap(), vec![Bytes::from_static(b"b"), Bytes::from_static(b"c")]);
}

#[test]
fn zrange_on_missing_key_is_empty_not_an_error() {
    let engine = Engine::new();
    assert!(zrange(&engine, b"missing", 0, -1).unwrap().is_empty());
}

#[test]
fn zrank_returns_zero_based_position_in_ascending_order() {
    let engine = Engine::new();
    zadd(&engine, Bytes::from_static(b"z"), 5.0, Bytes::from_static(b"alice")).unwrap();
    zadd(&engine, Bytes::from_static(b"z"), 2.0, Bytes::from_static(b"bob")).unwrap();
    assert_eq!(zrank(&engine, b"z", b"bob").unwrap(), Some(0));
    assert_eq!(zrank(&engine, b"z", b"alice").unwrap(), Some(1));
}

#[test]
fn zrank_on_missing_member_is_none_not_an_error() {
    let engine = Engine::new();
    zadd(&engine, Bytes::from_static(b"z"), 5.0, Bytes::from_static(b"alice")).unwrap();
    assert_eq!(zrank(&engine, b"z", b"missing").unwrap(), None);
}

#[test]
fn zrange_on_string_key_returns_wrongtype() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), crate::Value::String(Bytes::from_static(b"v")));
    assert_eq!(zrange(&engine, b"k", 0, -1).unwrap_err(), common::EngineError::WrongType);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine commands::sorted_set`
Expected: FAIL — `zrange`/`zrank` not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/commands/sorted_set.rs — add below zincrby
/// start/stop follow the same negative-index Redis semantics as `list::lrange`.
pub fn zrange(engine: &Engine, key: &[u8], start: i64, stop: i64) -> Result<Vec<Bytes>, common::EngineError> {
    let zset = get_zset(engine, key)?;
    let members: Vec<Bytes> = zset.members_ascending().cloned().collect();
    let len = members.len() as i64;
    let norm = |i: i64| -> i64 {
        if i < 0 {
            (len + i).max(0)
        } else {
            i.min(len)
        }
    };
    let (s, e) = (norm(start), norm(stop) + 1);
    if s >= e {
        return Ok(Vec::new());
    }
    Ok(members.into_iter().skip(s as usize).take((e - s) as usize).collect())
}

pub fn zrank(engine: &Engine, key: &[u8], member: &[u8]) -> Result<Option<usize>, common::EngineError> {
    let zset = get_zset(engine, key)?;
    Ok(zset.members_ascending().position(|m| m.as_ref() == member))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine commands::sorted_set`
Expected: PASS, all tests including the 7 new ones

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/commands/sorted_set.rs` — do not compose the commit message freeform.
Suggested subject: `feat(engine): add zrange/zrank sorted set commands`.

---

### Task 2: Wire `ZRANGE`/`ZRANK` into the dispatcher

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `commands::sorted_set::{zrange, zrank}` (Task 1).
- Produces: two new `match` arms.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn zrange_returns_members_in_score_order_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"ZADD", b"z", b"5", b"alice"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"ZADD", b"z", b"2", b"bob"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZRANGE", b"z", b"0", b"-1"]), &mut Protocol::default(), 1),
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"bob")),
            Frame::Bulk(Bytes::from_static(b"alice")),
        ])
    );
}

#[test]
fn zrange_with_a_non_integer_index_is_a_resp_error() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZRANGE", b"z", b"notanumber", b"-1"]), &mut Protocol::default(), 1),
        Frame::Error("ERR value is not an integer or out of range".into())
    );
}

#[test]
fn zrank_returns_the_zero_based_position_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"ZADD", b"z", b"5", b"alice"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"ZADD", b"z", b"2", b"bob"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZRANK", b"z", b"bob"]), &mut Protocol::default(), 1),
        Frame::Integer(0)
    );
}

#[test]
fn zrank_on_missing_member_returns_null() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"ZADD", b"z", b"5", b"alice"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZRANK", b"z", b"missing"]), &mut Protocol::default(), 1),
        Frame::Null
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — `ZRANGE`/`ZRANK` are currently unknown commands

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — add near ZINCRBY
"ZRANGE" => {
    require_args!(rest, 3, "zrange");
    let (start, stop) = match (
        std::str::from_utf8(&rest[1]).ok().and_then(|s| s.parse::<i64>().ok()),
        std::str::from_utf8(&rest[2]).ok().and_then(|s| s.parse::<i64>().ok()),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => return Frame::Error("ERR value is not an integer or out of range".into()),
    };
    match commands::sorted_set::zrange(engine, &rest[0], start, stop) {
        Ok(items) => Frame::Array(items.into_iter().map(Frame::Bulk).collect()),
        Err(e) => engine_error_to_frame(e),
    }
}
"ZRANK" => {
    require_args!(rest, 2, "zrank");
    match commands::sorted_set::zrank(engine, &rest[0], &rest[1]) {
        Ok(Some(r)) => Frame::Integer(r as i64),
        Ok(None) => Frame::Null,
        Err(e) => engine_error_to_frame(e),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 4 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): wire zrange/zrank sorted set commands`.
