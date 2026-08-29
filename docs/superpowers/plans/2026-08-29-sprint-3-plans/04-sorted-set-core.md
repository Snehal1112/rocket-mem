# Sorted Set Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a new `Value::SortedSet` type with `ZADD`/`ZSCORE`/`ZREM`/`ZCARD`/`ZINCRBY`, the foundation `05-sorted-set-range-and-rank.md` builds `ZRANGE`/`ZRANK` on top of.

**Architecture:** `SortedSet` (a `HashMap<Bytes, OrderedFloat<f64>>` for O(1) score lookup, plus a `BTreeSet<(OrderedFloat<f64>, Bytes)>` for free ascending iteration) lives in `crates/engine/src/value.rs` next to `Value`. Command-level logic lives in a new `crates/engine/src/commands/sorted_set.rs`, mirroring the `get_list`/`get_set` helper pattern already in `list.rs`/`set.rs`.

**Tech Stack:** `ordered-float = "4"` (new workspace dependency — `f64` doesn't implement `Eq`, and `Value` derives `Eq`).

**Spec:** `../../specs/2026-08-29-sprint-3-spec.md` — the `SortedSet` data structure, why both maps use `OrderedFloat` (not just the `BTreeSet` key), and the NaN/non-finite-score rejection point are authoritative.

**Depends on:** nothing this sprint. `05-sorted-set-range-and-rank.md` depends on this plan.

## Global Constraints

- `zadd`/`zincrby` reject non-finite scores (`NaN`, `inf`, `-inf`) at the dispatcher level with a syntax error, before they ever reach `SortedSet` — engine-level `SortedSet` never has to handle a non-finite score.
- `Value::type_name()`'s `match` has no wildcard arm — adding `SortedSet` without a `"zset"` arm is a compile error, not a silent gap.
- Every new command gets a wrongtype case in `crates/engine/src/commands/wrongtype_matrix_tests.rs` and a missing-key case in `crates/engine/src/commands/missing_key_semantics_tests.rs`.

---

### Task 1: `Value::SortedSet` and the `SortedSet` struct

**Files:**
- Modify: `crates/engine/src/value.rs`
- Modify: `crates/engine/Cargo.toml`
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/engine/src/lib.rs`

**Interfaces:**
- Produces: `pub struct SortedSet { .. }` with `new`, `insert(&mut self, member: Bytes, score: f64)`, `remove(&mut self, member: &[u8]) -> bool`, `score(&self, member: &[u8]) -> Option<f64>`, `len(&self) -> usize`, `is_empty(&self) -> bool`, `members_ascending(&self) -> impl Iterator<Item = &Bytes>` (score-ascending, member-lexicographic tie-break); `Value::SortedSet(SortedSet)` variant; `pub use value::SortedSet` from `lib.rs` so `commands/sorted_set.rs` can name it as `crate::SortedSet`.

- [ ] **Step 1: Add the `ordered-float` dependency**

```toml
# Cargo.toml — add to [workspace.dependencies]
ordered-float = "4"
```

```toml
# crates/engine/Cargo.toml — add to [dependencies]
ordered-float.workspace = true
```

- [ ] **Step 2: Write the failing tests**

```rust
// crates/engine/src/value.rs — add to the existing tests module
#[test]
fn sorted_set_insert_then_score_round_trips() {
    let mut z = SortedSet::new();
    z.insert(Bytes::from_static(b"alice"), 5.0);
    assert_eq!(z.score(b"alice"), Some(5.0));
    assert_eq!(z.score(b"missing"), None);
}

#[test]
fn sorted_set_insert_again_updates_the_score_not_adds_a_duplicate() {
    let mut z = SortedSet::new();
    z.insert(Bytes::from_static(b"alice"), 5.0);
    z.insert(Bytes::from_static(b"alice"), 9.0);
    assert_eq!(z.len(), 1);
    assert_eq!(z.score(b"alice"), Some(9.0));
}

#[test]
fn sorted_set_remove_reports_whether_the_member_existed() {
    let mut z = SortedSet::new();
    z.insert(Bytes::from_static(b"alice"), 5.0);
    assert!(z.remove(b"alice"));
    assert!(!z.remove(b"alice"));
    assert_eq!(z.len(), 0);
}

#[test]
fn sorted_set_members_ascending_orders_by_score_then_by_member() {
    let mut z = SortedSet::new();
    z.insert(Bytes::from_static(b"bob"), 2.0);
    z.insert(Bytes::from_static(b"alice"), 5.0);
    z.insert(Bytes::from_static(b"carol"), 2.0); // ties with bob on score, breaks lexicographically
    let ordered: Vec<Bytes> = z.members_ascending().cloned().collect();
    assert_eq!(
        ordered,
        vec![
            Bytes::from_static(b"bob"),
            Bytes::from_static(b"carol"),
            Bytes::from_static(b"alice"),
        ]
    );
}

#[test]
fn type_name_reports_zset_for_sorted_set_values() {
    assert_eq!(Value::SortedSet(SortedSet::new()).type_name(), "zset");
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p engine value::tests`
Expected: FAIL — `SortedSet` is not defined yet

- [ ] **Step 4: Write the implementation**

```rust
// crates/engine/src/value.rs — replace the top of the file up through the `Value` enum
use bytes::Bytes;
use ordered_float::OrderedFloat;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SortedSet {
    scores: HashMap<Bytes, OrderedFloat<f64>>,
    by_score: BTreeSet<(OrderedFloat<f64>, Bytes)>,
}

impl SortedSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, member: Bytes, score: f64) {
        let score = OrderedFloat(score);
        if let Some(&old) = self.scores.get(&member) {
            self.by_score.remove(&(old, member.clone()));
        }
        self.scores.insert(member.clone(), score);
        self.by_score.insert((score, member));
    }

    pub fn remove(&mut self, member: &[u8]) -> bool {
        match self.scores.remove(member) {
            Some(score) => {
                self.by_score.remove(&(score, Bytes::copy_from_slice(member)));
                true
            }
            None => false,
        }
    }

    pub fn score(&self, member: &[u8]) -> Option<f64> {
        self.scores.get(member).map(|s| s.0)
    }

    pub fn len(&self) -> usize {
        self.scores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }

    /// Ascending by (score, member) — real Redis's tie-break rule when scores are equal.
    pub fn members_ascending(&self) -> impl Iterator<Item = &Bytes> {
        self.by_score.iter().map(|(_, m)| m)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    String(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<Bytes, Bytes>),
    Set(HashSet<Bytes>),
    SortedSet(SortedSet),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::List(_) => "list",
            Value::Hash(_) => "hash",
            Value::Set(_) => "set",
            Value::SortedSet(_) => "zset",
        }
    }
}
```

```rust
// crates/engine/src/lib.rs
pub mod commands;
pub mod glob;
mod engine;
mod shard;
mod store;
mod value;
pub use engine::Engine;
pub use store::Store;
pub use value::{SortedSet, Value};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p engine value::tests`
Expected: PASS, all tests including the 5 new ones

- [ ] **Step 6: Verify the workspace still builds**

Run: `cargo build --workspace`
Expected: PASS — adding a new `Value` variant doesn't break `string.rs`/`hash.rs`/`list.rs`/`set.rs`, since their type-mismatch arms all use a wildcard `Some(_) => Err(WrongType)`, not an exhaustive per-variant match

- [ ] **Step 7: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/value.rs`, `crates/engine/src/lib.rs`, `crates/engine/Cargo.toml`,
`Cargo.toml`, and `Cargo.lock` — do not compose the commit message freeform. Suggested
subject: `feat(engine): add SortedSet value type`.

---

### Task 2: `ZADD`/`ZSCORE`/`ZREM`/`ZCARD`/`ZINCRBY` (engine level)

**Files:**
- Create: `crates/engine/src/commands/sorted_set.rs`
- Modify: `crates/engine/src/commands/mod.rs`
- Modify: `crates/engine/src/commands/wrongtype_matrix_tests.rs`
- Modify: `crates/engine/src/commands/missing_key_semantics_tests.rs`

**Interfaces:**
- Consumes: `crate::{Engine, Value, SortedSet}` (Task 1).
- Produces: `pub fn zadd(engine: &Engine, key: Bytes, score: f64, member: Bytes) -> Result<bool, common::EngineError>` (`true` if the member is new), `pub fn zscore(engine: &Engine, key: &[u8], member: &[u8]) -> Result<Option<f64>, common::EngineError>`, `pub fn zrem(engine: &Engine, key: &[u8], member: &[u8]) -> Result<bool, common::EngineError>`, `pub fn zcard(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError>`, `pub fn zincrby(engine: &Engine, key: Bytes, delta: f64, member: Bytes) -> Result<f64, common::EngineError>`. `05-sorted-set-range-and-rank.md` adds a private `get_zset` helper's sibling functions to this same file.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/commands/sorted_set.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use bytes::Bytes;

    #[test]
    fn zadd_new_member_returns_true() {
        let engine = Engine::new();
        assert!(zadd(&engine, Bytes::from_static(b"z"), 5.0, Bytes::from_static(b"alice")).unwrap());
    }

    #[test]
    fn zadd_existing_member_updates_score_and_returns_false() {
        let engine = Engine::new();
        zadd(&engine, Bytes::from_static(b"z"), 5.0, Bytes::from_static(b"alice")).unwrap();
        let is_new = zadd(&engine, Bytes::from_static(b"z"), 9.0, Bytes::from_static(b"alice")).unwrap();
        assert!(!is_new);
        assert_eq!(zscore(&engine, b"z", b"alice").unwrap(), Some(9.0));
    }

    #[test]
    fn zscore_on_missing_member_is_none_not_an_error() {
        let engine = Engine::new();
        zadd(&engine, Bytes::from_static(b"z"), 5.0, Bytes::from_static(b"alice")).unwrap();
        assert_eq!(zscore(&engine, b"z", b"bob").unwrap(), None);
    }

    #[test]
    fn zscore_on_missing_key_is_none_not_an_error() {
        let engine = Engine::new();
        assert_eq!(zscore(&engine, b"missing", b"alice").unwrap(), None);
    }

    #[test]
    fn zrem_removes_member_and_reports_it_existed() {
        let engine = Engine::new();
        zadd(&engine, Bytes::from_static(b"z"), 5.0, Bytes::from_static(b"alice")).unwrap();
        assert!(zrem(&engine, b"z", b"alice").unwrap());
        assert!(!zrem(&engine, b"z", b"alice").unwrap());
    }

    #[test]
    fn zcard_counts_members() {
        let engine = Engine::new();
        zadd(&engine, Bytes::from_static(b"z"), 5.0, Bytes::from_static(b"alice")).unwrap();
        zadd(&engine, Bytes::from_static(b"z"), 2.0, Bytes::from_static(b"bob")).unwrap();
        assert_eq!(zcard(&engine, b"z").unwrap(), 2);
    }

    #[test]
    fn zincrby_on_missing_member_starts_from_zero() {
        let engine = Engine::new();
        let score = zincrby(&engine, Bytes::from_static(b"z"), 5.0, Bytes::from_static(b"alice")).unwrap();
        assert_eq!(score, 5.0);
    }

    #[test]
    fn zincrby_adds_to_the_existing_score() {
        let engine = Engine::new();
        zadd(&engine, Bytes::from_static(b"z"), 5.0, Bytes::from_static(b"alice")).unwrap();
        let score = zincrby(&engine, Bytes::from_static(b"z"), 3.0, Bytes::from_static(b"alice")).unwrap();
        assert_eq!(score, 8.0);
    }

    #[test]
    fn zadd_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"k"), crate::Value::String(Bytes::from_static(b"v")));
        let err = zadd(&engine, Bytes::from_static(b"k"), 1.0, Bytes::from_static(b"m")).unwrap_err();
        assert_eq!(err, common::EngineError::WrongType);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine commands::sorted_set`
Expected: FAIL — module doesn't exist yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/commands/sorted_set.rs (above the tests module)
use crate::{Engine, SortedSet, Value};
use bytes::Bytes;

pub(crate) fn get_zset(engine: &Engine, key: &[u8]) -> Result<SortedSet, common::EngineError> {
    match engine.get(key) {
        None => Ok(SortedSet::new()),
        Some(Value::SortedSet(z)) => Ok(z),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn zadd(engine: &Engine, key: Bytes, score: f64, member: Bytes) -> Result<bool, common::EngineError> {
    let mut zset = get_zset(engine, &key)?;
    let is_new = zset.score(&member).is_none();
    zset.insert(member, score);
    engine.set(key, Value::SortedSet(zset));
    Ok(is_new)
}

pub fn zscore(engine: &Engine, key: &[u8], member: &[u8]) -> Result<Option<f64>, common::EngineError> {
    Ok(get_zset(engine, key)?.score(member))
}

pub fn zrem(engine: &Engine, key: &[u8], member: &[u8]) -> Result<bool, common::EngineError> {
    let mut zset = match engine.get(key) {
        None => return Ok(false),
        Some(Value::SortedSet(z)) => z,
        Some(_) => return Err(common::EngineError::WrongType),
    };
    let removed = zset.remove(member);
    engine.set(Bytes::copy_from_slice(key), Value::SortedSet(zset));
    Ok(removed)
}

pub fn zcard(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    Ok(get_zset(engine, key)?.len())
}

pub fn zincrby(engine: &Engine, key: Bytes, delta: f64, member: Bytes) -> Result<f64, common::EngineError> {
    let mut zset = get_zset(engine, &key)?;
    let new_score = zset.score(&member).unwrap_or(0.0) + delta;
    zset.insert(member, new_score);
    engine.set(key, Value::SortedSet(zset));
    Ok(new_score)
}
```

```rust
// crates/engine/src/commands/mod.rs
pub mod hash;
pub mod keys;
pub mod list;
pub mod set;
pub mod sorted_set;
pub mod string;

#[cfg(test)]
mod missing_key_semantics_tests;
#[cfg(test)]
mod wrongtype_matrix_tests;
```

- [ ] **Step 4: Add to the cross-cutting correctness test matrices**

```rust
// crates/engine/src/commands/wrongtype_matrix_tests.rs — add to imports and a new test
use crate::commands::{hash, list, set, sorted_set, string};
// (existing `use crate::{Engine, Value};` and `use bytes::Bytes;` stay as-is)

#[test]
fn sorted_set_commands_reject_non_sorted_set_keys() {
    assert_wrongtype!(sorted_set::zscore(&engine_with_string_key(), b"k", b"m"));
    assert_wrongtype!(sorted_set::zrem(&engine_with_hash_key(), b"k", b"m"));
    assert_wrongtype!(sorted_set::zcard(&engine_with_list_key(), b"k"));
    let e = engine_with_string_key();
    assert_wrongtype!(sorted_set::zadd(&e, Bytes::from_static(b"k"), 1.0, Bytes::from_static(b"m")));
}
```

```rust
// crates/engine/src/commands/missing_key_semantics_tests.rs — extend both existing tests
use crate::commands::{hash, list, set, sorted_set, string};
// add inside `missing_key_reads_return_empty_or_none_not_errors`:
assert_eq!(sorted_set::zscore(&engine, b"missing", b"m").unwrap(), None);
assert_eq!(sorted_set::zcard(&engine, b"missing").unwrap(), 0);
// add inside `deleting_a_missing_key_reports_false_not_an_error`:
assert_eq!(sorted_set::zrem(&engine, b"missing", b"m").unwrap(), false);
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p engine commands::`
Expected: PASS, all tests including the 9 new `sorted_set` tests and the matrix-test extensions

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/commands/sorted_set.rs`, `crates/engine/src/commands/mod.rs`,
`crates/engine/src/commands/wrongtype_matrix_tests.rs`, and
`crates/engine/src/commands/missing_key_semantics_tests.rs` — do not compose the commit
message freeform. Suggested subject: `feat(engine): add zadd/zscore/zrem/zcard/zincrby sorted set commands`.

---

### Task 3: Wire `ZADD`/`ZSCORE`/`ZREM`/`ZCARD`/`ZINCRBY` into the dispatcher

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `commands::sorted_set::{zadd, zscore, zrem, zcard, zincrby}` (Task 2).
- Produces: five new `match` arms, plus a private `format_score` helper `05-sorted-set-range-and-rank.md` doesn't need but later plans reusing float replies could.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn zadd_then_zscore_round_trips_through_dispatch() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZADD", b"z", b"5", b"alice"]), &mut Protocol::default(), 1),
        Frame::Integer(1)
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZSCORE", b"z", b"alice"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"5"))
    );
}

#[test]
fn zadd_existing_member_returns_zero_and_updates_score() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"ZADD", b"z", b"5", b"alice"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZADD", b"z", b"9", b"alice"]), &mut Protocol::default(), 1),
        Frame::Integer(0)
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZSCORE", b"z", b"alice"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"9"))
    );
}

#[test]
fn zadd_with_a_non_numeric_score_is_a_resp_error() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZADD", b"z", b"notanumber", b"alice"]), &mut Protocol::default(), 1),
        Frame::Error("ERR value is not a valid float".into())
    );
}

#[test]
fn zadd_with_nan_or_infinite_score_is_a_resp_error() {
    let engine = Engine::new();
    for bad in [&b"nan"[..], &b"inf"[..], &b"-inf"[..]] {
        assert_eq!(
            dispatch(&engine, cmd(&[b"ZADD", b"z", bad, b"alice"]), &mut Protocol::default(), 1),
            Frame::Error("ERR value is not a valid float".into())
        );
    }
}

#[test]
fn zscore_on_missing_member_returns_null() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZSCORE", b"z", b"missing"]), &mut Protocol::default(), 1),
        Frame::Null
    );
}

#[test]
fn zrem_then_zcard_round_trip_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"ZADD", b"z", b"5", b"alice"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"ZADD", b"z", b"2", b"bob"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZCARD", b"z"]), &mut Protocol::default(), 1),
        Frame::Integer(2)
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZREM", b"z", b"alice"]), &mut Protocol::default(), 1),
        Frame::Integer(1)
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZCARD", b"z"]), &mut Protocol::default(), 1),
        Frame::Integer(1)
    );
}

#[test]
fn zincrby_returns_the_new_score_as_a_bulk_string() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"ZADD", b"z", b"5", b"alice"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZINCRBY", b"z", b"3", b"alice"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"8"))
    );
}

#[test]
fn zscore_formats_a_fractional_score_without_trailing_zeros() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"ZADD", b"z", b"5.5", b"alice"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"ZSCORE", b"z", b"alice"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"5.5"))
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — none of `ZADD`/`ZSCORE`/`ZREM`/`ZCARD`/`ZINCRBY` are wired yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — add a helper function near `engine_error_to_frame`
fn format_score(score: f64) -> Bytes {
    if score.fract() == 0.0 && score.is_finite() {
        Bytes::from((score as i64).to_string())
    } else {
        Bytes::from(score.to_string())
    }
}

fn parse_score(raw: &[u8]) -> Result<f64, Frame> {
    let score: f64 = std::str::from_utf8(raw)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Frame::Error("ERR value is not a valid float".into()))?;
    if !score.is_finite() {
        return Err(Frame::Error("ERR value is not a valid float".into()));
    }
    Ok(score)
}
```

```rust
// crates/server/src/dispatcher.rs — add match arms near the other SET-type commands
"ZADD" => {
    require_args!(rest, 3, "zadd");
    let score = match parse_score(&rest[1]) {
        Ok(s) => s,
        Err(e) => return e,
    };
    match commands::sorted_set::zadd(engine, rest[0].clone(), score, rest[2].clone()) {
        Ok(true) => Frame::Integer(1),
        Ok(false) => Frame::Integer(0),
        Err(e) => engine_error_to_frame(e),
    }
}
"ZSCORE" => {
    require_args!(rest, 2, "zscore");
    match commands::sorted_set::zscore(engine, &rest[0], &rest[1]) {
        Ok(Some(score)) => Frame::Bulk(format_score(score)),
        Ok(None) => Frame::Null,
        Err(e) => engine_error_to_frame(e),
    }
}
"ZREM" => {
    require_args!(rest, 2, "zrem");
    match commands::sorted_set::zrem(engine, &rest[0], &rest[1]) {
        Ok(true) => Frame::Integer(1),
        Ok(false) => Frame::Integer(0),
        Err(e) => engine_error_to_frame(e),
    }
}
"ZCARD" => {
    require_args!(rest, 1, "zcard");
    match commands::sorted_set::zcard(engine, &rest[0]) {
        Ok(n) => Frame::Integer(n as i64),
        Err(e) => engine_error_to_frame(e),
    }
}
"ZINCRBY" => {
    require_args!(rest, 3, "zincrby");
    let delta = match parse_score(&rest[1]) {
        Ok(s) => s,
        Err(e) => return e,
    };
    match commands::sorted_set::zincrby(engine, rest[0].clone(), delta, rest[2].clone()) {
        Ok(score) => Frame::Bulk(format_score(score)),
        Err(e) => engine_error_to_frame(e),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 8 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): wire zadd/zscore/zrem/zcard/zincrby sorted set commands`.
