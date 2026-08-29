# String Commands Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** implement `SET`(`NX`/`XX`)/`GET`, `APPEND`/`STRLEN`, and `INCR`/`DECR`/`INCRBY` against the engine.

**Architecture:** each command is a free function taking `&Engine` plus its arguments, returning `Result<T, common::EngineError>`. This mirrors how Sprint 2's dispatcher will call them — no dispatcher exists yet, these are called directly from tests.

**Scope note:** `SET`'s `EX`/`PX` flags are deferred to Sprint 4 (see `../../specs/2026-08-28-sprint-1-spec.md`). Only `NX`/`XX` are implemented here.

**Depends on:** `03-engine-facade.md` must be complete.

---

### Task 1: `SET` (`NX`/`XX`) and `GET`

**Files:**
- Create: `crates/engine/src/commands/mod.rs`
- Create: `crates/engine/src/commands/string.rs`
- Modify: `crates/engine/src/lib.rs` — add `mod commands;`

- [ ] **Step 1: Write the failing test**

```rust
// crates/engine/src/commands/string.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use bytes::Bytes;

    #[test]
    fn set_nx_fails_when_key_already_exists() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"k"), crate::Value::String(Bytes::from_static(b"old")));
        let applied = set_nx(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"new"));
        assert!(!applied);
        assert_eq!(get(&engine, b"k").unwrap(), Some(Bytes::from_static(b"old")));
    }

    #[test]
    fn set_xx_fails_when_key_missing() {
        let engine = Engine::new();
        let applied = set_xx(&engine, Bytes::from_static(b"missing"), Bytes::from_static(b"v"));
        assert!(!applied);
        assert_eq!(get(&engine, b"missing").unwrap(), None);
    }

    #[test]
    fn get_on_hash_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"h"), crate::Value::Hash(Default::default()));
        assert_eq!(get(&engine, b"h").unwrap_err(), common::EngineError::WrongType);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine commands::string::tests`
Expected: FAIL — functions not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/commands/string.rs (above the test module)
use bytes::Bytes;
use crate::{Engine, Value};

pub fn set_nx(engine: &Engine, key: Bytes, val: Bytes) -> bool {
    if engine.exists(&key) { return false; }
    engine.set(key, Value::String(val));
    true
}

pub fn set_xx(engine: &Engine, key: Bytes, val: Bytes) -> bool {
    if !engine.exists(&key) { return false; }
    engine.set(key, Value::String(val));
    true
}

pub fn get(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    match engine.get(key) {
        None => Ok(None),
        Some(Value::String(b)) => Ok(Some(b)),
        Some(_) => Err(common::EngineError::WrongType),
    }
}
```

```rust
// crates/engine/src/commands/mod.rs
pub mod string;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p engine commands::string::tests`
Expected: PASS, 3/3

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/commands/
git commit -m "feat(engine): add SET (NX/XX) and GET"
```

---

### Task 2: `APPEND` and `STRLEN`

**Files:**
- Modify: `crates/engine/src/commands/string.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn append_to_missing_key_creates_it() {
    let engine = Engine::new();
    let len = append(&engine, Bytes::from_static(b"k"), b"hello").unwrap();
    assert_eq!(len, 5);
    assert_eq!(strlen(&engine, b"k").unwrap(), 5);
}

#[test]
fn append_to_existing_key_extends_it() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"hello")));
    let len = append(&engine, Bytes::from_static(b"k"), b" world").unwrap();
    assert_eq!(len, 11);
}

#[test]
fn strlen_on_missing_key_is_zero() {
    let engine = Engine::new();
    assert_eq!(strlen(&engine, b"missing").unwrap(), 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine commands::string::tests`
Expected: FAIL — `append`/`strlen` not defined

- [ ] **Step 3: Write the implementation**

```rust
pub fn append(engine: &Engine, key: Bytes, suffix: &[u8]) -> Result<usize, common::EngineError> {
    let mut buf = match engine.get(&key) {
        None => Vec::new(),
        Some(Value::String(b)) => b.to_vec(),
        Some(_) => return Err(common::EngineError::WrongType),
    };
    buf.extend_from_slice(suffix);
    let len = buf.len();
    engine.set(key, Value::String(Bytes::from(buf)));
    Ok(len)
}

pub fn strlen(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    match engine.get(key) {
        None => Ok(0),
        Some(Value::String(b)) => Ok(b.len()),
        Some(_) => Err(common::EngineError::WrongType),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p engine commands::string::tests`
Expected: PASS, 6/6

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/commands/string.rs
git commit -m "feat(engine): add APPEND and STRLEN"
```

---

### Task 3: `INCR`/`DECR`/`INCRBY`

**Files:**
- Modify: `crates/engine/src/commands/string.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn incr_on_missing_key_initializes_to_one() {
    let engine = Engine::new();
    assert_eq!(incr_by(&engine, Bytes::from_static(b"counter"), 1).unwrap(), 1);
}

#[test]
fn incr_by_adds_to_existing_value() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"counter"), Value::String(Bytes::from_static(b"10")));
    assert_eq!(incr_by(&engine, Bytes::from_static(b"counter"), 5).unwrap(), 15);
}

#[test]
fn decr_is_incr_by_with_negative_delta() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"counter"), Value::String(Bytes::from_static(b"10")));
    assert_eq!(incr_by(&engine, Bytes::from_static(b"counter"), -3).unwrap(), 7);
}

#[test]
fn incr_on_non_integer_string_returns_not_an_integer_error() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"abc")));
    let err = incr_by(&engine, Bytes::from_static(b"k"), 1).unwrap_err();
    assert_eq!(err, common::EngineError::NotAnInteger);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine commands::string::tests`
Expected: FAIL — `incr_by` not defined

- [ ] **Step 3: Write the implementation**

```rust
pub fn incr_by(engine: &Engine, key: Bytes, delta: i64) -> Result<i64, common::EngineError> {
    let current: i64 = match engine.get(&key) {
        None => 0,
        Some(Value::String(b)) => std::str::from_utf8(&b).ok()
            .and_then(|s| s.parse().ok())
            .ok_or(common::EngineError::NotAnInteger)?,
        Some(_) => return Err(common::EngineError::WrongType),
    };
    let next = current + delta;
    engine.set(key, Value::String(Bytes::from(next.to_string())));
    Ok(next)
}
```

Note: `INCR` and `DECR` are `incr_by(engine, key, 1)` and `incr_by(engine, key, -1)` respectively — no separate functions needed. Sprint 2's dispatcher maps the command names.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p engine commands::string::tests`
Expected: PASS, 10/10

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/commands/string.rs
git commit -m "feat(engine): add INCR/DECR/INCRBY via incr_by"
```
