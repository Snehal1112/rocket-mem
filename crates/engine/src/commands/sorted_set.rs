use crate::{Engine, SortedSet, Value};
use bytes::Bytes;

/// Mutates in place via `with_mut` -- see list.rs's top-of-file note for why this matters.
pub fn zadd(
    engine: &Engine,
    key: Bytes,
    score: f64,
    member: Bytes,
) -> Result<bool, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<bool>, common::EngineError> {
            match existing {
                Some(Value::SortedSet(zset)) => {
                    let is_new = zset.score(&member).is_none();
                    zset.insert(member.clone(), score);
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
            let mut zset = SortedSet::new();
            zset.insert(member, score);
            engine.set(key, Value::SortedSet(zset));
            Ok(true)
        }
    }
}

pub fn zscore(
    engine: &Engine,
    key: &[u8],
    member: &[u8],
) -> Result<Option<f64>, common::EngineError> {
    engine.with_ref(key, |v| match v {
        None => Ok(None),
        Some(Value::SortedSet(zset)) => Ok(zset.score(member)),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn zrem(engine: &Engine, key: &[u8], member: &[u8]) -> Result<bool, common::EngineError> {
    engine.with_mut(key, |existing| match existing {
        None => Ok(false),
        Some(Value::SortedSet(zset)) => Ok(zset.remove(member)),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn zcard(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    engine.with_ref(key, |v| match v {
        None => Ok(0),
        Some(Value::SortedSet(zset)) => Ok(zset.len()),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn zincrby(
    engine: &Engine,
    key: Bytes,
    delta: f64,
    member: Bytes,
) -> Result<f64, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<f64>, common::EngineError> {
            match existing {
                Some(Value::SortedSet(zset)) => {
                    let new_score = zset.score(&member).unwrap_or(0.0) + delta;
                    zset.insert(member.clone(), new_score);
                    Ok(Some(new_score))
                }
                Some(_) => Err(common::EngineError::WrongType),
                None => Ok(None),
            }
        },
    )?;
    match existed {
        Some(new_score) => Ok(new_score),
        None => {
            let mut zset = SortedSet::new();
            zset.insert(member, delta);
            engine.set(key, Value::SortedSet(zset));
            Ok(delta)
        }
    }
}

/// start/stop follow the same negative-index Redis semantics as `list::lrange`.
pub fn zrange(
    engine: &Engine,
    key: &[u8],
    start: i64,
    stop: i64,
) -> Result<Vec<Bytes>, common::EngineError> {
    engine.with_ref(key, |v| {
        let zset = match v {
            None => return Ok(Vec::new()),
            Some(Value::SortedSet(zset)) => zset,
            Some(_) => return Err(common::EngineError::WrongType),
        };
        let len = zset.len() as i64;
        let norm = |i: i64| -> i64 {
            if i < 0 {
                (len + i).max(0)
            } else {
                i.min(len)
            }
        };
        let (s, e) = (norm(start), norm(stop) + 1);
        if s >= e {
            return Ok(Vec::new());
        }
        Ok(zset
            .members_ascending()
            .skip(s as usize)
            .take((e - s) as usize)
            .cloned()
            .collect())
    })
}

pub fn zrank(
    engine: &Engine,
    key: &[u8],
    member: &[u8],
) -> Result<Option<usize>, common::EngineError> {
    engine.with_ref(key, |v| match v {
        None => Ok(None),
        Some(Value::SortedSet(zset)) => {
            Ok(zset.members_ascending().position(|m| m.as_ref() == member))
        }
        Some(_) => Err(common::EngineError::WrongType),
    })
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

    #[test]
    fn zrange_returns_members_ordered_by_score_ascending() {
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
        let result = zrange(&engine, b"z", 0, -1).unwrap();
        assert_eq!(
            result,
            vec![Bytes::from_static(b"bob"), Bytes::from_static(b"alice")]
        );
    }

    #[test]
    fn zrange_with_tied_scores_breaks_ties_lexicographically_by_member() {
        let engine = Engine::new();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            2.0,
            Bytes::from_static(b"carol"),
        )
        .unwrap();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            2.0,
            Bytes::from_static(b"bob"),
        )
        .unwrap();
        let result = zrange(&engine, b"z", 0, -1).unwrap();
        assert_eq!(
            result,
            vec![Bytes::from_static(b"bob"), Bytes::from_static(b"carol")]
        );
    }

    #[test]
    fn zrange_supports_a_partial_slice() {
        let engine = Engine::new();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            1.0,
            Bytes::from_static(b"a"),
        )
        .unwrap();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            2.0,
            Bytes::from_static(b"b"),
        )
        .unwrap();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            3.0,
            Bytes::from_static(b"c"),
        )
        .unwrap();
        assert_eq!(
            zrange(&engine, b"z", 0, 0).unwrap(),
            vec![Bytes::from_static(b"a")]
        );
        assert_eq!(
            zrange(&engine, b"z", -2, -1).unwrap(),
            vec![Bytes::from_static(b"b"), Bytes::from_static(b"c")]
        );
    }

    #[test]
    fn zrange_on_missing_key_is_empty_not_an_error() {
        let engine = Engine::new();
        assert!(zrange(&engine, b"missing", 0, -1).unwrap().is_empty());
    }

    #[test]
    fn zrank_returns_zero_based_position_in_ascending_order() {
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
        assert_eq!(zrank(&engine, b"z", b"bob").unwrap(), Some(0));
        assert_eq!(zrank(&engine, b"z", b"alice").unwrap(), Some(1));
    }

    #[test]
    fn zrank_on_missing_member_is_none_not_an_error() {
        let engine = Engine::new();
        zadd(
            &engine,
            Bytes::from_static(b"z"),
            5.0,
            Bytes::from_static(b"alice"),
        )
        .unwrap();
        assert_eq!(zrank(&engine, b"z", b"missing").unwrap(), None);
    }

    #[test]
    fn zrange_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            crate::Value::String(Bytes::from_static(b"v")),
        );
        assert_eq!(
            zrange(&engine, b"k", 0, -1).unwrap_err(),
            common::EngineError::WrongType
        );
    }
}
