# Engine Facade Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** expose one public `Engine` type wrapping `Store`, giving Sprint 2's dispatcher a single clean entry point, plus a pattern-free `keys()`.

**Architecture:** `Engine` is a thin facade — it holds a `Store` and forwards calls. No new logic beyond what `Store` already provides, except `keys()` returning the full keyspace listing.

**Depends on:** `02-sharded-keyspace.md` must be complete.

---

### Task 1: `Engine` facade — get/set/del/exists

**Files:**
- Create: `crates/engine/src/engine.rs`
- Modify: `crates/engine/src/lib.rs` — add `mod engine;`, `pub use engine::Engine;`

- [ ] **Step 1: Write the failing test**

```rust
// crates/engine/src/engine.rs
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use crate::Value;

    #[test]
    fn engine_get_set_del_exists_round_trip() {
        let engine = Engine::new();
        assert!(!engine.exists(b"foo"));
        engine.set(Bytes::from_static(b"foo"), Value::String(Bytes::from_static(b"bar")));
        assert!(engine.exists(b"foo"));
        assert_eq!(engine.get(b"foo"), Some(Value::String(Bytes::from_static(b"bar"))));
        assert!(engine.del(b"foo"));
        assert!(!engine.exists(b"foo"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine engine::tests`
Expected: FAIL — `Engine` is not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/engine.rs (above the test module)
use bytes::Bytes;
use crate::{store::Store, Value};

pub struct Engine {
    store: Store,
}

impl Engine {
    pub fn new() -> Self {
        Self { store: Store::new(16) }
    }

    pub fn get(&self, key: &[u8]) -> Option<Value> { self.store.get(key) }
    pub fn set(&self, key: Bytes, value: Value) { self.store.set(key, value) }
    pub fn del(&self, key: &[u8]) -> bool { self.store.del(key) }
    pub fn exists(&self, key: &[u8]) -> bool { self.store.exists(key) }
}

impl Default for Engine {
    fn default() -> Self { Self::new() }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p engine engine::tests`
Expected: PASS, 1/1

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/engine.rs crates/engine/src/lib.rs
git commit -m "feat(engine): add Engine facade with get/set/del/exists"
```

---

### Task 2: `keys()` — full keyspace listing

**Files:**
- Modify: `crates/engine/src/engine.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn keys_returns_every_key_that_was_set() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"a"), Value::String(Bytes::from_static(b"1")));
    engine.set(Bytes::from_static(b"b"), Value::String(Bytes::from_static(b"2")));
    let mut keys = engine.keys();
    keys.sort();
    assert_eq!(keys, vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine engine::tests::keys_returns_every_key_that_was_set`
Expected: FAIL — no method named `keys`

- [ ] **Step 3: Write the implementation**

```rust
// add to impl Engine
pub fn keys(&self) -> Vec<Bytes> { self.store.keys() }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p engine engine::tests`
Expected: PASS, 2/2

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/engine.rs
git commit -m "feat(engine): add keys() for full keyspace listing"
```

Note: this is intentionally pattern-free (no glob matching). Pattern support for `KEYS`/`SCAN` is Sprint 3 (Week 5) scope.
