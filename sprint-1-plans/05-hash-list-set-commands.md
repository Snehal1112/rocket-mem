# Hash, List & Set Commands Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** implement the Hash, List, and Set command families against the engine.

**Architecture:** same pattern as `04-string-commands.md` — free functions taking `&Engine`, returning `Result<T, common::EngineError>`. Each data type gets its own file under `commands/`.

**Depends on:** `03-engine-facade.md` must be complete. Independent of `04-string-commands.md` — can be worked in parallel.

---

### Task 1: Hash commands

**Files:**
- Create: `crates/engine/src/commands/hash.rs`
- Modify: `crates/engine/src/commands/mod.rs` — add `pub mod hash;`

- [ ] **Step 1: Write the failing test**

```rust
// crates/engine/src/commands/hash.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, Value};
    use bytes::Bytes;

    #[test]
    fn hset_then_hget_round_trips() {
        let engine = Engine::new();
        hset(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"field"), Bytes::from_static(b"val"));
        assert_eq!(hget(&engine, b"h", b"field").unwrap(), Some(Bytes::from_static(b"val")));
    }

    #[test]
    fn hget_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
        assert_eq!(hget(&engine, b"k", b"field").unwrap_err(), common::EngineError::WrongType);
    }

    #[test]
    fn hdel_removes_field_and_reports_it_existed() {
        let engine = Engine::new();
        hset(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"f"), Bytes::from_static(b"v"));
        assert!(hdel(&engine, b"h", b"f").unwrap());
        assert!(!hdel(&engine, b"h", b"f").unwrap());
    }

    #[test]
    fn hgetall_on_missing_key_is_empty_not_error() {
        let engine = Engine::new();
        assert!(hgetall(&engine, b"missing").unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine commands::hash::tests`
Expected: FAIL — functions not defined

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/commands/hash.rs (above the test module)
use bytes::Bytes;
use std::collections::HashMap;
use crate::{Engine, Value};

pub fn hset(engine: &Engine, key: Bytes, field: Bytes, val: Bytes) {
    let mut map = match engine.get(&key) {
        Some(Value::Hash(m)) => m,
        Some(_) => return, // caller (Sprint 2 dispatcher) is responsible for surfacing WRONGTYPE before calling
        None => HashMap::new(),
    };
    map.insert(field, val);
    engine.set(key, Value::Hash(map));
}

pub fn hget(engine: &Engine, key: &[u8], field: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    match engine.get(key) {
        None => Ok(None),
        Some(Value::Hash(m)) => Ok(m.get(field).cloned()),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn hdel(engine: &Engine, key: &[u8], field: &[u8]) -> Result<bool, common::EngineError> {
    match engine.get(key) {
        None => Ok(false),
        Some(Value::Hash(mut m)) => {
            let removed = m.remove(field).is_some();
            engine.set(Bytes::copy_from_slice(key), Value::Hash(m));
            Ok(removed)
        }
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn hgetall(engine: &Engine, key: &[u8]) -> Result<HashMap<Bytes, Bytes>, common::EngineError> {
    match engine.get(key) {
        None => Ok(HashMap::new()),
        Some(Value::Hash(m)) => Ok(m),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn hexists(engine: &Engine, key: &[u8], field: &[u8]) -> Result<bool, common::EngineError> {
    Ok(hgetall(engine, key)?.contains_key(field))
}

pub fn hlen(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    Ok(hgetall(engine, key)?.len())
}
```

Note: `hset`'s wrong-type case is a placeholder here (`Some(_) => return`) — Task 1 of `06-wrongtype-error-handling-test-matrix.md` closes this gap consistently across every mutating command, not just this one.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p engine commands::hash::tests`
Expected: PASS, 4/4

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/commands/hash.rs crates/engine/src/commands/mod.rs
git commit -m "feat(engine): add hash commands (HSET/HGET/HDEL/HGETALL/HEXISTS/HLEN)"
```

---

### Task 2: List commands

**Files:**
- Create: `crates/engine/src/commands/list.rs`
- Modify: `crates/engine/src/commands/mod.rs` — add `pub mod list;`

- [ ] **Step 1: Write the failing test**

```rust
// crates/engine/src/commands/list.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use bytes::Bytes;

    #[test]
    fn rpush_then_lrange_returns_in_insertion_order() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a"));
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b"));
        let items = lrange(&engine, b"l", 0, -1).unwrap();
        assert_eq!(items, vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]);
    }

    #[test]
    fn lpush_prepends() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b"));
        lpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a"));
        let items = lrange(&engine, b"l", 0, -1).unwrap();
        assert_eq!(items, vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]);
    }

    #[test]
    fn rpop_returns_and_removes_last_element() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a"));
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b"));
        assert_eq!(rpop(&engine, b"l").unwrap(), Some(Bytes::from_static(b"b")));
        assert_eq!(llen(&engine, b"l").unwrap(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine commands::list::tests`
Expected: FAIL — functions not defined

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/commands/list.rs (above the test module)
use bytes::Bytes;
use std::collections::VecDeque;
use crate::{Engine, Value};

fn get_list(engine: &Engine, key: &[u8]) -> Result<VecDeque<Bytes>, common::EngineError> {
    match engine.get(key) {
        None => Ok(VecDeque::new()),
        Some(Value::List(l)) => Ok(l),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn rpush(engine: &Engine, key: Bytes, val: Bytes) {
    let mut list = get_list(engine, &key).unwrap_or_default();
    list.push_back(val);
    engine.set(key, Value::List(list));
}

pub fn lpush(engine: &Engine, key: Bytes, val: Bytes) {
    let mut list = get_list(engine, &key).unwrap_or_default();
    list.push_front(val);
    engine.set(key, Value::List(list));
}

pub fn rpop(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    let mut list = get_list(engine, key)?;
    let popped = list.pop_back();
    engine.set(Bytes::copy_from_slice(key), Value::List(list));
    Ok(popped)
}

pub fn lpop(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    let mut list = get_list(engine, key)?;
    let popped = list.pop_front();
    engine.set(Bytes::copy_from_slice(key), Value::List(list));
    Ok(popped)
}

pub fn llen(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    Ok(get_list(engine, key)?.len())
}

/// start/stop follow Redis semantics: negative indices count from the end, -1 is the last element.
pub fn lrange(engine: &Engine, key: &[u8], start: i64, stop: i64) -> Result<Vec<Bytes>, common::EngineError> {
    let list = get_list(engine, key)?;
    let len = list.len() as i64;
    let norm = |i: i64| -> i64 { if i < 0 { (len + i).max(0) } else { i.min(len) } };
    let (s, e) = (norm(start), norm(stop) + 1);
    if s >= e { return Ok(Vec::new()); }
    Ok(list.into_iter().skip(s as usize).take((e - s) as usize).collect())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p engine commands::list::tests`
Expected: PASS, 3/3

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/commands/list.rs crates/engine/src/commands/mod.rs
git commit -m "feat(engine): add list commands (LPUSH/RPUSH/LPOP/RPOP/LRANGE/LLEN)"
```

---

### Task 3: Set commands

**Files:**
- Create: `crates/engine/src/commands/set.rs`
- Modify: `crates/engine/src/commands/mod.rs` — add `pub mod set;`

- [ ] **Step 1: Write the failing test**

```rust
// crates/engine/src/commands/set.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use bytes::Bytes;

    #[test]
    fn sadd_then_sismember_is_true() {
        let engine = Engine::new();
        sadd(&engine, Bytes::from_static(b"s"), Bytes::from_static(b"x"));
        assert!(sismember(&engine, b"s", b"x").unwrap());
        assert!(!sismember(&engine, b"s", b"y").unwrap());
    }

    #[test]
    fn srem_removes_member_and_reports_it_existed() {
        let engine = Engine::new();
        sadd(&engine, Bytes::from_static(b"s"), Bytes::from_static(b"x"));
        assert!(srem(&engine, b"s", b"x").unwrap());
        assert!(!srem(&engine, b"s", b"x").unwrap());
    }

    #[test]
    fn scard_counts_members() {
        let engine = Engine::new();
        sadd(&engine, Bytes::from_static(b"s"), Bytes::from_static(b"x"));
        sadd(&engine, Bytes::from_static(b"s"), Bytes::from_static(b"y"));
        assert_eq!(scard(&engine, b"s").unwrap(), 2);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine commands::set::tests`
Expected: FAIL — functions not defined

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/commands/set.rs (above the test module)
use bytes::Bytes;
use std::collections::HashSet;
use crate::{Engine, Value};

fn get_set(engine: &Engine, key: &[u8]) -> Result<HashSet<Bytes>, common::EngineError> {
    match engine.get(key) {
        None => Ok(HashSet::new()),
        Some(Value::Set(s)) => Ok(s),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn sadd(engine: &Engine, key: Bytes, member: Bytes) {
    let mut set = get_set(engine, &key).unwrap_or_default();
    set.insert(member);
    engine.set(key, Value::Set(set));
}

pub fn srem(engine: &Engine, key: &[u8], member: &[u8]) -> Result<bool, common::EngineError> {
    let mut set = get_set(engine, key)?;
    let removed = set.remove(member);
    engine.set(Bytes::copy_from_slice(key), Value::Set(set));
    Ok(removed)
}

pub fn smembers(engine: &Engine, key: &[u8]) -> Result<HashSet<Bytes>, common::EngineError> {
    get_set(engine, key)
}

pub fn sismember(engine: &Engine, key: &[u8], member: &[u8]) -> Result<bool, common::EngineError> {
    Ok(get_set(engine, key)?.contains(member))
}

pub fn scard(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    Ok(get_set(engine, key)?.len())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p engine commands::set::tests`
Expected: PASS, 3/3

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/commands/set.rs crates/engine/src/commands/mod.rs
git commit -m "feat(engine): add set commands (SADD/SREM/SMEMBERS/SISMEMBER/SCARD)"
```
