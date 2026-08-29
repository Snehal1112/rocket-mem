# TTL Passive Expiry Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a key can carry an optional expiry; reading, checking existence of, deleting, or listing keys all treat an expired-but-not-yet-swept key as if it were already gone — with zero changes to any existing `commands/*.rs` file or its tests.

**Architecture:** `Shard`'s map changes from `RwLock<HashMap<Bytes, Value>>` to `RwLock<HashMap<Bytes, Entry>>`, where `Entry` wraps a `Value` with an optional `Instant` expiry. `Store` and `Engine` gain `expire_at`/`persist`/`ttl` passthroughs on top of their existing `get`/`set`/`del`/`exists`/`keys`/`scan`, whose signatures and behavior for non-expiring keys are unchanged. `Shard::with_ref`/`with_mut` (already landed on `main` ahead of this plan, as in-place accessors several `commands/*.rs` files now depend on) are preserved and made expiry-aware in the same pass — an expired entry reads back as `None` through them exactly as it does through `get`.

**Tech Stack:** `std::time::Instant` (already in std, no new dependency).

**Spec:** `../../specs/2026-08-30-sprint-4-spec.md` — the `Entry`-in-`Shard` decision (not a `Value` variant), the check-then-remove passive expiry pattern, and the `TtlStatus` enum are authoritative; don't re-derive them here.

**Depends on:** nothing this sprint. `02-active-expiry-background-task.md` and `03-expire-family-and-set-ttl-dispatcher.md` both depend on this plan; `07-lru-eviction-maxmemory.md` depends on `Entry` existing (it adds a field to it).

## Global Constraints

- No existing test in `crates/engine/src/commands/*.rs` may need to change — this plan only touches `shard.rs`, `store.rs`, and `engine.rs`.
- A logically-expired key must be indistinguishable from a never-existing key to every caller of `get`/`exists`/`del`/`keys` (`CLAUDE.md`'s "missing key ≠ error" convention, extended).

---

### Task 1: `Entry` wrapper and passive expiry in `Shard`

**Files:**
- Modify: `crates/engine/src/shard.rs`

**Interfaces:**
- Consumes: `crate::Value` (existing), `Shard::with_ref`/`with_mut` (already on `main`, signatures below — this task keeps them and makes them expiry-aware, it does not introduce them).
- Produces: `pub fn expire_at(&self, key: &[u8], at: Instant) -> bool`, `pub fn persist(&self, key: &[u8]) -> bool` on `Shard`; `get`/`set`/`del`/`exists`/`keys`/`with_ref`/`with_mut` keep their existing signatures but now expiry-aware. `02-active-expiry-background-task.md` also consumes a new `pub fn remove_expired(&self) -> usize`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/shard.rs — add to the existing tests module
use std::time::{Duration, Instant};

#[test]
fn get_returns_none_for_a_key_whose_expiry_has_passed() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
    assert_eq!(shard.get(b"k"), None);
}

#[test]
fn get_returns_the_value_when_the_expiry_is_in_the_future() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    shard.expire_at(b"k", Instant::now() + Duration::from_secs(60));
    assert_eq!(shard.get(b"k"), Some(Value::String(Bytes::from_static(b"v"))));
}

#[test]
fn expired_key_is_actually_removed_from_the_map_after_a_get() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
    assert_eq!(shard.get(b"k"), None);
    assert_eq!(shard.keys(), Vec::<Bytes>::new()); // gone, not just hidden
}

#[test]
fn exists_treats_an_expired_key_as_absent() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
    assert!(!shard.exists(b"k"));
}

#[test]
fn del_on_an_expired_key_reports_it_did_not_exist() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
    assert!(!shard.del(b"k"));
}

#[test]
fn keys_excludes_expired_entries() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"live"), Value::String(Bytes::from_static(b"v")));
    shard.set(Bytes::from_static(b"dead"), Value::String(Bytes::from_static(b"v")));
    shard.expire_at(b"dead", Instant::now() - Duration::from_secs(1));
    assert_eq!(shard.keys(), vec![Bytes::from_static(b"live")]);
}

#[test]
fn expire_at_on_a_missing_key_returns_false() {
    let shard = Shard::new();
    assert!(!shard.expire_at(b"missing", Instant::now() + Duration::from_secs(60)));
}

#[test]
fn expire_at_on_an_existing_key_returns_true_and_takes_effect() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    assert!(shard.expire_at(b"k", Instant::now() - Duration::from_secs(1)));
    assert_eq!(shard.get(b"k"), None);
}

#[test]
fn persist_removes_an_existing_ttl_and_reports_true() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    shard.expire_at(b"k", Instant::now() + Duration::from_secs(60));
    assert!(shard.persist(b"k"));
    // the value is still there, and a second persist finds no TTL left to clear —
    // together those prove the first persist actually removed the expiry rather than
    // just reporting that it did
    assert_eq!(shard.get(b"k"), Some(Value::String(Bytes::from_static(b"v"))));
    assert!(!shard.persist(b"k"));
}

#[test]
fn persist_on_a_key_with_no_ttl_returns_false() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    assert!(!shard.persist(b"k"));
}

#[test]
fn persist_on_a_missing_key_returns_false() {
    let shard = Shard::new();
    assert!(!shard.persist(b"missing"));
}

#[test]
fn set_on_an_existing_key_clears_any_previous_ttl() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"old")));
    shard.expire_at(b"k", Instant::now() + Duration::from_secs(60));
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"new")));
    assert!(!shard.persist(b"k")); // no TTL left to clear — SET already cleared it
}

#[test]
fn with_ref_sees_none_for_a_key_whose_expiry_has_passed() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
    assert!(shard.with_ref(b"k", |v| v.is_none()));
}

#[test]
fn with_mut_sees_none_for_a_key_whose_expiry_has_passed_and_sweeps_it() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
    assert!(shard.with_mut(b"k", |v| v.is_none()));
    assert_eq!(shard.keys(), Vec::<Bytes>::new()); // gone, not just hidden
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine shard::tests`
Expected: FAIL — `expire_at`/`persist` are not defined yet, and `Shard::new`/`set`/`get`/`del`/`exists`/`keys`/`with_ref`/`with_mut` don't yet exist in a form that compiles against these calls (the module won't compile until Step 3 lands)

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/shard.rs — full replacement of the file's non-test content
use crate::Value;
use bytes::Bytes;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::time::Instant;

struct Entry {
    value: Value,
    expires_at: Option<Instant>,
}

impl Entry {
    fn is_expired(&self) -> bool {
        matches!(self.expires_at, Some(at) if Instant::now() >= at)
    }
}

pub struct Shard {
    map: RwLock<HashMap<Bytes, Entry>>,
}

impl Shard {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<Value> {
        {
            let guard = self.map.read();
            match guard.get(key) {
                None => return None,
                Some(entry) if !entry.is_expired() => return Some(entry.value.clone()),
                Some(_) => {} // expired — fall through to remove it under a write lock
            }
        }
        let mut guard = self.map.write();
        if matches!(guard.get(key), Some(e) if e.is_expired()) {
            guard.remove(key);
        }
        None
    }

    pub fn set(&self, key: Bytes, value: Value) {
        self.map.write().insert(
            key,
            Entry {
                value,
                expires_at: None,
            },
        );
    }

    pub fn del(&self, key: &[u8]) -> bool {
        let mut guard = self.map.write();
        match guard.remove(key) {
            None => false,
            Some(entry) => !entry.is_expired(),
        }
    }

    // Routes through `with_ref`, not `get`: both give the same "expired == absent" answer,
    // but `get` clones the whole stored value out just to throw it away, which would make
    // EXISTS on a large Hash/List/Set an O(collection size) copy — exactly the clone-out
    // pattern `with_ref` was introduced to remove.
    pub fn exists(&self, key: &[u8]) -> bool {
        self.with_ref(key, |v| v.is_some())
    }

    pub fn keys(&self) -> Vec<Bytes> {
        self.map
            .read()
            .iter()
            .filter(|(_, entry)| !entry.is_expired())
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn expire_at(&self, key: &[u8], at: Instant) -> bool {
        let mut guard = self.map.write();
        match guard.get_mut(key) {
            Some(entry) if !entry.is_expired() => {
                entry.expires_at = Some(at);
                true
            }
            _ => false,
        }
    }

    pub fn persist(&self, key: &[u8]) -> bool {
        let mut guard = self.map.write();
        match guard.get_mut(key) {
            Some(entry) if !entry.is_expired() && entry.expires_at.is_some() => {
                entry.expires_at = None;
                true
            }
            _ => false,
        }
    }

    // Preserved from the pre-existing in-place-mutation accessors `commands/{hash,list,set,
    // sorted_set}.rs` already call instead of clone-out/set-back. An expired entry reads back
    // as `None` here exactly as it does through `get`, so those callers don't need to know
    // expiry exists. Neither method auto-vivifies a missing key — a caller needing
    // create-on-missing (e.g. LPUSH) still calls `set` itself.
    pub fn with_ref<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&Value>) -> R,
    {
        let guard = self.map.read();
        match guard.get(key) {
            Some(entry) if !entry.is_expired() => f(Some(&entry.value)),
            _ => f(None),
        }
    }

    pub fn with_mut<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&mut Value>) -> R,
    {
        let mut guard = self.map.write();
        // Already holding the write lock, so an expired entry is swept here rather than
        // left for a future get/sweep — same "actually removed, not just hidden" guarantee
        // Task 1's get() gives.
        if matches!(guard.get(key), Some(entry) if entry.is_expired()) {
            guard.remove(key);
        }
        match guard.get_mut(key) {
            Some(entry) => f(Some(&mut entry.value)),
            None => f(None),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine shard::tests`
Expected: PASS, all tests including the 14 new ones

- [ ] **Step 5: Verify the workspace still builds**

Run: `cargo build --workspace`
Expected: PASS — `Shard::get`/`set`/`del`/`exists`/`keys`/`with_ref`/`with_mut` keep their existing public signatures, so `Store` (which only calls these) and every `commands/*.rs` file (which only ever goes through `Engine`, never `Shard` directly — including `hash.rs`/`list.rs`/`set.rs`/`sorted_set.rs`'s `with_ref`/`with_mut` call sites) compile unchanged

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/shard.rs` — do not compose the commit message freeform. Suggested
subject: `feat(engine): add TTL-aware Entry wrapper with passive expiry to Shard`.

---

### Task 2: `TtlStatus`, and `Store`/`Engine` passthroughs

**Files:**
- Modify: `crates/engine/src/store.rs`
- Modify: `crates/engine/src/engine.rs`

**Interfaces:**
- Consumes: `Shard::{expire_at, persist}` (Task 1).
- Produces: `pub enum TtlStatus { NoSuchKey, NoExpiry, Remaining(Duration) }` (in `engine.rs`, re-exported from the crate root); `pub fn expire_at(&self, key: &[u8], at: Instant) -> bool`, `pub fn persist(&self, key: &[u8]) -> bool`, `pub fn ttl(&self, key: &[u8]) -> TtlStatus` on both `Store` and `Engine`. `03-expire-family-and-set-ttl-dispatcher.md` consumes all three `Engine` methods and `TtlStatus`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/engine.rs — add to the existing tests module
use std::time::{Duration, Instant};

#[test]
fn ttl_on_a_missing_key_is_no_such_key() {
    let engine = Engine::new();
    assert_eq!(engine.ttl(b"missing"), TtlStatus::NoSuchKey);
}

#[test]
fn ttl_on_a_key_with_no_expiry_is_no_expiry() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    assert_eq!(engine.ttl(b"k"), TtlStatus::NoExpiry);
}

#[test]
fn ttl_on_a_key_with_a_future_expiry_reports_remaining_time() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    engine.expire_at(b"k", Instant::now() + Duration::from_secs(60));
    match engine.ttl(b"k") {
        TtlStatus::Remaining(d) => assert!(d <= Duration::from_secs(60) && d > Duration::from_secs(55)),
        other => panic!("expected Remaining, got {other:?}"),
    }
}

#[test]
fn expire_at_and_persist_round_trip_through_engine() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    assert!(engine.expire_at(b"k", Instant::now() + Duration::from_secs(60)));
    assert!(engine.persist(b"k"));
    assert_eq!(engine.ttl(b"k"), TtlStatus::NoExpiry);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine engine::tests`
Expected: FAIL — `TtlStatus`, `expire_at`, `persist`, `ttl` not defined yet on `Engine`

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/store.rs — add to the `impl Store` block
pub fn expire_at(&self, key: &[u8], at: std::time::Instant) -> bool {
    self.shard_for(key).expire_at(key, at)
}
pub fn persist(&self, key: &[u8]) -> bool {
    self.shard_for(key).persist(key)
}
pub fn ttl(&self, key: &[u8]) -> crate::engine::TtlStatus {
    self.shard_for(key).ttl(key)
}
```

```rust
// crates/engine/src/shard.rs — add to the `impl Shard` block (alongside expire_at/persist from Task 1)
pub fn ttl(&self, key: &[u8]) -> crate::engine::TtlStatus {
    use crate::engine::TtlStatus;
    let guard = self.map.read();
    match guard.get(key) {
        None => TtlStatus::NoSuchKey,
        Some(entry) if entry.is_expired() => TtlStatus::NoSuchKey,
        Some(entry) => match entry.expires_at {
            None => TtlStatus::NoExpiry,
            Some(at) => TtlStatus::Remaining(at.saturating_duration_since(Instant::now())),
        },
    }
}
```

```rust
// crates/engine/src/engine.rs — add above the `impl Engine` block
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtlStatus {
    NoSuchKey,
    NoExpiry,
    Remaining(Duration),
}
```

```rust
// crates/engine/src/engine.rs — add to the `impl Engine` block
pub fn expire_at(&self, key: &[u8], at: Instant) -> bool {
    self.store.expire_at(key, at)
}
pub fn persist(&self, key: &[u8]) -> bool {
    self.store.persist(key)
}
pub fn ttl(&self, key: &[u8]) -> TtlStatus {
    self.store.ttl(key)
}
```

```rust
// crates/engine/src/lib.rs — replace the `pub use engine::Engine;` line
pub use engine::{Engine, TtlStatus};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine engine::tests`
Expected: PASS, all tests including the 4 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/store.rs`, `crates/engine/src/shard.rs`, `crates/engine/src/engine.rs`,
and `crates/engine/src/lib.rs` — do not compose the commit message freeform. Suggested
subject: `feat(engine): add TtlStatus and expire_at/persist/ttl on Store and Engine`.
