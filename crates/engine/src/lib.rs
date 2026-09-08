pub mod commands;
mod engine;
pub mod glob;
mod shard;
mod snapshot;
mod store;
mod value;
/// How many independently-locked shards the keyspace is split across.
///
/// Public because the server's AOF ordering guards are sized and indexed by it: a key's ordering
/// guard must be the one for the shard that key actually lives in, so the two counts cannot drift.
/// See `docs/design/sharding-decision.md` for why 16.
pub const SHARD_COUNT: usize = 16;

pub use engine::{Engine, TtlStatus};
pub use snapshot::SnapshotError;
pub use store::Store;
pub use value::{SortedSet, Value};
