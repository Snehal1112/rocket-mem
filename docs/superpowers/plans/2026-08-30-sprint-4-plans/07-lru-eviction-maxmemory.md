# LRU Eviction & MAXMEMORY Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** an `Engine` opted into a byte ceiling (`Engine::with_maxmemory`) evicts approximately-least-recently-used keys after any `set` that pushes it over budget — memory stays bounded, matching the sprint goal's "memory stays bounded under a configured ceiling."

**Architecture:** `Entry` (from `01-ttl-passive-expiry-core.md`) gains a `last_touched: AtomicU64` field — a *logical* tick from a single `Store`-wide counter, not a wall-clock timestamp — bumped on every `get`/`set` via an atomic store, so the existing read-lock-only fast path in `Shard::get` never needs to escalate to a write lock just to record recency. `Value` gains `approx_size`. Eviction samples a handful of entries from each shard, compares their ticks (comparable across shards precisely because they all come from the same counter), and removes the globally-oldest-looking ones sampled until back under budget.

**Tech Stack:** `std::sync::atomic::{AtomicU64, AtomicUsize, Ordering}` (std, no new dependency).

**Spec:** `../../specs/2026-08-30-sprint-4-spec.md` — the "recency tick + sampling, not a true LRU list" decision is authoritative; this mirrors real Redis's own "approximated LRU," not a corner cut.

**Depends on:** `01-ttl-passive-expiry-core.md` (`Entry`, `Shard`'s map, `expire_at`/`persist`/`ttl`) **and `02-active-expiry-background-task.md`** (`Shard::remove_expired`, `Store::active_expire_cycle`) — Task 1 below replaces `shard.rs` and `store.rs` wholesale, so both of those plans' additions must already be in the files it replaces.

## Global Constraints

- `Engine::new()` (used by ~600 existing tests across every prior sprint) must keep meaning "no memory ceiling" — `maxmemory` is `None` by default, opted into only via the new `Engine::with_maxmemory` constructor.
- Eviction must be bounded — never an unbounded loop even if something is misconfigured (a ceiling smaller than a single entry's size, for instance).
- Recency must be comparable *across* shards for eviction to correctly find the globally-oldest sampled entry — a per-shard-local counter would make cross-shard comparisons meaningless, which is why the tick source is a single `Store`-wide `AtomicU64`, not one per shard.

---

### Task 1: `Value::approx_size`, `Entry.last_touched`, and per-shard byte accounting

**Files:**
- Modify: `crates/engine/src/value.rs`
- Modify: `crates/engine/src/shard.rs`
- Modify: `crates/engine/src/store.rs`

**Interfaces:**
- Consumes: `SortedSet::members_ascending` (existing, from Sprint 3), `Shard::with_ref`/`with_mut` (from `01-ttl-passive-expiry-core.md` — this task keeps them, threading the same clock through them that `get`/`set` gain here, and re-accounts `bytes_used` around `with_mut`'s closure since `commands/{hash,list,set,sorted_set}.rs` grow collections through it, not through `set`).
- Produces: `pub fn approx_size(&self) -> usize` on `Value`; `Shard::get`/`set`/`exists`/`with_ref`/`with_mut` now take an extra `&AtomicU64` clock parameter (`del`/`keys`/`expire_at`/`persist`/`ttl`/`remove_expired` do not) (internal to the engine crate — `Shard` isn't part of the crate's public API, so this is not a breaking change to anything outside `crates/engine/src/store.rs`); `pub fn bytes_used(&self) -> usize` and `pub fn sample_recency(&self, n: usize) -> Vec<(Bytes, u64)>` on `Shard`; `pub fn memory_used(&self) -> usize` and `pub fn sample_for_eviction(&self, per_shard: usize) -> Vec<(Bytes, u64)>` on `Store`. `Task 2` of this plan consumes all of the `Store`-level additions.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/value.rs — add to the existing tests module
#[test]
fn approx_size_grows_with_string_content_length() {
    let small = Value::String(Bytes::from_static(b"hi"));
    let big = Value::String(Bytes::from_static(b"a much longer string value"));
    assert!(big.approx_size() > small.approx_size());
}

#[test]
fn approx_size_is_never_zero_even_for_an_empty_value() {
    assert!(Value::String(Bytes::new()).approx_size() > 0);
    assert!(Value::List(VecDeque::new()).approx_size() > 0);
}
```

```rust
// crates/engine/src/shard.rs — add to the existing tests module
use std::sync::atomic::AtomicU64;

#[test]
fn bytes_used_increases_after_set_and_decreases_after_del() {
    let shard = Shard::new();
    let clock = AtomicU64::new(0);
    assert_eq!(shard.bytes_used(), 0);
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")), &clock);
    assert!(shard.bytes_used() > 0);
    let after_set = shard.bytes_used();
    shard.del(b"k");
    assert!(shard.bytes_used() < after_set);
}

#[test]
fn bytes_used_accounts_for_overwriting_an_existing_key_not_double_counting_it() {
    let shard = Shard::new();
    let clock = AtomicU64::new(0);
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"aaaaaaaaaa")), &clock);
    let after_first = shard.bytes_used();
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"aaaaaaaaaa")), &clock);
    assert_eq!(shard.bytes_used(), after_first); // same key, same size — no growth
}

#[test]
fn get_bumps_last_touched_to_a_fresh_tick() {
    let shard = Shard::new();
    let clock = AtomicU64::new(0);
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")), &clock);
    let before = shard.sample_recency(10)[0].1;
    shard.get(b"k", &clock);
    let after = shard.sample_recency(10)[0].1;
    assert!(after > before);
}

#[test]
fn sample_recency_returns_up_to_n_entries() {
    let shard = Shard::new();
    let clock = AtomicU64::new(0);
    for i in 0..5 {
        shard.set(Bytes::from(format!("k{i}")), Value::String(Bytes::from_static(b"v")), &clock);
    }
    assert_eq!(shard.sample_recency(3).len(), 3);
    assert_eq!(shard.sample_recency(100).len(), 5); // never more than what's actually there
}

#[test]
fn with_ref_bumps_last_touched_to_a_fresh_tick() {
    let shard = Shard::new();
    let clock = AtomicU64::new(0);
    shard.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")), &clock);
    let before = shard.sample_recency(10)[0].1;
    shard.with_ref(b"k", |v| v.is_some(), &clock);
    let after = shard.sample_recency(10)[0].1;
    assert!(after > before);
}

#[test]
fn with_mut_re_accounts_bytes_used_after_growing_a_value_in_place() {
    let shard = Shard::new();
    let clock = AtomicU64::new(0);
    shard.set(Bytes::from_static(b"k"), Value::List(std::collections::VecDeque::new()), &clock);
    let before = shard.bytes_used();
    shard.with_mut(
        b"k",
        |v| {
            if let Some(Value::List(list)) = v {
                list.push_back(Bytes::from_static(b"a much longer element than before"));
            }
        },
        &clock,
    );
    assert!(shard.bytes_used() > before);
}
```

```rust
// crates/engine/src/store.rs — add to the existing tests module
#[test]
fn memory_used_sums_bytes_used_across_all_shards() {
    let store = Store::new(16);
    assert_eq!(store.memory_used(), 0);
    store.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    assert!(store.memory_used() > 0);
}

#[test]
fn sample_for_eviction_collects_candidates_from_every_shard() {
    let store = Store::new(16);
    for i in 0..32 {
        store.set(Bytes::from(format!("k{i}")), Value::String(Bytes::from_static(b"v")));
    }
    // with 32 keys spread across 16 shards, sampling 1 per shard should find at least
    // several distinct shards' worth of candidates (exact count depends on hash distribution)
    assert!(store.sample_for_eviction(1).len() >= 8);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine value::tests shard::tests store::tests`
Expected: FAIL — `approx_size`, `bytes_used`, `sample_recency`, `memory_used`, `sample_for_eviction` not defined yet, and `Shard::set`/`get`/`with_ref`/`with_mut`'s call sites in these new tests don't match the current signatures

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/value.rs — add to the `impl Value` block
/// A rough byte-size estimate — not exact, not meant to be (see
/// ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md's LRU/MAXMEMORY decision).
pub fn approx_size(&self) -> usize {
    const OVERHEAD: usize = 48; // rough per-entry bookkeeping estimate
    let content = match self {
        Value::String(b) => b.len(),
        Value::List(l) => l.iter().map(|b| b.len() + 8).sum(),
        Value::Hash(m) => m.iter().map(|(k, v)| k.len() + v.len() + 16).sum(),
        Value::Set(s) => s.iter().map(|b| b.len() + 8).sum(),
        Value::SortedSet(z) => z.members_ascending().map(|m| m.len() + 24).sum(),
    };
    OVERHEAD + content
}
```

```rust
// crates/engine/src/shard.rs — full replacement of the file's non-test content
use crate::Value;
use bytes::Bytes;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

struct Entry {
    value: Value,
    expires_at: Option<Instant>,
    last_touched: AtomicU64,
}

impl Entry {
    fn is_expired(&self) -> bool {
        matches!(self.expires_at, Some(at) if Instant::now() >= at)
    }
}

fn entry_size(key: &[u8], value: &Value) -> usize {
    key.len() + value.approx_size()
}

pub struct Shard {
    map: RwLock<HashMap<Bytes, Entry>>,
    bytes_used: AtomicUsize,
}

impl Shard {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
            bytes_used: AtomicUsize::new(0),
        }
    }

    pub fn get(&self, key: &[u8], clock: &AtomicU64) -> Option<Value> {
        {
            let guard = self.map.read();
            match guard.get(key) {
                None => return None,
                Some(entry) if !entry.is_expired() => {
                    entry
                        .last_touched
                        .store(clock.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
                    return Some(entry.value.clone());
                }
                Some(_) => {} // expired — fall through to remove it under a write lock
            }
        }
        let mut guard = self.map.write();
        if let Some(entry) = guard.get(key) {
            if entry.is_expired() {
                let size = entry_size(key, &entry.value);
                guard.remove(key);
                self.bytes_used.fetch_sub(size, Ordering::Relaxed);
            }
        }
        None
    }

    pub fn set(&self, key: Bytes, value: Value, clock: &AtomicU64) {
        let new_size = entry_size(&key, &value);
        let mut guard = self.map.write();
        let old_size = guard.get(&key).map(|e| entry_size(&key, &e.value));
        guard.insert(
            key,
            Entry {
                value,
                expires_at: None,
                last_touched: AtomicU64::new(clock.fetch_add(1, Ordering::Relaxed)),
            },
        );
        drop(guard);
        if let Some(old) = old_size {
            self.bytes_used.fetch_sub(old, Ordering::Relaxed);
        }
        self.bytes_used.fetch_add(new_size, Ordering::Relaxed);
    }

    pub fn del(&self, key: &[u8]) -> bool {
        let mut guard = self.map.write();
        match guard.remove(key) {
            None => false,
            Some(entry) => {
                let existed = !entry.is_expired();
                self.bytes_used
                    .fetch_sub(entry_size(key, &entry.value), Ordering::Relaxed);
                existed
            }
        }
    }

    // Via `with_ref`, not `get` — same answer, without cloning the whole stored value out
    // just to discard it (see 01-ttl-passive-expiry-core.md's note). Still bumps recency,
    // since `with_ref` does.
    pub fn exists(&self, key: &[u8], clock: &AtomicU64) -> bool {
        self.with_ref(key, |v| v.is_some(), clock)
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

    pub fn remove_expired(&self) -> usize {
        let mut guard = self.map.write();
        let mut removed_bytes = 0usize;
        let before = guard.len();
        guard.retain(|k, entry| {
            if entry.is_expired() {
                removed_bytes += entry_size(k, &entry.value);
                false
            } else {
                true
            }
        });
        self.bytes_used.fetch_sub(removed_bytes, Ordering::Relaxed);
        before - guard.len()
    }

    pub fn bytes_used(&self) -> usize {
        self.bytes_used.load(Ordering::Relaxed)
    }

    /// Not a true random sample — an arbitrary `n` entries in current hash-iteration order.
    /// Good enough for approximated-LRU eviction (see the spec's decision); a fully random
    /// sample would need `rand`, which buys nothing extra here.
    pub fn sample_recency(&self, n: usize) -> Vec<(Bytes, u64)> {
        self.map
            .read()
            .iter()
            .take(n)
            .map(|(k, e)| (k.clone(), e.last_touched.load(Ordering::Relaxed)))
            .collect()
    }

    // Preserved from `01-ttl-passive-expiry-core.md`, now also bumping recency on every
    // access (not just get/set) so a key that's only ever touched through hash/list/set/
    // sorted_set commands — which route through here, not get/set — still looks "used" to
    // eviction instead of aging out immediately.
    pub fn with_ref<F, R>(&self, key: &[u8], f: F, clock: &AtomicU64) -> R
    where
        F: FnOnce(Option<&Value>) -> R,
    {
        let guard = self.map.read();
        match guard.get(key) {
            Some(entry) if !entry.is_expired() => {
                entry
                    .last_touched
                    .store(clock.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
                f(Some(&entry.value))
            }
            _ => f(None),
        }
    }

    // Re-accounts bytes_used around the closure: with_mut is how RPUSH/HSET/SADD/ZADD-style
    // growth happens in place (see 01's note and commands/{hash,list,set,sorted_set}.rs), and
    // unlike set() it never goes through entry_size() on its own — without this, MAXMEMORY
    // would never see memory grow from any command that mutates a collection in place, only
    // from whole-value SETs.
    pub fn with_mut<F, R>(&self, key: &[u8], f: F, clock: &AtomicU64) -> R
    where
        F: FnOnce(Option<&mut Value>) -> R,
    {
        let mut guard = self.map.write();
        if let Some(entry) = guard.get(key) {
            if entry.is_expired() {
                let size = entry_size(key, &entry.value);
                guard.remove(key);
                self.bytes_used.fetch_sub(size, Ordering::Relaxed);
            }
        }
        match guard.get_mut(key) {
            Some(entry) => {
                entry
                    .last_touched
                    .store(clock.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
                let old_size = entry.value.approx_size();
                let result = f(Some(&mut entry.value));
                let new_size = entry.value.approx_size();
                match new_size.cmp(&old_size) {
                    std::cmp::Ordering::Greater => {
                        self.bytes_used
                            .fetch_add(new_size - old_size, Ordering::Relaxed);
                    }
                    std::cmp::Ordering::Less => {
                        self.bytes_used
                            .fetch_sub(old_size - new_size, Ordering::Relaxed);
                    }
                    std::cmp::Ordering::Equal => {}
                }
                result
            }
            None => f(None),
        }
    }
}
```

```rust
// crates/engine/src/store.rs — add the clock field, update get/set/exists/with_ref/with_mut
// to pass it, and add the new methods. This block reproduces the WHOLE `impl Store`, including
// `scan` (Sprint 3), `expire_at`/`persist`/`ttl` (01-ttl-passive-expiry-core.md) and
// `active_expire_cycle` (02-active-expiry-background-task.md) — they are unchanged by this
// task and are shown only so the block is self-consistent. Keep them; do NOT drop them
// because they look like they belong to another plan.
use crate::{shard::Shard, Value};
use bytes::Bytes;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::AtomicU64;

pub struct Store {
    shards: Vec<Shard>,
    clock: AtomicU64,
}

impl Store {
    pub fn new(shard_count: usize) -> Self {
        Self {
            shards: (0..shard_count).map(|_| Shard::new()).collect(),
            clock: AtomicU64::new(0),
        }
    }

    fn shard_for(&self, key: &[u8]) -> &Shard {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let idx = (hasher.finish() as usize) % self.shards.len();
        &self.shards[idx]
    }

    pub fn get(&self, key: &[u8]) -> Option<Value> {
        self.shard_for(key).get(key, &self.clock)
    }
    pub fn set(&self, key: Bytes, value: Value) {
        self.shard_for(&key).set(key, value, &self.clock)
    }
    pub fn del(&self, key: &[u8]) -> bool {
        self.shard_for(key).del(key)
    }
    pub fn exists(&self, key: &[u8]) -> bool {
        self.shard_for(key).exists(key, &self.clock)
    }
    pub fn keys(&self) -> Vec<Bytes> {
        self.shards.iter().flat_map(|s| s.keys()).collect()
    }
    pub fn with_ref<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&Value>) -> R,
    {
        self.shard_for(key).with_ref(key, f, &self.clock)
    }
    pub fn with_mut<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&mut Value>) -> R,
    {
        self.shard_for(key).with_mut(key, f, &self.clock)
    }

    // --- unchanged by this task, reproduced so the impl block stays complete ---
    pub fn scan(&self, cursor: u64) -> (u64, Vec<Bytes>) {
        let idx = cursor as usize;
        if idx >= self.shards.len() {
            return (0, Vec::new());
        }
        let keys = self.shards[idx].keys();
        let next = if idx + 1 >= self.shards.len() {
            0
        } else {
            (idx + 1) as u64
        };
        (next, keys)
    }
    pub fn expire_at(&self, key: &[u8], at: std::time::Instant) -> bool {
        self.shard_for(key).expire_at(key, at)
    }
    pub fn persist(&self, key: &[u8]) -> bool {
        self.shard_for(key).persist(key)
    }
    pub fn ttl(&self, key: &[u8]) -> crate::engine::TtlStatus {
        self.shard_for(key).ttl(key)
    }
    pub fn active_expire_cycle(&self, shard_idx: usize) -> usize {
        self.shards[shard_idx % self.shards.len()].remove_expired()
    }
    // --- end unchanged ---

    pub fn memory_used(&self) -> usize {
        self.shards.iter().map(|s| s.bytes_used()).sum()
    }

    pub fn sample_for_eviction(&self, per_shard: usize) -> Vec<(Bytes, u64)> {
        self.shards
            .iter()
            .flat_map(|s| s.sample_recency(per_shard))
            .collect()
    }

    #[cfg(test)]
    pub fn shard_key_counts(&self) -> Vec<usize> {
        self.shards.iter().map(|s| s.keys().len()).collect()
    }
}
```

Note: the five methods in the "unchanged by this task" band above (`scan`, `expire_at`,
`persist`, `ttl`, `active_expire_cycle`) are genuinely unaffected — they don't touch recency
and keep calling the same `Shard` methods, whose signatures didn't change. They appear in the
block purely so a whole-`impl`-block replacement doesn't delete them; `Engine::scan` and
`Engine::active_expire_cycle` both call straight into them, and `commands/keys.rs` +
`dispatcher.rs`'s `SCAN`/`HSCAN` arms depend on `scan` transitively. `with_ref`/`with_mut`
*do* change underneath (their `Shard`-level counterparts now take `&self.clock`), but
`Store`'s own `with_ref`/`with_mut` signatures above stay the same 2-argument shape callers
already use, so `Engine` and `commands/*.rs` need no changes here either.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine value::tests shard::tests store::tests`
Expected: PASS, all tests including the 10 new ones

- [ ] **Step 5: Verify the workspace still builds**

Run: `cargo build --workspace`
Expected: PASS — `Store::get`/`set`/`del`/`exists`/`keys`/`with_ref`/`with_mut` keep their
existing public signatures (only `Shard`'s, which is crate-private, changed), so `Engine`
and every `commands/*.rs` file — including `hash.rs`/`list.rs`/`set.rs`/`sorted_set.rs`'s
`with_ref`/`with_mut` call sites — compile unchanged

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/value.rs`, `crates/engine/src/shard.rs`, and `crates/engine/src/store.rs`
— do not compose the commit message freeform. Suggested subject:
`feat(engine): add approx_size and recency-tracked byte accounting`.

---

### Task 2: `Engine::with_maxmemory` and eviction-on-set

**Files:**
- Modify: `crates/engine/src/engine.rs`

**Interfaces:**
- Consumes: `Store::{memory_used, sample_for_eviction}` (Task 1).
- Produces: `pub fn with_maxmemory(bytes: usize) -> Self`, `pub fn memory_used(&self) -> usize`, `pub fn eviction_count(&self) -> usize` on `Engine`; `Engine::set` and `Engine::with_mut` both trigger eviction. `09-memory-usage-object-encoding-stubs.md` consumes `Value::approx_size` (Task 1), not `memory_used`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/engine.rs — add to the existing tests module
#[test]
fn new_engine_has_no_memory_ceiling_and_never_evicts() {
    let engine = Engine::new();
    for i in 0..1000 {
        engine.set(Bytes::from(format!("k{i}")), Value::String(Bytes::from(vec![b'x'; 100])));
    }
    assert_eq!(engine.eviction_count(), 0);
}

#[test]
fn with_maxmemory_keeps_memory_used_under_the_configured_ceiling() {
    let engine = Engine::with_maxmemory(2_000);
    for i in 0..100 {
        engine.set(
            Bytes::from(format!("k{i}")),
            Value::String(Bytes::from(vec![b'x'; 100])),
        );
    }
    assert!(engine.memory_used() <= 2_000);
    assert!(engine.eviction_count() > 0);
}

#[test]
fn with_maxmemory_evicts_the_least_recently_touched_key_first() {
    // a ceiling that comfortably fits 2 entries but not 3
    let engine = Engine::with_maxmemory(300);
    engine.set(Bytes::from_static(b"old"), Value::String(Bytes::from(vec![b'x'; 50])));
    engine.set(Bytes::from_static(b"middle"), Value::String(Bytes::from(vec![b'x'; 50])));
    engine.get(b"old"); // touch "old" so it's fresher than "middle" going into the next set
    engine.set(Bytes::from_static(b"new"), Value::String(Bytes::from(vec![b'x'; 50])));
    // "middle" is now the least-recently-touched of the three and should be the one evicted
    // (not a strict guarantee under sampling, but true whenever "middle" is in the sample —
    // this test uses a small enough keyspace that every key is always sampled)
    assert_eq!(engine.get(b"middle"), None);
    assert!(engine.get(b"old").is_some());
    assert!(engine.get(b"new").is_some());
}

#[test]
fn with_maxmemory_also_bounds_memory_grown_in_place_not_only_through_set() {
    // RPUSH/HSET/SADD/ZADD never call Engine::set — they grow a value through with_mut.
    // Without eviction wired into with_mut too, this workload would blow straight past the
    // ceiling while memory accounting silently watched it happen.
    let engine = Engine::with_maxmemory(500);
    engine.set(
        Bytes::from_static(b"filler"),
        Value::String(Bytes::from(vec![b'x'; 100])),
    );
    engine.set(
        Bytes::from_static(b"list"),
        Value::List(std::collections::VecDeque::new()),
    );
    for i in 0..50 {
        engine.with_mut(b"list", |v| {
            if let Some(Value::List(l)) = v {
                l.push_back(Bytes::from(format!("element-{i}")));
            }
        });
    }
    assert!(engine.memory_used() <= 500);
    assert!(engine.eviction_count() > 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine engine::tests`
Expected: FAIL — `with_maxmemory`, `memory_used`, `eviction_count` not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/engine.rs — replace the existing `pub struct Engine` and the whole
// `impl Engine` block. Everything already in that block is reproduced below so it stays
// self-consistent: `scan` (Sprint 3), `with_ref`/`with_mut` (pre-sprint), `expire_at`/
// `persist`/`ttl` (01-ttl-passive-expiry-core.md) and `active_expire_cycle`
// (02-active-expiry-background-task.md). Keep every one of them — don't drop any because
// it's "already there" or "belongs to another plan." The `TtlStatus` enum above the block
// and the `impl Default for Engine` below it are untouched.
//
// Only ONE new import is needed — the file already has `use std::time::{Duration, Instant};`
// from 01-ttl-passive-expiry-core.md, so re-adding `use std::time::Instant;` here would be a
// duplicate-import error (E0252):
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Engine {
    store: Store,
    maxmemory: Option<usize>,
    eviction_count: AtomicUsize,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            store: Store::new(16),
            maxmemory: None,
            eviction_count: AtomicUsize::new(0),
        }
    }

    pub fn with_maxmemory(bytes: usize) -> Self {
        Self {
            store: Store::new(16),
            maxmemory: Some(bytes),
            eviction_count: AtomicUsize::new(0),
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<Value> {
        self.store.get(key)
    }
    pub fn set(&self, key: Bytes, value: Value) {
        self.store.set(key, value);
        self.maybe_evict();
    }
    pub fn del(&self, key: &[u8]) -> bool {
        self.store.del(key)
    }
    pub fn exists(&self, key: &[u8]) -> bool {
        self.store.exists(key)
    }
    pub fn keys(&self) -> Vec<Bytes> {
        self.store.keys()
    }
    pub fn scan(&self, cursor: u64) -> (u64, Vec<Bytes>) {
        self.store.scan(cursor)
    }
    pub fn with_ref<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&Value>) -> R,
    {
        self.store.with_ref(key, f)
    }
    /// Also evicts, because this — not `set` — is how RPUSH/HSET/SADD/ZADD grow a value:
    /// accounting the growth (which `Shard::with_mut` now does) without ever acting on it
    /// would leave a pure-collection workload permanently over the ceiling. `Shard::with_mut`
    /// has already released its write lock by the time it returns, so evicting here can't
    /// deadlock against it.
    pub fn with_mut<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&mut Value>) -> R,
    {
        let result = self.store.with_mut(key, f);
        self.maybe_evict();
        result
    }
    pub fn expire_at(&self, key: &[u8], at: Instant) -> bool {
        self.store.expire_at(key, at)
    }
    pub fn persist(&self, key: &[u8]) -> bool {
        self.store.persist(key)
    }
    pub fn ttl(&self, key: &[u8]) -> TtlStatus {
        self.store.ttl(key)
    }
    pub fn active_expire_cycle(&self, shard_idx: usize) -> usize {
        self.store.active_expire_cycle(shard_idx)
    }

    pub fn memory_used(&self) -> usize {
        self.store.memory_used()
    }

    pub fn eviction_count(&self) -> usize {
        self.eviction_count.load(Ordering::Relaxed)
    }

    /// Samples a handful of entries per shard and evicts the one with the oldest recorded
    /// touch, repeating until back under budget or `MAX_EVICTION_ATTEMPTS` is hit — a bounded
    /// loop even if the ceiling is misconfigured smaller than a single entry.
    fn maybe_evict(&self) {
        const MAX_EVICTION_ATTEMPTS: usize = 1000;
        const SAMPLE_PER_SHARD: usize = 5;
        let Some(ceiling) = self.maxmemory else {
            return;
        };
        let mut attempts = 0;
        while self.store.memory_used() > ceiling && attempts < MAX_EVICTION_ATTEMPTS {
            let candidates = self.store.sample_for_eviction(SAMPLE_PER_SHARD);
            let Some((key, _)) = candidates.into_iter().min_by_key(|(_, tick)| *tick) else {
                break; // nothing left to evict
            };
            self.store.del(&key);
            self.eviction_count.fetch_add(1, Ordering::Relaxed);
            attempts += 1;
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine engine::tests`
Expected: PASS, all tests including the 4 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/engine.rs` — do not compose the commit message freeform. Suggested
subject: `feat(engine): add with_maxmemory and approximated-LRU eviction on set`.
