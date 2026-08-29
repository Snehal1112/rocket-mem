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
