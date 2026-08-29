use crate::{Engine, Value};
use bytes::Bytes;
use std::collections::HashMap;

/// Returns whether `field` was newly added (`false` if it already existed and was overwritten) —
/// callers implementing variadic `HSET` sum this across pairs for the count Redis reports.
/// Mutates in place via `with_mut` -- see list.rs's top-of-file note for why this matters.
pub fn hset(
    engine: &Engine,
    key: Bytes,
    field: Bytes,
    val: Bytes,
) -> Result<bool, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<bool>, common::EngineError> {
            match existing {
                Some(Value::Hash(map)) => {
                    let is_new = !map.contains_key(&field);
                    map.insert(field.clone(), val.clone());
                    Ok(Some(is_new))
                }
                Some(_) => Err(common::EngineError::WrongType),
                None => Ok(None),
            }
        },
    )?;
    match existed {
        Some(is_new) => Ok(is_new),
        None => {
            let mut map = HashMap::new();
            map.insert(field, val);
            engine.set(key, Value::Hash(map));
            Ok(true)
        }
    }
}

pub fn hget(
    engine: &Engine,
    key: &[u8],
    field: &[u8],
) -> Result<Option<Bytes>, common::EngineError> {
    engine.with_ref(key, |v| match v {
        None => Ok(None),
        Some(Value::Hash(map)) => Ok(map.get(field).cloned()),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn hdel(engine: &Engine, key: &[u8], field: &[u8]) -> Result<bool, common::EngineError> {
    engine.with_mut(key, |existing| match existing {
        None => Ok(false),
        Some(Value::Hash(map)) => Ok(map.remove(field).is_some()),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn hgetall(engine: &Engine, key: &[u8]) -> Result<HashMap<Bytes, Bytes>, common::EngineError> {
    match engine.get(key) {
        None => Ok(HashMap::new()),
        Some(Value::Hash(m)) => Ok(m),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn hexists(engine: &Engine, key: &[u8], field: &[u8]) -> Result<bool, common::EngineError> {
    engine.with_ref(key, |v| match v {
        None => Ok(false),
        Some(Value::Hash(map)) => Ok(map.contains_key(field)),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn hlen(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    engine.with_ref(key, |v| match v {
        None => Ok(0),
        Some(Value::Hash(map)) => Ok(map.len()),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn hincrby(
    engine: &Engine,
    key: Bytes,
    field: Bytes,
    delta: i64,
) -> Result<i64, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<i64>, common::EngineError> {
            match existing {
                Some(Value::Hash(map)) => {
                    let current: i64 = match map.get(&field) {
                        Some(b) => std::str::from_utf8(b)
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .ok_or(common::EngineError::NotAnInteger)?,
                        None => 0,
                    };
                    let next = current + delta;
                    map.insert(field.clone(), Bytes::from(next.to_string()));
                    Ok(Some(next))
                }
                Some(_) => Err(common::EngineError::WrongType),
                None => Ok(None),
            }
        },
    )?;
    match existed {
        Some(next) => Ok(next),
        None => {
            let mut map = HashMap::new();
            map.insert(field, Bytes::from(delta.to_string()));
            engine.set(key, Value::Hash(map));
            Ok(delta)
        }
    }
}

pub fn hkeys(engine: &Engine, key: &[u8]) -> Result<Vec<Bytes>, common::EngineError> {
    Ok(hgetall(engine, key)?.into_keys().collect())
}

pub fn hvals(engine: &Engine, key: &[u8]) -> Result<Vec<Bytes>, common::EngineError> {
    Ok(hgetall(engine, key)?.into_values().collect())
}

pub fn hmget(
    engine: &Engine,
    key: &[u8],
    fields: &[Bytes],
) -> Result<Vec<Option<Bytes>>, common::EngineError> {
    engine.with_ref(key, |v| match v {
        None => Ok(fields.iter().map(|_| None).collect()),
        Some(Value::Hash(map)) => Ok(fields.iter().map(|f| map.get(f).cloned()).collect()),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn hsetnx(
    engine: &Engine,
    key: Bytes,
    field: Bytes,
    val: Bytes,
) -> Result<bool, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<bool>, common::EngineError> {
            match existing {
                Some(Value::Hash(map)) => {
                    if map.contains_key(&field) {
                        Ok(Some(false))
                    } else {
                        map.insert(field.clone(), val.clone());
                        Ok(Some(true))
                    }
                }
                Some(_) => Err(common::EngineError::WrongType),
                None => Ok(None),
            }
        },
    )?;
    match existed {
        Some(applied) => Ok(applied),
        None => {
            let mut map = HashMap::new();
            map.insert(field, val);
            engine.set(key, Value::Hash(map));
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, Value};
    use bytes::Bytes;

    #[test]
    fn hset_then_hget_round_trips() {
        let engine = Engine::new();
        hset(
            &engine,
            Bytes::from_static(b"h"),
            Bytes::from_static(b"field"),
            Bytes::from_static(b"val"),
        )
        .unwrap();
        assert_eq!(
            hget(&engine, b"h", b"field").unwrap(),
            Some(Bytes::from_static(b"val"))
        );
    }

    #[test]
    fn hset_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        let err = hset(
            &engine,
            Bytes::from_static(b"k"),
            Bytes::from_static(b"f"),
            Bytes::from_static(b"v"),
        )
        .unwrap_err();
        assert_eq!(err, common::EngineError::WrongType);
    }

    #[test]
    fn hget_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert_eq!(
            hget(&engine, b"k", b"field").unwrap_err(),
            common::EngineError::WrongType
        );
    }

    #[test]
    fn hdel_removes_field_and_reports_it_existed() {
        let engine = Engine::new();
        hset(
            &engine,
            Bytes::from_static(b"h"),
            Bytes::from_static(b"f"),
            Bytes::from_static(b"v"),
        )
        .unwrap();
        assert!(hdel(&engine, b"h", b"f").unwrap());
        assert!(!hdel(&engine, b"h", b"f").unwrap());
    }

    #[test]
    fn hgetall_on_missing_key_is_empty_not_error() {
        let engine = Engine::new();
        assert!(hgetall(&engine, b"missing").unwrap().is_empty());
    }

    #[test]
    fn hincrby_on_missing_field_initializes_from_zero() {
        let engine = Engine::new();
        assert_eq!(
            hincrby(
                &engine,
                Bytes::from_static(b"h"),
                Bytes::from_static(b"f"),
                5
            )
            .unwrap(),
            5
        );
    }

    #[test]
    fn hincrby_adds_to_an_existing_field() {
        let engine = Engine::new();
        hset(
            &engine,
            Bytes::from_static(b"h"),
            Bytes::from_static(b"f"),
            Bytes::from_static(b"10"),
        )
        .unwrap();
        assert_eq!(
            hincrby(
                &engine,
                Bytes::from_static(b"h"),
                Bytes::from_static(b"f"),
                5
            )
            .unwrap(),
            15
        );
    }

    #[test]
    fn hincrby_on_non_integer_field_returns_not_an_integer_error() {
        let engine = Engine::new();
        hset(
            &engine,
            Bytes::from_static(b"h"),
            Bytes::from_static(b"f"),
            Bytes::from_static(b"abc"),
        )
        .unwrap();
        let err = hincrby(
            &engine,
            Bytes::from_static(b"h"),
            Bytes::from_static(b"f"),
            1,
        )
        .unwrap_err();
        assert_eq!(err, common::EngineError::NotAnInteger);
    }

    #[test]
    fn hincrby_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        let err = hincrby(
            &engine,
            Bytes::from_static(b"k"),
            Bytes::from_static(b"f"),
            1,
        )
        .unwrap_err();
        assert_eq!(err, common::EngineError::WrongType);
    }

    #[test]
    fn hkeys_and_hvals_report_the_fields_and_values() {
        let engine = Engine::new();
        hset(
            &engine,
            Bytes::from_static(b"h"),
            Bytes::from_static(b"f1"),
            Bytes::from_static(b"v1"),
        )
        .unwrap();
        let mut keys = hkeys(&engine, b"h").unwrap();
        keys.sort();
        assert_eq!(keys, vec![Bytes::from_static(b"f1")]);
        let mut vals = hvals(&engine, b"h").unwrap();
        vals.sort();
        assert_eq!(vals, vec![Bytes::from_static(b"v1")]);
    }

    #[test]
    fn hkeys_on_missing_key_is_empty_not_an_error() {
        let engine = Engine::new();
        assert!(hkeys(&engine, b"missing").unwrap().is_empty());
    }

    #[test]
    fn hmget_returns_none_for_missing_fields_in_order() {
        let engine = Engine::new();
        hset(
            &engine,
            Bytes::from_static(b"h"),
            Bytes::from_static(b"f1"),
            Bytes::from_static(b"v1"),
        )
        .unwrap();
        let result = hmget(
            &engine,
            b"h",
            &[Bytes::from_static(b"f1"), Bytes::from_static(b"missing")],
        )
        .unwrap();
        assert_eq!(result, vec![Some(Bytes::from_static(b"v1")), None]);
    }

    #[test]
    fn hsetnx_sets_only_when_the_field_is_absent() {
        let engine = Engine::new();
        assert!(hsetnx(
            &engine,
            Bytes::from_static(b"h"),
            Bytes::from_static(b"f"),
            Bytes::from_static(b"first")
        )
        .unwrap());
        assert!(!hsetnx(
            &engine,
            Bytes::from_static(b"h"),
            Bytes::from_static(b"f"),
            Bytes::from_static(b"second")
        )
        .unwrap());
        assert_eq!(
            hget(&engine, b"h", b"f").unwrap(),
            Some(Bytes::from_static(b"first"))
        );
    }
}
