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
`Store::new()` knows the count.

Sprint 6's flamegraph pass (see
[`../benchmarks/2026-08-30-flamegraph-notes.md`](../benchmarks/2026-08-30-flamegraph-notes.md))
is the contention data this section was waiting for: at `-c 50` concurrent clients, the shard
lock's contended slow path (`parking_lot::raw_rwlock::RawRwLock::lock_shared_slow`) showed up at
only 0.01% self CPU time, under both `Shard::get` (0.16%) and `Shard::set` (0.14%) combined —
shard contention is real but small at this concurrency. On that evidence, 16 shards stays — the
escape hatch the Architecture Decision Record documented (swapping each shard's internals for a
lock-free structure) is still open and still unexercised, and the sprint plan's own risk table
calls acting on it mid-sprint the rabbit hole to avoid.

## Why `DefaultHasher`, not something fancier
It's already in `std`, it's fast enough for routing decisions, and shard assignment
doesn't need cryptographic properties — just a reasonably even spread. The stress test
in `02-sharded-keyspace.md` (Task 3) confirms keys spread across more than one shard
under load; it doesn't (and doesn't need to) prove a perfectly uniform distribution.

## Why this over the alternatives
The choice between a sharded-lock design and single-thread/thread-per-core/lock-free/
proxy-based alternatives was made at the architecture level, not here — see the
Architecture Decision Record in `../rocket-mem-production-plan.md`. This doc only covers
the concrete parameters (shard count, hash choice) within that already-made decision.

## Known limitation
Shard count is fixed at compile time via `Store::new(16)`. Making it configurable, or
supporting live resharding, is out of scope for v1 (see the master plan's Phase 4 /
"where this could go next" notes).

## Not to be confused with cluster hash slots

Sprint 6 added a second, unrelated mechanism also called sharding: 16384 Redis-Cluster hash slots
(`CRC16(hash_tag(key)) % 16384`, `crates/server/src/cluster.rs`) that decide which *node* owns a
key. That is a placement decision across processes; this document is about lock striping *within*
one process. The two never interact — the `engine` crate knows nothing about slots, `Store`'s
16-shard routing is unchanged, and every node of a cluster is a complete server with its own
16 internal shards. The digits matching is a coincidence. See
[`../superpowers/specs/2026-08-30-sprint-6-spec.md`](../superpowers/specs/2026-08-30-sprint-6-spec.md).
