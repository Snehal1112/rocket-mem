use crate::store::Store;
use crate::value::{SortedSet, Value};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize)]
enum SerializableValue {
    String(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<Bytes, Bytes>),
    Set(HashSet<Bytes>),
    SortedSet(Vec<(Bytes, f64)>), // (member, score) pairs; BTreeSet order is rebuilt on load
}

impl From<&Value> for SerializableValue {
    fn from(v: &Value) -> Self {
        match v {
            Value::String(b) => SerializableValue::String(b.clone()),
            Value::List(l) => SerializableValue::List(l.clone()),
            Value::Hash(m) => SerializableValue::Hash(m.clone()),
            Value::Set(s) => SerializableValue::Set(s.clone()),
            Value::SortedSet(z) => SerializableValue::SortedSet(
                z.members_ascending()
                    .map(|m| {
                        (
                            m.clone(),
                            z.score(m).expect("member came from members_ascending"),
                        )
                    })
                    .collect(),
            ),
        }
    }
}

impl From<SerializableValue> for Value {
    fn from(v: SerializableValue) -> Self {
        match v {
            SerializableValue::String(b) => Value::String(b),
            SerializableValue::List(l) => Value::List(l),
            SerializableValue::Hash(m) => Value::Hash(m),
            SerializableValue::Set(s) => Value::Set(s),
            SerializableValue::SortedSet(pairs) => {
                let mut z = SortedSet::new();
                for (member, score) in pairs {
                    z.insert(member, score);
                }
                Value::SortedSet(z)
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SerializableEntry {
    key: Bytes,
    value: SerializableValue,
    expires_at_unix_ms: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("snapshot file is too short to contain a valid header")]
    TooShort,
    #[error("failed to decode snapshot payload: {0}")]
    Decode(String),
}

/// `aof_offset` is written into the blob's 8-byte little-endian header — the caller (holding
/// `AofWriter::lock_for_ordering()`, per the sprint-5 spec's SAVE atomicity decision) is the
/// only one who knows the AOF's current durable length, so it's passed in rather than
/// discovered here. Pass `0` when there's no AOF to correlate against (a follower's `PSYNC`
/// reply, which discards the offset on the receiving end anyway).
pub fn serialize(store: &Store, aof_offset: u64) -> Vec<u8> {
    let entries: Vec<SerializableEntry> = store
        .snapshot_entries()
        .into_iter()
        .map(|(key, value, expires_at)| SerializableEntry {
            key,
            value: SerializableValue::from(&value),
            expires_at_unix_ms: expires_at.map(common::unix_ms_from_instant),
        })
        .collect();
    let payload = bincode::serialize(&entries).expect("SerializableEntry always serializes");
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&aof_offset.to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Replaces `store`'s entire contents with what's encoded in `bytes`, returning the AOF
/// offset from the blob's header. An entry whose `expires_at_unix_ms` is already in the past
/// (compared directly as wall-clock milliseconds, not via a round trip through `Instant` —
/// see the sprint-5 spec for why that distinction matters) is dropped rather than loaded and
/// left for the expiry reaper to clean up later.
pub fn deserialize(store: &Store, bytes: &[u8]) -> Result<u64, SnapshotError> {
    if bytes.len() < 8 {
        return Err(SnapshotError::TooShort);
    }
    let mut offset_bytes = [0u8; 8];
    offset_bytes.copy_from_slice(&bytes[..8]);
    let aof_offset = u64::from_le_bytes(offset_bytes);

    let entries: Vec<SerializableEntry> =
        bincode::deserialize(&bytes[8..]).map_err(|e| SnapshotError::Decode(e.to_string()))?;

    let now_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_millis() as i64;

    let loaded: Vec<(Bytes, Value, Option<std::time::Instant>)> = entries
        .into_iter()
        .filter_map(|e| {
            let expires_at = match e.expires_at_unix_ms {
                None => None,
                Some(ms) if ms <= now_unix_ms => return None, // already expired -- drop, don't load
                Some(ms) => Some(common::instant_from_unix_ms(ms)),
            };
            Some((e.key, Value::from(e.value), expires_at))
        })
        .collect();

    store.load_snapshot_entries(loaded);
    Ok(aof_offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use bytes::Bytes;
    use std::time::{Duration, Instant};

    #[test]
    fn serialize_then_deserialize_round_trips_a_string_value() {
        let store = Store::new(16);
        store.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        let bytes = serialize(&store, 42);

        let store2 = Store::new(16);
        let offset = deserialize(&store2, &bytes).unwrap();
        assert_eq!(offset, 42);
        assert_eq!(
            store2.get(b"k"),
            Some(Value::String(Bytes::from_static(b"v")))
        );
    }

    #[test]
    fn round_trips_every_value_type_including_a_sorted_set() {
        use crate::value::SortedSet;
        let store = Store::new(16);
        store.set(
            Bytes::from_static(b"s"),
            Value::String(Bytes::from_static(b"v")),
        );
        store.set(
            Bytes::from_static(b"l"),
            Value::List(std::collections::VecDeque::from([Bytes::from_static(b"a")])),
        );
        store.set(
            Bytes::from_static(b"h"),
            Value::Hash(std::collections::HashMap::from([(
                Bytes::from_static(b"f"),
                Bytes::from_static(b"v"),
            )])),
        );
        store.set(
            Bytes::from_static(b"set"),
            Value::Set(std::collections::HashSet::from([Bytes::from_static(b"m")])),
        );
        let mut z = SortedSet::new();
        z.insert(Bytes::from_static(b"alice"), 5.0);
        z.insert(Bytes::from_static(b"bob"), 2.0);
        store.set(Bytes::from_static(b"z"), Value::SortedSet(z));

        let bytes = serialize(&store, 0);
        let store2 = Store::new(16);
        deserialize(&store2, &bytes).unwrap();

        assert_eq!(
            store2.get(b"s"),
            Some(Value::String(Bytes::from_static(b"v")))
        );
        assert_eq!(
            store2.get(b"l"),
            Some(Value::List(std::collections::VecDeque::from([
                Bytes::from_static(b"a")
            ])))
        );
        assert_eq!(
            store2.get(b"h"),
            Some(Value::Hash(std::collections::HashMap::from([(
                Bytes::from_static(b"f"),
                Bytes::from_static(b"v")
            )])))
        );
        assert_eq!(
            store2.get(b"set"),
            Some(Value::Set(std::collections::HashSet::from([
                Bytes::from_static(b"m")
            ])))
        );
        let Some(Value::SortedSet(z2)) = store2.get(b"z") else {
            panic!("expected a sorted set")
        };
        assert_eq!(z2.score(b"alice"), Some(5.0));
        assert_eq!(z2.score(b"bob"), Some(2.0));
    }

    #[test]
    fn a_future_expiry_survives_the_round_trip_as_a_similar_remaining_duration() {
        let store = Store::new(16);
        store.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        store.expire_at(b"k", Instant::now() + Duration::from_secs(3600));
        let bytes = serialize(&store, 0);

        let store2 = Store::new(16);
        deserialize(&store2, &bytes).unwrap();
        let crate::engine::TtlStatus::Remaining(remaining) = store2.ttl(b"k") else {
            panic!("expected the loaded key to carry a TTL")
        };
        assert!(remaining.as_secs() > 3500 && remaining.as_secs() <= 3600);
    }

    #[test]
    fn an_already_past_expiry_is_dropped_at_load_time_not_round_tripped() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let store = Store::new(16);
        store.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
        );
        // bypass the normal expire_at path (which rejects an already-past Instant on some
        // internal paths) by encoding an already-past unix-ms timestamp directly
        let past_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            - 60_000;
        let entries = vec![SerializableEntry {
            key: Bytes::from_static(b"k"),
            value: SerializableValue::from(&Value::String(Bytes::from_static(b"v"))),
            expires_at_unix_ms: Some(past_ms),
        }];
        let payload = bincode::serialize(&entries).unwrap();
        let mut bytes = 0u64.to_le_bytes().to_vec();
        bytes.extend_from_slice(&payload);

        let store2 = Store::new(16);
        deserialize(&store2, &bytes).unwrap();
        assert_eq!(store2.get(b"k"), None);
    }

    #[test]
    fn deserialize_on_fewer_than_eight_bytes_is_too_short() {
        let store = Store::new(16);
        assert!(matches!(
            deserialize(&store, &[1, 2, 3]),
            Err(SnapshotError::TooShort)
        ));
    }

    #[test]
    fn deserialize_on_garbage_payload_bytes_is_a_decode_error() {
        let store = Store::new(16);
        let mut bytes = 0u64.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0xFF; 16]); // not a valid bincode-encoded Vec<SerializableEntry>
        assert!(matches!(
            deserialize(&store, &bytes),
            Err(SnapshotError::Decode(_))
        ));
    }
}
