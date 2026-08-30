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

/// Adds every member in `members` in one shard-lock acquisition and returns the count newly
/// added (duplicates within `members`, or members already present, don't count). Mutates in
/// place via `with_mut` -- see list.rs's top-of-file note for why this matters. When `key` is
/// missing, an empty `members` leaves it missing rather than creating a phantom empty set --
/// see CLAUDE.md's "Missing key ≠ error" convention.
pub fn sadd(engine: &Engine, key: Bytes, members: Vec<Bytes>) -> Result<i64, common::EngineError> {
    let existed = engine.with_mut_delta(
        &key,
        |existing| -> (Result<Option<i64>, common::EngineError>, isize) {
            match existing {
                Some(Value::Set(set)) => {
                    let mut added = 0i64;
                    let mut size_delta = 0isize;
                    for member in &members {
                        if set.insert(member.clone()) {
                            added += 1;
                            size_delta += member.len() as isize + 8;
                        }
                    }
                    (Ok(Some(added)), size_delta)
                }
                Some(_) => (Err(common::EngineError::WrongType), 0),
                None => (Ok(None), 0),
            }
        },
    )?;
    match existed {
        Some(added) => Ok(added),
        None => {
            if members.is_empty() {
                return Ok(0);
            }
            let set: HashSet<Bytes> = members.into_iter().collect();
            let added = set.len() as i64;
            engine.set(key, Value::Set(set));
            Ok(added)
        }
    }
}

/// Removes every member in `members` in one shard-lock acquisition and returns the count
/// actually removed.
pub fn srem(engine: &Engine, key: &[u8], members: &[Bytes]) -> Result<i64, common::EngineError> {
    engine.with_mut_delta(key, |existing| match existing {
        None => (Ok(0), 0),
        Some(Value::Set(set)) => {
            let mut removed = 0i64;
            let mut size_delta = 0isize;
            for member in members {
                if set.remove(member.as_ref()) {
                    removed += 1;
                    size_delta -= member.len() as isize + 8;
                }
            }
            (Ok(removed), size_delta)
        }
        Some(_) => (Err(common::EngineError::WrongType), 0),
    })
}

pub fn smembers(engine: &Engine, key: &[u8]) -> Result<HashSet<Bytes>, common::EngineError> {
    get_set(engine, key)
}

pub fn sismember(engine: &Engine, key: &[u8], member: &[u8]) -> Result<bool, common::EngineError> {
    engine.with_ref(key, |v| match v {
        None => Ok(false),
        Some(Value::Set(set)) => Ok(set.contains(member)),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn scard(engine: &Engine, key: &[u8]) -> Result<usize, common::EngineError> {
    engine.with_ref(key, |v| match v {
        None => Ok(0),
        Some(Value::Set(set)) => Ok(set.len()),
        Some(_) => Err(common::EngineError::WrongType),
    })
}

pub fn sinter(engine: &Engine, keys: &[Bytes]) -> Result<HashSet<Bytes>, common::EngineError> {
    let mut sets = Vec::with_capacity(keys.len());
    for k in keys {
        sets.push(get_set(engine, k)?);
    }
    let mut iter = sets.into_iter();
    let Some(first) = iter.next() else {
        return Ok(HashSet::new());
    };
    Ok(iter.fold(first, |acc, s| acc.intersection(&s).cloned().collect()))
}

pub fn sunion(engine: &Engine, keys: &[Bytes]) -> Result<HashSet<Bytes>, common::EngineError> {
    let mut result = HashSet::new();
    for k in keys {
        result.extend(get_set(engine, k)?);
    }
    Ok(result)
}

pub fn sdiff(engine: &Engine, keys: &[Bytes]) -> Result<HashSet<Bytes>, common::EngineError> {
    let mut iter = keys.iter();
    let Some(first_key) = iter.next() else {
        return Ok(HashSet::new());
    };
    let mut result = get_set(engine, first_key)?;
    for k in iter {
        let other = get_set(engine, k)?;
        result.retain(|m| !other.contains(m));
    }
    Ok(result)
}

pub fn sinterstore(
    engine: &Engine,
    dest: Bytes,
    keys: &[Bytes],
) -> Result<usize, common::EngineError> {
    let result = sinter(engine, keys)?;
    let len = result.len();
    engine.set(dest, Value::Set(result));
    Ok(len)
}

pub fn sunionstore(
    engine: &Engine,
    dest: Bytes,
    keys: &[Bytes],
) -> Result<usize, common::EngineError> {
    let result = sunion(engine, keys)?;
    let len = result.len();
    engine.set(dest, Value::Set(result));
    Ok(len)
}

pub fn sdiffstore(
    engine: &Engine,
    dest: Bytes,
    keys: &[Bytes],
) -> Result<usize, common::EngineError> {
    let result = sdiff(engine, keys)?;
    let len = result.len();
    engine.set(dest, Value::Set(result));
    Ok(len)
}

pub fn spop(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    use rand::seq::IteratorRandom;
    engine.with_mut_delta(key, |existing| {
        let set = match existing {
            None => return (Ok(None), 0),
            Some(Value::Set(set)) => set,
            Some(_) => return (Err(common::EngineError::WrongType), 0),
        };
        let Some(member) = set.iter().choose(&mut rand::thread_rng()).cloned() else {
            return (Ok(None), 0);
        };
        set.remove(&member);
        let size_delta = -(member.len() as isize + 8);
        (Ok(Some(member)), size_delta)
    })
}

pub fn srandmember(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    use rand::seq::IteratorRandom;
    let set = get_set(engine, key)?;
    Ok(set.into_iter().choose(&mut rand::thread_rng()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use bytes::Bytes;

    #[test]
    fn sadd_then_sismember_is_true() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"s"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        assert!(sismember(&engine, b"s", b"x").unwrap());
        assert!(!sismember(&engine, b"s", b"y").unwrap());
    }

    #[test]
    fn srem_removes_member_and_reports_the_count_removed() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"s"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        assert_eq!(srem(&engine, b"s", &[Bytes::from_static(b"x")]).unwrap(), 1);
        assert_eq!(srem(&engine, b"s", &[Bytes::from_static(b"x")]).unwrap(), 0);
    }

    #[test]
    fn scard_counts_members() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"s"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        sadd(
            &engine,
            Bytes::from_static(b"s"),
            vec![Bytes::from_static(b"y")],
        )
        .unwrap();
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
            sadd(
                &engine,
                Bytes::from_static(b"k"),
                vec![Bytes::from_static(b"x")]
            )
            .unwrap_err(),
            common::EngineError::WrongType
        );
    }

    #[test]
    fn sadd_with_multiple_members_pushes_all_in_one_call_and_returns_count_newly_added() {
        let engine = Engine::new();
        let added = sadd(
            &engine,
            Bytes::from_static(b"s"),
            vec![
                Bytes::from_static(b"x"),
                Bytes::from_static(b"y"),
                Bytes::from_static(b"x"), // duplicate within the same call
            ],
        )
        .unwrap();
        assert_eq!(added, 2);
        assert_eq!(scard(&engine, b"s").unwrap(), 2);
    }

    #[test]
    fn sadd_with_multiple_members_onto_an_existing_set_counts_only_the_new_ones() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"s"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        let added = sadd(
            &engine,
            Bytes::from_static(b"s"),
            vec![Bytes::from_static(b"x"), Bytes::from_static(b"y")],
        )
        .unwrap();
        assert_eq!(added, 1);
        assert_eq!(scard(&engine, b"s").unwrap(), 2);
    }

    #[test]
    fn sadd_with_no_members_on_a_missing_key_does_not_create_a_phantom_set() {
        let engine = Engine::new();
        let added = sadd(&engine, Bytes::from_static(b"s"), vec![]).unwrap();
        assert_eq!(added, 0);
        assert!(!engine.exists(b"s"));
    }

    #[test]
    fn srem_with_multiple_members_removes_all_in_one_call_and_returns_count_removed() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"s"),
            vec![Bytes::from_static(b"x"), Bytes::from_static(b"y")],
        )
        .unwrap();
        let removed = srem(
            &engine,
            b"s",
            &[
                Bytes::from_static(b"x"),
                Bytes::from_static(b"y"),
                Bytes::from_static(b"z"), // never a member
            ],
        )
        .unwrap();
        assert_eq!(removed, 2);
        assert_eq!(scard(&engine, b"s").unwrap(), 0);
    }

    #[test]
    fn sinter_returns_only_members_present_in_every_set() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"a"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        sadd(
            &engine,
            Bytes::from_static(b"a"),
            vec![Bytes::from_static(b"y")],
        )
        .unwrap();
        sadd(
            &engine,
            Bytes::from_static(b"b"),
            vec![Bytes::from_static(b"y")],
        )
        .unwrap();
        sadd(
            &engine,
            Bytes::from_static(b"b"),
            vec![Bytes::from_static(b"z")],
        )
        .unwrap();
        let result = sinter(
            &engine,
            &[Bytes::from_static(b"a"), Bytes::from_static(b"b")],
        )
        .unwrap();
        assert_eq!(result, HashSet::from([Bytes::from_static(b"y")]));
    }

    #[test]
    fn sinter_with_a_missing_key_is_empty() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"a"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        let result = sinter(
            &engine,
            &[Bytes::from_static(b"a"), Bytes::from_static(b"missing")],
        )
        .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn sunion_returns_every_member_from_every_set() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"a"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        sadd(
            &engine,
            Bytes::from_static(b"b"),
            vec![Bytes::from_static(b"y")],
        )
        .unwrap();
        let result = sunion(
            &engine,
            &[Bytes::from_static(b"a"), Bytes::from_static(b"b")],
        )
        .unwrap();
        assert_eq!(
            result,
            HashSet::from([Bytes::from_static(b"x"), Bytes::from_static(b"y")])
        );
    }

    #[test]
    fn sdiff_returns_members_of_the_first_set_absent_from_the_rest() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"a"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        sadd(
            &engine,
            Bytes::from_static(b"a"),
            vec![Bytes::from_static(b"y")],
        )
        .unwrap();
        sadd(
            &engine,
            Bytes::from_static(b"b"),
            vec![Bytes::from_static(b"y")],
        )
        .unwrap();
        let result = sdiff(
            &engine,
            &[Bytes::from_static(b"a"), Bytes::from_static(b"b")],
        )
        .unwrap();
        assert_eq!(result, HashSet::from([Bytes::from_static(b"x")]));
    }

    #[test]
    fn sinterstore_stores_the_result_and_returns_its_size() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"a"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        sadd(
            &engine,
            Bytes::from_static(b"b"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        let len = sinterstore(
            &engine,
            Bytes::from_static(b"dest"),
            &[Bytes::from_static(b"a"), Bytes::from_static(b"b")],
        )
        .unwrap();
        assert_eq!(len, 1);
        assert_eq!(
            smembers(&engine, b"dest").unwrap(),
            HashSet::from([Bytes::from_static(b"x")])
        );
    }

    #[test]
    fn spop_removes_and_returns_a_member() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"s"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        let popped = spop(&engine, b"s").unwrap();
        assert_eq!(popped, Some(Bytes::from_static(b"x")));
        assert_eq!(scard(&engine, b"s").unwrap(), 0);
    }

    #[test]
    fn spop_on_missing_key_returns_none_not_an_error() {
        let engine = Engine::new();
        assert_eq!(spop(&engine, b"missing").unwrap(), None);
    }

    #[test]
    fn srandmember_returns_a_member_without_removing_it() {
        let engine = Engine::new();
        sadd(
            &engine,
            Bytes::from_static(b"s"),
            vec![Bytes::from_static(b"x")],
        )
        .unwrap();
        let picked = srandmember(&engine, b"s").unwrap();
        assert_eq!(picked, Some(Bytes::from_static(b"x")));
        assert_eq!(scard(&engine, b"s").unwrap(), 1);
    }

    #[test]
    fn srandmember_on_missing_key_returns_none_not_an_error() {
        let engine = Engine::new();
        assert_eq!(srandmember(&engine, b"missing").unwrap(), None);
    }

    #[test]
    fn sinter_on_string_key_returns_wrongtype() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        assert_eq!(
            sinter(&engine, &[Bytes::from_static(b"k")]).unwrap_err(),
            common::EngineError::WrongType
        );
    }

    /// `engine.memory_used()` after each mutation must equal independently recomputing the
    /// entry's true size from scratch (`key.len() + Value::approx_size()`) -- proving the
    /// delta each function reports (once converted to `with_mut_delta`) is exactly right.
    /// Written to pass against today's `with_mut`-based code too, since this is a refactor,
    /// not a bug fix -- it stays green through every step below.
    fn assert_memory_used_matches_recomputed_size(engine: &Engine, key: &[u8]) {
        let value = engine.get(key).expect("key must exist");
        let expected = key.len() + value.approx_size();
        assert_eq!(engine.memory_used(), expected);
    }

    #[test]
    fn set_mutations_keep_memory_used_exactly_in_sync() {
        let engine = Engine::new();
        let key = Bytes::from_static(b"s");

        // sadd: two new members in one call, plus a duplicate within the same call that must
        // not be double-counted
        sadd(
            &engine,
            key.clone(),
            vec![
                Bytes::from_static(b"x"),
                Bytes::from_static(b"y"),
                Bytes::from_static(b"x"),
            ],
        )
        .unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // sadd: mix of an already-present member and a genuinely new one
        sadd(
            &engine,
            key.clone(),
            vec![Bytes::from_static(b"x"), Bytes::from_static(b"z")],
        )
        .unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // srem: mix of a present member and one that was never a member
        srem(
            &engine,
            &key,
            &[Bytes::from_static(b"y"), Bytes::from_static(b"never-there")],
        )
        .unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // spop: removes one remaining member (x or z)
        spop(&engine, &key).unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);

        // spop: on the single remaining member
        spop(&engine, &key).unwrap();
        assert_memory_used_matches_recomputed_size(&engine, &key);
    }
}
