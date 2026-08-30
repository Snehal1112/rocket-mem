use crate::{store::Store, Value};
use bytes::Bytes;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtlStatus {
    NoSuchKey,
    NoExpiry,
    Remaining(Duration),
}

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

    pub fn memory_used(&self) -> usize {
        self.store.memory_used()
    }

    /// The configured `MAXMEMORY` ceiling, if any. `INFO`'s memory section reports it; note the
    /// shipped binary always answers `None`, because `main.rs` builds its `Engine` through
    /// `aof::recover`, which calls `Engine::new()`. Wiring a `ROCKET_MEM_MAXMEMORY` env var is
    /// deliberately out of this sprint's scope; the gap is recorded in the README.
    pub fn maxmemory(&self) -> Option<usize> {
        self.maxmemory
    }

    /// `(live keys, of which carry an expiry)`. A thin facade over `Store`, matching `Engine`'s
    /// established role. Feeds the `rocket_mem_keys` gauges and `INFO`'s keyspace section.
    pub fn key_counts(&self) -> (usize, usize) {
        self.store.key_counts()
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

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
    use bytes::Bytes;
    use std::time::{Duration, Instant};

    #[test]
    fn maxmemory_reports_the_configured_ceiling_or_none() {
        assert_eq!(Engine::new().maxmemory(), None);
        assert_eq!(Engine::with_maxmemory(4_096).maxmemory(), Some(4_096));
    }

    #[test]
    fn ttl_on_a_missing_key_is_no_such_key() {
        let engine = Engine::new();
        assert_eq!(engine.ttl(b"missing"), TtlStatus::NoSuchKey);
    }

    #[test]
    fn ttl_on_a_key_with_no_expiry_is_no_expiry() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert_eq!(engine.ttl(b"k"), TtlStatus::NoExpiry);
    }

    #[test]
    fn ttl_on_a_key_with_a_future_expiry_reports_remaining_time() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        engine.expire_at(b"k", Instant::now() + Duration::from_secs(60));
        match engine.ttl(b"k") {
            TtlStatus::Remaining(d) => {
                assert!(d <= Duration::from_secs(60) && d > Duration::from_secs(55))
            }
            other => panic!("expected Remaining, got {other:?}"),
        }
    }

    #[test]
    fn expire_at_and_persist_round_trip_through_engine() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert!(engine.expire_at(b"k", Instant::now() + Duration::from_secs(60)));
        assert!(engine.persist(b"k"));
        assert_eq!(engine.ttl(b"k"), TtlStatus::NoExpiry);
    }

    #[test]
    fn engine_get_set_del_exists_round_trip() {
        let engine = Engine::new();
        assert!(!engine.exists(b"foo"));
        engine.set(
            Bytes::from_static(b"foo"),
            Value::String(Bytes::from_static(b"bar")),
        );
        assert!(engine.exists(b"foo"));
        assert_eq!(
            engine.get(b"foo"),
            Some(Value::String(Bytes::from_static(b"bar")))
        );
        assert!(engine.del(b"foo"));
        assert!(!engine.exists(b"foo"));
    }

    #[test]
    fn with_ref_sees_none_for_a_missing_key() {
        let engine = Engine::new();
        assert!(engine.with_ref(b"missing", |v| v.is_none()));
    }

    #[test]
    fn with_ref_borrows_the_stored_value_without_cloning_it_out() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert_eq!(
            engine.with_ref(b"k", |v| v.cloned()),
            Some(Value::String(Bytes::from_static(b"v")))
        );
    }

    #[test]
    fn with_mut_mutates_the_stored_value_in_place() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        engine.with_mut(b"k", |v| {
            if let Some(Value::String(s)) = v {
                *s = Bytes::from_static(b"updated");
            }
        });
        assert_eq!(
            engine.get(b"k"),
            Some(Value::String(Bytes::from_static(b"updated")))
        );
    }

    #[test]
    fn with_mut_sees_none_for_a_missing_key_and_does_not_create_it() {
        let engine = Engine::new();
        assert!(engine.with_mut(b"missing", |v| v.is_none()));
        assert!(!engine.exists(b"missing"));
    }

    #[test]
    fn active_expire_cycle_removes_expired_keys_in_the_targeted_shard() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        engine.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        // sweep every shard once — the key's shard is wherever it landed
        let total_removed: usize = (0..16).map(|i| engine.active_expire_cycle(i)).sum();
        assert_eq!(total_removed, 1);
    }

    #[test]
    fn keys_returns_every_key_that_was_set() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"a"),
            Value::String(Bytes::from_static(b"1")),
        );
        engine.set(
            Bytes::from_static(b"b"),
            Value::String(Bytes::from_static(b"2")),
        );
        let mut keys = engine.keys();
        keys.sort();
        assert_eq!(
            keys,
            vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]
        );
    }

    #[test]
    fn new_engine_has_no_memory_ceiling_and_never_evicts() {
        let engine = Engine::new();
        for i in 0..1000 {
            engine.set(
                Bytes::from(format!("k{i}")),
                Value::String(Bytes::from(vec![b'x'; 100])),
            );
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
        engine.set(
            Bytes::from_static(b"old"),
            Value::String(Bytes::from(vec![b'x'; 50])),
        );
        engine.set(
            Bytes::from_static(b"middle"),
            Value::String(Bytes::from(vec![b'x'; 50])),
        );
        engine.get(b"old"); // touch "old" so it's fresher than "middle" going into the next set
        engine.set(
            Bytes::from_static(b"new"),
            Value::String(Bytes::from(vec![b'x'; 50])),
        );
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

    #[test]
    fn snapshot_then_load_snapshot_round_trips_through_the_engine_facade() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        let bytes = engine.snapshot(7);

        let engine2 = Engine::new();
        let offset = engine2.load_snapshot(&bytes).unwrap();
        assert_eq!(offset, 7);
        assert_eq!(
            engine2.get(b"k"),
            Some(Value::String(Bytes::from_static(b"v")))
        );
    }

    #[test]
    fn load_snapshot_on_garbage_bytes_is_a_snapshot_error_not_a_panic() {
        let engine = Engine::new();
        assert!(engine.load_snapshot(&[1, 2, 3]).is_err());
    }

    #[test]
    fn key_counts_reports_live_keys_and_how_many_have_an_expiry() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"a"),
            Value::String(Bytes::from_static(b"1")),
        );
        engine.set(
            Bytes::from_static(b"b"),
            Value::String(Bytes::from_static(b"2")),
        );
        engine.expire_at(
            b"b",
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        assert_eq!(engine.key_counts(), (2, 1));
    }

    #[test]
    fn key_counts_ignores_already_expired_keys() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"gone"),
            Value::String(Bytes::from_static(b"1")),
        );
        engine.expire_at(
            b"gone",
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );
        assert_eq!(engine.key_counts(), (0, 0));
    }

    #[test]
    fn load_snapshot_bypasses_maxmemory_eviction_so_a_large_snapshot_loads_whole() {
        // load_snapshot_entries goes through Store::set, not Engine::set -- a snapshot larger
        // than the ceiling must land whole, not be silently trimmed on the way in
        let engine = Engine::with_maxmemory(1); // absurdly small ceiling
        let big = Engine::new();
        for i in 0..20 {
            big.set(
                Bytes::from(format!("k{i}")),
                Value::String(Bytes::from_static(b"some value")),
            );
        }
        let bytes = big.snapshot(0);
        engine.load_snapshot(&bytes).unwrap();
        assert_eq!(engine.keys().len(), 20);
    }
}
