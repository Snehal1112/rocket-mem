use crate::{store::Store, Value};
use bytes::Bytes;

pub struct Engine {
    store: Store,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            store: Store::new(16),
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<Value> {
        self.store.get(key)
    }
    pub fn set(&self, key: Bytes, value: Value) {
        self.store.set(key, value)
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
    pub fn with_mut<F, R>(&self, key: &[u8], f: F) -> R
    where
        F: FnOnce(Option<&mut Value>) -> R,
    {
        self.store.with_mut(key, f)
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
}
