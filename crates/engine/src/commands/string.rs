use crate::{Engine, Value};
use bytes::Bytes;

pub fn set_nx(engine: &Engine, key: Bytes, val: Bytes) -> bool {
    if engine.exists(&key) {
        return false;
    }
    engine.set(key, Value::String(val));
    true
}

pub fn set_xx(engine: &Engine, key: Bytes, val: Bytes) -> bool {
    if !engine.exists(&key) {
        return false;
    }
    engine.set(key, Value::String(val));
    true
}

pub fn get(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    match engine.get(key) {
        None => Ok(None),
        Some(Value::String(b)) => Ok(Some(b)),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn append(engine: &Engine, key: Bytes, suffix: &[u8]) -> Result<usize, common::EngineError> {
    let mut buf = match engine.get(&key) {
        None => Vec::new(),
        Some(Value::String(b)) => b.to_vec(),
        Some(_) => return Err(common::EngineError::WrongType),
    };
    buf.extend_from_slice(suffix);
    let len = buf.len();
    engine.set(key, Value::String(Bytes::from(buf)));
    Ok(len)
}

pub fn strlen(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    match engine.get(key) {
        None => Ok(0),
        Some(Value::String(b)) => Ok(b.len()),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn incr_by(engine: &Engine, key: Bytes, delta: i64) -> Result<i64, common::EngineError> {
    let current: i64 = match engine.get(&key) {
        None => 0,
        Some(Value::String(b)) => std::str::from_utf8(&b)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(common::EngineError::NotAnInteger)?,
        Some(_) => return Err(common::EngineError::WrongType),
    };
    let next = current + delta;
    engine.set(key, Value::String(Bytes::from(next.to_string())));
    Ok(next)
}

pub fn getset(engine: &Engine, key: Bytes, val: Bytes) -> Result<Option<Bytes>, common::EngineError> {
    let old = get(engine, &key)?;
    engine.set(key, Value::String(val));
    Ok(old)
}

pub fn mset(engine: &Engine, pairs: Vec<(Bytes, Bytes)>) {
    for (k, v) in pairs {
        engine.set(k, Value::String(v));
    }
}

/// A missing key and a wrong-type key are indistinguishable in the result — both come back
/// `None` — matching real Redis's documented MGET behavior of never erroring.
pub fn mget(engine: &Engine, keys: &[Bytes]) -> Vec<Option<Bytes>> {
    keys.iter()
        .map(|k| match engine.get(k) {
            Some(Value::String(b)) => Some(b),
            _ => None,
        })
        .collect()
}

pub fn msetnx(engine: &Engine, pairs: Vec<(Bytes, Bytes)>) -> bool {
    if pairs.iter().any(|(k, _)| engine.exists(k)) {
        return false;
    }
    for (k, v) in pairs {
        engine.set(k, Value::String(v));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use bytes::Bytes;

    #[test]
    fn set_nx_fails_when_key_already_exists() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            crate::Value::String(Bytes::from_static(b"old")),
        );
        let applied = set_nx(
            &engine,
            Bytes::from_static(b"k"),
            Bytes::from_static(b"new"),
        );
        assert!(!applied);
        assert_eq!(
            get(&engine, b"k").unwrap(),
            Some(Bytes::from_static(b"old"))
        );
    }

    #[test]
    fn set_xx_fails_when_key_missing() {
        let engine = Engine::new();
        let applied = set_xx(
            &engine,
            Bytes::from_static(b"missing"),
            Bytes::from_static(b"v"),
        );
        assert!(!applied);
        assert_eq!(get(&engine, b"missing").unwrap(), None);
    }

    #[test]
    fn get_on_hash_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"h"),
            crate::Value::Hash(Default::default()),
        );
        assert_eq!(
            get(&engine, b"h").unwrap_err(),
            common::EngineError::WrongType
        );
    }

    #[test]
    fn append_to_missing_key_creates_it() {
        let engine = Engine::new();
        let len = append(&engine, Bytes::from_static(b"k"), b"hello").unwrap();
        assert_eq!(len, 5);
        assert_eq!(strlen(&engine, b"k").unwrap(), 5);
    }

    #[test]
    fn append_to_existing_key_extends_it() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"hello")),
        );
        let len = append(&engine, Bytes::from_static(b"k"), b" world").unwrap();
        assert_eq!(len, 11);
    }

    #[test]
    fn strlen_on_missing_key_is_zero() {
        let engine = Engine::new();
        assert_eq!(strlen(&engine, b"missing").unwrap(), 0);
    }

    #[test]
    fn incr_on_missing_key_initializes_to_one() {
        let engine = Engine::new();
        assert_eq!(
            incr_by(&engine, Bytes::from_static(b"counter"), 1).unwrap(),
            1
        );
    }

    #[test]
    fn incr_by_adds_to_existing_value() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"counter"),
            Value::String(Bytes::from_static(b"10")),
        );
        assert_eq!(
            incr_by(&engine, Bytes::from_static(b"counter"), 5).unwrap(),
            15
        );
    }

    #[test]
    fn decr_is_incr_by_with_negative_delta() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"counter"),
            Value::String(Bytes::from_static(b"10")),
        );
        assert_eq!(
            incr_by(&engine, Bytes::from_static(b"counter"), -3).unwrap(),
            7
        );
    }

    #[test]
    fn incr_on_non_integer_string_returns_not_an_integer_error() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"abc")),
        );
        let err = incr_by(&engine, Bytes::from_static(b"k"), 1).unwrap_err();
        assert_eq!(err, common::EngineError::NotAnInteger);
    }

    #[test]
    fn getset_returns_old_value_and_sets_new_one() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"old")));
        let old = getset(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"new")).unwrap();
        assert_eq!(old, Some(Bytes::from_static(b"old")));
        assert_eq!(get(&engine, b"k").unwrap(), Some(Bytes::from_static(b"new")));
    }

    #[test]
    fn getset_on_missing_key_returns_none_and_creates_it() {
        let engine = Engine::new();
        let old = getset(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"v")).unwrap();
        assert_eq!(old, None);
        assert_eq!(get(&engine, b"k").unwrap(), Some(Bytes::from_static(b"v")));
    }

    #[test]
    fn getset_on_hash_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"h"), Value::Hash(Default::default()));
        let err = getset(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"v")).unwrap_err();
        assert_eq!(err, common::EngineError::WrongType);
    }

    #[test]
    fn mset_sets_every_pair() {
        let engine = Engine::new();
        mset(
            &engine,
            vec![
                (Bytes::from_static(b"a"), Bytes::from_static(b"1")),
                (Bytes::from_static(b"b"), Bytes::from_static(b"2")),
            ],
        );
        assert_eq!(get(&engine, b"a").unwrap(), Some(Bytes::from_static(b"1")));
        assert_eq!(get(&engine, b"b").unwrap(), Some(Bytes::from_static(b"2")));
    }

    #[test]
    fn mget_returns_none_for_missing_keys_in_order() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"a"), Value::String(Bytes::from_static(b"1")));
        let result = mget(&engine, &[Bytes::from_static(b"a"), Bytes::from_static(b"missing")]);
        assert_eq!(result, vec![Some(Bytes::from_static(b"1")), None]);
    }

    #[test]
    fn mget_returns_none_for_a_wrongtype_key_instead_of_erroring() {
        // MGET is Redis's documented exception to the WRONGTYPE convention: a non-string key
        // among the requested keys comes back nil for that key, not an error for the whole command.
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"h"), Value::Hash(Default::default()));
        let result = mget(&engine, &[Bytes::from_static(b"h")]);
        assert_eq!(result, vec![None]);
    }

    #[test]
    fn msetnx_fails_and_sets_nothing_if_any_key_already_exists() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"a"), Value::String(Bytes::from_static(b"existing")));
        let applied = msetnx(
            &engine,
            vec![
                (Bytes::from_static(b"a"), Bytes::from_static(b"new")),
                (Bytes::from_static(b"b"), Bytes::from_static(b"new")),
            ],
        );
        assert!(!applied);
        assert_eq!(get(&engine, b"a").unwrap(), Some(Bytes::from_static(b"existing")));
        assert_eq!(get(&engine, b"b").unwrap(), None);
    }

    #[test]
    fn msetnx_succeeds_when_no_key_exists() {
        let engine = Engine::new();
        let applied = msetnx(&engine, vec![(Bytes::from_static(b"a"), Bytes::from_static(b"1"))]);
        assert!(applied);
        assert_eq!(get(&engine, b"a").unwrap(), Some(Bytes::from_static(b"1")));
    }
}
