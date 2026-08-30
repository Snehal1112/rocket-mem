# Sprint 5 — Snapshotting & Replication: Spec & Design

**Goal:** a follower stays in sync with a leader in real time; startup time drops sharply via snapshot + incremental AOF — matching `../../rocket-mem-sprint-plan.md`'s Sprint 5 goal.

**Scope:** covers Sprint 5's 5 backlog items (see `../../rocket-mem-sprint-plan.md`, Sprint 5, and `../../rocket-mem-production-plan.md`, Weeks 9–10). This doc fixes the shared design decisions — the snapshot wire/on-disk format, the hybrid-recovery offset scheme, the replication transport, and the follower's read-only/reconnect semantics — that every implementation plan below assumes as ground truth. Individual plans don't re-derive these; they reference this doc.

**Architecture recap:** this sprint adds two new capabilities on top of Sprint 4's AOF layer without touching its shape. Snapshotting is a new `engine`-crate module (`snapshot.rs`) plus two new `Store` methods; it never changes `Shard`'s `Entry` struct or any existing command path. Replication reuses the *exact* ordered, already-rewritten (`SPOP`→`SREM`, `EXPIRE`-family→`PEXPIREAT`) RESP frame stream `dispatch_and_log` already produces for the AOF — a leader fans those same encoded bytes out to connected followers from inside the same `AofWriter::lock_for_ordering()` critical section that already serializes AOF writes. A follower is a normal `rocket-mem` server process that additionally runs one background task applying a leader's stream via the existing non-logging `dispatch()` (the same function Sprint 4's AOF replay uses) — no new dispatch path, no new command-application logic.

## Global Constraints

- No AOF compaction/rewrite this sprint (no `BGREWRITEAOF`-equivalent) — the AOF keeps growing regardless of snapshots taken; a snapshot is a startup-time optimization only, not a space-reclamation mechanism.
- No partial resync / replication offset tracking this sprint — every (re)connect between a follower and its leader is a full resync (fresh snapshot transfer), matching the sprint plan's own stated acceptable fallback. `REPLICAOF`'s "resume from offset" language in the sprint backlog names the *command*, not a promise of partial resync semantics this sprint.
- No authentication/authorization on `PSYNC` or replica connections — any client that sends `PSYNC` is treated as a legitimate replica. Sprint 8 (`AUTH`/ACLs/TLS) is the first point auth exists anywhere in this project; adding it only to replication now would be inconsistent, narrow scope.
- A follower keeps no AOF of its own this sprint — its state is fully ephemeral, reconstructed from the leader on every start/reconnect. It never calls `dispatch_and_log`, only `dispatch`.

---

## Decision: snapshot serialization uses `bincode` over a `SerializableValue` mirror type, not a direct `Value` derive

`Value`'s `SortedSet` variant stores both a `HashMap<Bytes, OrderedFloat<f64>>` (`scores`) and a `BTreeSet<(OrderedFloat<f64>, Bytes)>` (`by_score`) — two views of the same data, kept in sync by `SortedSet::insert`/`remove`. Deriving `serde::Serialize`/`Deserialize` directly on `Value` would either require `ordered-float`'s `serde` feature (an extra dependency knob solely for this one variant) and would serialize both redundant views, doubling the on-disk size of every sorted set for no benefit.

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

TTLs cross the snapshot boundary as wall-clock, not monotonic time — `std::time::Instant` (what `Entry::expires_at` actually stores) has no defined relationship to a Unix timestamp and cannot be reconstructed after a process restart, the same problem Sprint 4's `EXPIREAT` dispatcher arm solved with an `Instant`↔`SystemTime` conversion. Each snapshot entry carries `expires_at_unix_ms: Option<i64>`, computed at snapshot time via `SystemTime::now() + (expires_at - Instant::now())` and converted back to an `Instant` on load via the same delta approach. **Decision:** this conversion helper (`instant_from_unix_ms`/its inverse) moves out of `crates/server/src/dispatcher.rs`, where Sprint 4 left it, into `crates/common/src/lib.rs` — the one crate both `engine` (which needs it for `snapshot.rs`) and `server` (which still needs it for `EXPIREAT`/`PEXPIREAT`) already depend on, and which Sprint 1 established as having zero dependencies of its own, so a pure-`std::time` helper fits it without adding anything. `dispatcher.rs`'s existing `EXPIREAT`/`PEXPIREAT` arms switch to calling the moved version; no behavior change, pure relocation.

**New workspace dependencies:** `serde = { version = "1", features = ["derive"] }`, `bincode = "1"`, and `bytes` gains a `serde` feature (`bytes = { version = "1", features = ["serde"] }`) so `Bytes` fields serialize without a manual `Vec<u8>` round-trip.

## Decision: `Store` gains `snapshot_entries`/`load_snapshot_entries`; `Shard`'s `Entry` stays private

Snapshotting needs to walk every key, its `Value`, and its expiry across all 16 shards — but `Entry` (holding the raw `Instant` and the `last_touched: AtomicU64` recency counter) is intentionally `Shard`-private, and should stay that way: `snapshot.rs` has no business knowing about the recency clock, and exposing `Entry` publicly would leak an implementation detail past the boundary Sprint 4 deliberately drew. **Decision:** two new `Store` methods do the shard-walking, returning/accepting only what a snapshot actually needs:

```rust
// crates/engine/src/store.rs
pub fn snapshot_entries(&self) -> Vec<(Bytes, Value, Option<std::time::Instant>)> {
    // walks each shard under its own read lock in turn, cloning out
    // (key, value, expires_at) for every non-expired entry — same pattern
    // Shard::keys() already uses for its own read-lock iteration
}

pub fn load_snapshot_entries(&self, entries: Vec<(Bytes, Value, Option<std::time::Instant>)>) {
    // clears every shard's map first (a snapshot load always fully replaces
    // current state), then re-inserts each entry via the existing set() +
    // expire_at() paths — no new Shard-level method needed
}
```

`last_touched` is **not** preserved across a snapshot round-trip — every loaded entry starts at whatever "just touched" value `set` already assigns new entries, which is correct: eviction only needs *relative* freshness going forward, not a value that survived a restart. `snapshot::serialize`/`deserialize` (in `snapshot.rs`) call these two `Store` methods and own the actual `bincode` encode/decode plus the `SerializableValue` conversion from the decision above — `store.rs` never imports `serde` or `bincode`. `Engine` (`crates/engine/src/engine.rs`) gains two thin wrappers, matching its existing role as "a thin public facade over `Store`" (per `CLAUDE.md`): `pub fn snapshot(&self) -> Vec<u8>` (calls `self.store.snapshot_entries()` then `snapshot::serialize`) and `pub fn load_snapshot(&self, bytes: &[u8]) -> Result<(), SnapshotError>` (calls `snapshot::deserialize` then `self.store.load_snapshot_entries`) — these are what `SAVE`, leader-side `PSYNC` handling, and follower-side sync all actually call, never `Store`'s methods directly, matching how every other engine-crate consumer already goes through `Engine`, not `Store`.

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

`aof::replay` gains a `start_at: u64` parameter (default `0` for every existing Sprint 4 call site — AOF-only recovery is unaffected): decoding begins at that byte offset into the file rather than byte 0, and the corrupt-tail-truncation logic (unchanged from Sprint 4) still measures `valid_len` relative to the whole file, not relative to `start_at`, so truncation-on-corruption still removes bytes from the true end of the file regardless of where replay started reading.

**Atomicity requirement — `SAVE` must hold `lock_for_ordering()` across both the offset read and the snapshot walk:** `current_offset()` and `Store::snapshot_entries()` are two separate, non-atomic operations. Taken independently, a concurrent write could land between them — e.g. an `RPUSH` commits to the engine and appends to the AOF in the gap between `SAVE` reading the offset and walking the shards, so the pushed value would be captured in *both* the snapshot (already reflecting the push) *and* the AOF tail after the recorded offset (the append that just landed) — a replay would then apply that `RPUSH` a second time, corrupting the list with a duplicate (unlike idempotent commands such as `SET`/`SADD`, replaying `RPUSH`/`LPUSH` twice is not a no-op). **Decision:** the `SAVE` dispatcher arm acquires `aof.lock_for_ordering()` — the exact same lock `dispatch_and_log` already holds across every write's "mutate, then log" step — before calling `current_offset()`, and holds it across the subsequent `Store::snapshot_entries()` call too, releasing it only once both have completed. Because every concurrent writer's `dispatch_and_log` must acquire that same lock before it can even begin mutating the engine, holding it for the duration of `SAVE` guarantees no write can commit between the offset read and the snapshot walk — the engine state captured by `snapshot_entries()` is exactly the state implied by "every write up to `current_offset()`'s returned length, and no more." This makes `SAVE` a genuine global write-stall for its duration (not just a stall on the calling connection, as an earlier framing of this decision assumed) — an accepted P0 tradeoff at this project's scale, and precisely the complexity a real non-blocking `BGSAVE` (P2, only sketched — see below) would need to solve properly (e.g. via a copy-on-write style point-in-time view) rather than being a plain `spawn_blocking` wrapper.

**On leader startup** (`main.rs`, rewritten again — this is now its third revision after Sprint 4's plans 05 and 06): if `ROCKET_MEM_SNAPSHOT_PATH` exists, load it via `snapshot::deserialize` into a fresh `Engine`, read its embedded offset, then call `aof::replay(aof_path, &engine, start_at: offset)`. If no snapshot file exists, behavior is unchanged from Sprint 4 (`start_at: 0`, full AOF replay).

## Decision: `SAVE` is a blocking P0 command; a non-blocking `BGSAVE`-equivalent is P2 and only sketched, not built

The sprint backlog already scopes "`BGSAVE`-equivalent non-blocking snapshot" as P2 — the first thing to cut if the sprint runs long. **Decision:** the P0 path is a plain, blocking `SAVE` dispatcher command: it acquires `aof.lock_for_ordering()`, calls `current_offset()` then `Store::snapshot_entries()` under that lock (per the atomicity requirement above), releases the lock, then `bincode`-encodes the result plus the offset header and writes it to `ROCKET_MEM_SNAPSHOT_PATH` (new env var, default `./dump.snapshot`, read once at startup exactly like `ROCKET_MEM_AOF_PATH`) before replying `+OK` — all synchronously on the calling connection's task. Because it holds `lock_for_ordering()`, `SAVE` is a genuine **global write-stall**: every other connection's `dispatch_and_log` blocks trying to acquire the same lock until `SAVE` releases it, not merely the calling connection — an accepted P0 tradeoff at this project's scale, and exactly why the sprint backlog scopes a true non-blocking version separately. If time allows within the sprint, `BGSAVE` is a P2 stretch item whose non-blocking property is genuinely harder than a `tokio::task::spawn_blocking` wrapper around the same call — spawning the same lock-holding work onto a blocking thread still stalls every writer for the same duration, just off the async runtime's reactor thread — so it is intentionally left unscoped beyond this note; a real solution needs either the same global stall (acceptable, just moved off the client-facing connection) or a copy-on-write-style point-in-time mechanism this sprint doesn't build.

`SAVE` is **not** added to `WRITE_COMMANDS` (`crates/server/src/aof.rs`) — it doesn't mutate the keyspace, so it has nothing for the AOF to log, and it must still be permitted on a read-only follower (an operator may want to snapshot a follower's current state) — the read-only gate decision below only rejects commands in `WRITE_COMMANDS`.

**Where `SAVE` lives:** the plain `dispatch(engine, frame, protocol, client_id) -> Frame` function Sprint 4 established has no `&AofWriter` parameter — only `dispatch_and_log` receives one — so `SAVE` cannot be just another match arm inside `dispatch`'s existing giant match statement the way every other command is. **Decision:** `dispatch_and_log` special-cases `SAVE` the same way it already special-cases the `WRITE_COMMANDS` rewrites (`SPOP`→`SREM`, etc.) — a check for the command name *before* delegating to `dispatch` — except `SAVE` never reaches `dispatch` at all: `dispatch_and_log` recognizes it, performs the lock-acquire/offset/snapshot/write sequence directly (it already has `aof` in scope), and returns `Frame::Simple("OK")` (or a `Frame::Error` on an I/O failure writing the snapshot file) without calling `dispatch`. This keeps `dispatch`'s signature and its ~250 existing direct call sites (which never pass an `AofWriter`) completely untouched, matching Sprint 4's established constraint.

## Decision: one `ReplicationHandle` struct threads leader/follower replication state through `dispatch_and_log`

`REPLICAOF`/`REPLICAOF NO ONE` have the same problem `SAVE` does above: they need state plain `dispatch(engine, frame, protocol, client_id)` has no parameter for — specifically, the `is_replica: Arc<AtomicBool>` flag, a way to cancel a previously-started `replication_client_loop` task, and (for the leader side of every *other* write command) the `ReplicaRegistry` fan-out target list. **Decision:** a single new struct bundles all three, constructed once in `main.rs` and threaded through `serve()`/`handle_connection()` exactly like `engine: Arc<Engine>` and `aof: Arc<AofWriter>` already are, and added as a new parameter to `dispatch_and_log`:

```rust
// crates/server/src/replication.rs
pub struct ReplicationHandle {
    pub registry: ReplicaRegistry,              // leader side: connected replicas to fan writes out to
    pub is_replica: std::sync::Arc<std::sync::atomic::AtomicBool>, // follower side: read-only gate
    follower_task: std::sync::Mutex<Option<FollowerTaskHandle>>,   // follower side: cancels a running replication_client_loop
}
```

`dispatch_and_log`'s signature becomes `dispatch_and_log(engine: &Engine, aof: &AofWriter, replication: &ReplicationHandle, frame: Frame, protocol: &mut Protocol, client_id: u64) -> Frame` — its ~250 existing call sites across `dispatcher.rs`'s and `connection.rs`'s tests all construct a `ReplicationHandle::default()` (an idle one: no replicas registered, `is_replica` false, no follower task running), so none of them observe any behavior change; only `connection.rs`'s real `handle_connection` and the new `crates/server/tests/replication.rs` integration tests construct a live one. `REPLICAOF`/`REPLICAOF NO ONE`/`SAVE` are all intercepted inside `dispatch_and_log` before it would otherwise delegate to `dispatch`, exactly like the `SAVE` decision above — `dispatch` itself never learns any of these three command names exist.

## Decision: replication transport reuses the RESP port; `PSYNC` transitions a connection into one-way streaming mode

**Decision:** no second listener, no separate replication port. A follower's connection to a leader is a plain TCP connection to the leader's existing `ROCKET_MEM_ADDR` port, and `PSYNC` (a new command with no arguments this sprint — see Global Constraints on why partial resync isn't supported) is the signal that flips that connection's handling in `connection.rs::handle_connection` from the normal request/reply loop into a dedicated replica-serving path:

```rust
// crates/server/src/connection.rs — sketch, exact framing decided by the implementing plan
async fn serve_replica(mut framed: Framed<TcpStream, RespCodec>, engine: &Engine, replication: &ReplicationHandle) {
    let snapshot_bytes = engine.snapshot(); // Engine::snapshot, offset-prefixed — see the Store decision above
    // write snapshot_bytes length-prefixed (u32 LE) directly to the underlying socket,
    // bypassing the RESP codec — this is not a RESP frame, it's the raw snapshot blob
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
    replication.registry.register(tx);
    // drain rx, writing each already-RESP-encoded frame's raw bytes to the socket as they arrive;
    // this connection never reads again once PSYNC has been handled
}
```

A new `ReplicaRegistry` (`crates/server/src/replication.rs`, `Mutex<Vec<mpsc::UnboundedSender<Bytes>>>`, wrapped in the `Arc` the leader's `main.rs` already threads through `serve()`) holds one sender per connected replica. **Fan-out hook:** `dispatch_and_log` (`crates/server/src/dispatcher.rs`), still holding the `lock_for_ordering()` guard, after `aof.append(frame.clone())` succeeds, calls `replicas.broadcast(encoded_bytes)` using the *same already-RESP-encoded bytes* the AOF append just wrote — no second encode pass. A send failure (channel closed — the replica connection died) removes that sender from the registry rather than erroring the write; the leader's own mutation and AOF append already succeeded and must not be rolled back because a replica dropped. A leader with zero connected replicas pays only the cost of an empty `Vec` check per write.

## Decision: a follower keeps no AOF; it applies the leader's stream via plain `dispatch()`, never `dispatch_and_log`

Per the Global Constraints, follower state is fully ephemeral this sprint. **Decision:** `REPLICAOF <host> <port>` (intercepted in `dispatch_and_log`, per the `ReplicationHandle` decision above) spawns a background task, `replication_client_loop` (new `crates/server/src/replication.rs` function), storing its cancellation handle in `ReplicationHandle::follower_task` — a prior `REPLICAOF` call's task, if any, is cancelled first so only one replication task ever runs at a time. The command itself returns `+OK` immediately — connecting, handshaking, and syncing all happen in the background, not on the client's connection.

The task: connects to `<host>:<port>`, sends `PSYNC`, reads the length-prefixed snapshot blob and loads it via `Store::load_snapshot_entries` (replacing whatever this follower held before — every sync, first or reconnect, starts from a clean slate), then loops: read a length-prefixed RESP frame, decode it with `RespCodec` (the same decoder the TCP accept path already uses), and apply it via `dispatcher::dispatch(&engine, frame, &mut Protocol::default(), 0)` — the exact function AOF replay already calls, so no new command-application code path exists anywhere in this project. `REPLICAOF NO ONE` sets the cancellation flag, the loop observes it (checked once per iteration, not via `select!` against a channel — this sprint's scope doesn't need sub-tick cancellation latency) and returns, and the follower goes back to normal (non-replicating, writable) operation.

**Reconnect:** any read/write error on the leader connection (leader restarted, network blip, leader process killed) logs to stderr and, after a fixed ~1s backoff, restarts the entire loop body from the top — a fresh `PSYNC`, a fresh full snapshot. This is deliberately simple: there is no distinction in this sprint's code between "first sync" and "resync after disconnect," they are the same code path run again.

## Decision: read-only enforcement is one `Arc<AtomicBool>` role flag checked in `dispatch_and_log`

**Decision:** `ReplicationHandle::is_replica` (from the decision above) starts `false` in `main.rs`'s constructed handle, and the `REPLICAOF`/`REPLICAOF NO ONE` arms `dispatch_and_log` intercepts set it `true`/`false` respectively. `dispatch_and_log` gains a check at its top: if `replication.is_replica.load(Ordering::Relaxed)` is true and the incoming command's name is in `WRITE_COMMANDS` (the same static allowlist Sprint 4 already defined in `aof.rs`), it returns `Frame::Error("READONLY You can't write against a read only replica.".into())` immediately, without calling `dispatch` or touching the AOF/replica fan-out at all — matching real Redis's exact error text and behavior. This check is bypassed entirely by the follower's own `replication_client_loop`, which calls `dispatch()` directly (not `dispatch_and_log`), so replicated writes are never blocked by the gate meant for client-originated ones. Read commands (`GET`, `KEYS`, `TTL`, etc. — anything not in `WRITE_COMMANDS`) are unaffected on a replica, matching this sprint's "read replica" scope decision.

## Decision: replication integration tests run in-process, not via subprocess

Sprint 4's kill-and-recover suite is explicitly `../../docs/superpowers/specs/2026-08-29-sprint-2-spec.md`'s one deliberate exception to in-process-only testing, because it needed real OS `SIGKILL`/filesystem crash semantics an in-process test cannot simulate. Replication has no equivalent requirement — the three DoD items (recovery-time benchmark, 1-leader-2-follower propagation, kill-and-reconnect-follower) all test this project's own application-level logic, not OS/filesystem crash behavior. **Decision:** `crates/server/tests/replication.rs` follows the existing `integration.rs` pattern — `serve()` spawned in-process against real `127.0.0.1:0` TCP listeners, driven via the `redis` crate already in `dev-dependencies`. "Kill" a follower in the kill-and-reconnect test means closing its TCP connection to the leader (simulating a network drop or leader-side restart) and/or aborting its `serve()` task — not sending a real `SIGKILL` to a subprocess, since no subprocess exists in this test shape.

---

## Sequencing

Plans depend on each other in this order (to be written by `superpowers:writing-plans`, living in `../plans/2026-08-30-sprint-5-plans/`):

1. `01-snapshot-serialization-and-save.md` — `SerializableValue`, `Store::snapshot_entries`/`load_snapshot_entries`, `snapshot::serialize`/`deserialize`, the `SAVE` dispatcher command. Independent of everything else this sprint.
2. `02-hybrid-recovery-and-aof-offset.md` (depends on 1) — `AofWriter::current_offset`, the offset-prefixed snapshot file header, `aof::replay`'s new `start_at` parameter, `main.rs`'s startup rewrite to prefer snapshot+tail when a snapshot exists.
3. `03-replica-registry-and-leader-fanout.md` (depends on 1, and on Sprint 4's `dispatch_and_log`/`AofWriter::lock_for_ordering`) — `ReplicaRegistry`, `PSYNC` leader-side handling in `connection.rs`, the `dispatch_and_log` fan-out hook.
4. `04-replicaof-and-follower-apply-loop.md` (depends on 3) — `REPLICAOF`/`REPLICAOF NO ONE` dispatcher commands, `replication_client_loop`, the `is_replica` flag and `-READONLY` gating in `dispatch_and_log`.
5. `05-replication-integration-tests.md` (depends on 1–4) — the three DoD tests: recovery-time benchmark, 1-leader-2-follower propagation, kill-and-reconnect-follower.
6. `06-sprint-5-close.md` (depends on 1–5) — README update (new `SAVE`/`REPLICAOF`/`PSYNC` commands, new env vars), full workspace verification, Sprint 5 status/DoD tick in `../../rocket-mem-sprint-plan.md`, matching Sprint 4's plan-10 close-out pattern.

## Definition of done for the sprint

Matches Sprint 5 in `../../rocket-mem-sprint-plan.md`:
- [ ] Recovery time benchmark (snapshot+AOF vs full AOF replay) recorded, showing clear improvement
- [ ] 1 leader + 2 follower integration test passes, writes visible within a bounded time window
- [ ] Kill-and-reconnect-follower test passes (even if it falls back to full resync)
- [ ] `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` clean, `cargo fmt --all -- --check` clean (carried forward from Sprints 1–4, not re-stated per item below)
