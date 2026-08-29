use crate::commands::{hash, list, set, sorted_set, string};
use crate::Engine;

#[test]
fn missing_key_reads_return_empty_or_none_not_errors() {
    let engine = Engine::new();
    assert_eq!(string::get(&engine, b"missing").unwrap(), None);
    assert_eq!(string::strlen(&engine, b"missing").unwrap(), 0);
    assert!(hash::hgetall(&engine, b"missing").unwrap().is_empty());
    assert_eq!(hash::hlen(&engine, b"missing").unwrap(), 0);
    assert!(list::lrange(&engine, b"missing", 0, -1).unwrap().is_empty());
    assert_eq!(list::llen(&engine, b"missing").unwrap(), 0);
    assert_eq!(list::lpop(&engine, b"missing").unwrap(), None);
    assert!(set::smembers(&engine, b"missing").unwrap().is_empty());
    assert_eq!(set::scard(&engine, b"missing").unwrap(), 0);
    assert!(!set::sismember(&engine, b"missing", b"x").unwrap());
    assert_eq!(sorted_set::zscore(&engine, b"missing", b"m").unwrap(), None);
    assert_eq!(sorted_set::zcard(&engine, b"missing").unwrap(), 0);
}

#[test]
fn deleting_a_missing_key_reports_false_not_an_error() {
    let engine = Engine::new();
    assert!(!engine.del(b"missing"));
    assert_eq!(hash::hdel(&engine, b"missing", b"f").unwrap(), false);
    assert_eq!(set::srem(&engine, b"missing", b"m").unwrap(), false);
    assert_eq!(sorted_set::zrem(&engine, b"missing", b"m").unwrap(), false);
}
