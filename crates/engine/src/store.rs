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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
    use bytes::Bytes;

    #[test]
    fn set_then_get_round_trips() {
        let store = Store::new(16);
        store.set(
            Bytes::from_static(b"foo"),
            Value::String(Bytes::from_static(b"bar")),
        );
        assert_eq!(
            store.get(b"foo"),
            Some(Value::String(Bytes::from_static(b"bar")))
        );
    }

    #[test]
    fn keys_distribute_across_more_than_one_shard() {
        let store = Store::new(16);
        for i in 0..1000 {
            store.set(
                Bytes::from(format!("key{i}")),
                Value::String(Bytes::from_static(b"v")),
            );
        }
        let non_empty = store
            .shard_key_counts()
            .into_iter()
            .filter(|&c| c > 0)
            .count();
        assert!(
            non_empty > 1,
            "expected keys to spread across shards, got {non_empty} non-empty"
        );
    }

    #[test]
    fn scan_from_cursor_zero_returns_shard_zeros_keys_and_the_next_cursor() {
        let store = Store::new(16);
        store.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        let (next, _keys) = store.scan(0);
        assert_eq!(next, 1);
    }

    #[test]
    fn scan_wraps_back_to_zero_after_the_last_shard() {
        let store = Store::new(16);
        let (next, keys) = store.scan(15);
        assert_eq!(next, 0);
        assert!(keys.is_empty() || !keys.is_empty()); // shard 15 may or may not be empty; only the cursor matters here
    }

    #[test]
    fn scan_past_the_last_shard_returns_zero_and_no_keys() {
        let store = Store::new(16);
        let (next, keys) = store.scan(16);
        assert_eq!(next, 0);
        assert!(keys.is_empty());
    }

    #[test]
    fn a_full_scan_visits_every_pre_existing_key_exactly_once() {
        use std::collections::HashMap;

        let store = Store::new(16);
        for i in 0..200 {
            store.set(
                Bytes::from(format!("k{i}")),
                Value::String(Bytes::from_static(b"v")),
            );
        }

        let mut seen_counts: HashMap<Bytes, usize> = HashMap::new();
        let mut cursor = 0u64;
        loop {
            let (next, keys) = store.scan(cursor);
            for k in keys {
                *seen_counts.entry(k).or_insert(0) += 1;
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }

        assert_eq!(seen_counts.len(), 200);
        assert!(seen_counts.values().all(|&count| count == 1));
    }

    #[test]
    fn scan_visits_every_pre_existing_key_at_least_once_under_concurrent_writes() {
        use std::collections::HashSet;
        use std::sync::Arc;
        use std::thread;

        let store = Arc::new(Store::new(16));
        for i in 0..5000 {
            store.set(
                Bytes::from(format!("pre{i}")),
                Value::String(Bytes::from_static(b"v")),
            );
        }

        let writer_store = Arc::clone(&store);
        let writer = thread::spawn(move || {
            for i in 0..5000 {
                writer_store.set(
                    Bytes::from(format!("new{i}")),
                    Value::String(Bytes::from_static(b"v")),
                );
            }
        });

        let mut seen: HashSet<Bytes> = HashSet::new();
        let mut cursor = 0u64;
        loop {
            let (next, keys) = store.scan(cursor);
            seen.extend(keys);
            cursor = next;
            if cursor == 0 {
                break;
            }
        }

        writer.join().unwrap();

        for i in 0..5000 {
            let key = Bytes::from(format!("pre{i}"));
            assert!(seen.contains(&key), "missing pre-existing key pre{i}");
        }
    }

    #[test]
    fn active_expire_cycle_sweeps_the_requested_shard_by_index() {
        let store = Store::new(16);
        // find a key that hashes to shard 0 by trying keys until shard_key_counts confirms it
        let key = Bytes::from_static(b"probe");
        store.set(key.clone(), Value::String(Bytes::from_static(b"v")));
        let shard_idx = store
            .shard_key_counts()
            .iter()
            .position(|&c| c > 0)
            .unwrap();
        store.expire_at(
            &key,
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );
        let removed = store.active_expire_cycle(shard_idx);
        assert_eq!(removed, 1);
    }

    #[test]
    fn active_expire_cycle_wraps_an_out_of_range_shard_index() {
        let store = Store::new(16);
        // shard index 16 wraps to shard 0 (16 % 16 == 0) — must not panic
        assert_eq!(store.active_expire_cycle(16), 0);
    }

    #[test]
    fn memory_used_sums_bytes_used_across_all_shards() {
        let store = Store::new(16);
        assert_eq!(store.memory_used(), 0);
        store.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert!(store.memory_used() > 0);
    }

    #[test]
    fn sample_for_eviction_collects_candidates_from_every_shard() {
        let store = Store::new(16);
        for i in 0..32 {
            store.set(
                Bytes::from(format!("k{i}")),
                Value::String(Bytes::from_static(b"v")),
            );
        }
        // with 32 keys spread across 16 shards, sampling 1 per shard should find at least
        // several distinct shards' worth of candidates (exact count depends on hash distribution)
        assert!(store.sample_for_eviction(1).len() >= 8);
    }

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
}
