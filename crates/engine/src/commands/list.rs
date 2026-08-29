use bytes::Bytes;
use std::collections::VecDeque;
use crate::{Engine, Value};

fn get_list(engine: &Engine, key: &[u8]) -> Result<VecDeque<Bytes>, common::EngineError> {
    match engine.get(key) {
        None => Ok(VecDeque::new()),
        Some(Value::List(l)) => Ok(l),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn rpush(engine: &Engine, key: Bytes, val: Bytes) -> Result<(), common::EngineError> {
    let mut list = get_list(engine, &key)?;
    list.push_back(val);
    engine.set(key, Value::List(list));
    Ok(())
}

pub fn lpush(engine: &Engine, key: Bytes, val: Bytes) -> Result<(), common::EngineError> {
    let mut list = get_list(engine, &key)?;
    list.push_front(val);
    engine.set(key, Value::List(list));
    Ok(())
}

pub fn rpop(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    let mut list = match engine.get(key) {
        None => return Ok(None),
        Some(Value::List(l)) => l,
        Some(_) => return Err(common::EngineError::WrongType),
    };
    let popped = list.pop_back();
    engine.set(Bytes::copy_from_slice(key), Value::List(list));
    Ok(popped)
}

pub fn lpop(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    let mut list = match engine.get(key) {
        None => return Ok(None),
        Some(Value::List(l)) => l,
        Some(_) => return Err(common::EngineError::WrongType),
    };
    let popped = list.pop_front();
    engine.set(Bytes::copy_from_slice(key), Value::List(list));
    Ok(popped)
}

pub fn llen(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    Ok(get_list(engine, key)?.len())
}

/// start/stop follow Redis semantics: negative indices count from the end, -1 is the last element.
pub fn lrange(engine: &Engine, key: &[u8], start: i64, stop: i64) -> Result<Vec<Bytes>, common::EngineError> {
    let list = get_list(engine, key)?;
    let len = list.len() as i64;
    let norm = |i: i64| -> i64 { if i < 0 { (len + i).max(0) } else { i.min(len) } };
    let (s, e) = (norm(start), norm(stop) + 1);
    if s >= e { return Ok(Vec::new()); }
    Ok(list.into_iter().skip(s as usize).take((e - s) as usize).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use bytes::Bytes;

    #[test]
    fn rpush_then_lrange_returns_in_insertion_order() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b")).unwrap();
        let items = lrange(&engine, b"l", 0, -1).unwrap();
        assert_eq!(items, vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]);
    }

    #[test]
    fn lpush_prepends() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b")).unwrap();
        lpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        let items = lrange(&engine, b"l", 0, -1).unwrap();
        assert_eq!(items, vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]);
    }

    #[test]
    fn rpop_returns_and_removes_last_element() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b")).unwrap();
        assert_eq!(rpop(&engine, b"l").unwrap(), Some(Bytes::from_static(b"b")));
        assert_eq!(llen(&engine, b"l").unwrap(), 1);
    }

    #[test]
    fn rpush_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
        assert_eq!(rpush(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"x")).unwrap_err(), common::EngineError::WrongType);
    }

    #[test]
    fn lpush_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
        assert_eq!(lpush(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"x")).unwrap_err(), common::EngineError::WrongType);
    }
}
