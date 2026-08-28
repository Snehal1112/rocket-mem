use bytes::Bytes;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    String(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<Bytes, Bytes>),
    Set(HashSet<Bytes>),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::List(_) => "list",
            Value::Hash(_) => "hash",
            Value::Set(_) => "set",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_values_compare_equal_by_content() {
        let a = Value::String(Bytes::from_static(b"bar"));
        let b = Value::String(Bytes::from_static(b"bar"));
        assert_eq!(a, b);
    }

    #[test]
    fn different_variants_are_not_equal() {
        let s = Value::String(Bytes::from_static(b"x"));
        let l = Value::List(VecDeque::new());
        assert_ne!(s, l);
    }

    #[test]
    fn type_name_matches_redis_naming() {
        assert_eq!(Value::String(Bytes::new()).type_name(), "string");
        assert_eq!(Value::List(VecDeque::new()).type_name(), "list");
    }
}
