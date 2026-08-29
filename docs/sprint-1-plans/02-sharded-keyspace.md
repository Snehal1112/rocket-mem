# Sharded Keyspace Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** build the N-shard, lock-based keyspace that every command operates through — the concurrency backbone of the whole server.

**Architecture:** a fixed array of 16 `Shard`s, each wrapping `RwLock<HashMap<Bytes, Value>>`. Keys route to a shard via `DefaultHasher` over the key bytes, modulo shard count. See `00-sprint-1-spec.md`.

**Tech Stack:** `parking_lot::RwLock`, `std::collections::HashMap`.

**Depends on:** `01-workspace-scaffold-and-value-enum.md` must be complete.

---

### Task 1: `Shard` — a single lock-protected map

**Files:**
- Create: `crates/engine/src/shard.rs`
- Modify: `crates/engine/src/lib.rs` — add `mod shard;`

- [ ] **Step 1: Write the failing test**

```rust
// crates/engine/src/shard.rs
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use crate::Value;

    #[test]
    fn set_then_get_returns_value() {
        let shard = Shard::new();
        shard.set(Bytes::from_static(b"foo"), Value::String(Bytes::from_static(b"bar")));
        assert_eq!(shard.get(b"foo"), Some(Value::String(Bytes::from_static(b"bar"))));
    }

    #[test]
    fn del_removes_key_and_reports_it_existed() {
        let shard = Shard::new();
        shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
        assert!(shard.del(b"k"));
        assert_eq!(shard.get(b"k"), None);
        assert!(!shard.del(b"k"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine shard::tests`
Expected: FAIL — `Shard` is not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/shard.rs (above the test module)
use bytes::Bytes;
use parking_lot::RwLock;
use std::collections::HashMap;
use crate::Value;

pub struct Shard {
    map: RwLock<HashMap<Bytes, Value>>,
}

impl Shard {
    pub fn new() -> Self {
        Self { map: RwLock::new(HashMap::new()) }
    }

    pub fn get(&self, key: &[u8]) -> Option<Value> {
        self.map.read().get(key).cloned()
    }

    pub fn set(&self, key: Bytes, value: Value) {
        self.map.write().insert(key, value);
    }

    pub fn del(&self, key: &[u8]) -> bool {
        self.map.write().remove(key).is_some()
    }

    pub fn exists(&self, key: &[u8]) -> bool {
        self.map.read().contains_key(key)
    }

    pub fn keys(&self) -> Vec<Bytes> {
        self.map.read().keys().cloned().collect()
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p engine shard::tests`
Expected: PASS, 2/2

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/shard.rs crates/engine/src/lib.rs
git commit -m "feat(engine): add Shard, a single lock-protected key/value map"
```

---

### Task 2: `Store` — routes keys to shards

**Files:**
- Create: `crates/engine/src/store.rs`
- Modify: `crates/engine/src/lib.rs` — add `mod store;`, `pub use store::Store;`

- [ ] **Step 1: Write the failing test**

```rust
// crates/engine/src/store.rs
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use crate::Value;

    #[test]
    fn set_then_get_round_trips() {
        let store = Store::new(16);
        store.set(Bytes::from_static(b"foo"), Value::String(Bytes::from_static(b"bar")));
        assert_eq!(store.get(b"foo"), Some(Value::String(Bytes::from_static(b"bar"))));
    }

    #[test]
    fn keys_distribute_across_more_than_one_shard() {
        let store = Store::new(16);
        for i in 0..1000 {
            store.set(Bytes::from(format!("key{i}")), Value::String(Bytes::from_static(b"v")));
        }
        let non_empty = store.shard_key_counts().into_iter().filter(|&c| c > 0).count();
        assert!(non_empty > 1, "expected keys to spread across shards, got {non_empty} non-empty");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine store::tests`
Expected: FAIL — `Store` is not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/store.rs (above the test module)
use bytes::Bytes;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use crate::{shard::Shard, Value};

pub struct Store {
    shards: Vec<Shard>,
}

impl Store {
    pub fn new(shard_count: usize) -> Self {
        Self { shards: (0..shard_count).map(|_| Shard::new()).collect() }
    }

    fn shard_for(&self, key: &[u8]) -> &Shard {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let idx = (hasher.finish() as usize) % self.shards.len();
        &self.shards[idx]
    }

    pub fn get(&self, key: &[u8]) -> Option<Value> { self.shard_for(key).get(key) }
    pub fn set(&self, key: Bytes, value: Value) { self.shard_for(&key).set(key, value) }
    pub fn del(&self, key: &[u8]) -> bool { self.shard_for(key).del(key) }
    pub fn exists(&self, key: &[u8]) -> bool { self.shard_for(key).exists(key) }
    pub fn keys(&self) -> Vec<Bytes> { self.shards.iter().flat_map(|s| s.keys()).collect() }

    #[cfg(test)]
    pub fn shard_key_counts(&self) -> Vec<usize> {
        self.shards.iter().map(|s| s.keys().len()).collect()
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p engine store::tests`
Expected: PASS, 2/2

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/store.rs crates/engine/src/lib.rs
git commit -m "feat(engine): add Store routing keys across shards"
```

---

### Task 3: Concurrency stress test

No new production code — this task proves Task 1 and 2 hold up under real concurrent access, which unit tests alone can't show.

**Files:**
- Modify: `crates/engine/src/store.rs` — add to the existing test module

- [ ] **Step 1: Write the stress test**

```rust
#[test]
fn concurrent_reads_and_writes_do_not_panic_or_deadlock() {
    use std::sync::Arc;
    use std::thread;

    let store = Arc::new(Store::new(16));
    let mut handles = vec![];

    for t in 0..8 {
        let store = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            for i in 0..2000 {
                let key = Bytes::from(format!("t{t}-k{i}"));
                store.set(key.clone(), Value::String(Bytes::from_static(b"v")));
                let _ = store.get(&key);
                if i % 7 == 0 {
                    store.del(&key);
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("worker thread panicked");
    }
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p engine store::tests -- --nocapture`
Expected: PASS, completes without hanging (a deadlock would hang the test runner, not fail cleanly — if it hangs past ~10s, that's the signal something's wrong)

- [ ] **Step 3: Commit**

```bash
git add crates/engine/src/store.rs
git commit -m "test(engine): add concurrent access stress test for sharded store"
```
