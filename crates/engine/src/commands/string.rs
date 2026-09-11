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
    // Mutate in place via `with_mut_delta` when the key already holds a string, so its TTL
    // (which `Engine::set` would unconditionally clear) survives -- matching real Redis. Only
    // fall back to `engine.set` to create a genuinely new key, which has no TTL to preserve.
    let existed = engine.with_mut_delta(
        &key,
        |existing| -> (Result<Option<usize>, common::EngineError>, isize) {
            match existing {
                Some(Value::String(b)) => {
                    let mut buf = b.to_vec();
                    buf.extend_from_slice(suffix);
                    let len = buf.len();
                    *b = Bytes::from(buf);
                    (Ok(Some(len)), suffix.len() as isize)
                }
                Some(_) => (Err(common::EngineError::WrongType), 0),
                None => (Ok(None), 0),
            }
        },
    )?;
    match existed {
        Some(len) => Ok(len),
        None => {
            let len = suffix.len();
            engine.set(key, Value::String(Bytes::copy_from_slice(suffix)));
            Ok(len)
        }
    }
}

pub fn strlen(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    match engine.get(key) {
        None => Ok(0),
        Some(Value::String(b)) => Ok(b.len()),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn incr_by(engine: &Engine, key: Bytes, delta: i64) -> Result<i64, common::EngineError> {
    // Mutate in place via `with_mut_delta` when the key already holds a string, so its TTL
    // (which `Engine::set` would unconditionally clear) survives -- matching real Redis. Only
    // fall back to `engine.set` to create a genuinely new key, which has no TTL to preserve.
    let existed = engine.with_mut_delta(
        &key,
        |existing| -> (Result<Option<i64>, common::EngineError>, isize) {
            match existing {
                Some(Value::String(b)) => {
                    let current: i64 =
                        match std::str::from_utf8(b).ok().and_then(|s| s.parse().ok()) {
                            Some(v) => v,
                            None => return (Err(common::EngineError::NotAnInteger), 0),
                        };
                    let next = match current.checked_add(delta) {
                        Some(v) => v,
                        None => return (Err(common::EngineError::IncrementOverflow), 0),
                    };
                    let new_bytes = Bytes::from(next.to_string());
                    let size_delta = new_bytes.len() as isize - b.len() as isize;
                    *b = new_bytes;
                    (Ok(Some(next)), size_delta)
                }
                Some(_) => (Err(common::EngineError::WrongType), 0),
                None => (Ok(None), 0),
            }
        },
    )?;
    match existed {
        Some(next) => Ok(next),
        None => {
            // A missing key initializes as if the current value were 0; 0 + delta can never
            // overflow an i64, so no overflow check is needed on this path.
            let next = delta;
            engine.set(key, Value::String(Bytes::from(next.to_string())));
            Ok(next)
        }
    }
}

pub fn getset(
    engine: &Engine,
    key: Bytes,
    val: Bytes,
) -> Result<Option<Bytes>, common::EngineError> {
    let old = get(engine, &key)?;
    engine.set(key, Value::String(val));
    Ok(old)
}

/// start/end follow Redis semantics: negative indices count from the end, -1 is the last byte,
/// out-of-range indices clamp rather than error.
pub fn getrange(
    engine: &Engine,
    key: &[u8],
    start: i64,
    end: i64,
) -> Result<Bytes, common::EngineError> {
    let buf = match engine.get(key) {
        None => return Ok(Bytes::new()),
        Some(Value::String(b)) => b,
        Some(_) => return Err(common::EngineError::WrongType),
    };
    let len = buf.len() as i64;
    let norm = |i: i64| -> i64 {
        if i < 0 {
            (len + i).max(0)
        } else {
            i.min(len)
        }
    };
    let s = norm(start);
    let e = (norm(end) + 1).min(len);
    if s >= e {
        return Ok(Bytes::new());
    }
    Ok(buf.slice(s as usize..e as usize))
}

/// Overwrites `value` into the string starting at byte `offset`, zero-padding first if `offset`
/// extends past the current length. Returns the string's length after the write. An empty
/// `value` against a missing key is a documented Redis no-op — it must not create the key.
pub fn setrange(
    engine: &Engine,
    key: Bytes,
    offset: usize,
    value: &[u8],
) -> Result<usize, common::EngineError> {
    // An empty `value` is a pure no-op (even against an existing key) -- it must not create a
    // missing key, and it must not touch an existing one, so this never reaches `with_mut_delta`.
    if value.is_empty() {
        return match engine.get(&key) {
            None => Ok(0),
            Some(Value::String(b)) => Ok(b.len()),
            Some(_) => Err(common::EngineError::WrongType),
        };
    }
    // Mutate in place via `with_mut_delta` when the key already holds a string, so its TTL
    // (which `Engine::set` would unconditionally clear) survives -- matching real Redis. Only
    // fall back to `engine.set` to create a genuinely new key, which has no TTL to preserve.
    let existed = engine.with_mut_delta(
        &key,
        |existing| -> (Result<Option<usize>, common::EngineError>, isize) {
            match existing {
                Some(Value::String(b)) => {
                    let mut buf = b.to_vec();
                    let old_len = buf.len();
                    let end = offset + value.len();
                    if buf.len() < end {
                        buf.resize(end, 0);
                    }
                    buf[offset..end].copy_from_slice(value);
                    let len = buf.len();
                    let size_delta = len as isize - old_len as isize;
                    *b = Bytes::from(buf);
                    (Ok(Some(len)), size_delta)
                }
                Some(_) => (Err(common::EngineError::WrongType), 0),
                None => (Ok(None), 0),
            }
        },
    )?;
    match existed {
        Some(len) => Ok(len),
        None => {
            let end = offset + value.len();
            let mut buf = vec![0u8; end];
            buf[offset..end].copy_from_slice(value);
            let len = buf.len();
            engine.set(key, Value::String(Bytes::from(buf)));
            Ok(len)
        }
    }
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
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"old")),
        );
        let old = getset(
            &engine,
            Bytes::from_static(b"k"),
            Bytes::from_static(b"new"),
        )
        .unwrap();
        assert_eq!(old, Some(Bytes::from_static(b"old")));
        assert_eq!(
            get(&engine, b"k").unwrap(),
            Some(Bytes::from_static(b"new"))
        );
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
    fn getrange_on_missing_key_is_empty_not_error() {
        let engine = Engine::new();
        assert_eq!(getrange(&engine, b"missing", 0, -1).unwrap(), Bytes::new());
    }

    #[test]
    fn getrange_with_positive_indices_returns_the_inclusive_slice() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"Hello World")),
        );
        assert_eq!(
            getrange(&engine, b"k", 0, 4).unwrap(),
            Bytes::from_static(b"Hello")
        );
    }

    #[test]
    fn getrange_with_negative_indices_counts_from_the_end() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"Hello World")),
        );
        assert_eq!(
            getrange(&engine, b"k", -5, -1).unwrap(),
            Bytes::from_static(b"World")
        );
    }

    #[test]
    fn getrange_past_the_end_clamps_to_empty_rather_than_panicking() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"Hello")),
        );
        assert_eq!(getrange(&engine, b"k", 10, 100).unwrap(), Bytes::new());
    }

    #[test]
    fn getrange_on_hash_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"h"), Value::Hash(Default::default()));
        assert_eq!(
            getrange(&engine, b"h", 0, -1).unwrap_err(),
            common::EngineError::WrongType
        );
    }

    #[test]
    fn setrange_on_missing_key_zero_pads_up_to_the_offset() {
        let engine = Engine::new();
        let len = setrange(&engine, Bytes::from_static(b"k"), 5, b"World").unwrap();
        assert_eq!(len, 10);
        assert_eq!(
            get(&engine, b"k").unwrap(),
            Some(Bytes::from(b"\0\0\0\0\0World".to_vec()))
        );
    }

    #[test]
    fn setrange_overwrites_within_an_existing_value() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"Hello World")),
        );
        let len = setrange(&engine, Bytes::from_static(b"k"), 6, b"Redis!").unwrap();
        assert_eq!(len, 12);
        assert_eq!(
            get(&engine, b"k").unwrap(),
            Some(Bytes::from_static(b"Hello Redis!"))
        );
    }

    #[test]
    fn setrange_with_an_empty_value_on_a_missing_key_does_not_create_it() {
        let engine = Engine::new();
        let len = setrange(&engine, Bytes::from_static(b"missing"), 0, b"").unwrap();
        assert_eq!(len, 0);
        assert_eq!(get(&engine, b"missing").unwrap(), None);
    }

    #[test]
    fn setrange_on_hash_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"h"), Value::Hash(Default::default()));
        assert_eq!(
            setrange(&engine, Bytes::from_static(b"h"), 0, b"x").unwrap_err(),
            common::EngineError::WrongType
        );
    }

    #[test]
    fn append_preserves_an_existing_ttl() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"hello")),
        );
        engine.expire_at(
            b"k",
            std::time::Instant::now() + std::time::Duration::from_secs(100),
        );
        append(&engine, Bytes::from_static(b"k"), b" world").unwrap();
        match engine.ttl(b"k") {
            crate::engine::TtlStatus::Remaining(d) => {
                assert!(d > std::time::Duration::from_secs(90))
            }
            other => panic!("expected Remaining, got {other:?}"),
        }
    }

    #[test]
    fn incr_by_preserves_an_existing_ttl() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"counter"),
            Value::String(Bytes::from_static(b"10")),
        );
        engine.expire_at(
            b"counter",
            std::time::Instant::now() + std::time::Duration::from_secs(100),
        );
        incr_by(&engine, Bytes::from_static(b"counter"), 5).unwrap();
        match engine.ttl(b"counter") {
            crate::engine::TtlStatus::Remaining(d) => {
                assert!(d > std::time::Duration::from_secs(90))
            }
            other => panic!("expected Remaining, got {other:?}"),
        }
    }

    #[test]
    fn setrange_preserves_an_existing_ttl() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"Hello World")),
        );
        engine.expire_at(
            b"k",
            std::time::Instant::now() + std::time::Duration::from_secs(100),
        );
        setrange(&engine, Bytes::from_static(b"k"), 6, b"Redis!").unwrap();
        match engine.ttl(b"k") {
            crate::engine::TtlStatus::Remaining(d) => {
                assert!(d > std::time::Duration::from_secs(90))
            }
            other => panic!("expected Remaining, got {other:?}"),
        }
    }

    #[test]
    fn incr_by_on_i64_max_returns_increment_overflow_and_leaves_the_value_unchanged() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"counter"),
            Value::String(Bytes::from(i64::MAX.to_string())),
        );
        let err = incr_by(&engine, Bytes::from_static(b"counter"), 1).unwrap_err();
        assert_eq!(err, common::EngineError::IncrementOverflow);
        assert_eq!(
            get(&engine, b"counter").unwrap(),
            Some(Bytes::from(i64::MAX.to_string()))
        );
    }

    #[test]
    fn incr_by_on_i64_min_with_a_negative_delta_returns_increment_overflow_and_leaves_the_value_unchanged(
    ) {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"counter"),
            Value::String(Bytes::from(i64::MIN.to_string())),
        );
        let err = incr_by(&engine, Bytes::from_static(b"counter"), -1).unwrap_err();
        assert_eq!(err, common::EngineError::IncrementOverflow);
        assert_eq!(
            get(&engine, b"counter").unwrap(),
            Some(Bytes::from(i64::MIN.to_string()))
        );
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
        engine.set(
            Bytes::from_static(b"a"),
            Value::String(Bytes::from_static(b"1")),
        );
        let result = mget(
            &engine,
            &[Bytes::from_static(b"a"), Bytes::from_static(b"missing")],
        );
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
        engine.set(
            Bytes::from_static(b"a"),
            Value::String(Bytes::from_static(b"existing")),
        );
        let applied = msetnx(
            &engine,
            vec![
                (Bytes::from_static(b"a"), Bytes::from_static(b"new")),
                (Bytes::from_static(b"b"), Bytes::from_static(b"new")),
            ],
        );
        assert!(!applied);
        assert_eq!(
            get(&engine, b"a").unwrap(),
            Some(Bytes::from_static(b"existing"))
        );
        assert_eq!(get(&engine, b"b").unwrap(), None);
    }

    #[test]
    fn msetnx_succeeds_when_no_key_exists() {
        let engine = Engine::new();
        let applied = msetnx(
            &engine,
            vec![(Bytes::from_static(b"a"), Bytes::from_static(b"1"))],
        );
        assert!(applied);
        assert_eq!(get(&engine, b"a").unwrap(), Some(Bytes::from_static(b"1")));
    }
}
