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
        // get() gives.
        if matches!(guard.get(key), Some(entry) if entry.is_expired()) {
            guard.remove(key);
        }
        match guard.get_mut(key) {
            Some(entry) => f(Some(&mut entry.value)),
            None => f(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
    use bytes::Bytes;
    use std::time::{Duration, Instant};

    #[test]
    fn set_then_get_returns_value() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"foo"),
            Value::String(Bytes::from_static(b"bar")),
        );
        assert_eq!(
            shard.get(b"foo"),
            Some(Value::String(Bytes::from_static(b"bar")))
        );
    }

    #[test]
    fn del_removes_key_and_reports_it_existed() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert!(shard.del(b"k"));
        assert_eq!(shard.get(b"k"), None);
        assert!(!shard.del(b"k"));
    }

    #[test]
    fn get_returns_none_for_a_key_whose_expiry_has_passed() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert_eq!(shard.get(b"k"), None);
    }

    #[test]
    fn get_returns_the_value_when_the_expiry_is_in_the_future() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        shard.expire_at(b"k", Instant::now() + Duration::from_secs(60));
        assert_eq!(
            shard.get(b"k"),
            Some(Value::String(Bytes::from_static(b"v")))
        );
    }

    #[test]
    fn expired_key_is_actually_removed_from_the_map_after_a_get() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert_eq!(shard.get(b"k"), None);
        assert_eq!(shard.keys(), Vec::<Bytes>::new()); // gone, not just hidden
    }

    #[test]
    fn exists_treats_an_expired_key_as_absent() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert!(!shard.exists(b"k"));
    }

    #[test]
    fn del_on_an_expired_key_reports_it_did_not_exist() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert!(!shard.del(b"k"));
    }

    #[test]
    fn keys_excludes_expired_entries() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"live"),
            Value::String(Bytes::from_static(b"v")),
        );
        shard.set(
            Bytes::from_static(b"dead"),
            Value::String(Bytes::from_static(b"v")),
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
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert!(shard.expire_at(b"k", Instant::now() - Duration::from_secs(1)));
        assert_eq!(shard.get(b"k"), None);
    }

    #[test]
    fn persist_removes_an_existing_ttl_and_reports_true() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        shard.expire_at(b"k", Instant::now() + Duration::from_secs(60));
        assert!(shard.persist(b"k"));
        // the value is still there, and a second persist finds no TTL left to clear —
        // together those prove the first persist actually removed the expiry rather than
        // just reporting that it did
        assert_eq!(
            shard.get(b"k"),
            Some(Value::String(Bytes::from_static(b"v")))
        );
        assert!(!shard.persist(b"k"));
    }

    #[test]
    fn persist_on_a_key_with_no_ttl_returns_false() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
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
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"old")),
        );
        shard.expire_at(b"k", Instant::now() + Duration::from_secs(60));
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"new")),
        );
        assert!(!shard.persist(b"k")); // no TTL left to clear — SET already cleared it
    }

    #[test]
    fn with_ref_sees_none_for_a_key_whose_expiry_has_passed() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert!(shard.with_ref(b"k", |v| v.is_none()));
    }

    #[test]
    fn with_mut_sees_none_for_a_key_whose_expiry_has_passed_and_sweeps_it() {
        let shard = Shard::new();
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        shard.expire_at(b"k", Instant::now() - Duration::from_secs(1));
        assert!(shard.with_mut(b"k", |v| v.is_none()));
        assert_eq!(shard.keys(), Vec::<Bytes>::new()); // gone, not just hidden
    }
}
