use crate::{Engine, Value};
use bytes::Bytes;
use std::collections::HashSet;

fn get_set(engine: &Engine, key: &[u8]) -> Result<HashSet<Bytes>, common::EngineError> {
    match engine.get(key) {
        None => Ok(HashSet::new()),
        Some(Value::Set(s)) => Ok(s),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn sadd(engine: &Engine, key: Bytes, member: Bytes) -> Result<(), common::EngineError> {
    let mut set = get_set(engine, &key)?;
    set.insert(member);
    engine.set(key, Value::Set(set));
    Ok(())
}

pub fn srem(engine: &Engine, key: &[u8], member: &[u8]) -> Result<bool, common::EngineError> {
    let mut set = match engine.get(key) {
        None => return Ok(false),
        Some(Value::Set(s)) => s,
        Some(_) => return Err(common::EngineError::WrongType),
    };
    let removed = set.remove(member);
    engine.set(Bytes::copy_from_slice(key), Value::Set(set));
    Ok(removed)
}

pub fn smembers(engine: &Engine, key: &[u8]) -> Result<HashSet<Bytes>, common::EngineError> {
    get_set(engine, key)
}

pub fn sismember(engine: &Engine, key: &[u8], member: &[u8]) -> Result<bool, common::EngineError> {
    Ok(get_set(engine, key)?.contains(member))
}

pub fn scard(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    Ok(get_set(engine, key)?.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use bytes::Bytes;

    #[test]
    fn sadd_then_sismember_is_true() {
        let engine = Engine::new();
        sadd(&engine, Bytes::from_static(b"s"), Bytes::from_static(b"x")).unwrap();
        assert!(sismember(&engine, b"s", b"x").unwrap());
        assert!(!sismember(&engine, b"s", b"y").unwrap());
    }

    #[test]
    fn srem_removes_member_and_reports_it_existed() {
        let engine = Engine::new();
        sadd(&engine, Bytes::from_static(b"s"), Bytes::from_static(b"x")).unwrap();
        assert!(srem(&engine, b"s", b"x").unwrap());
        assert!(!srem(&engine, b"s", b"x").unwrap());
    }

    #[test]
    fn scard_counts_members() {
        let engine = Engine::new();
        sadd(&engine, Bytes::from_static(b"s"), Bytes::from_static(b"x")).unwrap();
        sadd(&engine, Bytes::from_static(b"s"), Bytes::from_static(b"y")).unwrap();
        assert_eq!(scard(&engine, b"s").unwrap(), 2);
    }

    #[test]
    fn sadd_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert_eq!(
            sadd(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"x")).unwrap_err(),
            common::EngineError::WrongType
        );
    }
}
