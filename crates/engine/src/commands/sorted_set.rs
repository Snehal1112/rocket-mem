use crate::{Engine, SortedSet, Value};
use bytes::Bytes;

pub(crate) fn get_zset(engine: &Engine, key: &[u8]) -> Result<SortedSet, common::EngineError> {
    match engine.get(key) {
        None => Ok(SortedSet::new()),
        Some(Value::SortedSet(z)) => Ok(z),
        Some(_) => Err(common::EngineError::WrongType),
    }
}

pub fn zadd(
    engine: &Engine,
    key: Bytes,
    score: f64,
    member: Bytes,
) -> Result<bool, common::EngineError> {
    let mut zset = get_zset(engine, &key)?;
    let is_new = zset.score(&member).is_none();
    zset.insert(member, score);
    engine.set(key, Value::SortedSet(zset));
    Ok(is_new)
}

pub fn zscore(
    engine: &Engine,
    key: &[u8],
    member: &[u8],
) -> Result<Option<f64>, common::EngineError> {
    Ok(get_zset(engine, key)?.score(member))
}

pub fn zrem(engine: &Engine, key: &[u8], member: &[u8]) -> Result<bool, common::EngineError> {
    let mut zset = match engine.get(key) {
        None => return Ok(false),
        Some(Value::SortedSet(z)) => z,
        Some(_) => return Err(common::EngineError::WrongType),
    };
    let removed = zset.remove(member);
    engine.set(Bytes::copy_from_slice(key), Value::SortedSet(zset));
    Ok(removed)
}

pub fn zcard(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    Ok(get_zset(engine, key)?.len())
}

pub fn zincrby(
    engine: &Engine,
    key: Bytes,
    delta: f64,
    member: Bytes,
) -> Result<f64, common::EngineError> {
    let mut zset = get_zset(engine, &key)?;
    let new_score = zset.score(&member).unwrap_or(0.0) + delta;
    zset.insert(member, new_score);
    engine.set(key, Value::SortedSet(zset));
    Ok(new_score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use bytes::Bytes;

    #[test]
    fn zadd_new_member_returns_true() {
        let engine = Engine::new();
        assert!(zadd(
            &engine,
            Bytes::from_static(b"z"),
            5.0,
            Bytes::from_static(b"alice")
        )
        .unwrap());
    }

    #[test]
    fn zadd_existing_member_updates_score_and_returns_false() {
        let engine = Engine::new();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            5.0,
            Bytes::from_static(b"alice"),
        )
        .unwrap();
        let is_new = zadd(
            &engine,
            Bytes::from_static(b"z"),
            9.0,
            Bytes::from_static(b"alice"),
        )
        .unwrap();
        assert!(!is_new);
        assert_eq!(zscore(&engine, b"z", b"alice").unwrap(), Some(9.0));
    }

    #[test]
    fn zscore_on_missing_member_is_none_not_an_error() {
        let engine = Engine::new();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            5.0,
            Bytes::from_static(b"alice"),
        )
        .unwrap();
        assert_eq!(zscore(&engine, b"z", b"bob").unwrap(), None);
    }

    #[test]
    fn zscore_on_missing_key_is_none_not_an_error() {
        let engine = Engine::new();
        assert_eq!(zscore(&engine, b"missing", b"alice").unwrap(), None);
    }

    #[test]
    fn zrem_removes_member_and_reports_it_existed() {
        let engine = Engine::new();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            5.0,
            Bytes::from_static(b"alice"),
        )
        .unwrap();
        assert!(zrem(&engine, b"z", b"alice").unwrap());
        assert!(!zrem(&engine, b"z", b"alice").unwrap());
    }

    #[test]
    fn zcard_counts_members() {
        let engine = Engine::new();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            5.0,
            Bytes::from_static(b"alice"),
        )
        .unwrap();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            2.0,
            Bytes::from_static(b"bob"),
        )
        .unwrap();
        assert_eq!(zcard(&engine, b"z").unwrap(), 2);
    }

    #[test]
    fn zincrby_on_missing_member_starts_from_zero() {
        let engine = Engine::new();
        let score = zincrby(
            &engine,
            Bytes::from_static(b"z"),
            5.0,
            Bytes::from_static(b"alice"),
        )
        .unwrap();
        assert_eq!(score, 5.0);
    }

    #[test]
    fn zincrby_adds_to_the_existing_score() {
        let engine = Engine::new();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            5.0,
            Bytes::from_static(b"alice"),
        )
        .unwrap();
        let score = zincrby(
            &engine,
            Bytes::from_static(b"z"),
            3.0,
            Bytes::from_static(b"alice"),
        )
        .unwrap();
        assert_eq!(score, 8.0);
    }

    #[test]
    fn zadd_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            crate::Value::String(Bytes::from_static(b"v")),
        );
        let err = zadd(
            &engine,
            Bytes::from_static(b"k"),
            1.0,
            Bytes::from_static(b"m"),
        )
        .unwrap_err();
        assert_eq!(err, common::EngineError::WrongType);
    }
}
