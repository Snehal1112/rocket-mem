use crate::Value;
use bytes::Bytes;
use parking_lot::RwLock;
use std::collections::HashMap;

pub struct Shard {
    map: RwLock<HashMap<Bytes, Value>>,
}

impl Shard {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
        }
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

    /// Reads `key`'s value in place under the shard's read lock, without cloning the
    /// collection out first -- `f` gets a borrow, not an owned copy, so a single-field
    /// lookup on a large Hash/Set/List/SortedSet costs O(1)/O(field size), not O(collection size).
    pub fn with_ref<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&Value>) -> R,
    {
        let map = self.map.read();
        f(map.get(key))
    }

    /// Mutates `key`'s value in place under the shard's write lock, without cloning the
    /// collection out and writing a replacement back -- `f` gets a direct `&mut` into the
    /// stored value, so e.g. a single push/pop costs O(1)/O(1), not O(collection size).
    /// `f` sees `None` for a missing key; it does not auto-vivify an entry, matching this
    /// codebase's convention that a mutation finding nothing must not write back a phantom
    /// empty collection -- callers that need create-on-missing (e.g. LPUSH) insert via `set`
    /// themselves when `f` reports there was nothing to mutate.
    pub fn with_mut<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&mut Value>) -> R,
    {
        let mut map = self.map.write();
        f(map.get_mut(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
    use bytes::Bytes;

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
}
