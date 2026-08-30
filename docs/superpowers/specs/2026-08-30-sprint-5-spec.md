# Sprint 5 — Snapshotting & Replication: Spec & Design

**Goal:** a follower stays in sync with a leader in real time; startup time drops sharply via snapshot + incremental AOF — matching `../../rocket-mem-sprint-plan.md`'s Sprint 5 goal.

**Scope:** covers Sprint 5's 5 backlog items (see `../../rocket-mem-sprint-plan.md`, Sprint 5, and `../../rocket-mem-production-plan.md`, Weeks 9–10). This doc fixes the shared design decisions — the snapshot wire/on-disk format, the hybrid-recovery offset scheme, the replication transport, and the follower's read-only/reconnect semantics — that every implementation plan below assumes as ground truth. Individual plans don't re-derive these; they reference this doc.

**Architecture recap:** this sprint adds two new capabilities on top of Sprint 4's AOF layer without touching its shape. Snapshotting is a new `engine`-crate module (`snapshot.rs`) plus two new `Store` methods and the two `Shard` methods they need; it never changes `Shard`'s `Entry` struct (which stays private) or any existing command path. Replication reuses the *exact* ordered, already-rewritten (`SPOP`→`SREM`, `EXPIRE`-family→`PEXPIREAT`) RESP frame stream `dispatch_and_log` already produces for the AOF — a leader fans those same encoded bytes out to connected followers from inside the same `AofWriter::lock_for_ordering()` critical section that already serializes AOF writes. A follower is a normal `rocket-mem` server process that additionally runs one background task applying a leader's stream via the existing non-logging `dispatch()` (the same function Sprint 4's AOF replay uses) — no new dispatch path, no new command-application logic.

## Global Constraints

- No AOF compaction/rewrite this sprint (no `BGREWRITEAOF`-equivalent) — the AOF keeps growing regardless of snapshots taken; a snapshot is a startup-time optimization only, not a space-reclamation mechanism.
- No partial resync / replication offset tracking this sprint — every (re)connect between a follower and its leader is a full resync (fresh snapshot transfer), matching the sprint plan's own stated acceptable fallback. `REPLICAOF`'s "resume from offset" language in the sprint backlog names the *command*, not a promise of partial resync semantics this sprint.
- No authentication/authorization on `PSYNC` or replica connections — any client that sends `PSYNC` is treated as a legitimate replica. Sprint 8 (`AUTH`/ACLs/TLS) is the first point auth exists anywhere in this project; adding it only to replication now would be inconsistent, narrow scope.
- A follower keeps no AOF of its own this sprint — its state is fully ephemeral, reconstructed from the leader on every start/reconnect. It never calls `dispatch_and_log`, only `dispatch`.

---

## Decision: snapshot serialization uses `bincode` over a `SerializableValue` mirror type, not a direct `Value` derive

`Value`'s `SortedSet` variant stores both a `HashMap<Bytes, OrderedFloat<f64>>` (`scores`) and a `BTreeSet<(OrderedFloat<f64>, Bytes)>` (`by_score`) — two views of the same data, kept in sync by `SortedSet::insert`/`remove`. Deriving `serde::Serialize`/`Deserialize` directly on `Value` would both require `ordered-float`'s `serde` feature (an extra dependency knob solely for this one variant) and serialize both redundant views, doubling the on-disk size of every sorted set for no benefit.

**Decision:** a new `crates/engine/src/snapshot.rs` module defines a mirror enum that represents `SortedSet` as its minimal form, and converts in both directions:

```rust
// crates/engine/src/snapshot.rs
use crate::value::{SortedSet, Value};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

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
                    .map(|m| (m.clone(), z.score(m).expect("member came from members_ascending")))
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
```

The unit that actually gets encoded is a `Vec<SerializableEntry>`, where `SerializableEntry { key: Bytes, value: SerializableValue, expires_at_unix_ms: Option<i64> }` — `SerializableValue` alone carries no key and no expiry. Both that struct and the enum stay private to `snapshot.rs`; nothing outside the module ever names them.

There is deliberately **no magic number and no format-version byte** in front of the payload — the sprint plan's own risk table calls snapshot-format compatibility a known v1 limitation not worth solving now, so an implementer should not add one on their own initiative. The practical consequence is that pointing `ROCKET_MEM_SNAPSHOT_PATH` at some unrelated file yields a `SnapshotError` from `bincode` rather than a clean "that isn't a snapshot" message, which is acceptable while the format has exactly one version.

TTLs cross the snapshot boundary as wall-clock, not monotonic time — `std::time::Instant` (what `Entry::expires_at` actually stores) has no defined relationship to a Unix timestamp and cannot be reconstructed after a process restart, the same problem Sprint 4's `EXPIREAT` dispatcher arm solved with an `Instant`↔`SystemTime` conversion. Each snapshot entry carries `expires_at_unix_ms: Option<i64>`, computed at snapshot time via `SystemTime::now() + (expires_at - Instant::now())` and converted back to an `Instant` on load via the same delta approach. **Decision:** this conversion moves out of `crates/server/src/dispatcher.rs`, where Sprint 4 left it, into `crates/common/src/lib.rs` as a matched pair — the existing `instant_from_unix_ms(i64) -> Instant` (currently a private fn at `dispatcher.rs`'s top, made `pub` on the move) plus a new inverse `unix_ms_from_instant(Instant) -> i64` that `snapshot.rs` needs. `common` is the one crate both `engine` (which needs the pair for `snapshot.rs`) and `server` (which still needs `instant_from_unix_ms` for `EXPIREAT`/`PEXPIREAT`) already depend on, and it depends on no other workspace crate (only `thiserror`), so a pure-`std::time` helper pair adds nothing new to it. `dispatcher.rs`'s existing `EXPIREAT`/`PEXPIREAT` arms switch to calling the moved version; no behavior change, pure relocation.

A snapshot entry with an `expires_at_unix_ms` already in the past is **dropped at load time**, not inserted and left for the expiry reaper: `Shard::set` unconditionally clears any prior expiry and `load_snapshot_entries` re-inserts via `set` + `expire_at`, so an already-past instant would round-trip fine — but skipping it is cheaper and matches what `Shard::keys`/`snapshot_entries` already do on the way out (expired entries are never written to the snapshot in the first place; this is just the same filter applied again on the way in, since arbitrary time can pass between writing a snapshot and loading it).

**New dependencies:** `serde = { version = "1", features = ["derive"] }` and `bincode = "1"` join `[workspace.dependencies]` and are added to `crates/engine/Cargo.toml`; `bytes` gains a `serde` feature in `[workspace.dependencies]` (`bytes = { version = "1", features = ["serde"] }`) so `Bytes` fields serialize without a manual `Vec<u8>` round-trip; and `engine` picks up the already-declared workspace `thiserror` for `SnapshotError` (below). No new dependency lands in `common`, `protocol`, or `server`.

## Decision: `Shard` gains `entries`/`clear`, `Store` gains `snapshot_entries`/`load_snapshot_entries`; `Entry` stays private

Snapshotting needs to walk every key, its `Value`, and its expiry across all 16 shards — but `Entry` (holding the raw `Instant` and the `last_touched: AtomicU64` recency counter) is intentionally private to `shard.rs`, along with `Shard::map` itself, and both should stay that way: `snapshot.rs` has no business knowing about the recency clock, and exposing `Entry` publicly would leak an implementation detail past the boundary Sprint 4 deliberately drew. That privacy also means `store.rs` **cannot** iterate a shard's entries itself the way it can already call `Shard::keys()` — so this needs two new `Shard` methods as well as the two `Store` ones:

```rust
// crates/engine/src/shard.rs — Entry itself stays private; only this projection of it escapes
pub fn entries(&self) -> Vec<(Bytes, Value, Option<Instant>)> {
    // one read lock, cloning out (key, value, expires_at) for every entry that
    // isn't already expired — the same `!entry.is_expired()` filter keys() uses
}

pub fn clear(&self) {
    // one write lock: empties the map and resets bytes_used to 0, so the
    // accounting MAXMEMORY reads stays truthful after a wholesale replacement
}

// crates/engine/src/store.rs
pub fn snapshot_entries(&self) -> Vec<(Bytes, Value, Option<std::time::Instant>)> {
    // flat_maps Shard::entries() across all 16 shards, exactly as keys() flat_maps
    // Shard::keys(). Each shard is locked and released in turn, so this is NOT a
    // whole-store point-in-time view on its own — see SAVE's atomicity requirement
    // below for the lock that actually makes it one.
}

pub fn load_snapshot_entries(&self, entries: Vec<(Bytes, Value, Option<std::time::Instant>)>) {
    // Shard::clear() on every shard first (a snapshot load always fully replaces
    // current state, never merges into it), then re-inserts each entry via the
    // existing set() + expire_at() paths, which re-account bytes_used correctly.
}
```

`last_touched` is **not** preserved across a snapshot round-trip — every loaded entry starts at whatever "just touched" value `set` already assigns new entries, which is correct: eviction only needs *relative* freshness going forward, not a value that survived a restart. `snapshot::serialize`/`deserialize` (in `snapshot.rs`) call these two `Store` methods and own the actual `bincode` encode/decode plus the `SerializableValue` conversion from the decision above — `store.rs` never imports `serde` or `bincode`.

`Engine` (`crates/engine/src/engine.rs`) gains two thin wrappers, matching its existing role as "a thin public facade over `Store`" (per `CLAUDE.md`) — these are what `SAVE`, leader-side `PSYNC` handling, follower-side sync, and startup recovery all call, never `Store`'s methods directly, matching how every other engine-crate consumer already goes through `Engine`:

```rust
// crates/engine/src/engine.rs
/// `aof_offset` is written into the blob's 8-byte header (see the hybrid-recovery
/// decision below). The Engine has no idea what the AOF's length is — only the
/// caller, holding `AofWriter::lock_for_ordering()`, does — so it is passed in
/// rather than discovered. `PSYNC` passes 0: a follower keeps no AOF and discards it.
pub fn snapshot(&self, aof_offset: u64) -> Vec<u8>;

/// Returns the offset that was in the loaded blob's header, so startup can hand it
/// straight to `aof::replay` as `start_at` without re-parsing the file.
pub fn load_snapshot(&self, bytes: &[u8]) -> Result<u64, SnapshotError>;
```

**`SnapshotError`** (`crates/engine/src/snapshot.rs`, re-exported from `engine`'s `lib.rs`) is a two-variant `thiserror` enum: `TooShort` (fewer than the 8 header bytes) and `Decode(String)` (a `bincode` decode failure, stringified rather than wrapping `bincode::Error` so `bincode` stays an implementation detail of the crate rather than leaking into every caller's error type). Callers do not need to distinguish them; both mean "this file is not a snapshot this build can read."

`Engine::load_snapshot` deliberately bypasses `maybe_evict` — `load_snapshot_entries` goes through `Store::set`, not `Engine::set` — so a snapshot larger than a configured `MAXMEMORY` lands whole and is only trimmed back under the ceiling by the next write that calls `Engine::set`/`with_mut`. That is the right order of operations (evicting *while* loading would silently discard keys the operator asked to restore) and is called out here so an implementer doesn't "fix" it.

## Decision: the snapshot file embeds its own AOF offset for hybrid recovery — no separate side-file

Hybrid recovery ("latest snapshot + AOF tail replay," the sprint's other P0 recovery item) needs to know *which byte position in the AOF* a given snapshot corresponds to, so replay only decodes what happened *after* the snapshot instead of replaying the whole file from byte 0 — the entire point of the optimization. **Decision:** the snapshot file's first 8 bytes are a little-endian `u64` — the leader's AOF file length at the exact moment the snapshot was taken — followed immediately by the `bincode`-encoded entry list. One self-contained artifact, no separate offset side-file to keep in sync or lose.

Getting a *durable* offset requires the AOF to actually be flushed to that exact length before reading it, not merely queued in the writer thread's channel (`AofWriter::append` is async-from-the-caller's-perspective under `EverySecond`/`Never` policies — see Sprint 4's `aof.rs`). **Decision:** `AofWriter` gains a stored `path: PathBuf` field (captured in `open()`, alongside the existing `tx`/`policy`/`order` fields) and a new method:

```rust
// crates/server/src/aof.rs
/// Flushes and fsyncs (via the existing `Flush` message the writer thread
/// already handles), then returns the file's length in bytes. The returned
/// offset is guaranteed durable: every byte before it is confirmed on disk.
/// Must be called while the caller already holds `lock_for_ordering()` — see
/// the atomicity note on `SAVE` below for why.
pub fn current_offset(&self) -> std::io::Result<u64> {
    self.fsync()?;
    Ok(std::fs::metadata(&self.path)?.len())
}
```

Calling this while holding the ordering lock cannot deadlock, despite `fsync()` blocking on an ack from the writer thread: that thread only ever drains its channel and touches the file — it never acquires `order`, and it never calls back into the dispatcher. The worst case is a wait, bounded by however long the queued writes ahead of the `Flush` take to land.

`aof::replay` gains a `start_at: u64` parameter — Rust has no default arguments, so every existing Sprint 4 call site (`main.rs` plus the `aof.rs` and `kill_and_recover.rs` tests) is updated to pass a literal `0`, leaving AOF-only recovery behaviorally unchanged. Decoding begins at that byte offset into the file rather than byte 0. The corrupt-tail-truncation logic (unchanged from Sprint 4) must still measure `valid_len` relative to the *whole file*, not relative to `start_at`, so truncation-on-corruption removes bytes from the true end of the file regardless of where replay started reading — concretely, `valid_len` is initialised to `start_at as usize` rather than `0`, and the decode buffer is built from `&raw[start_at as usize..]`.

**When the recorded offset doesn't fit the AOF** (`start_at > raw.len()` — the AOF was deleted, replaced, or manually truncated since the snapshot was taken), the snapshot and the AOF are not a matched pair, and replaying the tail would apply the wrong history. **Decision:** log the mismatch to stderr, discard the snapshot entirely, and fall back to a full replay from byte `0` on a fresh `Engine`. That fallback is always correct this sprint precisely because of the no-compaction global constraint: the AOF is never rewritten, so byte 0 onward is always the complete history. (A missing AOF file remains the Sprint 4 no-op, in which case the snapshot alone is the recovered state.)

**Atomicity requirement — `SAVE` must hold `lock_for_ordering()` across both the offset read and the snapshot walk:** `current_offset()` and `Store::snapshot_entries()` are two separate, non-atomic operations. Taken independently, a concurrent write could land between them — e.g. an `RPUSH` commits to the engine and appends to the AOF in the gap between `SAVE` reading the offset and walking the shards, so the pushed value would be captured in *both* the snapshot (already reflecting the push) *and* the AOF tail after the recorded offset (the append that just landed) — a replay would then apply that `RPUSH` a second time, corrupting the list with a duplicate (unlike idempotent commands such as `SET`/`SADD`, replaying `RPUSH`/`LPUSH` twice is not a no-op). **Decision:** the `SAVE` dispatcher arm acquires `aof.lock_for_ordering()` — the exact same lock `dispatch_and_log` already holds across every write's "mutate, then log" step — before calling `current_offset()`, and holds it across the subsequent `engine.snapshot(offset)` call too (which is what wraps `Store::snapshot_entries()`), releasing it only once both have completed. Because every concurrent writer's `dispatch_and_log` must acquire that same lock before it can even begin mutating the engine, holding it for the duration of `SAVE` guarantees no write can commit between the offset read and the snapshot walk — the engine state captured by `snapshot_entries()` is exactly the state implied by "every write up to `current_offset()`'s returned length, and no more." Note this necessarily puts the `bincode` encode inside the stall too, since `Engine::snapshot` walks and encodes in one call; splitting them would mean exposing `Store`'s walk directly to the dispatcher, which the `Engine`-facade decision above rules out, and the extra stall is bounded by the same keyspace size the walk already traverses. The one thing deliberately left *outside* the lock is writing the resulting bytes to disk.

This makes `SAVE` a genuine global write-stall for its duration (not just a stall on the calling connection, as an earlier framing of this decision assumed) — an accepted P0 tradeoff at this project's scale, and precisely the complexity a real non-blocking `BGSAVE` (P2, only sketched — see below) would need to solve properly (e.g. via a copy-on-write style point-in-time view) rather than being a plain `spawn_blocking` wrapper. Two concurrent `SAVE`s need no extra machinery for the same reason: they serialize on that one lock, so neither can observe a torn walk. What they *would* otherwise collide on is the output file, which the write-then-rename decision below handles.

**On startup** (`main.rs`, rewritten again — this is now its third revision after Sprint 4's plans 05 and 06): if a file exists at `ROCKET_MEM_SNAPSHOT_PATH`, read it and call `engine.load_snapshot(&bytes)` on the fresh `Engine`; the offset it returns is passed straight to `aof::replay(aof_path, &engine, offset)`. If the file is absent, or `load_snapshot` returns a `SnapshotError`, or the returned offset overshoots the AOF (above), startup logs why and falls back to Sprint 4's exact behavior: a fresh `Engine` and `aof::replay(aof_path, &engine, 0)`. This path is identical on a leader and on a would-be follower — the replica role is not persisted (see the `REPLICAOF` decision below), so every process starts from its own on-disk state and only becomes a follower once a client sends `REPLICAOF`.

## Decision: `SAVE` is a blocking P0 command; a non-blocking `BGSAVE`-equivalent is P2 and only sketched, not built

The sprint backlog already scopes "`BGSAVE`-equivalent non-blocking snapshot" as P2 — the first thing to cut if the sprint runs long. **Decision:** the P0 path is a plain, blocking `SAVE` dispatcher command: it acquires `aof.lock_for_ordering()`, calls `current_offset()` then `engine.snapshot(offset)` under that lock (per the atomicity requirement above), releases the lock, and writes the returned bytes to `ROCKET_MEM_SNAPSHOT_PATH` before replying `+OK` — all synchronously on the calling connection's task.

**The snapshot path is a new env var** (`ROCKET_MEM_SNAPSHOT_PATH`, default `./dump.snapshot`, read once at startup exactly like `ROCKET_MEM_AOF_PATH`) and reaches `dispatch_and_log` as a `snapshot_path: PathBuf` field on the `ReplicationHandle` introduced below. That handle is a slight naming stretch for a snapshot path, but it is already the one carrier of server-level state threaded through `serve()`→`handle_connection()`→`dispatch_and_log`, and adding a whole extra parameter for a single `PathBuf` buys nothing. Note the consequence for tests: `ReplicationHandle::default()` carries the `./dump.snapshot` default, so **any test that issues `SAVE` must construct a handle with a `tempfile::tempdir()` path** rather than defaulting, or it will litter the repo root the way a stray `appendonly.aof` already can.

**Writing is write-to-temp-then-`rename`**, never a direct write onto the live path: `SAVE` writes to `<snapshot_path>.tmp`, `sync_data`s it, then `std::fs::rename`s it over `<snapshot_path>` — an atomic replace on the same filesystem. Without this, a crash (or a `kill -9`, which Sprint 4's suite does for real) partway through writing leaves a truncated file at the exact path startup will try to load next boot, and a half-written `bincode` blob is precisely the input that turns into a `SnapshotError` — or worse, a short-but-parseable one. The fallback above keeps that from being fatal, but silently discarding a snapshot on every restart defeats the whole feature.

Because it holds `lock_for_ordering()`, `SAVE` is a genuine **global write-stall**: every other connection's `dispatch_and_log` blocks trying to acquire the same lock until `SAVE` releases it, not merely the calling connection — an accepted P0 tradeoff at this project's scale, and exactly why the sprint backlog scopes a true non-blocking version separately. If time allows within the sprint, `BGSAVE` is a P2 stretch item whose non-blocking property is genuinely harder than a `tokio::task::spawn_blocking` wrapper around the same call — spawning the same lock-holding work onto a blocking thread still stalls every writer for the same duration, just off the async runtime's reactor thread — so it is intentionally left unscoped beyond this note; a real solution needs either the same global stall (acceptable, just moved off the client-facing connection) or a copy-on-write-style point-in-time mechanism this sprint doesn't build.

`SAVE` is **not** added to `WRITE_COMMANDS` (`crates/server/src/aof.rs`) — it doesn't mutate the keyspace, so it has nothing for the AOF to log, and it must still be permitted on a read-only follower (an operator may want to snapshot a follower's current state) — the read-only gate decision below only rejects commands in `WRITE_COMMANDS`.

**Where `SAVE` lives:** the plain `dispatch(engine, frame, protocol, client_id) -> Frame` function Sprint 4 established has no `&AofWriter` parameter — only `dispatch_and_log` receives one — so `SAVE` cannot be just another match arm inside `dispatch`'s existing giant match statement the way every other command is. **Decision:** `dispatch_and_log` special-cases `SAVE` the same way it already special-cases the `WRITE_COMMANDS` rewrites (`SPOP`→`SREM`, etc.) — a check for the command name *before* delegating to `dispatch` — except `SAVE` never reaches `dispatch` at all: `dispatch_and_log` recognizes it, performs the lock-acquire/offset/snapshot/write sequence directly (it already has `aof` in scope), and returns `Frame::Simple("OK".into())` (or a `Frame::Error` on an I/O failure writing the snapshot file) without calling `dispatch`. This keeps `dispatch`'s signature and its ~250 existing direct call sites (which never pass an `AofWriter`) completely untouched, matching Sprint 4's established constraint. The corollary is that `dispatch` alone still answers `SAVE` with its usual unknown-command error — which is exactly right for the two callers that use it directly, `aof::replay` and the follower apply loop, neither of which should ever see a `SAVE`.

## Decision: one `ReplicationHandle` struct threads leader/follower replication state through `dispatch_and_log`

`REPLICAOF`/`REPLICAOF NO ONE` have the same problem `SAVE` does above: they need state plain `dispatch(engine, frame, protocol, client_id)` has no parameter for — specifically, the `is_replica: AtomicBool` flag, a way to cancel a previously-started `replication_client_loop` task, and (for the leader side of every *other* write command) the `ReplicaRegistry` fan-out target list. **Decision:** a single new struct bundles them, constructed once in `main.rs`, wrapped in an `Arc`, and threaded through `serve()`/`handle_connection()` exactly like `engine: Arc<Engine>` and `aof: Arc<AofWriter>` already are, and added as a new parameter to `dispatch_and_log`:

```rust
// crates/server/src/replication.rs
pub struct ReplicationHandle {
    pub registry: ReplicaRegistry,                  // leader side: connected replicas to fan writes out to
    pub is_replica: std::sync::atomic::AtomicBool,  // follower side: read-only gate
    /// Follower side: the running `replication_client_loop`, aborted by
    /// `REPLICAOF NO ONE` or replaced by a subsequent `REPLICAOF <host> <port>`.
    follower_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The engine the follower task applies the leader's stream to. Needed as an
    /// owned `Arc` because a spawned task is `'static` — `dispatch_and_log`'s own
    /// `engine: &Engine` parameter cannot be moved into one. Invariant: this is
    /// the *same* `Engine` `serve()` was handed; `ReplicationHandle::new` is what
    /// enforces it by taking it from the caller.
    engine: std::sync::Arc<Engine>,
    /// Where `SAVE` writes, per the decision above.
    snapshot_path: std::path::PathBuf,
}

impl ReplicationHandle {
    pub fn new(engine: Arc<Engine>, snapshot_path: PathBuf) -> Self { /* ... */ }
    /// For `serve_replica`, which needs the shared `Engine` to snapshot from.
    pub fn engine(&self) -> &std::sync::Arc<Engine> { &self.engine }
}

/// An idle handle: no replicas registered, `is_replica` false, no follower task,
/// its own throwaway `Engine`, and the `./dump.snapshot` default path. Exists only
/// so the existing `dispatch_and_log` tests stay one-liners. Any test that actually
/// exercises `SAVE` or `REPLICAOF` must use `new` instead — `default`'s `Engine` is
/// not the one such a test asserts against, and its snapshot path is the repo root.
impl Default for ReplicationHandle { /* ... */ }
```

The `is_replica` flag is a plain `AtomicBool` field rather than a nested `Arc<AtomicBool>`: the whole handle is already behind one `Arc`, so a second layer of sharing would buy nothing.

`dispatch_and_log`'s signature becomes `dispatch_and_log(engine: &Engine, aof: &AofWriter, replication: &ReplicationHandle, frame: Frame, protocol: &mut Protocol, client_id: u64) -> Frame`. That touches **~20 call sites**, not ~250: the ~250 figure belongs to plain `dispatch`, which this sprint leaves untouched (`dispatch_and_log` is called 19 times in `dispatcher.rs`'s tests plus once in `connection.rs::handle_connection`). Each existing test call site gains a `&ReplicationHandle::default()`, so none of them observe any behavior change; only the real `handle_connection` and the new `crates/server/tests/replication.rs` integration tests construct a live one via `new`. `serve()` likewise gains a trailing `replication: Arc<ReplicationHandle>` parameter, updated at its handful of call sites (`main.rs`, `connection.rs`'s tests, `tests/integration.rs`).

`REPLICAOF`/`REPLICAOF NO ONE`/`SAVE` are all intercepted inside `dispatch_and_log` before it would otherwise delegate to `dispatch`, exactly like the `SAVE` decision above — `dispatch` itself never learns any of these three command names exist. None of the three is in `WRITE_COMMANDS`, so none of them takes the ordering lock on the way in (`dispatch_and_log` only acquires it when `extract_write_command_name` returns `Some`); `SAVE` acquires it explicitly in its own arm. One implementation consequence worth stating up front: the `REPLICAOF` arm calls `tokio::spawn`, which panics outside a runtime, so its `dispatcher.rs` unit tests must be `#[tokio::test]`, not `#[test]`.

## Decision: replication transport reuses the RESP port; `PSYNC` transitions a connection into one-way streaming mode

**Decision:** no second listener, no separate replication port. A follower's connection to a leader is a plain TCP connection to the leader's existing `ROCKET_MEM_ADDR` port, and `PSYNC` (a new command with no arguments this sprint — see Global Constraints on why partial resync isn't supported) is the signal that flips that connection's handling from the normal request/reply loop into a dedicated replica-serving path. Unlike `SAVE`/`REPLICAOF`, `PSYNC` is recognized in `connection.rs::handle_connection` itself, *before* the frame is handed to `dispatch_and_log` — it has to be, because handling it means taking ownership of the whole `Framed` socket, which no dispatcher function has access to. `handle_connection` matches the command name, breaks out of its loop, and moves the socket into `serve_replica`, which never returns; the dispatcher never sees a `PSYNC` at all:

```rust
// crates/server/src/connection.rs
async fn serve_replica(
    framed: Framed<TcpStream, RespCodec>,
    aof: &AofWriter,
    replication: &ReplicationHandle,
) {
    // ONE critical section: snapshot + register, so no write can slip between them.
    let (snapshot_bytes, rx) = {
        let _order_guard = aof.lock_for_ordering();
        let bytes = replication.engine().snapshot(0); // 0: a follower keeps no AOF, so the header is moot
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        replication.registry.register(tx);
        (bytes, rx)
    };
    // Take the raw socket back from the codec, keeping any bytes it already buffered.
    let mut parts = framed.into_parts();
    // 1. the blob: an 8-byte LE length, then that many raw bytes. Not a RESP frame.
    // 2. from then on, plain RESP frames, byte-for-byte as the AOF received them —
    //    no extra length prefix, since RESP is already self-delimiting.
    // Drain rx onto the socket forever; this connection never reads again.
}
```

**Atomicity requirement — the snapshot and the registration must happen under one `lock_for_ordering()` critical section.** Taken separately, this is the same class of bug as `SAVE`'s, one step worse: a write that commits *after* `Engine::snapshot` walked the shards but *before* `register` added the replica's sender is in neither the blob nor the stream, so the follower misses it permanently and silently — no reconnect ever repairs it, because a reconnect just takes a fresh snapshot of a leader that has long since moved on. Registering first and snapshotting second trades the lost write for a duplicated one (the write appears in the blob *and* on the stream), which is equally wrong for non-idempotent `RPUSH`/`LPUSH`. Holding the ordering lock across both is what makes "the blob, then every write after it, exactly once" true. **Lock ordering:** `lock_for_ordering()` is always acquired *before* the `ReplicaRegistry`'s own mutex, and never the reverse — that holds here and in the fan-out hook below, which is the only other place both are taken, so there is no path to an inversion.

A new `ReplicaRegistry` (`crates/server/src/replication.rs`, `Mutex<Vec<mpsc::UnboundedSender<Bytes>>>`) holds one sender per connected replica. The channel is **unbounded on purpose**: `broadcast` runs inside the `lock_for_ordering()` critical section, so a bounded channel filled by a slow or stalled replica would block that replica's send while holding the lock that gates *every* write on the leader — one wedged follower would stall the whole server. Unbounded means a stalled replica instead grows the leader's memory, invisibly to `MAXMEMORY` accounting, which is the same tradeoff Sprint 4 rejected for the AOF queue (`AOF_QUEUE_CAPACITY`) and accepts here for the opposite reason: the AOF queue's consumer is a local disk that always drains eventually, a replica's is a remote socket that may not. This is a known, documented limit of this sprint's replication, not an oversight.

**Fan-out hook:** the hook lives in `dispatch_and_log` (`crates/server/src/dispatcher.rs`), inside the existing `for frame_to_log in to_log` loop and still under the `lock_for_ordering()` guard. The frames fanned out are `to_log`'s — the *rewritten* ones (`SPOP`→`SREM`, the `EXPIRE` family→`PEXPIREAT`, `SET ... EX`→`SET` + `PEXPIREAT`), not the client's original frame — which is what makes a follower's applied history deterministic for exactly the reasons Sprint 4 introduced those rewrites. Two consequences for `aof.rs`:

- `AofWriter::append` currently encodes internally and returns nothing, so `dispatch_and_log` has no encoded bytes to hand the registry. **Decision:** `aof.rs` grows a free `pub fn encode_frame(frame: &Frame) -> std::io::Result<Vec<u8>>` (the `RespCodec::default().encode(...)` that today sits inline in `append`) plus `pub fn append_encoded(&self, bytes: Vec<u8>) -> std::io::Result<()>`; `append` becomes `self.append_encoded(encode_frame(&frame)?)`, keeping its existing signature and every existing call site. `dispatch_and_log` encodes once, then calls `append_encoded` and `broadcast` with the same buffer — one encode pass, as intended.
- The broadcast is attempted **regardless of the append's I/O result**. The engine mutation has already committed at that point, so a leader that fails to log a write but still replies to the client must not also withhold it from its replicas — that would diverge them permanently for a purely local disk problem. Ordering within the critical section is: encode, append, broadcast.

A send failure (channel closed — the replica connection died) removes that sender from the registry rather than erroring the write; the leader's own mutation and AOF append already succeeded and must not be rolled back because a replica dropped. A leader with zero connected replicas pays only the cost of an empty `Vec` check per write.

## Decision: a follower keeps no AOF; it applies the leader's stream via plain `dispatch()`, never `dispatch_and_log`

Per the Global Constraints, follower state is fully ephemeral this sprint. **Decision:** `REPLICAOF <host> <port>` (intercepted in `dispatch_and_log`, per the `ReplicationHandle` decision above) spawns a background task, `replication_client_loop` (new `crates/server/src/replication.rs` function), and stores the resulting `JoinHandle` in `ReplicationHandle::follower_task`. The whole sequence — take the `follower_task` mutex, `abort()` whatever handle is already there, spawn the new task, store its handle, set `is_replica` — happens **under that one mutex**, so two clients issuing `REPLICAOF` concurrently can only serialize, never end up with two live apply loops racing each other into the same `Engine`. The command itself returns `+OK` immediately: connecting, handshaking, and syncing all happen in the background, not on the client's connection.

The task: connects to `<host>:<port>`, sends a RESP-encoded `PSYNC`, reads the 8-byte length and then exactly that many bytes of snapshot blob, and loads it via `Engine::load_snapshot` (which clears every shard first — every sync, first or reconnect, starts from a clean slate). It then wraps the *same* socket in a `Framed<_, RespCodec>` and loops: decode a RESP frame, apply it via `dispatcher::dispatch(&engine, frame, &mut Protocol::default(), 0)` — the exact function AOF replay already calls, so no new command-application code path exists anywhere in this project.

Two details the framing switch depends on: the follower must build that `Framed` with `FramedParts`, carrying over any bytes it already read past the end of the blob, or the first replicated write is silently eaten; and it must **not** decode the blob with `RespCodec` at all — the blob is `bincode`, and the 8-byte length prefix is what tells the follower where RESP resumes. The `Protocol::default()` handed to `dispatch` is a throwaway: the follower never replies to the leader, so no `HELLO`-driven protocol state is meaningful here (the same reason `aof::replay` passes one).

**An applied frame that returns `Frame::Error` is logged to stderr and the loop continues** — it is not a reason to drop the connection or resync. A leader only fans out commands whose local execution already succeeded, so an error here means the two sides genuinely disagree (a bug, or version skew), and tearing down the connection would just spin a reconnect/resync loop that reproduces the same error. Logging and continuing keeps the divergence visible and bounded to the one key.

**Cancellation is `JoinHandle::abort()`, not a cooperative flag.** A flag "checked once per iteration" would only be observed when the *next* frame arrives from the leader, so `REPLICAOF NO ONE` against an idle stream would leave the task parked in its socket read indefinitely, still holding a live subscription and still able to apply writes to a node the operator believes is now a standalone leader — the exact failure that makes the read-only gate's release unsafe. `abort()` cancels at the awaiting read, which also drops the TCP connection and deregisters the follower on the leader's side. It cannot interrupt a partially applied command: `dispatch` is synchronous, so the only cancellation points in the loop are the socket awaits, never mid-mutation. `REPLICAOF NO ONE` therefore takes the `follower_task` mutex, aborts and clears the handle, sets `is_replica` false, and returns `+OK`; the follower keeps whatever keyspace it last had and resumes normal, writable operation.

The replica role is **not persisted** — it lives only in `ReplicationHandle`. A restarted follower comes back as a standalone node holding whatever its own snapshot/AOF say, until a client sends `REPLICAOF` again. Related and deliberate: while a node is a follower its own `AofWriter` goes quiet (client writes are rejected by the gate below, replicated writes go through `dispatch`, which never logs), so its AOF freezes at whatever it held when the role changed and is stale on the next restart. That is consistent with "follower state is ephemeral" — the stale file is superseded by the next full resync — but it is a real footgun if a follower is ever promoted by restarting it standalone, so it is called out here rather than discovered later.

**Reconnect:** any read/write error on the leader connection (leader restarted, network blip, leader process killed) logs to stderr and, after a fixed ~1s backoff, restarts the entire loop body from the top — a fresh `PSYNC`, a fresh full snapshot, which replaces the follower's partially-applied state wholesale. This is deliberately simple: there is no distinction in this sprint's code between "first sync" and "resync after disconnect," they are the same code path run again. A leader that dies mid-fan-out therefore needs no special handling on either side: the leader's registry drops the closed sender on its next `broadcast`, and the follower's next read errors into the retry.

## Decision: read-only enforcement is one `AtomicBool` role flag checked in `dispatch_and_log`

**Decision:** `ReplicationHandle::is_replica` (from the decision above) starts `false` in `main.rs`'s constructed handle, and the `REPLICAOF`/`REPLICAOF NO ONE` arms `dispatch_and_log` intercepts set it `true`/`false` respectively. `dispatch_and_log` gains a check at its top — *before* `extract_write_command_name`, so a rejected write never touches the ordering lock: if `replication.is_replica.load(Ordering::Relaxed)` is true and the incoming command's name is in `WRITE_COMMANDS` (the same static allowlist Sprint 4 already defined in `aof.rs`), it returns `Frame::Error("READONLY You can't write against a read only replica.".into())` immediately, without calling `dispatch` or touching the AOF/replica fan-out at all — matching real Redis's exact error text and behavior. `Relaxed` is the right ordering here: the flag guards nothing but itself, and a client whose write races the exact instant of a role change may legitimately land on either side of it.

This check is bypassed entirely by the follower's own `replication_client_loop`, which calls `dispatch()` directly (not `dispatch_and_log`), so replicated writes are never blocked by the gate meant for client-originated ones. Read commands (`GET`, `KEYS`, `TTL`, etc. — anything not in `WRITE_COMMANDS`) are unaffected on a replica, matching this sprint's "read replica" scope decision. Three commands are deliberately *not* gated even though a purist might expect them to be: `SAVE` (snapshotting a follower is a legitimate operator action, per the decision above), `PSYNC` (chained replication isn't in scope, but nothing about serving a snapshot mutates the keyspace), and `REPLICAOF` itself (which must stay reachable, or `REPLICAOF NO ONE` could never un-replica a node). None of the three is in `WRITE_COMMANDS`, so the gate lets them through for free — no special-casing needed.

**Knowingly left alone:** `HELLO`'s reply hardcodes `role: master` (`dispatcher.rs`'s `hello_reply`) and `INFO` returns only a `# Server` section. Both are now inaccurate on a follower. Making them role-aware would mean threading `ReplicationHandle` into `dispatch` — the one thing this sprint's whole interception design exists to avoid — for a cosmetic field no DoD item depends on. It stays wrong this sprint and is noted as such in the close-out plan's README update.

## Decision: replication integration tests run in-process, not via subprocess

Sprint 4's kill-and-recover suite is explicitly `2026-08-29-sprint-2-spec.md`'s one deliberate exception to in-process-only testing, because it needed real OS `SIGKILL`/filesystem crash semantics an in-process test cannot simulate. Replication has no equivalent requirement — the three DoD items (recovery-time benchmark, 1-leader-2-follower propagation, kill-and-reconnect-follower) all test this project's own application-level logic, not OS/filesystem crash behavior. **Decision:** `crates/server/tests/replication.rs` follows the existing `integration.rs` pattern — `serve()` spawned in-process against real `127.0.0.1:0` TCP listeners, driven via the `redis` crate already in `dev-dependencies`. Each node in a test is a full, independent triple: its own `Engine`, its own `AofWriter` over its own `tempfile::tempdir()` path, and its own `ReplicationHandle::new` — sharing any of the three between "nodes" would make a test pass for the wrong reason.

Two things this test shape has to pin down that a single-process test never had to:

- **"Kill" a follower** means dropping the follower→leader TCP connection (a network drop, or a leader-side restart), or aborting the follower's `replication_client_loop` and re-issuing `REPLICAOF` — not a real `SIGKILL`, since no subprocess exists here. Sprint 4's `kill_and_recover.rs` keeps its subprocess shape and is untouched.
- **"Within a bounded time window"** (the DoD's wording for the 1-leader-2-follower test) means the assertion polls the follower's value on an interval until it matches or a deadline expires, rather than sleeping a fixed duration and asserting once. Replication here is asynchronous by construction — the leader returns to its client the moment `broadcast` enqueues — so a fixed sleep is either flaky or needlessly slow. A 2s deadline with ~10ms polling is the shape; the bound is generous because CI machines are not fast, and the test fails on the deadline, never on a single early read.

---

## Sequencing

Plans depend on each other in this order (to be written by `superpowers:writing-plans`, living in `../plans/2026-08-30-sprint-5-plans/`):

1. `01-snapshot-serialization.md` — the `instant_from_unix_ms`/`unix_ms_from_instant` relocation into `common`, `SerializableValue`, `Shard::entries`/`clear`, `Store::snapshot_entries`/`load_snapshot_entries`, `snapshot::serialize`/`deserialize`, `SnapshotError`, `Engine::snapshot`/`load_snapshot`. Engine-crate only — no dispatcher or server changes, so it is independent of everything else this sprint.
2. `02-hybrid-recovery-and-aof-offset.md` (depends on 1) — `AofWriter`'s new `path` field and `current_offset`, the offset-prefixed snapshot file header, `aof::replay`'s new `start_at` parameter (and every existing call site passing `0`), `main.rs`'s startup rewrite to prefer snapshot+tail when a snapshot exists, including the offset-overshoot fallback.
3. `03-replication-handle-and-save.md` (depends on 1 **and** 2) — `ReplicationHandle` (struct, `new`/`Default`, `ROCKET_MEM_SNAPSHOT_PATH`), threading it through `serve()`/`handle_connection()`/`dispatch_and_log`, and the `SAVE` interception with its `lock_for_ordering` atomicity and write-then-rename. `SAVE` sits here, not in 01, because it needs *both* `current_offset` (from 2) and the handle's `snapshot_path` (from this plan) — it is not implementable earlier.
4. `04-replica-registry-and-leader-fanout.md` (depends on 3, and on Sprint 4's `dispatch_and_log`/`AofWriter::lock_for_ordering`) — `ReplicaRegistry`, the `encode_frame`/`append_encoded` split in `aof.rs`, `PSYNC` handling and `serve_replica` in `connection.rs` (including the snapshot-and-register critical section), the `dispatch_and_log` fan-out hook.
5. `05-replicaof-and-follower-apply-loop.md` (depends on 4) — `REPLICAOF`/`REPLICAOF NO ONE` dispatcher interception, `replication_client_loop`, abort-based cancellation, the `is_replica` flag and `-READONLY` gating in `dispatch_and_log`.
6. `06-replication-integration-tests.md` (depends on 1–5) — the three DoD tests: recovery-time benchmark, 1-leader-2-follower propagation, kill-and-reconnect-follower. The benchmark is a `#[test]`-driven measurement whose *numbers* are recorded in the README's persistence section (not asserted on in CI, where timings are too noisy to gate on); what CI asserts is only that both recovery paths reconstruct identical state.
7. `07-sprint-5-close.md` (depends on 1–6) — README update (new `SAVE`/`REPLICAOF`/`PSYNC` commands, the `ROCKET_MEM_SNAPSHOT_PATH` env var, the recovery-time numbers, and the known limits: no partial resync, no auth on `PSYNC`, unbounded replica fan-out queues, stale `role: master` in `HELLO`/`INFO`), full workspace verification, Sprint 5 status/DoD tick in `../../rocket-mem-sprint-plan.md`, matching Sprint 4's plan-10 close-out pattern.

## Definition of done for the sprint

Matches Sprint 5 in `../../rocket-mem-sprint-plan.md`:
- [ ] Recovery time benchmark (snapshot+AOF vs full AOF replay) recorded, showing clear improvement
- [ ] 1 leader + 2 follower integration test passes, writes visible within a bounded time window
- [ ] Kill-and-reconnect-follower test passes (even if it falls back to full resync)
- [ ] `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` clean, `cargo fmt --all -- --check` clean (carried forward from Sprints 1–4, not re-stated per item below)
