use crate::{Engine, TtlStatus};
use bytes::Bytes;
use std::time::Instant;

pub fn rename(engine: &Engine, src: &[u8], dst: Bytes) -> Result<(), common::EngineError> {
    let val = engine.get(src).ok_or(common::EngineError::NoSuchKey)?;
    if src == dst.as_ref() {
        return Ok(());
    }
    // Read the source's TTL before `del` removes the entry -- and thus its TTL -- entirely.
    let ttl = engine.ttl(src);
    engine.set(dst.clone(), val);
    engine.del(src);
    // `engine.set` already cleared any TTL the destination previously had, matching Redis:
    // RENAME's destination always ends up exactly matching the source's TTL state. Only a
    // source that carried a remaining TTL needs it re-applied to the destination.
    if let TtlStatus::Remaining(remaining) = ttl {
        engine.expire_at(&dst, Instant::now() + remaining);
    }
    Ok(())
}

pub fn renamenx(engine: &Engine, src: &[u8], dst: Bytes) -> Result<bool, common::EngineError> {
    let val = engine.get(src).ok_or(common::EngineError::NoSuchKey)?;
    if engine.exists(&dst) {
        return Ok(false);
    }
    // Read the source's TTL before `del` removes the entry -- and thus its TTL -- entirely.
    let ttl = engine.ttl(src);
    engine.set(dst.clone(), val);
    engine.del(src);
    if let TtlStatus::Remaining(remaining) = ttl {
        engine.expire_at(&dst, Instant::now() + remaining);
    }
    Ok(true)
}

pub fn key_type(engine: &Engine, key: &[u8]) -> &'static str {
    match engine.get(key) {
        None => "none",
        Some(v) => v.type_name(),
    }
}

pub fn randomkey(engine: &Engine) -> Option<Bytes> {
    use rand::Rng;
    let keys = engine.keys();
    if keys.is_empty() {
        return None;
    }
    let idx = rand::thread_rng().gen_range(0..keys.len());
    Some(keys[idx].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, TtlStatus, Value};
    use bytes::Bytes;
    use std::time::{Duration, Instant};

    #[test]
    fn rename_moves_the_value_and_removes_the_source() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"src"),
            Value::String(Bytes::from_static(b"v")),
        );
        rename(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        assert!(!engine.exists(b"src"));
        assert_eq!(
            engine.get(b"dst"),
            Some(Value::String(Bytes::from_static(b"v")))
        );
    }

    #[test]
    fn rename_on_missing_source_returns_no_such_key() {
        let engine = Engine::new();
        let err = rename(&engine, b"missing", Bytes::from_static(b"dst")).unwrap_err();
        assert_eq!(err, common::EngineError::NoSuchKey);
    }

    #[test]
    fn rename_to_itself_is_a_no_op_success() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        rename(&engine, b"k", Bytes::from_static(b"k")).unwrap();
        assert_eq!(
            engine.get(b"k"),
            Some(Value::String(Bytes::from_static(b"v")))
        );
    }

    #[test]
    fn rename_overwrites_an_existing_destination() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"src"),
            Value::String(Bytes::from_static(b"new")),
        );
        engine.set(
            Bytes::from_static(b"dst"),
            Value::String(Bytes::from_static(b"old")),
        );
        rename(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        assert_eq!(
            engine.get(b"dst"),
            Some(Value::String(Bytes::from_static(b"new")))
        );
    }

    #[test]
    fn renamenx_fails_without_error_when_destination_exists() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"src"),
            Value::String(Bytes::from_static(b"v")),
        );
        engine.set(
            Bytes::from_static(b"dst"),
            Value::String(Bytes::from_static(b"existing")),
        );
        let applied = renamenx(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        assert!(!applied);
        assert_eq!(
            engine.get(b"dst"),
            Some(Value::String(Bytes::from_static(b"existing")))
        );
        assert!(engine.exists(b"src"));
    }

    #[test]
    fn renamenx_succeeds_when_destination_is_free() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"src"),
            Value::String(Bytes::from_static(b"v")),
        );
        let applied = renamenx(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        assert!(applied);
        assert!(!engine.exists(b"src"));
        assert_eq!(
            engine.get(b"dst"),
            Some(Value::String(Bytes::from_static(b"v")))
        );
    }

    #[test]
    fn renamenx_on_missing_source_returns_no_such_key() {
        let engine = Engine::new();
        let err = renamenx(&engine, b"missing", Bytes::from_static(b"dst")).unwrap_err();
        assert_eq!(err, common::EngineError::NoSuchKey);
    }

    #[test]
    fn rename_moves_the_sources_ttl_to_the_destination() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"src"),
            Value::String(Bytes::from_static(b"v")),
        );
        engine.expire_at(b"src", Instant::now() + Duration::from_secs(100));
        rename(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        match engine.ttl(b"dst") {
            TtlStatus::Remaining(d) => {
                assert!(d > Duration::from_secs(55) && d <= Duration::from_secs(100))
            }
            other => panic!("expected Remaining, got {other:?}"),
        }
    }

    #[test]
    fn rename_of_a_source_with_no_ttl_leaves_the_destination_with_no_ttl() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"src"),
            Value::String(Bytes::from_static(b"v")),
        );
        engine.set(
            Bytes::from_static(b"dst"),
            Value::String(Bytes::from_static(b"old")),
        );
        engine.expire_at(b"dst", Instant::now() + Duration::from_secs(100));
        rename(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        assert_eq!(engine.ttl(b"dst"), TtlStatus::NoExpiry);
    }

    #[test]
    fn renamenx_moves_the_sources_ttl_to_the_destination() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"src"),
            Value::String(Bytes::from_static(b"v")),
        );
        engine.expire_at(b"src", Instant::now() + Duration::from_secs(100));
        renamenx(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        match engine.ttl(b"dst") {
            TtlStatus::Remaining(d) => {
                assert!(d > Duration::from_secs(55) && d <= Duration::from_secs(100))
            }
            other => panic!("expected Remaining, got {other:?}"),
        }
    }

    #[test]
    fn renamenx_of_a_source_with_no_ttl_leaves_the_destination_with_no_ttl() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"src"),
            Value::String(Bytes::from_static(b"v")),
        );
        renamenx(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        assert_eq!(engine.ttl(b"dst"), TtlStatus::NoExpiry);
    }

    #[test]
    fn key_type_reports_none_for_a_missing_key() {
        let engine = Engine::new();
        assert_eq!(key_type(&engine, b"missing"), "none");
    }

    #[test]
    fn key_type_reports_the_real_type_name_for_each_variant() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"s"),
            Value::String(Bytes::from_static(b"v")),
        );
        engine.set(Bytes::from_static(b"h"), Value::Hash(Default::default()));
        assert_eq!(key_type(&engine, b"s"), "string");
        assert_eq!(key_type(&engine, b"h"), "hash");
    }

    #[test]
    fn randomkey_on_empty_keyspace_returns_none() {
        let engine = Engine::new();
        assert_eq!(randomkey(&engine), None);
    }

    #[test]
    fn randomkey_returns_one_of_the_existing_keys() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"a"),
            Value::String(Bytes::from_static(b"1")),
        );
        engine.set(
            Bytes::from_static(b"b"),
            Value::String(Bytes::from_static(b"2")),
        );
        let picked = randomkey(&engine).unwrap();
        assert!(picked == Bytes::from_static(b"a") || picked == Bytes::from_static(b"b"));
    }
}
