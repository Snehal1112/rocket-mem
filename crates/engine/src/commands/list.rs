use crate::{Engine, Value};
use bytes::Bytes;
use std::collections::VecDeque;

// Every function below reads/mutates a list in place via `Engine::with_ref`/`with_mut`
// instead of cloning the whole `VecDeque` out and writing a replacement back -- the old
// clone-mutate-writeback pattern made every single-element push/pop O(current list length)
// instead of O(1), which compounds into O(n²) total cost for n sequential pushes to one key
// (confirmed by `redis-benchmark`: a single-key LPUSH benchmark visibly degraded over time
// and didn't finish 100k requests in 60s before this fix).

pub fn rpush(
    engine: &Engine,
    key: Bytes,
    values: Vec<Bytes>,
) -> Result<usize, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<usize>, common::EngineError> {
            match existing {
                Some(Value::List(list)) => {
                    for val in &values {
                        list.push_back(val.clone()); // Bytes clone is O(1), not a deep copy
                    }
                    Ok(Some(list.len()))
                }
                Some(_) => Err(common::EngineError::WrongType),
                None => Ok(None),
            }
        },
    )?;
    match existed {
        Some(len) => Ok(len),
        None => {
            // An empty `values` on a missing key is a no-op -- it must not create a
            // phantom empty list, matching the missing-key convention every other
            // mutating command follows (see missing_key_semantics_tests.rs).
            if values.is_empty() {
                return Ok(0);
            }
            let list: VecDeque<Bytes> = values.into_iter().collect();
            let len = list.len();
            engine.set(key, Value::List(list));
            Ok(len)
        }
    }
}

pub fn lpush(
    engine: &Engine,
    key: Bytes,
    values: Vec<Bytes>,
) -> Result<usize, common::EngineError> {
    let existed = engine.with_mut(
        &key,
        |existing| -> Result<Option<usize>, common::EngineError> {
            match existing {
                Some(Value::List(list)) => {
                    for val in &values {
                        list.push_front(val.clone());
                    }
                    Ok(Some(list.len()))
                }
                Some(_) => Err(common::EngineError::WrongType),
                None => Ok(None),
            }
        },
    )?;
    match existed {
        Some(len) => Ok(len),
        None => {
            // An empty `values` on a missing key is a no-op -- it must not create a
            // phantom empty list, matching the missing-key convention every other
            // mutating command follows (see missing_key_semantics_tests.rs).
            if values.is_empty() {
                return Ok(0);
            }
            // LPUSH with multiple values prepends each in argument order, so the *last*
            // argument ends up first (matches the dispatcher-level test and real Redis) --
            // pushing onto a fresh VecDeque front-to-back in argument order achieves that
            // directly, so no reversal is needed here.
            let mut list = VecDeque::new();
            for val in values {
                list.push_front(val);
            }
            let len = list.len();
            engine.set(key, Value::List(list));
            Ok(len)
        }
    }
}

pub fn rpop(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    engine.with_mut(key, |existing| match existing {
        None => Ok(None),
        Some(Value::List(list)) => Ok(list.pop_back()),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn lpop(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    engine.with_mut(key, |existing| match existing {
        None => Ok(None),
        Some(Value::List(list)) => Ok(list.pop_front()),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn llen(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    engine.with_ref(key, |v| match v {
        None => Ok(0),
        Some(Value::List(list)) => Ok(list.len()),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

/// start/stop follow Redis semantics: negative indices count from the end, -1 is the last element.
pub fn lrange(
    engine: &Engine,
    key: &[u8],
    start: i64,
    stop: i64,
) -> Result<Vec<Bytes>, common::EngineError> {
    engine.with_ref(key, |v| {
        let list = match v {
            None => return Ok(Vec::new()),
            Some(Value::List(list)) => list,
            Some(_) => return Err(common::EngineError::WrongType),
        };
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
        // Only the requested slice gets cloned, not the whole list -- e.g. `LRANGE key 0 9`
        // on a million-element list clones 10 Bytes handles, not a million.
        Ok(list
            .iter()
            .skip(s as usize)
            .take((e - s) as usize)
            .cloned()
            .collect())
    })
}

pub fn lindex(
    engine: &Engine,
    key: &[u8],
    index: i64,
) -> Result<Option<Bytes>, common::EngineError> {
    engine.with_ref(key, |v| {
        let list = match v {
            None => return Ok(None),
            Some(Value::List(list)) => list,
            Some(_) => return Err(common::EngineError::WrongType),
        };
        let len = list.len() as i64;
        let idx = if index < 0 { len + index } else { index };
        if idx < 0 || idx >= len {
            return Ok(None);
        }
        Ok(list.get(idx as usize).cloned())
    })
}

pub fn lset(
    engine: &Engine,
    key: Bytes,
    index: i64,
    val: Bytes,
) -> Result<bool, common::EngineError> {
    engine.with_mut(&key, |existing| {
        let list = match existing {
            None => return Ok(false),
            Some(Value::List(list)) => list,
            Some(_) => return Err(common::EngineError::WrongType),
        };
        let len = list.len() as i64;
        let idx = if index < 0 { len + index } else { index };
        if idx < 0 || idx >= len {
            return Ok(false);
        }
        list[idx as usize] = val;
        Ok(true)
    })
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
    engine.with_mut(&key, |existing| {
        let list = match existing {
            None => return Ok(0),
            Some(Value::List(list)) => list,
            Some(_) => return Err(common::EngineError::WrongType),
        };
        let mut removed = 0usize;
        if count >= 0 {
            let mut remaining = if count == 0 {
                usize::MAX
            } else {
                count as usize
            };
            list.retain(|item| {
                if remaining > 0 && item.as_ref() == val {
                    remaining -= 1;
                    removed += 1;
                    false
                } else {
                    true
                }
            });
        } else {
            let mut remaining = (-count) as usize;
            // Scan from the tail; indices come out highest-first, which is exactly the
            // order VecDeque::remove needs so earlier removals don't shift later ones.
            let mut to_remove = Vec::new();
            for (idx, item) in list.iter().enumerate().rev() {
                if remaining == 0 {
                    break;
                }
                if item.as_ref() == val {
                    to_remove.push(idx);
                    remaining -= 1;
                }
            }
            removed = to_remove.len();
            for idx in to_remove {
                list.remove(idx);
            }
        }
        Ok(removed)
    })
}

pub fn linsert(
    engine: &Engine,
    key: Bytes,
    before: bool,
    pivot: &[u8],
    val: Bytes,
) -> Result<i64, common::EngineError> {
    engine.with_mut(&key, |existing| {
        let list = match existing {
            None => return Ok(0),
            Some(Value::List(list)) => list,
            Some(_) => return Err(common::EngineError::WrongType),
        };
        let Some(pos) = list.iter().position(|item| item.as_ref() == pivot) else {
            return Ok(-1);
        };
        let insert_at = if before { pos } else { pos + 1 };
        list.insert(insert_at, val);
        Ok(list.len() as i64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use bytes::Bytes;

    #[test]
    fn rpush_then_lrange_returns_in_insertion_order() {
        let engine = Engine::new();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"a")],
        )
        .unwrap();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"b")],
        )
        .unwrap();
        let items = lrange(&engine, b"l", 0, -1).unwrap();
        assert_eq!(
            items,
            vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]
        );
    }

    #[test]
    fn lpush_prepends() {
        let engine = Engine::new();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"b")],
        )
        .unwrap();
        lpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"a")],
        )
        .unwrap();
        let items = lrange(&engine, b"l", 0, -1).unwrap();
        assert_eq!(
            items,
            vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")]
        );
    }

    #[test]
    fn rpush_with_multiple_values_pushes_all_in_one_call_in_order() {
        let engine = Engine::new();
        let len = rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"c"),
            ],
        )
        .unwrap();
        assert_eq!(len, 3);
        assert_eq!(
            lrange(&engine, b"l", 0, -1).unwrap(),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"c"),
            ]
        );
    }

    #[test]
    fn lpush_with_multiple_values_prepends_each_so_the_last_argument_ends_up_first() {
        let engine = Engine::new();
        let len = lpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"c"),
            ],
        )
        .unwrap();
        assert_eq!(len, 3);
        assert_eq!(
            lrange(&engine, b"l", 0, -1).unwrap(),
            vec![
                Bytes::from_static(b"c"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"a"),
            ]
        );
    }

    #[test]
    fn rpush_multiple_values_onto_an_existing_list_appends_after_the_existing_tail() {
        let engine = Engine::new();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"a")],
        )
        .unwrap();
        let len = rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"b"), Bytes::from_static(b"c")],
        )
        .unwrap();
        assert_eq!(len, 3);
        assert_eq!(
            lrange(&engine, b"l", 0, -1).unwrap(),
            vec![
                Bytes::from_static(b"a"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"c"),
            ]
        );
    }

    #[test]
    fn rpush_with_no_values_on_a_missing_key_does_not_create_a_phantom_list() {
        let engine = Engine::new();
        let len = rpush(&engine, Bytes::from_static(b"l"), vec![]).unwrap();
        assert_eq!(len, 0);
        assert!(!engine.exists(b"l"));
    }

    #[test]
    fn lpush_with_no_values_on_a_missing_key_does_not_create_a_phantom_list() {
        let engine = Engine::new();
        let len = lpush(&engine, Bytes::from_static(b"l"), vec![]).unwrap();
        assert_eq!(len, 0);
        assert!(!engine.exists(b"l"));
    }

    #[test]
    fn rpop_returns_and_removes_last_element() {
        let engine = Engine::new();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"a")],
        )
        .unwrap();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"b")],
        )
        .unwrap();
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
            rpush(
                &engine,
                Bytes::from_static(b"k"),
                vec![Bytes::from_static(b"x")]
            )
            .unwrap_err(),
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
            lpush(
                &engine,
                Bytes::from_static(b"k"),
                vec![Bytes::from_static(b"x")]
            )
            .unwrap_err(),
            common::EngineError::WrongType
        );
    }

    #[test]
    fn lindex_returns_the_element_at_a_positive_index() {
        let engine = Engine::new();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"a")],
        )
        .unwrap();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"b")],
        )
        .unwrap();
        assert_eq!(
            lindex(&engine, b"l", 1).unwrap(),
            Some(Bytes::from_static(b"b"))
        );
    }

    #[test]
    fn lindex_supports_negative_indices() {
        let engine = Engine::new();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"a")],
        )
        .unwrap();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"b")],
        )
        .unwrap();
        assert_eq!(
            lindex(&engine, b"l", -1).unwrap(),
            Some(Bytes::from_static(b"b"))
        );
    }

    #[test]
    fn lindex_out_of_range_returns_none_not_an_error() {
        let engine = Engine::new();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"a")],
        )
        .unwrap();
        assert_eq!(lindex(&engine, b"l", 5).unwrap(), None);
    }

    #[test]
    fn lset_replaces_the_element_at_index_and_reports_success() {
        let engine = Engine::new();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"a")],
        )
        .unwrap();
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
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"a")],
        )
        .unwrap();
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
            rpush(
                &engine,
                Bytes::from_static(b"l"),
                vec![Bytes::from_static(v)],
            )
            .unwrap();
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
            rpush(
                &engine,
                Bytes::from_static(b"l"),
                vec![Bytes::from_static(v)],
            )
            .unwrap();
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
            rpush(
                &engine,
                Bytes::from_static(b"l"),
                vec![Bytes::from_static(v)],
            )
            .unwrap();
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
            rpush(
                &engine,
                Bytes::from_static(b"l"),
                vec![Bytes::from_static(v)],
            )
            .unwrap();
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
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"a")],
        )
        .unwrap();
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"c")],
        )
        .unwrap();
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
        rpush(
            &engine,
            Bytes::from_static(b"l"),
            vec![Bytes::from_static(b"a")],
        )
        .unwrap();
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
