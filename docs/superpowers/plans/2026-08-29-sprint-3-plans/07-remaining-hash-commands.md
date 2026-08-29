# Remaining Hash Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `HINCRBY`/`HKEYS`/`HVALS`/`HMGET`/`HSETNX`, rounding out Hash command coverage per `../../rocket-mem-production-plan.md`'s Week 6 detail.

**Architecture:** all five extend `crates/engine/src/commands/hash.rs`, reusing the existing `hgetall` (already the de facto "load the whole map or empty" helper other hash commands build on).

**Tech Stack:** no new dependencies.

**Spec:** `../../specs/2026-08-29-sprint-3-spec.md` — no hash-specific decisions; this plan follows Sprint 1's established `hash.rs` conventions directly.

**Depends on:** nothing this sprint. Independent of every other Sprint 3 plan.

---

### Task 1: `HINCRBY`/`HKEYS`/`HVALS`/`HMGET`/`HSETNX` (engine level)

**Files:**
- Modify: `crates/engine/src/commands/hash.rs`
- Modify: `crates/engine/src/commands/wrongtype_matrix_tests.rs`

**Interfaces:**
- Consumes: the existing `hgetall` (in `hash.rs`).
- Produces: `pub fn hincrby(engine: &Engine, key: Bytes, field: Bytes, delta: i64) -> Result<i64, common::EngineError>`, `pub fn hkeys(engine: &Engine, key: &[u8]) -> Result<Vec<Bytes>, common::EngineError>`, `pub fn hvals(engine: &Engine, key: &[u8]) -> Result<Vec<Bytes>, common::EngineError>`, `pub fn hmget(engine: &Engine, key: &[u8], fields: &[Bytes]) -> Result<Vec<Option<Bytes>>, common::EngineError>`, `pub fn hsetnx(engine: &Engine, key: Bytes, field: Bytes, val: Bytes) -> Result<bool, common::EngineError>`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/commands/hash.rs — add to the existing tests module
#[test]
fn hincrby_on_missing_field_initializes_from_zero() {
    let engine = Engine::new();
    assert_eq!(hincrby(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"f"), 5).unwrap(), 5);
}

#[test]
fn hincrby_adds_to_an_existing_field() {
    let engine = Engine::new();
    hset(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"f"), Bytes::from_static(b"10")).unwrap();
    assert_eq!(hincrby(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"f"), 5).unwrap(), 15);
}

#[test]
fn hincrby_on_non_integer_field_returns_not_an_integer_error() {
    let engine = Engine::new();
    hset(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"f"), Bytes::from_static(b"abc")).unwrap();
    let err = hincrby(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"f"), 1).unwrap_err();
    assert_eq!(err, common::EngineError::NotAnInteger);
}

#[test]
fn hincrby_on_string_key_returns_wrongtype() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    let err = hincrby(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"f"), 1).unwrap_err();
    assert_eq!(err, common::EngineError::WrongType);
}

#[test]
fn hkeys_and_hvals_report_the_fields_and_values() {
    let engine = Engine::new();
    hset(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"f1"), Bytes::from_static(b"v1")).unwrap();
    let mut keys = hkeys(&engine, b"h").unwrap();
    keys.sort();
    assert_eq!(keys, vec![Bytes::from_static(b"f1")]);
    let mut vals = hvals(&engine, b"h").unwrap();
    vals.sort();
    assert_eq!(vals, vec![Bytes::from_static(b"v1")]);
}

#[test]
fn hkeys_on_missing_key_is_empty_not_an_error() {
    let engine = Engine::new();
    assert!(hkeys(&engine, b"missing").unwrap().is_empty());
}

#[test]
fn hmget_returns_none_for_missing_fields_in_order() {
    let engine = Engine::new();
    hset(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"f1"), Bytes::from_static(b"v1")).unwrap();
    let result = hmget(&engine, b"h", &[Bytes::from_static(b"f1"), Bytes::from_static(b"missing")]).unwrap();
    assert_eq!(result, vec![Some(Bytes::from_static(b"v1")), None]);
}

#[test]
fn hsetnx_sets_only_when_the_field_is_absent() {
    let engine = Engine::new();
    assert!(hsetnx(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"f"), Bytes::from_static(b"first")).unwrap());
    assert!(!hsetnx(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"f"), Bytes::from_static(b"second")).unwrap());
    assert_eq!(hget(&engine, b"h", b"f").unwrap(), Some(Bytes::from_static(b"first")));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine commands::hash`
Expected: FAIL — the five new functions don't exist yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/commands/hash.rs — add below the existing `hlen`
pub fn hincrby(engine: &Engine, key: Bytes, field: Bytes, delta: i64) -> Result<i64, common::EngineError> {
    let mut map = match engine.get(&key) {
        Some(Value::Hash(m)) => m,
        Some(_) => return Err(common::EngineError::WrongType),
        None => HashMap::new(),
    };
    let current: i64 = match map.get(&field) {
        Some(b) => std::str::from_utf8(b)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(common::EngineError::NotAnInteger)?,
        None => 0,
    };
    let next = current + delta;
    map.insert(field, Bytes::from(next.to_string()));
    engine.set(key, Value::Hash(map));
    Ok(next)
}

pub fn hkeys(engine: &Engine, key: &[u8]) -> Result<Vec<Bytes>, common::EngineError> {
    Ok(hgetall(engine, key)?.into_keys().collect())
}

pub fn hvals(engine: &Engine, key: &[u8]) -> Result<Vec<Bytes>, common::EngineError> {
    Ok(hgetall(engine, key)?.into_values().collect())
}

pub fn hmget(engine: &Engine, key: &[u8], fields: &[Bytes]) -> Result<Vec<Option<Bytes>>, common::EngineError> {
    let map = hgetall(engine, key)?;
    Ok(fields.iter().map(|f| map.get(f).cloned()).collect())
}

pub fn hsetnx(engine: &Engine, key: Bytes, field: Bytes, val: Bytes) -> Result<bool, common::EngineError> {
    let mut map = match engine.get(&key) {
        Some(Value::Hash(m)) => m,
        Some(_) => return Err(common::EngineError::WrongType),
        None => HashMap::new(),
    };
    if map.contains_key(&field) {
        return Ok(false);
    }
    map.insert(field, val);
    engine.set(key, Value::Hash(map));
    Ok(true)
}
```

- [ ] **Step 4: Add wrongtype coverage**

```rust
// crates/engine/src/commands/wrongtype_matrix_tests.rs — extend hash_commands_reject_non_hash_keys
assert_wrongtype!(hash::hkeys(&engine_with_string_key(), b"k"));
let e2 = engine_with_string_key();
assert_wrongtype!(hash::hincrby(&e2, Bytes::from_static(b"k"), Bytes::from_static(b"f"), 1));
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p engine commands::hash`
Expected: PASS, all tests including the 8 new ones

- [ ] **Step 6: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 7: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/commands/hash.rs` and `crates/engine/src/commands/wrongtype_matrix_tests.rs`
— do not compose the commit message freeform. Suggested subject:
`feat(engine): add hincrby/hkeys/hvals/hmget/hsetnx hash commands`.

---

### Task 2: Wire the five commands into the dispatcher

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `commands::hash::{hincrby, hkeys, hvals, hmget, hsetnx}` (Task 1).
- Produces: five new `match` arms.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn hincrby_round_trips_through_dispatch() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"HINCRBY", b"h", b"f", b"5"]), &mut Protocol::default(), 1),
        Frame::Integer(5)
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"HINCRBY", b"h", b"f", b"3"]), &mut Protocol::default(), 1),
        Frame::Integer(8)
    );
}

#[test]
fn hincrby_on_a_non_integer_field_is_a_resp_error() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"HSET", b"h", b"f", b"abc"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"HINCRBY", b"h", b"f", b"1"]), &mut Protocol::default(), 1),
        Frame::Error("value is not an integer or out of range".into())
    );
}

#[test]
fn hkeys_and_hvals_round_trip_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"HSET", b"h", b"f", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"HKEYS", b"h"]), &mut Protocol::default(), 1),
        Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"f"))])
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"HVALS", b"h"]), &mut Protocol::default(), 1),
        Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"v"))])
    );
}

#[test]
fn hmget_returns_null_for_missing_fields_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"HSET", b"h", b"f1", b"v1"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"HMGET", b"h", b"f1", b"missing"]), &mut Protocol::default(), 1),
        Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"v1")), Frame::Null])
    );
}

#[test]
fn hsetnx_returns_zero_when_the_field_already_exists() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"HSET", b"h", b"f", b"first"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"HSETNX", b"h", b"f", b"second"]), &mut Protocol::default(), 1),
        Frame::Integer(0)
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"HGET", b"h", b"f"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"first"))
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — none of the five commands are wired yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — add near the other Hash commands
"HINCRBY" => {
    require_args!(rest, 3, "hincrby");
    let delta: i64 = match std::str::from_utf8(&rest[2]).ok().and_then(|s| s.parse().ok()) {
        Some(n) => n,
        None => return Frame::Error("ERR value is not an integer or out of range".into()),
    };
    match commands::hash::hincrby(engine, rest[0].clone(), rest[1].clone(), delta) {
        Ok(n) => Frame::Integer(n),
        Err(e) => engine_error_to_frame(e),
    }
}
"HKEYS" => {
    require_args!(rest, 1, "hkeys");
    match commands::hash::hkeys(engine, &rest[0]) {
        Ok(fields) => Frame::Array(fields.into_iter().map(Frame::Bulk).collect()),
        Err(e) => engine_error_to_frame(e),
    }
}
"HVALS" => {
    require_args!(rest, 1, "hvals");
    match commands::hash::hvals(engine, &rest[0]) {
        Ok(vals) => Frame::Array(vals.into_iter().map(Frame::Bulk).collect()),
        Err(e) => engine_error_to_frame(e),
    }
}
"HMGET" => {
    require_args!(rest, 2, "hmget");
    match commands::hash::hmget(engine, &rest[0], &rest[1..]) {
        Ok(vals) => Frame::Array(
            vals.into_iter()
                .map(|v| match v {
                    Some(b) => Frame::Bulk(b),
                    None => Frame::Null,
                })
                .collect(),
        ),
        Err(e) => engine_error_to_frame(e),
    }
}
"HSETNX" => {
    require_args!(rest, 3, "hsetnx");
    match commands::hash::hsetnx(engine, rest[0].clone(), rest[1].clone(), rest[2].clone()) {
        Ok(true) => Frame::Integer(1),
        Ok(false) => Frame::Integer(0),
        Err(e) => engine_error_to_frame(e),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 5 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): wire hincrby/hkeys/hvals/hmget/hsetnx hash commands`.
