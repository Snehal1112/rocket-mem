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
