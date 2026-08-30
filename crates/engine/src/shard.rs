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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
    use bytes::Bytes;
    use std::sync::atomic::AtomicU64;
    use std::time::{Duration, Instant};

    #[test]
    fn set_then_get_returns_value() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"foo"),
            Value::String(Bytes::from_static(b"bar")),
            &clock,
        );
        assert_eq!(
            shard.get(b"foo", &clock),
            Some(Value::String(Bytes::from_static(b"bar")))
        );
    }

    #[test]
    fn del_removes_key_and_reports_it_existed() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        assert!(shard.del(b"k"));
        assert_eq!(shard.get(b"k", &clock), None);
        assert!(!shard.del(b"k"));
    }

    #[test]
    fn get_returns_none_for_a_key_whose_expiry_has_passed() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert_eq!(shard.get(b"k", &clock), None);
    }

    #[test]
    fn get_returns_the_value_when_the_expiry_is_in_the_future() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.expire_at(b"k", Instant::now() + Duration::from_secs(60));
        assert_eq!(
            shard.get(b"k", &clock),
            Some(Value::String(Bytes::from_static(b"v")))
        );
    }

    #[test]
    fn expired_key_is_actually_removed_from_the_map_after_a_get() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert_eq!(shard.get(b"k", &clock), None);
        assert_eq!(shard.keys(), Vec::<Bytes>::new()); // gone, not just hidden
    }

    #[test]
    fn exists_treats_an_expired_key_as_absent() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert!(!shard.exists(b"k", &clock));
    }

    #[test]
    fn del_on_an_expired_key_reports_it_did_not_exist() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert!(!shard.del(b"k"));
    }

    #[test]
    fn keys_excludes_expired_entries() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"live"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.set(
            Bytes::from_static(b"dead"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
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
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        assert!(shard.expire_at(b"k", Instant::now() - Duration::from_secs(1)));
        assert_eq!(shard.get(b"k", &clock), None);
    }

    #[test]
    fn persist_removes_an_existing_ttl_and_reports_true() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.expire_at(b"k", Instant::now() + Duration::from_secs(60));
        assert!(shard.persist(b"k"));
        // the value is still there, and a second persist finds no TTL left to clear —
        // together those prove the first persist actually removed the expiry rather than
        // just reporting that it did
        assert_eq!(
            shard.get(b"k", &clock),
            Some(Value::String(Bytes::from_static(b"v")))
        );
        assert!(!shard.persist(b"k"));
    }

    #[test]
    fn persist_on_a_key_with_no_ttl_returns_false() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
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
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"old")),
            &clock,
        );
        shard.expire_at(b"k", Instant::now() + Duration::from_secs(60));
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"new")),
            &clock,
        );
        assert!(!shard.persist(b"k")); // no TTL left to clear — SET already cleared it
    }

    #[test]
    fn with_ref_sees_none_for_a_key_whose_expiry_has_passed() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert!(shard.with_ref(b"k", |v| v.is_none(), &clock));
    }

    #[test]
    fn with_mut_sees_none_for_a_key_whose_expiry_has_passed_and_sweeps_it() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert!(shard.with_mut(b"k", |v| v.is_none(), &clock));
        assert_eq!(shard.keys(), Vec::<Bytes>::new()); // gone, not just hidden
    }

    #[test]
    fn remove_expired_deletes_only_expired_entries_and_reports_the_count() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"live"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.set(
            Bytes::from_static(b"dead1"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.set(
            Bytes::from_static(b"dead2"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.expire_at(b"dead1", Instant::now() - Duration::from_secs(1));
        shard.expire_at(b"dead2", Instant::now() - Duration::from_secs(1));
        assert_eq!(shard.remove_expired(), 2);
        assert_eq!(shard.keys(), vec![Bytes::from_static(b"live")]);
    }

    #[test]
    fn remove_expired_on_a_shard_with_nothing_expired_removes_nothing() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"live"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        assert_eq!(shard.remove_expired(), 0);
    }

    #[test]
    fn bytes_used_increases_after_set_and_decreases_after_del() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        assert_eq!(shard.bytes_used(), 0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        assert!(shard.bytes_used() > 0);
        let after_set = shard.bytes_used();
        shard.del(b"k");
        assert!(shard.bytes_used() < after_set);
    }

    #[test]
    fn bytes_used_accounts_for_overwriting_an_existing_key_not_double_counting_it() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"aaaaaaaaaa")),
            &clock,
        );
        let after_first = shard.bytes_used();
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"aaaaaaaaaa")),
            &clock,
        );
        assert_eq!(shard.bytes_used(), after_first); // same key, same size — no growth
    }

    #[test]
    fn get_bumps_last_touched_to_a_fresh_tick() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
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
            shard.set(
                Bytes::from(format!("k{i}")),
                Value::String(Bytes::from_static(b"v")),
                &clock,
            );
        }
        assert_eq!(shard.sample_recency(3).len(), 3);
        assert_eq!(shard.sample_recency(100).len(), 5); // never more than what's actually there
    }

    #[test]
    fn with_ref_bumps_last_touched_to_a_fresh_tick() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        let before = shard.sample_recency(10)[0].1;
        shard.with_ref(b"k", |v| v.is_some(), &clock);
        let after = shard.sample_recency(10)[0].1;
        assert!(after > before);
    }

    #[test]
    fn with_mut_re_accounts_bytes_used_after_growing_a_value_in_place() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::List(std::collections::VecDeque::new()),
            &clock,
        );
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

    #[test]
    fn entries_returns_every_unexpired_key_value_and_expiry() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"a"),
            Value::String(Bytes::from_static(b"1")),
            &clock,
        );
        shard.set(
            Bytes::from_static(b"b"),
            Value::String(Bytes::from_static(b"2")),
            &clock,
        );
        let at = Instant::now() + std::time::Duration::from_secs(60);
        shard.expire_at(b"b", at);

        let mut got = shard.entries();
        got.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(
            got[0],
            (
                Bytes::from_static(b"a"),
                Value::String(Bytes::from_static(b"1")),
                None
            )
        );
        assert_eq!(got[1].0, Bytes::from_static(b"b"));
        assert_eq!(got[1].1, Value::String(Bytes::from_static(b"2")));
        assert_eq!(got[1].2, Some(at));
    }

    #[test]
    fn entries_excludes_expired_keys() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"a"),
            Value::String(Bytes::from_static(b"1")),
            &clock,
        );
        shard.expire_at(b"a", Instant::now() - std::time::Duration::from_secs(1));
        assert!(shard.entries().is_empty());
    }

    #[test]
    fn clear_empties_the_map_and_resets_bytes_used() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"a"),
            Value::String(Bytes::from_static(b"1")),
            &clock,
        );
        assert!(shard.bytes_used() > 0);
        shard.clear();
        assert_eq!(shard.bytes_used(), 0);
        assert!(shard.entries().is_empty());
    }
}
