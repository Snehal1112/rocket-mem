use crate::commands::{hash, list, set, sorted_set, string};
use crate::{Engine, Value};
use bytes::Bytes;

fn engine_with_string_key() -> Engine {
    let engine = Engine::new();
    engine.set(
        Bytes::from_static(b"k"),
        Value::String(Bytes::from_static(b"v")),
    );
    engine
}

fn engine_with_hash_key() -> Engine {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::Hash(Default::default()));
    engine
}

fn engine_with_list_key() -> Engine {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::List(Default::default()));
    engine
}

fn engine_with_set_key() -> Engine {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::Set(Default::default()));
    engine
}

macro_rules! assert_wrongtype {
    ($result:expr) => {
        assert_eq!($result.unwrap_err(), common::EngineError::WrongType);
    };
}

#[test]
fn string_commands_reject_non_string_keys() {
    assert_wrongtype!(string::get(&engine_with_hash_key(), b"k"));
    assert_wrongtype!(string::append(
        &engine_with_list_key(),
        Bytes::from_static(b"k"),
        b"x"
    ));
    assert_wrongtype!(string::strlen(&engine_with_set_key(), b"k"));
    assert_wrongtype!(string::incr_by(
        &engine_with_hash_key(),
        Bytes::from_static(b"k"),
        1
    ));
}

#[test]
fn hash_commands_reject_non_hash_keys() {
    assert_wrongtype!(hash::hget(&engine_with_string_key(), b"k", b"f"));
    assert_wrongtype!(hash::hdel(&engine_with_list_key(), b"k", b"f"));
    assert_wrongtype!(hash::hgetall(&engine_with_set_key(), b"k"));
    let e = engine_with_string_key();
    assert_wrongtype!(hash::hset(
        &e,
        Bytes::from_static(b"k"),
        Bytes::from_static(b"f"),
        Bytes::from_static(b"v")
    ));
    assert_wrongtype!(hash::hkeys(&engine_with_string_key(), b"k"));
    let e2 = engine_with_string_key();
    assert_wrongtype!(hash::hincrby(
        &e2,
        Bytes::from_static(b"k"),
        Bytes::from_static(b"f"),
        1
    ));
}

#[test]
fn list_commands_reject_non_list_keys() {
    assert_wrongtype!(list::lrange(&engine_with_string_key(), b"k", 0, -1));
    assert_wrongtype!(list::llen(&engine_with_hash_key(), b"k"));
    assert_wrongtype!(list::rpop(&engine_with_set_key(), b"k"));
    let e = engine_with_string_key();
    assert_wrongtype!(list::rpush(
        &e,
        Bytes::from_static(b"k"),
        Bytes::from_static(b"x")
    ));
    assert_wrongtype!(list::lindex(&engine_with_string_key(), b"k", 0));
}

#[test]
fn set_commands_reject_non_set_keys() {
    assert_wrongtype!(set::smembers(&engine_with_string_key(), b"k"));
    assert_wrongtype!(set::scard(&engine_with_hash_key(), b"k"));
    assert_wrongtype!(set::sismember(&engine_with_list_key(), b"k", b"m"));
    let e = engine_with_string_key();
    assert_wrongtype!(set::sadd(
        &e,
        Bytes::from_static(b"k"),
        Bytes::from_static(b"m")
    ));
}

#[test]
fn sorted_set_commands_reject_non_sorted_set_keys() {
    assert_wrongtype!(sorted_set::zscore(&engine_with_string_key(), b"k", b"m"));
    assert_wrongtype!(sorted_set::zrem(&engine_with_hash_key(), b"k", b"m"));
    assert_wrongtype!(sorted_set::zcard(&engine_with_list_key(), b"k"));
    let e = engine_with_string_key();
    assert_wrongtype!(sorted_set::zadd(
        &e,
        Bytes::from_static(b"k"),
        1.0,
        Bytes::from_static(b"m")
    ));
}
