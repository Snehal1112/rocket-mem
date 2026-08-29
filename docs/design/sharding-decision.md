# Sharding decision

**Status:** implemented, Sprint 1

## What we built
The keyspace is split into 16 fixed shards (`crates/engine/src/store.rs`), each an
independent `RwLock<HashMap<Bytes, Value>>` (`crates/engine/src/shard.rs`). A key is
routed to its shard by hashing the key bytes with `std::hash::DefaultHasher` and taking
the result modulo 16.

## Why 16 shards
No load testing has happened yet — 16 is a reasonable starting point (matches common
defaults in similar sharded designs) and is cheap to change later since nothing outside
`Store::new()` knows the count. Revisit once Sprint 6 (Week 12) benchmarking gives real
contention data.

## Why `DefaultHasher`, not something fancier
It's already in `std`, it's fast enough for routing decisions, and shard assignment
doesn't need cryptographic properties — just a reasonably even spread. The stress test
in `02-sharded-keyspace.md` (Task 3) confirms keys spread across more than one shard
under load; it doesn't (and doesn't need to) prove a perfectly uniform distribution.

## Why this over the alternatives
The choice between a sharded-lock design and single-thread/thread-per-core/lock-free/
proxy-based alternatives was made at the architecture level, not here — see the
Architecture Decision Record in `rocket-mem-production-plan.md`. This doc only covers
the concrete parameters (shard count, hash choice) within that already-made decision.

## Known limitation
Shard count is fixed at compile time via `Store::new(16)`. Making it configurable, or
supporting live resharding, is out of scope for v1 (see the master plan's Phase 4 /
"where this could go next" notes).
