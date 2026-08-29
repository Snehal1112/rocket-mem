use bytes::Bytes;
use ordered_float::OrderedFloat;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SortedSet {
    scores: HashMap<Bytes, OrderedFloat<f64>>,
    by_score: BTreeSet<(OrderedFloat<f64>, Bytes)>,
}

impl SortedSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, member: Bytes, score: f64) {
        let score = OrderedFloat(score);
        if let Some(&old) = self.scores.get(&member) {
            self.by_score.remove(&(old, member.clone()));
        }
        self.scores.insert(member.clone(), score);
        self.by_score.insert((score, member));
    }

    pub fn remove(&mut self, member: &[u8]) -> bool {
        match self.scores.remove(member) {
            Some(score) => {
                self.by_score
                    .remove(&(score, Bytes::copy_from_slice(member)));
                true
            }
            None => false,
        }
    }

    pub fn score(&self, member: &[u8]) -> Option<f64> {
        self.scores.get(member).map(|s| s.0)
    }

    pub fn len(&self) -> usize {
        self.scores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }

    /// Ascending by (score, member) — real Redis's tie-break rule when scores are equal.
    pub fn members_ascending(&self) -> impl Iterator<Item = &Bytes> {
        self.by_score.iter().map(|(_, m)| m)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    String(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<Bytes, Bytes>),
    Set(HashSet<Bytes>),
    SortedSet(SortedSet),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::List(_) => "list",
            Value::Hash(_) => "hash",
            Value::Set(_) => "set",
            Value::SortedSet(_) => "zset",
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

    #[test]
    fn sorted_set_insert_then_score_round_trips() {
        let mut z = SortedSet::new();
        z.insert(Bytes::from_static(b"alice"), 5.0);
        assert_eq!(z.score(b"alice"), Some(5.0));
        assert_eq!(z.score(b"missing"), None);
    }

    #[test]
    fn sorted_set_insert_again_updates_the_score_not_adds_a_duplicate() {
        let mut z = SortedSet::new();
        z.insert(Bytes::from_static(b"alice"), 5.0);
        z.insert(Bytes::from_static(b"alice"), 9.0);
        assert_eq!(z.len(), 1);
        assert_eq!(z.score(b"alice"), Some(9.0));
    }

    #[test]
    fn sorted_set_remove_reports_whether_the_member_existed() {
        let mut z = SortedSet::new();
        z.insert(Bytes::from_static(b"alice"), 5.0);
        assert!(z.remove(b"alice"));
        assert!(!z.remove(b"alice"));
        assert_eq!(z.len(), 0);
    }

    #[test]
    fn sorted_set_members_ascending_orders_by_score_then_by_member() {
        let mut z = SortedSet::new();
        z.insert(Bytes::from_static(b"bob"), 2.0);
        z.insert(Bytes::from_static(b"alice"), 5.0);
        z.insert(Bytes::from_static(b"carol"), 2.0); // ties with bob on score, breaks lexicographically
        let ordered: Vec<Bytes> = z.members_ascending().cloned().collect();
        assert_eq!(
            ordered,
            vec![
                Bytes::from_static(b"bob"),
                Bytes::from_static(b"carol"),
                Bytes::from_static(b"alice"),
            ]
        );
    }

    #[test]
    fn type_name_reports_zset_for_sorted_set_values() {
        assert_eq!(Value::SortedSet(SortedSet::new()).type_name(), "zset");
    }
}
