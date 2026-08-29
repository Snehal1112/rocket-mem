use crate::{Engine, Value};
use bytes::Bytes;
use std::collections::VecDeque;

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
pub fn lrange(
    engine: &Engine,
    key: &[u8],
    start: i64,
    stop: i64,
) -> Result<Vec<Bytes>, common::EngineError> {
    let list = get_list(engine, key)?;
    let len = list.len() as i64;
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
    Ok(list
        .into_iter()
        .skip(s as usize)
        .take((e - s) as usize)
        .collect())
}

pub fn lindex(
    engine: &Engine,
    key: &[u8],
    index: i64,
) -> Result<Option<Bytes>, common::EngineError> {
    let list = get_list(engine, key)?;
    let len = list.len() as i64;
    let idx = if index < 0 { len + index } else { index };
    if idx < 0 || idx >= len {
        return Ok(None);
    }
    Ok(list.get(idx as usize).cloned())
}

pub fn lset(
    engine: &Engine,
    key: Bytes,
    index: i64,
    val: Bytes,
) -> Result<bool, common::EngineError> {
    let mut list = get_list(engine, &key)?;
    let len = list.len() as i64;
    let idx = if index < 0 { len + index } else { index };
    if idx < 0 || idx >= len {
        return Ok(false);
    }
    list[idx as usize] = val;
    engine.set(key, Value::List(list));
    Ok(true)
}

pub fn ltrim(
    engine: &Engine,
    key: Bytes,
    start: i64,
    stop: i64,
) -> Result<(), common::EngineError> {
    let trimmed = lrange(engine, &key, start, stop)?;
    engine.set(key, Value::List(trimmed.into_iter().collect()));
    Ok(())
}

/// Removes occurrences of `val`: `count > 0` removes up to `count` from the head,
/// `count < 0` removes up to `-count` from the tail, `count == 0` removes every occurrence.
/// Returns the number actually removed.
pub fn lrem(
    engine: &Engine,
    key: Bytes,
    count: i64,
    val: &[u8],
) -> Result<usize, common::EngineError> {
    let list = get_list(engine, &key)?;
    let mut removed = 0usize;
    let new_list: VecDeque<Bytes> = if count >= 0 {
        let mut remaining = if count == 0 {
            usize::MAX
        } else {
            count as usize
        };
        let mut items: Vec<Bytes> = list.into_iter().collect();
        items.retain(|item| {
            if remaining > 0 && item.as_ref() == val {
                remaining -= 1;
                removed += 1;
                false
            } else {
                true
            }
        });
        items.into_iter().collect()
    } else {
        let mut remaining = (-count) as usize;
        let mut items: Vec<Bytes> = list.into_iter().rev().collect();
        items.retain(|item| {
            if remaining > 0 && item.as_ref() == val {
                remaining -= 1;
                removed += 1;
                false
            } else {
                true
            }
        });
        items.into_iter().rev().collect()
    };
    engine.set(key, Value::List(new_list));
    Ok(removed)
}

pub fn linsert(
    engine: &Engine,
    key: Bytes,
    before: bool,
    pivot: &[u8],
    val: Bytes,
) -> Result<i64, common::EngineError> {
    if !engine.exists(&key) {
        return Ok(0);
    }
    let mut list = get_list(engine, &key)?;
    let Some(pos) = list.iter().position(|item| item.as_ref() == pivot) else {
        return Ok(-1);
    };
    let insert_at = if before { pos } else { pos + 1 };
    list.insert(insert_at, val);
    let len = list.len() as i64;
    engine.set(key, Value::List(list));
    Ok(len)
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
        assert_eq!(
            items,
            vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]
        );
    }

    #[test]
    fn lpush_prepends() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b")).unwrap();
        lpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        let items = lrange(&engine, b"l", 0, -1).unwrap();
        assert_eq!(
            items,
            vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]
        );
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
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert_eq!(
            rpush(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"x")).unwrap_err(),
            common::EngineError::WrongType
        );
    }

    #[test]
    fn lpush_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert_eq!(
            lpush(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"x")).unwrap_err(),
            common::EngineError::WrongType
        );
    }

    #[test]
    fn lindex_returns_the_element_at_a_positive_index() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b")).unwrap();
        assert_eq!(
            lindex(&engine, b"l", 1).unwrap(),
            Some(Bytes::from_static(b"b"))
        );
    }

    #[test]
    fn lindex_supports_negative_indices() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"b")).unwrap();
        assert_eq!(
            lindex(&engine, b"l", -1).unwrap(),
            Some(Bytes::from_static(b"b"))
        );
    }

    #[test]
    fn lindex_out_of_range_returns_none_not_an_error() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        assert_eq!(lindex(&engine, b"l", 5).unwrap(), None);
    }

    #[test]
    fn lset_replaces_the_element_at_index_and_reports_success() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        assert!(lset(
            &engine,
            Bytes::from_static(b"l"),
            0,
            Bytes::from_static(b"z")
        )
        .unwrap());
        assert_eq!(
            lindex(&engine, b"l", 0).unwrap(),
            Some(Bytes::from_static(b"z"))
        );
    }

    #[test]
    fn lset_out_of_range_returns_false_not_an_error() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        assert!(!lset(
            &engine,
            Bytes::from_static(b"l"),
            5,
            Bytes::from_static(b"z")
        )
        .unwrap());
    }

    #[test]
    fn ltrim_keeps_only_the_requested_range() {
        let engine = Engine::new();
        for v in [b"a", b"b", b"c", b"d"] {
            rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(v)).unwrap();
        }
        ltrim(&engine, Bytes::from_static(b"l"), 1, 2).unwrap();
        assert_eq!(
            lrange(&engine, b"l", 0, -1).unwrap(),
            vec![Bytes::from_static(b"b"), Bytes::from_static(b"c")]
        );
    }

    #[test]
    fn lrem_positive_count_removes_from_head_up_to_count() {
        let engine = Engine::new();
        for v in [b"a", b"x", b"b", b"x", b"c"] {
            rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(v)).unwrap();
        }
        let removed = lrem(&engine, Bytes::from_static(b"l"), 1, b"x").unwrap();
        assert_eq!(removed, 1);
        assert_eq!(
            lrange(&engine, b"l", 0, -1).unwrap(),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"x"),
                Bytes::from_static(b"c"),
            ]
        );
    }

    #[test]
    fn lrem_negative_count_removes_from_tail_up_to_count() {
        let engine = Engine::new();
        for v in [b"a", b"x", b"b", b"x", b"c"] {
            rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(v)).unwrap();
        }
        let removed = lrem(&engine, Bytes::from_static(b"l"), -1, b"x").unwrap();
        assert_eq!(removed, 1);
        assert_eq!(
            lrange(&engine, b"l", 0, -1).unwrap(),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"x"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"c"),
            ]
        );
    }

    #[test]
    fn lrem_zero_count_removes_every_occurrence() {
        let engine = Engine::new();
        for v in [b"a", b"x", b"b", b"x", b"c"] {
            rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(v)).unwrap();
        }
        let removed = lrem(&engine, Bytes::from_static(b"l"), 0, b"x").unwrap();
        assert_eq!(removed, 2);
        assert_eq!(
            lrange(&engine, b"l", 0, -1).unwrap(),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"c")
            ]
        );
    }

    #[test]
    fn linsert_before_pivot_shifts_the_pivot_right() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"c")).unwrap();
        let len = linsert(
            &engine,
            Bytes::from_static(b"l"),
            true,
            b"c",
            Bytes::from_static(b"b"),
        )
        .unwrap();
        assert_eq!(len, 3);
        assert_eq!(
            lrange(&engine, b"l", 0, -1).unwrap(),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"c")
            ]
        );
    }

    #[test]
    fn linsert_pivot_not_found_returns_negative_one() {
        let engine = Engine::new();
        rpush(&engine, Bytes::from_static(b"l"), Bytes::from_static(b"a")).unwrap();
        assert_eq!(
            linsert(
                &engine,
                Bytes::from_static(b"l"),
                true,
                b"missing",
                Bytes::from_static(b"x")
            )
            .unwrap(),
            -1
        );
    }

    #[test]
    fn linsert_on_missing_key_returns_zero() {
        let engine = Engine::new();
        assert_eq!(
            linsert(
                &engine,
                Bytes::from_static(b"missing"),
                true,
                b"pivot",
                Bytes::from_static(b"x")
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn lindex_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert_eq!(
            lindex(&engine, b"k", 0).unwrap_err(),
            common::EngineError::WrongType
        );
    }
}
