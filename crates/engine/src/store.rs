use crate::{shard::Shard, Value};
use bytes::Bytes;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub struct Store {
    shards: Vec<Shard>,
}

impl Store {
    pub fn new(shard_count: usize) -> Self {
        Self {
            shards: (0..shard_count).map(|_| Shard::new()).collect(),
        }
    }

    fn shard_for(&self, key: &[u8]) -> &Shard {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let idx = (hasher.finish() as usize) % self.shards.len();
        &self.shards[idx]
    }

    pub fn get(&self, key: &[u8]) -> Option<Value> {
        self.shard_for(key).get(key)
    }
    pub fn set(&self, key: Bytes, value: Value) {
        self.shard_for(&key).set(key, value)
    }
    pub fn del(&self, key: &[u8]) -> bool {
        self.shard_for(key).del(key)
    }
    pub fn exists(&self, key: &[u8]) -> bool {
        self.shard_for(key).exists(key)
    }
    pub fn keys(&self) -> Vec<Bytes> {
        self.shards.iter().flat_map(|s| s.keys()).collect()
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
