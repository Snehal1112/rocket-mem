# Snapshot Serialization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the `engine` crate can turn its entire keyspace into a `Vec<u8>` blob and back, with TTLs surviving as wall-clock time — the foundation both hybrid recovery and replication's full-resync build on.

**Architecture:** a new `crates/engine/src/snapshot.rs` module owns `bincode` encoding via a private `SerializableValue`/`SerializableEntry` mirror of `Value`, called only from two new thin `Engine` methods (`snapshot`/`load_snapshot`). Two new `Shard` methods (`entries`/`clear`) are the only new surface exposed past `Shard`'s existing privacy boundary — `Entry` itself, and `Shard::map`, stay private. Two new `Store` methods walk/replace all 16 shards. `common` gains a wall-clock↔monotonic conversion pair, relocated (not duplicated) from `dispatcher.rs`.

**Tech Stack:** `serde`/`bincode` (new), `thiserror` (already a workspace dependency, newly used by `engine`).

**Spec:** `../../specs/2026-08-30-sprint-5-spec.md` — "snapshot serialization uses `bincode`..." and "`Shard` gains `entries`/`clear`..." decisions are authoritative for this plan.

## Global Constraints

- No format-version byte or magic number in the snapshot payload — one version exists, per the spec's explicit decision not to add one preemptively.
- `serde = { version = "1", features = ["derive"] }` and `bincode = "1"` join `[workspace.dependencies]` in the root `Cargo.toml` and are added to `crates/engine/Cargo.toml`. `bytes` gains a `serde` feature in `[workspace.dependencies]`. `thiserror` (already workspace-declared) is added to `crates/engine/Cargo.toml`. No new dependency lands in `common`, `protocol`, or `server`.
- `common` currently depends only on `thiserror` (see `crates/common/Cargo.toml`) — the new wall-clock helpers must not add anything to that.

---

### Task 1: relocate the `Instant`↔Unix-ms conversion into `common`

**Files:**
- Modify: `crates/common/src/lib.rs`
- Modify: `crates/server/src/dispatcher.rs:42-49` (the existing private `instant_from_unix_ms`, and its one call site at line 805)

**Interfaces:**
- Consumes: nothing from other tasks in this plan.
- Produces: `common::instant_from_unix_ms(i64) -> std::time::Instant` and `common::unix_ms_from_instant(std::time::Instant) -> i64`, both `pub`, both used by Task 4 below and (unchanged in behavior) by `dispatcher.rs`'s existing `EXPIREAT`/`PEXPIREAT` arm.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/common/src/lib.rs — add to the existing tests module
#[test]
fn instant_from_unix_ms_of_a_future_timestamp_is_in_the_future() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let future_ms = (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64) + 60_000;
    assert!(instant_from_unix_ms(future_ms) > std::time::Instant::now());
}

#[test]
fn unix_ms_from_instant_round_trips_through_instant_from_unix_ms() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
    let target_ms = now_ms + 5_000;
    let at = instant_from_unix_ms(target_ms);
    let round_tripped = unix_ms_from_instant(at);
    // millisecond rounding through two clock reads on each side can drift a few ms
    assert!((round_tripped - target_ms).abs() < 50, "round-tripped to {round_tripped}, expected near {target_ms}");
}

#[test]
fn unix_ms_from_instant_of_a_past_instant_is_less_than_now() {
    let past = std::time::Instant::now() - std::time::Duration::from_secs(10);
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
    assert!(unix_ms_from_instant(past) < now_ms);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p common`
Expected: FAIL with "cannot find function `instant_from_unix_ms`/`unix_ms_from_instant` in this scope"

- [ ] **Step 3: Move `instant_from_unix_ms` and add its inverse**

Delete the existing private function from `crates/server/src/dispatcher.rs:42-49` entirely, then add both functions to `crates/common/src/lib.rs`, above the existing `EngineError` enum:

```rust
// crates/common/src/lib.rs
/// Converts an absolute Unix-millisecond timestamp into a monotonic `Instant` this process's
/// clock can compare against. A target already in the past collapses to "right now" (the
/// delta saturates to zero), so an already-elapsed expiry takes effect on the very next
/// passive-expiry check rather than needing special-casing.
pub fn instant_from_unix_ms(target_unix_ms: i64) -> std::time::Instant {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let target = UNIX_EPOCH + Duration::from_millis(target_unix_ms.max(0) as u64);
    let delta = target
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO);
    std::time::Instant::now() + delta
}

/// The inverse of `instant_from_unix_ms`: converts a monotonic `Instant` (which has no defined
/// relationship to wall-clock time on its own) into an absolute Unix-millisecond timestamp, by
/// measuring `at`'s offset from *now* on both clocks and applying that offset to the wall clock.
/// Uses `saturating_duration_since` on both sides (never panics, unlike plain `Instant` subtraction
/// on some historical Rust versions) so a caller never needs to reason about which `Instant` is
/// later.
pub fn unix_ms_from_instant(at: std::time::Instant) -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_instant = std::time::Instant::now();
    let now_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_millis() as i64;
    if at >= now_instant {
        now_unix_ms + at.saturating_duration_since(now_instant).as_millis() as i64
    } else {
        now_unix_ms - now_instant.saturating_duration_since(at).as_millis() as i64
    }
}
```

Then in `dispatcher.rs`, replace the one call site (originally at line 805, inside the `EXPIREAT`/`PEXPIREAT` dispatcher arm):

```rust
// crates/server/src/dispatcher.rs — was: instant_from_unix_ms(target_unix_ms)
match engine.expire_at(&rest[0], common::instant_from_unix_ms(target_unix_ms)) {
```

`dispatcher.rs` already has `common` in scope as a dependency (see `crates/server/Cargo.toml`), so no new `Cargo.toml` edit is needed for this step.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p common && cargo test -p rocket-mem`
Expected: PASS, including every existing `EXPIREAT`/`PEXPIREAT` dispatcher test (unchanged behavior, just relocated)

- [ ] **Step 5: Commit**

```bash
git add crates/common/src/lib.rs crates/server/src/dispatcher.rs
git commit -m "refactor(common): move Instant<->unix-ms conversion out of dispatcher"
```

---

### Task 2: `Shard::entries`/`Shard::clear`

**Files:**
- Modify: `crates/engine/src/shard.rs`

**Interfaces:**
- Consumes: `Entry` (private, unchanged), `entry_size` (private, unchanged).
- Produces: `Shard::entries(&self) -> Vec<(Bytes, Value, Option<Instant>)>` and `Shard::clear(&self)`, both `pub`, both used by Task 3.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/shard.rs — add to the existing tests module
#[test]
fn entries_returns_every_unexpired_key_value_and_expiry() {
    let shard = Shard::new();
    let clock = AtomicU64::new(0);
    shard.set(Bytes::from_static(b"a"), Value::String(Bytes::from_static(b"1")), &clock);
    shard.set(Bytes::from_static(b"b"), Value::String(Bytes::from_static(b"2")), &clock);
    let at = Instant::now() + std::time::Duration::from_secs(60);
    shard.expire_at(b"b", at);

    let mut got = shard.entries();
    got.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(got[0], (Bytes::from_static(b"a"), Value::String(Bytes::from_static(b"1")), None));
    assert_eq!(got[1].0, Bytes::from_static(b"b"));
    assert_eq!(got[1].1, Value::String(Bytes::from_static(b"2")));
    assert_eq!(got[1].2, Some(at));
}

#[test]
fn entries_excludes_expired_keys() {
    let shard = Shard::new();
    let clock = AtomicU64::new(0);
    shard.set(Bytes::from_static(b"a"), Value::String(Bytes::from_static(b"1")), &clock);
    shard.expire_at(b"a", Instant::now() - std::time::Duration::from_secs(1));
    assert!(shard.entries().is_empty());
}

#[test]
fn clear_empties_the_map_and_resets_bytes_used() {
    let shard = Shard::new();
    let clock = AtomicU64::new(0);
    shard.set(Bytes::from_static(b"a"), Value::String(Bytes::from_static(b"1")), &clock);
    assert!(shard.bytes_used() > 0);
    shard.clear();
    assert_eq!(shard.bytes_used(), 0);
    assert!(shard.entries().is_empty());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p engine shard::tests`
Expected: FAIL with "no method named `entries`/`clear` found for struct `Shard`"

- [ ] **Step 3: Implement `entries`/`clear`**

Add both methods to `crates/engine/src/shard.rs`, next to the existing `keys` method:

```rust
// crates/engine/src/shard.rs
/// A full, point-in-time (per this one shard) projection of every unexpired entry — the
/// building block `Store::snapshot_entries` flat-maps across all 16 shards. `Entry` itself
/// never escapes this module; only this `(key, value, expiry)` tuple does.
pub fn entries(&self) -> Vec<(Bytes, Value, Option<Instant>)> {
    self.map
        .read()
        .iter()
        .filter(|(_, entry)| !entry.is_expired())
        .map(|(k, entry)| (k.clone(), entry.value.clone(), entry.expires_at))
        .collect()
}

/// Empties this shard entirely and resets its byte accounting to zero — used by
/// `Store::load_snapshot_entries` to fully replace a shard's contents rather than merge
/// into them. `bytes_used` must be reset here, not left for the caller to reconcile,
/// or `MAXMEMORY` accounting would silently overcount every key this call removed.
pub fn clear(&self) {
    self.map.write().clear();
    self.bytes_used.store(0, Ordering::Relaxed);
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p engine shard::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/shard.rs
git commit -m "feat(engine): add Shard::entries and Shard::clear"
```

---

### Task 3: `Store::snapshot_entries`/`Store::load_snapshot_entries`

**Files:**
- Modify: `crates/engine/src/store.rs`

**Interfaces:**
- Consumes: `Shard::entries`/`Shard::clear` (Task 2), `Store::set`/`Store::expire_at` (existing).
- Produces: `Store::snapshot_entries(&self) -> Vec<(Bytes, Value, Option<Instant>)>` and `Store::load_snapshot_entries(&self, entries: Vec<(Bytes, Value, Option<Instant>)>)`, both `pub`, both used by Task 4.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/store.rs — add to the existing tests module
#[test]
fn snapshot_entries_collects_keys_from_every_shard() {
    let store = Store::new(16);
    // enough distinct keys that DefaultHasher spreads them across multiple shards
    for i in 0..50 {
        store.set(Bytes::from(format!("k{i}")), Value::String(Bytes::from_static(b"v")));
    }
    assert_eq!(store.snapshot_entries().len(), 50);
}

#[test]
fn load_snapshot_entries_replaces_existing_state_wholesale() {
    let store = Store::new(16);
    store.set(Bytes::from_static(b"stale"), Value::String(Bytes::from_static(b"old")));

    let at = std::time::Instant::now() + std::time::Duration::from_secs(60);
    store.load_snapshot_entries(vec![
        (Bytes::from_static(b"fresh"), Value::String(Bytes::from_static(b"new")), Some(at)),
    ]);

    assert_eq!(store.get(b"stale"), None);
    assert_eq!(store.get(b"fresh"), Some(Value::String(Bytes::from_static(b"new"))));
    let engine::TtlStatus::Remaining(remaining) = store.ttl(b"fresh") else {
        panic!("expected a TTL on the loaded key")
    };
    assert!(remaining.as_secs() > 0 && remaining.as_secs() <= 60);
}
```

Note: `store.rs`'s existing tests module already imports `engine::TtlStatus` indirectly via `crate::engine::TtlStatus` — use whichever path the file's existing tests already use (check the top of `store.rs`'s `#[cfg(test)] mod tests` block before writing this).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p engine store::tests`
Expected: FAIL with "no method named `snapshot_entries`/`load_snapshot_entries` found for struct `Store`"

- [ ] **Step 3: Implement `snapshot_entries`/`load_snapshot_entries`**

Add both methods to `crates/engine/src/store.rs`, next to the existing `keys` method:

```rust
// crates/engine/src/store.rs
/// Flat-maps `Shard::entries()` across all 16 shards, each locked and released in turn —
/// this is NOT a whole-store point-in-time view on its own. A caller that needs one (e.g.
/// `SAVE`, coordinating this with an AOF offset) must hold its own external lock across the
/// call; see `../../specs/2026-08-30-sprint-5-spec.md`'s SAVE atomicity decision.
pub fn snapshot_entries(&self) -> Vec<(Bytes, Value, Option<std::time::Instant>)> {
    self.shards.iter().flat_map(|s| s.entries()).collect()
}

/// Replaces every shard's contents wholesale: clears all 16 first, then re-inserts each
/// entry via the existing `set`/`expire_at` paths (which re-account `bytes_used` correctly,
/// so no separate accounting step is needed here).
pub fn load_snapshot_entries(&self, entries: Vec<(Bytes, Value, Option<std::time::Instant>)>) {
    for shard in &self.shards {
        shard.clear();
    }
    for (key, value, expires_at) in entries {
        self.set(key.clone(), value);
        if let Some(at) = expires_at {
            self.expire_at(&key, at);
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p engine store::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/store.rs
git commit -m "feat(engine): add Store::snapshot_entries and load_snapshot_entries"
```

---

### Task 4: `snapshot.rs` — `SerializableValue`, `SerializableEntry`, `SnapshotError`, `serialize`/`deserialize`

**Files:**
- Create: `crates/engine/src/snapshot.rs`
- Modify: `crates/engine/src/lib.rs`
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/engine/Cargo.toml`

**Interfaces:**
- Consumes: `common::instant_from_unix_ms`/`unix_ms_from_instant` (Task 1), `Store::snapshot_entries`/`load_snapshot_entries` (Task 3), `Value`/`SortedSet` (existing, `crates/engine/src/value.rs`).
- Produces: `snapshot::serialize(store: &Store, aof_offset: u64) -> Vec<u8>`, `snapshot::deserialize(store: &Store, bytes: &[u8]) -> Result<u64, SnapshotError>`, both crate-private (`pub(crate)` visibility is implied by `snapshot` staying a non-`pub` module — see Task 5), used by Task 5. `SnapshotError` is `pub` and re-exported from `lib.rs` in Task 5.

- [ ] **Step 1: Add the new dependencies**

```toml
# Cargo.toml (workspace root) — add to [workspace.dependencies]
serde = { version = "1", features = ["derive"] }
bincode = "1"
```

Change the existing `bytes` line in the same `[workspace.dependencies]` block:

```toml
bytes = { version = "1", features = ["serde"] }
```

```toml
# crates/engine/Cargo.toml — add to [dependencies]
serde.workspace = true
bincode.workspace = true
thiserror.workspace = true
```

- [ ] **Step 2: Write the failing tests**

```rust
// crates/engine/src/snapshot.rs — new file, tests module at the bottom
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use bytes::Bytes;
    use std::time::{Duration, Instant};

    #[test]
    fn serialize_then_deserialize_round_trips_a_string_value() {
        let store = Store::new(16);
        store.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
        let bytes = serialize(&store, 42);

        let store2 = Store::new(16);
        let offset = deserialize(&store2, &bytes).unwrap();
        assert_eq!(offset, 42);
        assert_eq!(store2.get(b"k"), Some(Value::String(Bytes::from_static(b"v"))));
    }

    #[test]
    fn round_trips_every_value_type_including_a_sorted_set() {
        use crate::value::SortedSet;
        let store = Store::new(16);
        store.set(Bytes::from_static(b"s"), Value::String(Bytes::from_static(b"v")));
        store.set(Bytes::from_static(b"l"), Value::List(std::collections::VecDeque::from([Bytes::from_static(b"a")])));
        store.set(Bytes::from_static(b"h"), Value::Hash(std::collections::HashMap::from([(Bytes::from_static(b"f"), Bytes::from_static(b"v"))])));
        store.set(Bytes::from_static(b"set"), Value::Set(std::collections::HashSet::from([Bytes::from_static(b"m")])));
        let mut z = SortedSet::new();
        z.insert(Bytes::from_static(b"alice"), 5.0);
        z.insert(Bytes::from_static(b"bob"), 2.0);
        store.set(Bytes::from_static(b"z"), Value::SortedSet(z));

        let bytes = serialize(&store, 0);
        let store2 = Store::new(16);
        deserialize(&store2, &bytes).unwrap();

        assert_eq!(store2.get(b"s"), Some(Value::String(Bytes::from_static(b"v"))));
        assert_eq!(store2.get(b"l"), Some(Value::List(std::collections::VecDeque::from([Bytes::from_static(b"a")]))));
        assert_eq!(store2.get(b"h"), Some(Value::Hash(std::collections::HashMap::from([(Bytes::from_static(b"f"), Bytes::from_static(b"v"))]))));
        assert_eq!(store2.get(b"set"), Some(Value::Set(std::collections::HashSet::from([Bytes::from_static(b"m")]))));
        let Some(Value::SortedSet(z2)) = store2.get(b"z") else { panic!("expected a sorted set") };
        assert_eq!(z2.score(b"alice"), Some(5.0));
        assert_eq!(z2.score(b"bob"), Some(2.0));
    }

    #[test]
    fn a_future_expiry_survives_the_round_trip_as_a_similar_remaining_duration() {
        let store = Store::new(16);
        store.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
        store.expire_at(b"k", Instant::now() + Duration::from_secs(3600));
        let bytes = serialize(&store, 0);

        let store2 = Store::new(16);
        deserialize(&store2, &bytes).unwrap();
        let crate::engine::TtlStatus::Remaining(remaining) = store2.ttl(b"k") else {
            panic!("expected the loaded key to carry a TTL")
        };
        assert!(remaining.as_secs() > 3500 && remaining.as_secs() <= 3600);
    }

    #[test]
    fn an_already_past_expiry_is_dropped_at_load_time_not_round_tripped() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let store = Store::new(16);
        store.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
        // bypass the normal expire_at path (which rejects an already-past Instant on some
        // internal paths) by encoding an already-past unix-ms timestamp directly
        let past_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64 - 60_000;
        let entries = vec![SerializableEntry {
            key: Bytes::from_static(b"k"),
            value: SerializableValue::from(&Value::String(Bytes::from_static(b"v"))),
            expires_at_unix_ms: Some(past_ms),
        }];
        let payload = bincode::serialize(&entries).unwrap();
        let mut bytes = 0u64.to_le_bytes().to_vec();
        bytes.extend_from_slice(&payload);

        let store2 = Store::new(16);
        deserialize(&store2, &bytes).unwrap();
        assert_eq!(store2.get(b"k"), None);
    }

    #[test]
    fn deserialize_on_fewer_than_eight_bytes_is_too_short() {
        let store = Store::new(16);
        assert!(matches!(deserialize(&store, &[1, 2, 3]), Err(SnapshotError::TooShort)));
    }

    #[test]
    fn deserialize_on_garbage_payload_bytes_is_a_decode_error() {
        let store = Store::new(16);
        let mut bytes = 0u64.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0xFF; 16]); // not a valid bincode-encoded Vec<SerializableEntry>
        assert!(matches!(deserialize(&store, &bytes), Err(SnapshotError::Decode(_))));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p engine snapshot::tests`
Expected: FAIL with "module `snapshot` doesn't exist" — `crates/engine/src/snapshot.rs` isn't declared in `lib.rs` yet; the next step creates the file, and Task 5 wires the module declaration. For this step only, temporarily add `mod snapshot;` to the top of `crates/engine/src/lib.rs` so the test file compiles in isolation — Task 5's Step 3 replaces this with the real, permanent module declaration plus the `SnapshotError` re-export, so don't commit this temporary line.

- [ ] **Step 3: Implement `snapshot.rs`**

```rust
// crates/engine/src/snapshot.rs
use crate::store::Store;
use crate::value::{SortedSet, Value};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize)]
enum SerializableValue {
    String(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<Bytes, Bytes>),
    Set(HashSet<Bytes>),
    SortedSet(Vec<(Bytes, f64)>), // (member, score) pairs; BTreeSet order is rebuilt on load
}

impl From<&Value> for SerializableValue {
    fn from(v: &Value) -> Self {
        match v {
            Value::String(b) => SerializableValue::String(b.clone()),
            Value::List(l) => SerializableValue::List(l.clone()),
            Value::Hash(m) => SerializableValue::Hash(m.clone()),
            Value::Set(s) => SerializableValue::Set(s.clone()),
            Value::SortedSet(z) => SerializableValue::SortedSet(
                z.members_ascending()
                    .map(|m| (m.clone(), z.score(m).expect("member came from members_ascending")))
                    .collect(),
            ),
        }
    }
}

impl From<SerializableValue> for Value {
    fn from(v: SerializableValue) -> Self {
        match v {
            SerializableValue::String(b) => Value::String(b),
            SerializableValue::List(l) => Value::List(l),
            SerializableValue::Hash(m) => Value::Hash(m),
            SerializableValue::Set(s) => Value::Set(s),
            SerializableValue::SortedSet(pairs) => {
                let mut z = SortedSet::new();
                for (member, score) in pairs {
                    z.insert(member, score);
                }
                Value::SortedSet(z)
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SerializableEntry {
    key: Bytes,
    value: SerializableValue,
    expires_at_unix_ms: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("snapshot file is too short to contain a valid header")]
    TooShort,
    #[error("failed to decode snapshot payload: {0}")]
    Decode(String),
}

/// `aof_offset` is written into the blob's 8-byte little-endian header — the caller (holding
/// `AofWriter::lock_for_ordering()`, per the sprint-5 spec's SAVE atomicity decision) is the
/// only one who knows the AOF's current durable length, so it's passed in rather than
/// discovered here. Pass `0` when there's no AOF to correlate against (a follower's `PSYNC`
/// reply, which discards the offset on the receiving end anyway).
pub fn serialize(store: &Store, aof_offset: u64) -> Vec<u8> {
    let entries: Vec<SerializableEntry> = store
        .snapshot_entries()
        .into_iter()
        .map(|(key, value, expires_at)| SerializableEntry {
            key,
            value: SerializableValue::from(&value),
            expires_at_unix_ms: expires_at.map(common::unix_ms_from_instant),
        })
        .collect();
    let payload = bincode::serialize(&entries).expect("SerializableEntry always serializes");
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&aof_offset.to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Replaces `store`'s entire contents with what's encoded in `bytes`, returning the AOF
/// offset from the blob's header. An entry whose `expires_at_unix_ms` is already in the past
/// (compared directly as wall-clock milliseconds, not via a round trip through `Instant` —
/// see the sprint-5 spec for why that distinction matters) is dropped rather than loaded and
/// left for the expiry reaper to clean up later.
pub fn deserialize(store: &Store, bytes: &[u8]) -> Result<u64, SnapshotError> {
    if bytes.len() < 8 {
        return Err(SnapshotError::TooShort);
    }
    let mut offset_bytes = [0u8; 8];
    offset_bytes.copy_from_slice(&bytes[..8]);
    let aof_offset = u64::from_le_bytes(offset_bytes);

    let entries: Vec<SerializableEntry> =
        bincode::deserialize(&bytes[8..]).map_err(|e| SnapshotError::Decode(e.to_string()))?;

    let now_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_millis() as i64;

    let loaded: Vec<(Bytes, Value, Option<std::time::Instant>)> = entries
        .into_iter()
        .filter_map(|e| {
            let expires_at = match e.expires_at_unix_ms {
                None => None,
                Some(ms) if ms <= now_unix_ms => return None, // already expired -- drop, don't load
                Some(ms) => Some(common::instant_from_unix_ms(ms)),
            };
            Some((e.key, Value::from(e.value), expires_at))
        })
        .collect();

    store.load_snapshot_entries(loaded);
    Ok(aof_offset)
}
```

`engine` doesn't yet depend on `common` — check `crates/engine/Cargo.toml`'s `[dependencies]` before this step; if `common` isn't listed, add `common = { path = "../common" }` alongside `serde`/`bincode`/`thiserror` from Step 1 above (it's likely already there, since `EngineError` — from `common` — is `engine`'s existing error type; confirm rather than assume).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p engine snapshot::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/engine/Cargo.toml crates/engine/src/snapshot.rs
git commit -m "feat(engine): add bincode-based snapshot serialization"
```

---

### Task 5: `Engine::snapshot`/`Engine::load_snapshot`, module wiring

**Files:**
- Modify: `crates/engine/src/engine.rs`
- Modify: `crates/engine/src/lib.rs`

**Interfaces:**
- Consumes: `snapshot::serialize`/`deserialize` (Task 4).
- Produces: `Engine::snapshot(&self, aof_offset: u64) -> Vec<u8>` and `Engine::load_snapshot(&self, bytes: &[u8]) -> Result<u64, SnapshotError>`, both `pub`, both consumed by `04-replica-registry-and-leader-fanout.md`'s `SAVE`/`PSYNC` handling and `02-hybrid-recovery-and-aof-offset.md`'s startup path. `SnapshotError` becomes reachable as `engine::SnapshotError`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/engine.rs — add to the existing tests module
#[test]
fn snapshot_then_load_snapshot_round_trips_through_the_engine_facade() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    let bytes = engine.snapshot(7);

    let engine2 = Engine::new();
    let offset = engine2.load_snapshot(&bytes).unwrap();
    assert_eq!(offset, 7);
    assert_eq!(engine2.get(b"k"), Some(Value::String(Bytes::from_static(b"v"))));
}

#[test]
fn load_snapshot_on_garbage_bytes_is_a_snapshot_error_not_a_panic() {
    let engine = Engine::new();
    assert!(engine.load_snapshot(&[1, 2, 3]).is_err());
}

#[test]
fn load_snapshot_bypasses_maxmemory_eviction_so_a_large_snapshot_loads_whole() {
    // load_snapshot_entries goes through Store::set, not Engine::set -- a snapshot larger
    // than the ceiling must land whole, not be silently trimmed on the way in
    let engine = Engine::with_maxmemory(1); // absurdly small ceiling
    let big = Engine::new();
    for i in 0..20 {
        big.set(Bytes::from(format!("k{i}")), Value::String(Bytes::from_static(b"some value")));
    }
    let bytes = big.snapshot(0);
    engine.load_snapshot(&bytes).unwrap();
    assert_eq!(engine.keys().len(), 20);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p engine engine::tests`
Expected: FAIL with "no method named `snapshot`/`load_snapshot` found for struct `Engine`"

- [ ] **Step 3: Implement, and wire the module**

```rust
// crates/engine/src/engine.rs — add to impl Engine
/// A thin facade over `snapshot::serialize`, matching `Engine`'s existing role over `Store`
/// (see `CLAUDE.md`). `aof_offset` is opaque to `Engine` — it's only ever the caller's AOF
/// length, which `Engine` has no access to; see `snapshot::serialize`'s own doc comment.
pub fn snapshot(&self, aof_offset: u64) -> Vec<u8> {
    crate::snapshot::serialize(&self.store, aof_offset)
}

/// A thin facade over `snapshot::deserialize`. Deliberately bypasses `maxmemory` eviction —
/// `load_snapshot_entries` goes through `Store::set`, not `Engine::set` — so a snapshot
/// larger than a configured ceiling lands whole and is only trimmed back under it by the
/// next write that calls `Engine::set`/`with_mut`. Evicting *while* loading would silently
/// discard keys the operator asked to restore, which is never the right behavior for a
/// restore path.
pub fn load_snapshot(&self, bytes: &[u8]) -> Result<u64, crate::snapshot::SnapshotError> {
    crate::snapshot::deserialize(&self.store, bytes)
}
```

```rust
// crates/engine/src/lib.rs — replace the temporary `mod snapshot;` from Task 4 Step 2
pub mod commands;
mod engine;
pub mod glob;
mod shard;
mod snapshot;
mod store;
mod value;
pub use engine::{Engine, TtlStatus};
pub use snapshot::SnapshotError;
pub use store::Store;
pub use value::{SortedSet, Value};
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p engine`
Expected: PASS, every test in the crate (this is the final step of the plan touching `engine`'s public surface, so a full-crate run is the right scope)

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/engine.rs crates/engine/src/lib.rs
git commit -m "feat(engine): add Engine::snapshot and Engine::load_snapshot"
```
