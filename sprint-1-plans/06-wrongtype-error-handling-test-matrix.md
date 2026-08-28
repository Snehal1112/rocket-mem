# WRONGTYPE Error Handling & Test Matrix Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** close the wrong-type gaps left in the mutating hash/list/set commands, then build a systematic test sweep proving every command handles wrong-type and missing-key cases correctly.

**Why this exists as its own plan:** `hset`, `rpush`/`lpush`, and `sadd` (from `05-hash-list-set-commands.md`) currently swallow the wrong-type case silently via `unwrap_or_default()` instead of returning `WrongType`. That's a real correctness bug, not a style nit — e.g. `LPUSH` on a key holding a String should error, not silently create a new list. This plan fixes it and then proves it's fixed everywhere, not just in the three spots found so far.

**Depends on:** `04-string-commands.md` and `05-hash-list-set-commands.md` must both be complete.

---

### Task 1: Fix the swallowed WRONGTYPE cases

**Files:**
- Modify: `crates/engine/src/commands/hash.rs` — `hset`
- Modify: `crates/engine/src/commands/list.rs` — `rpush`, `lpush`
- Modify: `crates/engine/src/commands/set.rs` — `sadd`

- [ ] **Step 1: Write the failing tests**

```rust
// add to crates/engine/src/commands/hash.rs tests
#[test]
fn hset_on_string_key_returns_wrongtype() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    let err = hset(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"f"), Bytes::from_static(b"v")).unwrap_err();
    assert_eq!(err, common::EngineError::WrongType);
}
```

```rust
// add to crates/engine/src/commands/list.rs tests
#[test]
fn rpush_on_string_key_returns_wrongtype() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    assert_eq!(rpush(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"x")).unwrap_err(), common::EngineError::WrongType);
}

#[test]
fn lpush_on_string_key_returns_wrongtype() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    assert_eq!(lpush(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"x")).unwrap_err(), common::EngineError::WrongType);
}
```

```rust
// add to crates/engine/src/commands/set.rs tests
#[test]
fn sadd_on_string_key_returns_wrongtype() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    assert_eq!(sadd(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"x")).unwrap_err(), common::EngineError::WrongType);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine commands::`
Expected: FAIL — current signatures return `()`/`bool`, not `Result`, so these won't even compile; that's the "fail" signal here

- [ ] **Step 3: Fix the implementations**

```rust
// crates/engine/src/commands/hash.rs — replace hset
pub fn hset(engine: &Engine, key: Bytes, field: Bytes, val: Bytes) -> Result<(), common::EngineError> {
    let mut map = match engine.get(&key) {
        Some(Value::Hash(m)) => m,
        Some(_) => return Err(common::EngineError::WrongType),
        None => HashMap::new(),
    };
    map.insert(field, val);
    engine.set(key, Value::Hash(map));
    Ok(())
}
```

```rust
// crates/engine/src/commands/list.rs — replace rpush and lpush
pub fn rpush(engine: &Engine, key: Bytes, val: Bytes) -> Result<(), common::EngineError> {
    let mut list = get_list(engine, &key)?;
    list.push_back(val);
    engine.set(key, Value::List(list));
    Ok(())
}

pub fn lpush(engine: &Engine, key: Bytes, val: Bytes) -> Result<(), common::EngineError> {
    let mut list = get_list(engine, &key)?;
    list.push_front(val);
    engine.set(key, Value::List(list));
    Ok(())
}
```

```rust
// crates/engine/src/commands/set.rs — replace sadd
pub fn sadd(engine: &Engine, key: Bytes, member: Bytes) -> Result<(), common::EngineError> {
    let mut set = get_set(engine, &key)?;
    set.insert(member);
    engine.set(key, Value::Set(set));
    Ok(())
}
```

Also update the existing passing tests in `05-hash-list-set-commands.md`'s files that called these functions without handling a `Result` — add `.unwrap()` at each call site (e.g. `hset(&engine, ..., ...).unwrap();`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine commands::`
Expected: PASS, all tests including the 4 new ones and the updated call sites

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/commands/
git commit -m "fix(engine): stop swallowing WRONGTYPE in hset/rpush/lpush/sadd"
```

---

### Task 2: Cross-command WRONGTYPE test matrix

A single test module that, for every command from Sprints 1 (string, hash, list, set), sets up a key of the "wrong" type and asserts `WrongType` comes back. This is the systematic sweep — Task 1 fixed the bugs it happened to find; this task proves there are no more.

**Files:**
- Create: `crates/engine/src/commands/wrongtype_matrix_tests.rs`
- Modify: `crates/engine/src/commands/mod.rs` — add `#[cfg(test)] mod wrongtype_matrix_tests;`

- [ ] **Step 1: Write the test matrix**

```rust
// crates/engine/src/commands/wrongtype_matrix_tests.rs
use crate::{Engine, Value};
use crate::commands::{hash, list, set, string};
use bytes::Bytes;

fn engine_with_string_key() -> Engine {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    engine
}

fn engine_with_hash_key() -> Engine {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::Hash(Default::default()));
    engine
}

fn engine_with_list_key() -> Engine {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::List(Default::default()));
    engine
}

fn engine_with_set_key() -> Engine {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::Set(Default::default()));
    engine
}

macro_rules! assert_wrongtype {
    ($result:expr) => {
        assert_eq!($result.unwrap_err(), common::EngineError::WrongType);
    };
}

#[test]
fn string_commands_reject_non_string_keys() {
    assert_wrongtype!(string::get(&engine_with_hash_key(), b"k"));
    assert_wrongtype!(string::append(&engine_with_list_key(), Bytes::from_static(b"k"), b"x"));
    assert_wrongtype!(string::strlen(&engine_with_set_key(), b"k"));
    assert_wrongtype!(string::incr_by(&engine_with_hash_key(), Bytes::from_static(b"k"), 1));
}

#[test]
fn hash_commands_reject_non_hash_keys() {
    assert_wrongtype!(hash::hget(&engine_with_string_key(), b"k", b"f"));
    assert_wrongtype!(hash::hdel(&engine_with_list_key(), b"k", b"f"));
    assert_wrongtype!(hash::hgetall(&engine_with_set_key(), b"k"));
    let e = engine_with_string_key();
    assert_wrongtype!(hash::hset(&e, Bytes::from_static(b"k"), Bytes::from_static(b"f"), Bytes::from_static(b"v")));
}

#[test]
fn list_commands_reject_non_list_keys() {
    assert_wrongtype!(list::lrange(&engine_with_string_key(), b"k", 0, -1));
    assert_wrongtype!(list::llen(&engine_with_hash_key(), b"k"));
    assert_wrongtype!(list::rpop(&engine_with_set_key(), b"k"));
    let e = engine_with_string_key();
    assert_wrongtype!(list::rpush(&e, Bytes::from_static(b"k"), Bytes::from_static(b"x")));
}

#[test]
fn set_commands_reject_non_set_keys() {
    assert_wrongtype!(set::smembers(&engine_with_string_key(), b"k"));
    assert_wrongtype!(set::scard(&engine_with_hash_key(), b"k"));
    assert_wrongtype!(set::sismember(&engine_with_list_key(), b"k", b"m"));
    let e = engine_with_string_key();
    assert_wrongtype!(set::sadd(&e, Bytes::from_static(b"k"), Bytes::from_static(b"m")));
}
```

- [ ] **Step 2: Run to verify it fails (or reveals a real gap)**

Run: `cargo test -p engine commands::wrongtype_matrix_tests`
Expected: if Task 1 was done correctly, this should pass immediately — its purpose is to catch anything Task 1 missed, so a failure here means go back and fix that command, not fix the test

- [ ] **Step 3: Fix anything the matrix reveals**

If a command fails, apply the same pattern as Task 1: match on `Value`, return `WrongType` for the wrong variant, don't swallow it via `unwrap_or_default()` or similar.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine commands::wrongtype_matrix_tests`
Expected: PASS, 4/4

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/commands/wrongtype_matrix_tests.rs crates/engine/src/commands/mod.rs
git commit -m "test(engine): add cross-command WRONGTYPE test matrix"
```

---

### Task 3: Missing-key and empty-value semantics sweep

Codifies the "missing key ≠ error" rule Redis follows — `HGETALL` on a missing key returns an empty map, not an error; `LPOP` on a missing key returns `None`, not an error. These are easy to get backwards, and this sweep makes the expected behavior explicit and tested in one place.

**Files:**
- Create: `crates/engine/src/commands/missing_key_semantics_tests.rs`
- Modify: `crates/engine/src/commands/mod.rs` — add `#[cfg(test)] mod missing_key_semantics_tests;`

- [ ] **Step 1: Write the tests**

```rust
// crates/engine/src/commands/missing_key_semantics_tests.rs
use crate::Engine;
use crate::commands::{hash, list, set, string};

#[test]
fn missing_key_reads_return_empty_or_none_not_errors() {
    let engine = Engine::new();
    assert_eq!(string::get(&engine, b"missing").unwrap(), None);
    assert_eq!(string::strlen(&engine, b"missing").unwrap(), 0);
    assert!(hash::hgetall(&engine, b"missing").unwrap().is_empty());
    assert_eq!(hash::hlen(&engine, b"missing").unwrap(), 0);
    assert!(list::lrange(&engine, b"missing", 0, -1).unwrap().is_empty());
    assert_eq!(list::llen(&engine, b"missing").unwrap(), 0);
    assert_eq!(list::lpop(&engine, b"missing").unwrap(), None);
    assert!(set::smembers(&engine, b"missing").unwrap().is_empty());
    assert_eq!(set::scard(&engine, b"missing").unwrap(), 0);
    assert!(!set::sismember(&engine, b"missing", b"x").unwrap());
}

#[test]
fn deleting_a_missing_key_reports_false_not_an_error() {
    let engine = Engine::new();
    assert!(!engine.del(b"missing"));
    assert_eq!(hash::hdel(&engine, b"missing", b"f").unwrap(), false);
    assert_eq!(set::srem(&engine, b"missing", b"m").unwrap(), false);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p engine commands::missing_key_semantics_tests`
Expected: if it fails, a command is returning an error or wrong default where it shouldn't — that's a real bug to fix, not a test to loosen

- [ ] **Step 3: Fix anything the sweep reveals**

Apply the "missing key → empty/None, never an error" rule consistently.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine commands::missing_key_semantics_tests`
Expected: PASS, 2/2

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/commands/missing_key_semantics_tests.rs crates/engine/src/commands/mod.rs
git commit -m "test(engine): codify missing-key semantics across all command families"
```
