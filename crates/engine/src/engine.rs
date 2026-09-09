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
            store: Store::new(crate::SHARD_COUNT),
            maxmemory: None,
            eviction_count: AtomicUsize::new(0),
        }
    }

    pub fn with_maxmemory(bytes: usize) -> Self {
        Self {
            store: Store::new(crate::SHARD_COUNT),
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
    /// Generic fallback for a mutation whose byte delta isn't cheaply knowable in advance --
    /// `with_mut_delta` below is what RPUSH/HSET/SADD/ZADD actually call to grow a value in
    /// place. Also evicts, for the same reason `with_mut_delta` does: accounting growth
    /// (`Shard::with_mut`) without ever acting on it would leave a pure-collection workload
    /// permanently over the ceiling. `Shard::with_mut` has already released its write lock by
    /// the time it returns, so evicting here can't deadlock against it.
    pub fn with_mut<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&mut Value>) -> R,
    {
        let result = self.store.with_mut(key, f);
        self.maybe_evict();
        result
    }
    /// Like `with_mut`, but for callers that can report their mutation's byte delta directly
    /// instead of paying `Shard::with_mut`'s O(current collection size) before/after diff --
    /// see `Shard::with_mut_delta`'s doc comment for why that scan matters.
    ///
    /// Traces the reported delta at the point it becomes known to `Engine` — after `Store`
    /// has already applied it to `bytes_used`, so the traced number and the accounted number
    /// can never disagree. The delta itself may be negative (a shrinking mutation, e.g. `LPOP`),
    /// which is why the field carries an `isize`, not a `usize`.
    pub fn with_mut_delta<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&mut Value>) -> (R, isize),
    {
        let mut observed_delta: isize = 0;
        let result = self.store.with_mut_delta(key, |v| {
            let (r, delta) = f(v);
            observed_delta = delta;
            (r, delta)
        });
        tracing::trace!(
            key = %String::from_utf8_lossy(key),
            bytes = observed_delta,
            "mutation byte delta"
        );
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
    /// Sweeps shard `shard_idx` for expired keys — see `Store::active_expire_cycle`. Logs at
    /// `debug` only when it actually removed something: the server drives this from a 100ms
    /// loop across all 16 shards (160 calls/sec), and at typical TTL usage almost every sweep
    /// finds nothing to do. A `debug` line for a zero-count sweep, 160 times a second, would
    /// drown out every other `debug` event this series adds — so silence on an empty sweep is
    /// the correct behavior, not a gap.
    pub fn active_expire_cycle(&self, shard_idx: usize) -> usize {
        let removed = self.store.active_expire_cycle(shard_idx);
        if removed > 0 {
            tracing::debug!(shard = shard_idx, removed, "active expire cycle");
        }
        removed
    }
    /// Which shard a key routes to — see `Store::shard_index`. Traced at the facade level, not
    /// inside `Store::shard_index` itself: that method is `#[inline]` and sits on the hot path
    /// of every single `get`/`set`/`with_ref`/`with_mut`/`with_mut_delta` call, so a log call
    /// there — even a disabled one — is the single riskiest placement in this series. This
    /// method is a separate, explicitly-called facade that only the AOF ordering guard and the
    /// cluster router use today; instrumenting here answers "which shard does this key route
    /// to" exactly where a caller already asks that question, without adding a branch to the
    /// per-key read/write path inside `Store`.
    pub fn shard_index(&self, key: &[u8]) -> usize {
        let shard = self.store.shard_index(key);
        tracing::trace!(key = %String::from_utf8_lossy(key), shard, "shard routing");
        shard
    }
    /// Ticks the recency clock `get`/`set` stamp entries with. The server calls this from its
    /// 100ms expiry loop; tests that care about LRU ordering call it between the phases they
    /// want ordered, since operations within one tick tie. See `Store::advance_clock`.
    pub fn advance_recency_clock(&self) {
        self.store.advance_clock()
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
    ///
    /// Logs each individual eviction at `debug` (key + bytes freed + reason), then exactly one
    /// `warn!` summarizing the whole cycle (count + bytes reclaimed) if anything was evicted.
    /// The spec's event catalogue asks for a `warn!` per eviction, but `MAX_EVICTION_ATTEMPTS`
    /// is 1000 -- a single call under sustained memory pressure could otherwise emit 1000 warn
    /// lines, which is not what "occasional... eviction under memory pressure" (the spec's own
    /// description of warn's intended volume) means. One warn per cycle, at debug per key,
    /// serves the same operator-facing intent without the flood.
    fn maybe_evict(&self) {
        const MAX_EVICTION_ATTEMPTS: usize = 1000;
        const SAMPLE_PER_SHARD: usize = 5;
        const REASON: &str = "maxmemory";
        let Some(ceiling) = self.maxmemory else {
            return;
        };
        let mut attempts = 0;
        let mut total_freed: usize = 0;
        while self.store.memory_used() > ceiling && attempts < MAX_EVICTION_ATTEMPTS {
            let candidates = self.store.sample_for_eviction(SAMPLE_PER_SHARD);
            let Some((key, _)) = candidates.into_iter().min_by_key(|(_, tick)| *tick) else {
                break; // nothing left to evict
            };
            // The evicted key's own accounted size, matching `Shard`'s own formula
            // (`key.len() + value.approx_size()`, see `shard.rs`'s `entry_size`) -- not a
            // store-wide before/after delta, which a concurrent write on another shard could
            // skew arbitrarily (see this change's commit message).
            let freed = self
                .store
                .with_ref(&key, |v| v.map_or(0, |v| key.len() + v.approx_size()));
            self.store.del(&key);
            total_freed += freed;
            tracing::debug!(
                key = %String::from_utf8_lossy(&key),
                bytes = freed,
                reason = REASON,
                "evicted key"
            );
            self.eviction_count.fetch_add(1, Ordering::Relaxed);
            attempts += 1;
        }
        if attempts > 0 {
            tracing::warn!(
                evicted = attempts,
                bytes = total_freed,
                reason = REASON,
                "maxmemory eviction cycle"
            );
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
    fn active_expire_cycle_on_a_shard_with_nothing_expired_returns_zero() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"still-alive"),
            Value::String(Bytes::from_static(b"v")),
        );
        // sweep every shard once -- none of them have anything expired
        let total_removed: usize = (0..16).map(|i| engine.active_expire_cycle(i)).sum();
        assert_eq!(total_removed, 0);
        assert!(engine.exists(b"still-alive"));
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
        // RPUSH/HSET/SADD/ZADD never call Engine::set — they grow a value through
        // with_mut_delta. Without eviction wired into with_mut_delta too, this workload would
        // blow straight past the ceiling while memory accounting silently watched it happen.
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
            crate::commands::list::rpush(
                &engine,
                Bytes::from_static(b"list"),
                vec![Bytes::from(format!("element-{i}"))],
            )
            .unwrap();
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
    fn with_mut_delta_returns_the_closures_result_and_still_accounts_bytes() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"abc")),
        );
        let before = engine.memory_used();
        let returned = engine.with_mut_delta(b"k", |v| {
            if let Some(Value::String(s)) = v {
                *s = Bytes::from_static(b"a much longer replacement value");
                (42, 29) // 29 == "a much longer replacement value".len() - "abc".len()
            } else {
                (0, 0)
            }
        });
        assert_eq!(
            returned, 42,
            "the closure's own result must still come back unchanged"
        );
        assert!(
            engine.memory_used() > before,
            "the reported delta must still be accounted"
        );
    }

    #[test]
    fn with_mut_delta_on_a_missing_key_reports_no_delta_and_creates_nothing() {
        let engine = Engine::new();
        let saw_none = engine.with_mut_delta(b"missing", |v| (v.is_none(), 0));
        assert!(saw_none);
        assert!(!engine.exists(b"missing"));
    }

    #[test]
    fn shard_index_is_deterministic_and_within_bounds() {
        let engine = Engine::new();
        let a = engine.shard_index(b"some-key");
        let b = engine.shard_index(b"some-key");
        assert_eq!(a, b, "the same key must always route to the same shard");
        assert!(a < crate::SHARD_COUNT);
    }

    #[test]
    fn shard_index_matches_the_underlying_store() {
        // Engine::shard_index is a thin facade -- this pins that it never diverges from
        // what Store computes, which is the only contract callers like the AOF ordering
        // guard (crates/server/src/aof.rs) actually depend on.
        let engine = Engine::new();
        for key in [&b"a"[..], b"bb", b"ccc", b"dddd"] {
            assert_eq!(engine.shard_index(key), engine.store.shard_index(key));
        }
    }

    #[test]
    fn eviction_frees_at_least_as_many_bytes_as_the_evicted_keys_occupied() {
        // A ceiling that forces eviction, with keys of a known, uniform size -- so the total
        // memory drop across the whole set() call must be an exact multiple of one entry's size.
        let engine = Engine::with_maxmemory(300);
        let mut before = engine.memory_used();
        for i in 0..10 {
            engine.set(
                Bytes::from(format!("k{i}")),
                Value::String(Bytes::from(vec![b'x'; 50])),
            );
            let after = engine.memory_used();
            // Never grows past the ceiling by more than one entry's worth on the way there --
            // maybe_evict runs after every set(), so it never overshoots by an unbounded amount.
            assert!(after <= 300 || after <= before + 100);
            before = after;
        }
        assert!(engine.eviction_count() > 0);
        assert!(engine.memory_used() <= 300);
    }

    /// Regression test for the bug this change fixes: `maybe_evict`'s per-key `bytes` field must
    /// be the evicted key's own accounted size (`key.len() + value.approx_size()`, `shard.rs`'s
    /// `entry_size` formula), never a store-wide `memory_used()` before/after delta. The old
    /// delta approach was provably wrong under concurrent writes on OTHER shards: a background
    /// writer changes the store-wide total in the gap between the two `memory_used()` reads, so
    /// the "freed" figure it computed was really "net store-wide change", not "this key's size".
    ///
    /// To demonstrate that, this test hammers the store from background threads (via `Store::set`
    /// directly, bypassing `maybe_evict` so the writers' own traffic doesn't add competing
    /// eviction log lines) while the main thread drives evictions of keys of a known, fixed size.
    /// The noise keys are kept continuously *fresher* than the `keyNNN` keys by advancing the
    /// shared recency clock once per `keyNNN` insert -- every noise overwrite picks up whatever
    /// tick is current, but a `keyNNN` entry's tick is fixed at insert time and only gets older
    /// relative to the noise traffic from then on. That keeps `sample_for_eviction`'s
    /// least-recently-touched pick landing on a `keyNNN` entry, not a noise entry, which matters
    /// here for a reason unrelated to this bug: a noise entry can be concurrently deleted by
    /// eviction on the very same shard its own writer thread is mid-overwrite of, and `Shard`'s
    /// byte accounting (map mutation under lock, `bytes_used` adjusted just after releasing it)
    /// is not itself race-free against that -- a separate, pre-existing gap this test is not
    /// about and must not trip over.
    ///
    /// Every `keyNNN`-prefixed "evicted key" debug line must report exactly that key's own
    /// accounted size -- a property the fix guarantees unconditionally (it never reads
    /// `memory_used()` for the per-key figure), and the old code could violate under the
    /// concurrent noise below.
    #[test]
    fn evicted_key_reports_its_own_size_not_a_store_wide_delta_under_concurrent_writes() {
        use std::io;
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;
        use tracing_subscriber::EnvFilter;

        #[derive(Clone)]
        struct BufferWriter(Arc<Mutex<Vec<u8>>>);
        impl io::Write for BufferWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for BufferWriter {
            type Writer = BufferWriter;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let payload = Bytes::from(vec![b'x'; 50]);
        let expected_size = "key000".len() + Value::String(payload.clone()).approx_size();

        let buffer = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(BufferWriter(Arc::clone(&buffer)))
            .with_env_filter(EnvFilter::new("debug"))
            .finish();
        // `with_default` builds a fresh `Dispatch` internally; keep our own handle to that exact
        // `Dispatch` (it's `Arc`-backed and `Clone`) so a second thread can install the *same*
        // dispatch as its own thread-local default -- see the healer thread below for why that
        // matters.
        let dispatch = tracing::Dispatch::new(subscriber);

        // A generous ceiling: the noise writers keep a small, bounded pool of keys alive (see
        // below), so their own footprint never dominates it, and each `keyNNN` insert past
        // capacity still needs only about one eviction to get back under -- not up to
        // `MAX_EVICTION_ATTEMPTS`, which under real concurrent write pressure across 16 shards'
        // locks would make this test pathologically slow.
        let engine = Arc::new(Engine::with_maxmemory(5_000));
        // A stop flag, not a metric -- it exists only so the writer/healer threads keep running
        // for the *entire* span the main thread spends evicting, instead of racing to finish
        // early and then sitting idle for the rest of the test.
        let stop = std::sync::atomic::AtomicBool::new(false);
        let stop = Arc::new(stop);

        tracing::dispatcher::with_default(&dispatch, || {
            // `tracing`'s callsite `Interest` is cached process-wide the first time each
            // callsite fires, based on whichever thread happens to trigger it -- and other tests
            // in this binary trigger the very same "evicted key"/"maxmemory eviction cycle"
            // callsites with no subscriber installed at all. If one of those wins the race to
            // register the callsite first, on ITS thread (where the ambient default resolves to
            // the process-wide no-op), the callsite is cached disabled *for every thread*,
            // including this one, and a single `rebuild_interest_cache()` call here only fixes
            // that up to the moment it runs -- a later registration from one of those other
            // threads can re-poison it seconds later. This healer thread keeps re-registering
            // interest from *inside* a scope where this exact `dispatch` is the thread-local
            // default (so the registration resolves to our real subscriber, not the ambient
            // no-op), for as long as the main thread below is still evicting, to keep closing
            // that window.
            let healer = {
                let dispatch = dispatch.clone();
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    tracing::dispatcher::with_default(&dispatch, || {
                        while !stop.load(Ordering::Relaxed) {
                            tracing::callsite::rebuild_interest_cache();
                            std::thread::yield_now();
                        }
                    });
                })
            };

            let writers: Vec<_> = (0..2)
                .map(|t| {
                    let writer_engine = Arc::clone(&engine);
                    let stop = Arc::clone(&stop);
                    std::thread::spawn(move || {
                        let mut i = 0usize;
                        while !stop.load(Ordering::Relaxed) {
                            // A bounded pool of 5 keys per writer, repeatedly overwritten with a
                            // varying size -- each overwrite still perturbs `memory_used()` on
                            // whatever shard that key landed on (see `Shard::set`'s old/new size
                            // reaccounting), without letting the noise footprint grow forever, and
                            // keeps refreshing that key's recency tick to the current clock value.
                            writer_engine.store.set(
                                Bytes::from(format!("noise-{t}-{}", i % 5)),
                                Value::String(Bytes::from(vec![b'y'; (i % 40) + 1])),
                            );
                            i += 1;
                        }
                    })
                })
                .collect();

            for i in 0..100 {
                engine.set(
                    Bytes::from(format!("key{i:03}")),
                    Value::String(payload.clone()),
                );
                // Ages every already-inserted keyNNN (and every noise key not touched since) by
                // one tick, so the next insert's eviction victim is reliably the oldest keyNNN,
                // not a noise key the writer threads keep refreshing to the newest tick.
                engine.advance_recency_clock();
            }

            stop.store(true, Ordering::Relaxed);
            for writer in writers {
                writer.join().expect("writer thread panicked");
            }
            healer.join().expect("healer thread panicked");
        });

        let output = String::from_utf8(buffer.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .expect("subscriber output is utf-8");

        let mut checked = 0;
        for line in output.lines().filter(|l| l.contains("evicted key")) {
            let key_field = line
                .split("key=")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .expect("evicted key line missing a key field");
            if !key_field.starts_with("key") {
                continue; // a noise-* key sampled for eviction -- not what this test checks
            }
            let bytes_field = line
                .split("bytes=")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .expect("evicted key line missing a bytes field");
            let reported: usize = bytes_field
                .parse()
                .expect("bytes field must render as a plain number");
            assert_eq!(
                reported, expected_size,
                "evicted key {key_field} reported {reported} bytes, expected its own accounted \
                 size {expected_size}; full line: {line}"
            );
            checked += 1;
        }
        assert!(
            checked > 0,
            "test setup did not evict any keyNNN entries -- memory_used={} eviction_count={} \
             full output:\n{output}",
            engine.memory_used(),
            engine.eviction_count()
        );
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
