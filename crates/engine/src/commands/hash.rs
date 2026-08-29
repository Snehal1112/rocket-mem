use bytes::Bytes;
use std::collections::HashMap;
use crate::{Engine, Value};

pub fn hset(engine: &Engine, key: Bytes, field: Bytes, val: Bytes) {
    let mut map = match engine.get(&key) {
        Some(Value::Hash(m)) => m,
        Some(_) => return, // caller (Sprint 2 dispatcher) is responsible for surfacing WRONGTYPE before calling
        None => HashMap::new(),
    };
    map.insert(field, val);
    engine.set(key, Value::Hash(map));
}

pub fn hget(engine: &Engine, key: &[u8], field: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    match engine.get(key) {
        None => Ok(None),
        Some(Value::Hash(m)) => Ok(m.get(field).cloned()),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn hdel(engine: &Engine, key: &[u8], field: &[u8]) -> Result<bool, common::EngineError> {
    match engine.get(key) {
        None => Ok(false),
        Some(Value::Hash(mut m)) => {
            let removed = m.remove(field).is_some();
            engine.set(Bytes::copy_from_slice(key), Value::Hash(m));
            Ok(removed)
        }
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn hgetall(engine: &Engine, key: &[u8]) -> Result<HashMap<Bytes, Bytes>, common::EngineError> {
    match engine.get(key) {
        None => Ok(HashMap::new()),
        Some(Value::Hash(m)) => Ok(m),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn hexists(engine: &Engine, key: &[u8], field: &[u8]) -> Result<bool, common::EngineError> {
    Ok(hgetall(engine, key)?.contains_key(field))
}

pub fn hlen(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    Ok(hgetall(engine, key)?.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, Value};
    use bytes::Bytes;

    #[test]
    fn hset_then_hget_round_trips() {
        let engine = Engine::new();
        hset(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"field"), Bytes::from_static(b"val"));
        assert_eq!(hget(&engine, b"h", b"field").unwrap(), Some(Bytes::from_static(b"val")));
    }

    #[test]
    fn hget_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
        assert_eq!(hget(&engine, b"k", b"field").unwrap_err(), common::EngineError::WrongType);
    }

    #[test]
    fn hdel_removes_field_and_reports_it_existed() {
        let engine = Engine::new();
        hset(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"f"), Bytes::from_static(b"v"));
        assert!(hdel(&engine, b"h", b"f").unwrap());
        assert!(!hdel(&engine, b"h", b"f").unwrap());
    }

    #[test]
    fn hgetall_on_missing_key_is_empty_not_error() {
        let engine = Engine::new();
        assert!(hgetall(&engine, b"missing").unwrap().is_empty());
    }
}
