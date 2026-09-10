# Failover Safety Primitives — Shared Design Contract

**Status: normative.** This document is not a plan. It is the single source of truth that every
plan in this folder builds on. Plans are written and executed independently, often by different
agents that never see each other's work — this contract is what stops them from inventing five
different names for the same field.

**If you are implementing a plan in this folder: read this document first, in full.** Where a
plan and this contract disagree, this contract wins, and the disagreement is a bug worth
reporting before you write code.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md)

---

## 0. What this is and is not

The spec's central finding: automatic failover built on rocket-mem's current offset-less,
ack-less replication would silently discard acknowledged writes — **strictly worse than today's
honest "no failover."** So none of these plans implement automatic promotion. They build the
safety primitives that make a *correct* failover possible later, plus honest reporting of what
the system actually knows.

| Chain | Plans | Spec step |
|---|---|---|
| A — Replication offsets | 01-06 | Step 1 |
| B — `min-replicas-to-write` fencing | 07-09 | Step 2 |
| C — Runbook + alerting probe | 10-11 | Step 3 |
| D — Cluster health honesty | 12-14 | "cheapest honest first step" |

**Explicit non-goals across every plan in this folder:** automatic promotion, automatic client
redirect, automated `cluster.conf` reconciliation, embedded consensus. No plan here may add a
code path that promotes a replica or rewrites cluster topology on its own. If a plan seems to
require it, stop and re-read the spec.

---

## 1. Ground truth: the code as it exists today

Verified 2026-09-09 against the current tree. Quoted so plan implementers do not have to
re-derive it, and so a plan's code compiles against the real signatures.

### 1.1 `ReplicaRegistry` — the leader's fan-out list

`crates/server/src/replication.rs:23-86`:

```rust
#[derive(Default)]
pub struct ReplicaRegistry {
    replicas: std::sync::Mutex<
        Vec<(
            Option<String>,
            tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
        )>,
    >,
}

pub fn broadcast(&self, bytes: bytes::Bytes) {
    let mut replicas = self.replicas.lock().unwrap_or_else(|e| e.into_inner());
    replicas.retain(|(_, tx)| tx.send(bytes.clone()).is_ok());
}
```

The channel carries **already-encoded RESP wire bytes**, not `Frame`s. Pruning of dead replicas
is lazy — it happens when a send fails.

### 1.2 The broadcast call site

`crates/server/src/dispatcher.rs:3212-3253`, inside `dispatch_and_log_inner`, while still holding
the AOF per-shard ordering guard `_order_guard`:

```rust
let mut to_broadcast: Vec<Bytes> = Vec::new();
for frame_to_log in to_log {
    let encoded = match crate::aof::encode_frame(&frame_to_log) { ... };
    if let Err(e) = aof.append_encoded(encoded.clone()) { ... }
    to_broadcast.push(Bytes::from(encoded));
}
for encoded in to_broadcast {
    replication.registry.broadcast(encoded);
}
drop(_order_guard);
```

Only `crate::aof::WRITE_COMMANDS` (an allowlist in `crates/server/src/aof.rs:408`) reach this
point, and only after a successful, non-error `dispatch` reply.

### 1.3 `serve_replica` — the leader's PSYNC handler, **write-only today**

`crates/server/src/connection.rs:305-364`:

```rust
async fn serve_replica<S>(
    framed: Framed<S, RespCodec>,
    aof: &AofWriter,
    replication: &crate::replication::ReplicationHandle,
    advertised_addr: Option<String>,
) where S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin {
    use tokio::io::AsyncWriteExt;
    let (snapshot_bytes, mut rx) = {
        let _order_guard = aof.lock_all_shards();
        let bytes = replication.engine().snapshot(0);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        replication.registry.register(advertised_addr, tx);
        (bytes, rx)
    };
    let mut parts = framed.into_parts();
    if !parts.write_buf.is_empty() && parts.io.write_all(&parts.write_buf).await.is_err() {
        return;
    }
    let io = &mut parts.io;
    if io.write_all(&(snapshot_bytes.len() as u64).to_le_bytes()).await.is_err() { return; }
    if io.write_all(&snapshot_bytes).await.is_err() { return; }
    while let Some(bytes) = rx.recv().await {
        if io.write_all(&bytes).await.is_err() { return; }
    }
}
```

**Critical:** there is no `read` call anywhere after PSYNC. The source comment says so outright:
*"this connection never reads again once PSYNC has been handled."* A `REPLCONF ACK` sent by a
follower today would be received by the kernel and never read. Plan 04 exists solely to fix this.

The PSYNC wire reply is: **8-byte little-endian length prefix, then the snapshot blob, then a
raw stream of pre-encoded RESP frames, forever.** There is no `+FULLRESYNC` line.

### 1.4 The snapshot's own 8-byte header — the free offset channel

`Engine::snapshot(&self, aof_offset: u64) -> Vec<u8>` (`crates/engine/src/engine.rs:112`) writes an
8-byte little-endian header, and `Engine::load_snapshot(&self, bytes: &[u8]) -> Result<u64, SnapshotError>`
(`:122`) returns it. `serve_replica` currently passes `0`, and `sync_once` currently discards the
returned value. **Both are free channels this design claims** — see §2.2.

### 1.5 `sync_once` — the follower's sync

`crates/server/src/replication.rs:608-743`. Sends optional `AUTH`, then `PSYNC`, then calls
`framed.into_parts()` and reads the raw 8-byte length prefix + blob off `parts.io` directly. A
leading `-` byte means the leader sent a RESP error instead. It then rebuilds
`Framed::from_parts(parts)` and loops on `framed.next()`, applying each frame with plain
`crate::dispatcher::dispatch(...)` (never `dispatch_and_log` — replicated writes bypass AOF
logging by design). Reconnect is a fixed 1-second backoff loop in `replication_client_loop`
(`:510-546`). Generation-counter staleness is re-checked before each applied frame.

### 1.6 `INFO REPLICATION` today

`crates/server/src/dispatcher.rs:1899-1934`. Leader emits `role:master`, `connected_slaves:<n>`,
and per replica `slave{i}:ip={ip},port={port},state=online`. Follower emits `role:slave`,
`master_host`, `master_port`, `master_link_status:up|down`. **No offset key exists anywhere.**

A unit test at `dispatcher.rs:5142` pins the exact current slave line:
`"slave0:ip=127.0.0.1,port=6480,state=online\r\n"` — **the plan that extends this line must update
that test in the same commit.** That is **plan 05**, which rewrites the slave line's spelling to add
`offset=`/`lag=`.

Plans 01-03 do *not* break it: every `info_text` assertion is `contains`-based, and those plans only
*add* whole new lines (`master_repl_offset:`, `slave_repl_offset:`) without touching the existing
`slave{i}:...state=online` spelling. They must still pin their new keys with positive assertions in
the same commit that adds them — adding a line nothing asserts on is how a dropped `push_str`
survives CI.

### 1.7 The read-only gate — the precedent every new gate copies

`crates/server/src/dispatcher.rs:3066-3076`:

```rust
if replication
    .is_replica
    .load(std::sync::atomic::Ordering::Relaxed)
    && extract_write_command_name(&frame).is_some()
{
    return Frame::Error("READONLY You can't write against a read only replica.".into());
}
```

Gate order in `dispatch_and_log_inner` is load-bearing and documented in-source:
`auth_gate` → `cluster_redirect` → **READONLY** → command interceptions → write path.

### 1.8 Hardcoded cluster health

- `cluster_info_text` (`dispatcher.rs:1602`): `cluster_state:ok`, `cluster_slots_pfail:0`,
  `cluster_slots_fail:0`, `cluster_my_epoch:0`, `cluster_current_epoch:0` — all literals.
- `cluster_nodes_text` (`dispatcher.rs:1624`): the literal `connected`, flags always
  `master`/`myself,master`. Its own doc comment admits: *"`connected` is likewise unconditional:
  nothing here can observe a peer disconnecting."*
- `cluster_shards_reply` (`dispatcher.rs:1658`): `role` → `"master"`, `health` → `"online"`,
  `replication-offset` → `Frame::Integer(0)`, all literals.

`ClusterConfig` is `Option<Arc<ClusterConfig>>` on `ReplicationHandle`, set once via
`with_cluster`, with **no interior mutability anywhere**. Its doc comment: *"There is no gossip
and no resharding, so this never changes for the life of the process."*

### 1.9 Config, metrics, and test conventions

- Config: `pub struct Config` with `#[serde(default)]`, a matching `Cli` where every field is
  `#[arg(long)] pub name: Option<T>`, and `cli_overrides`'s `set!` macro — **which only handles
  `Option<String>`** (`Value::from(v.as_str())`). Numeric fields use the manual pattern:
  ```rust
  if let Some(v) = cli.slowlog_threshold_micros {
      map.insert("slowlog_threshold_micros", Value::from(v));
  }
  ```
- Validators: `pub fn validate_X(config: &Config) -> Result<(), std::io::Error>` returning
  `std::io::Error::new(std::io::ErrorKind::InvalidInput, "<message>")`, called from `main.rs`
  **before any listener binds**.
- Metrics: `::metrics::gauge!("rocket_mem_x").set(v as f64)` and
  `::metrics::counter!("rocket_mem_x_total").increment(1)` / `.absolute(v)`. Sampled gauges are
  refreshed in `metrics::refresh_sampled_gauges(engine, replication)` at scrape time.
- Config tests use `figment::Jail::expect_with(|jail| { ...; Ok(()) })`.
- Integration tests use `spawn_node()` in `crates/server/tests/replication.rs:11-40`, which
  returns `(TempDir, Arc<Engine>, Arc<AofWriter>, Arc<ReplicationHandle>, String /* addr */)`,
  and the bounded-poll `wait_for(&engine, key, value)` helper at `:42-60`.
- There is **no** integration-level `INFO` assertion helper. Build one where needed following
  `crates/server/tests/cluster.rs:95-104`'s raw-RESP `send()` pattern.

---

## 2. Normative decisions

### 2.1 The offset counts **bytes**, not frames

`master_repl_offset` is a monotonic count of **replication-stream bytes** — the summed
`encoded.len()` of every frame handed to `registry.broadcast`.

**Why bytes and not a simpler frame count:** the key names `master_repl_offset` /
`slave_repl_offset` are Redis's, and every tool that reads them assumes byte semantics. Emitting a
frame count under a byte-count name would make `INFO` lie in exactly the way this spec exists to
stop. The leader already has `encoded.len()` for free at the broadcast site.

**The invariant that makes the follower's count exact:** the follower must re-encode each applied
frame to learn its length (`crate::aof::encode_frame(&frame)?.len()`), and that re-encoding is
byte-identical to what the leader counted **because only `WRITE_COMMANDS` frames are replicated,
and those are always `Frame::Array` of `Frame::Bulk`** — a shape whose RESP encoding is identical
under RESP2 and RESP3. (`Frame::Null` and `Frame::Map` *do* encode differently per protocol
version; neither can ever appear in a replicated write command.) Any plan that widens what gets
replicated breaks this invariant and must revisit this decision.

**The counter advances even with zero replicas connected.** It measures the write stream, not
what anyone received.

**The offset is process-local and resets to 0 on restart.** There is no replication ID and no
partial resync. This is safe *only because* every reconnect is a full resync that re-seeds the
follower's offset from the snapshot header (§2.2) — a follower can never carry a stale offset
across a leader restart. **Do not** add cross-restart offset persistence; it would imply a partial
resync capability that does not exist.

### 2.2 The snapshot's 8-byte header carries the handoff offset

`serve_replica` passes the leader's live `master_repl_offset` where it currently passes `0`, read
under the same `aof.lock_all_shards()` guard that captures the snapshot and registers the replica.
`sync_once` seeds the follower's `slave_repl_offset` from `load_snapshot`'s already-returned value.

**This requires no wire-format change at all** — the field exists, is already transmitted, and is
currently zero in this path.

**Semantic note plans must preserve:** the header's parameter is named `aof_offset`. Its meaning is
generalized to *"the stream position this snapshot image corresponds to"* — the AOF offset for a
disk snapshot, the replication-stream offset for a PSYNC snapshot. Do not rename the parameter
(needless churn across the engine crate); do document the dual meaning.

Plan 02 **must** update **all four** of these doc comments, not just the first two — the other two
become fresh lies the moment this lands:

| Item | Why it must change |
|---|---|
| `Engine::snapshot` | Names the parameter's new dual meaning. |
| `snapshot::serialize` | Currently instructs: *"Pass `0` when there's no AOF to correlate against (a follower's `PSYNC` reply, which discards the offset on the receiving end anyway)"* — this sentence **directly contradicts** the new design and must go. |
| `Engine::load_snapshot` | Currently says it returns "the AOF offset". |
| `snapshot::deserialize` | Same stale "AOF offset" claim. |

Atomicity requirement: reading the offset, capturing the snapshot, and registering the replica
**must** happen inside one `aof.lock_all_shards()` critical section, or a write can slip between
the snapshot and the registration and be lost with no way to detect it.

### 2.3 `REPLCONF ACK` is a normal RESP array on the existing socket

Follower → leader: `Frame::Array([Bulk("REPLCONF"), Bulk("ACK"), Bulk("<offset>")])`, the offset
formatted as decimal ASCII. No new port, no new connection.

The leader must therefore **read** from a replica connection, which it does not do today (§1.3).
Plan 04 restructures `serve_replica` to `tokio::io::split` the socket and `tokio::select!` between
the outbound `rx.recv()` and an inbound `FramedRead<_, RespCodec>`.

**Gotcha plans must handle:** `framed.into_parts()` yields `parts.read_buf`, which may already hold
bytes the decoder read ahead. Those bytes are gone from the socket, so the inbound reader **must**
replay them before it touches the socket again:

```rust
let (rd, wr) = tokio::io::split(parts.io);
let rd = std::io::Cursor::new(parts.read_buf).chain(rd);
let mut inbound = FramedRead::new(rd, RespCodec::default());
```

Dropping `read_buf` silently loses whatever the follower pipelined behind its `PSYNC`.

> **Corrected twice. Read this whole note before using the snippet — the obvious forms are both
> wrong, in different ways.**
>
> **First correction, 2026-09-10.** The original draft showed `FramedRead::from_parts(read_parts)`.
> **That does not compile:** `tokio_util::codec::FramedRead` has `into_parts` but no `from_parts` —
> only `Framed` has both (verified against tokio-util 0.7.19, the pinned version).
>
> **Second correction, 2026-09-10, and this one is the dangerous one.** The replacement this section
> then carried —
> ```rust
> *inbound.read_buffer_mut() = parts.read_buf;
> ```
> — compiles, preserves the bytes, and **still loses the ack**, silently. `read_buffer_mut()`
> exposes only the `BytesMut`. `FramedRead::new` initialises state via `ReadFrame::default()`, whose
> `is_readable` is **false**, and `poll_next` decodes the buffer only inside `if state.is_readable`
> (`framed_impl.rs:183`), otherwise reading the socket first and setting the flag afterwards
> (`:248`). So seeded bytes sit undecoded until more bytes arrive or the connection hits EOF. That
> tokio-util's own `impl From<BytesMut> for ReadFrame` sets `is_readable = !buffer.is_empty()`
> (`:66-79`) is the proof that seeding a buffer requires setting that flag — and that path is
> reachable only through `Framed::from_parts`, which `FramedRead` does not have.
>
> Production impact had it shipped unnoticed: a follower's **first** ack after every attach would
> stick until its second arrived — one full ack interval of permanent phantom lag on every
> reconnect — and an ack sent before a quiet period would surface only when the follower hung up.
>
> **How it was caught, because the method generalises.** Plan 04 shipped the `read_buffer_mut` form
> and every one of its 1005 tests passed, because nothing in the repo pipelined anything behind a
> `PSYNC`. Plan 04's final review noticed the line was unguarded and made pinning it a **mandatory**
> deliberate-break verification for plan 05: write the test, then delete the line and require the
> test to fail. That verification is what surfaced the defect — the test stayed red after a correct
> implementation, which is the signal a plausible-looking snippet was wrong rather than the code
> using it.
>
> The lesson for this contract: a normative snippet that compiles has been checked for the wrong
> property. Snippets here must be pinned by a test that fails without them.

**Tolerance rule, and the one place its literal reading is unimplementable.** A well-formed inbound
frame that is not a `REPLCONF ACK` is logged at `debug` and ignored. The leader must remain tolerant
of a follower that never acks at all (an older build), treating it as a replica with no ack
information rather than a failure.

But "log and ignore, never disconnect" **cannot** be applied literally to a codec-level decode
error. `RespCodec::decode` returns `Err` on an unknown type byte **without consuming any bytes**
(`crates/protocol/src/codec.rs`), so the reader can never resynchronise — the offending byte stays
at the head of the buffer forever. Simply `continue`-ing past the error therefore drops the replica
anyway, by a less obvious route: `FramedRead` **fuses** after a decoder error, so the poll following
an `Err` yields `None` (`tokio-util-0.7.19/src/codec/framed_impl.rs:164-170`, guarded by
`has_errored` set at `:204`), and `None` is end-of-stream, which correctly ends the connection.

> **Corrected 2026-09-10.** This paragraph previously claimed that `continue` would "spin in an
> infinite hot error loop". That is **false**, and the false claim reached the plan text and a
> source comment before plan 04's Task 2 review caught it and proved the real behavior empirically
> against the pinned tokio-util. The error came from verifying `RespCodec::decode` (correct: it
> never calls `advance` on the error path) without checking the layer above it, where `FramedRead`
> interposes `has_errored`. The *conclusion* — never `continue`, stop reading instead — is
> unchanged and still required; only the mechanism was wrong. Recorded rather than silently edited
> because plans 05 and 06 reason on top of this section.

The resolution, which honors the rule's intent:

- Unparseable *bytes* → stop reading inbound on that connection, while the outbound write stream
  continues untouched. The leader loses that follower's ack information — exactly the state it
  would be in for a follower that never acks — and the follower keeps replicating normally.
- **Never** drop the replica, never send an error reply.

This is strictly more tolerant than disconnecting, and it is what the plans implement.

### 2.4 Names, fixed

Any plan using a different spelling is wrong.

**`ReplicationHandle` additions:**

| Item | Exact form |
|---|---|
| Leader offset | `master_repl_offset: Arc<AtomicU64>` |
| Follower offset | `slave_repl_offset: Arc<AtomicU64>` |
| Read leader offset | `pub fn master_repl_offset(&self) -> u64` |
| Read follower offset | `pub fn slave_repl_offset(&self) -> u64` |
| Advance leader offset | `pub fn advance_master_repl_offset(&self, bytes: u64) -> u64` |
| Set follower offset | `pub fn set_slave_repl_offset(&self, offset: u64)` |
| Advance follower offset | `pub fn advance_slave_repl_offset(&self, bytes: u64) -> u64` |
| Hand the follower slot to the spawned task | `pub fn slave_repl_offset_slot(&self) -> Arc<AtomicU64>` |
| Fencing thresholds | `pub fn with_min_replicas(self, to_write: u64, max_lag: std::time::Duration) -> Self` |

**The `FollowerStatus` field is named `slave_offset`, not `slave_repl_offset`.** `sync_once` has no
`ReplicationHandle` — it receives `FollowerStatus`/`FollowerHandles`, whose existing fields are
`last_apply` and `link_up` (unprefixed). Plans 02, 03, and 06 all write against `status.slave_offset`;
keep that spelling. The `slave_repl_offset` name belongs to the `ReplicationHandle` accessors only.

**Note on the two follower mutators:** the spawned follower task is `'static` and cannot borrow
from `self`, so `sync_once` holds the raw `&AtomicU64` (via `FollowerStatus`, exactly as it already
does for `last_apply` and `link_up`) and does the real seed/advance with `store`/`fetch_add`.
`set_slave_repl_offset` / `advance_slave_repl_offset` compile and behave correctly, but **neither
has a production caller** — as of plan 06 every call site of both is inside a `#[cfg(test)]` module
(`replication.rs`, `metrics.rs:196`, `dispatcher.rs:5834`). They exist so a test can position the
counter, and for a future caller that holds a `ReplicationHandle`; they are not the code path that
moves the counter. **What `INFO` and the metrics exporter actually call is the reader,
`slave_repl_offset()`** — an earlier revision of this note claimed the mutators were used by `INFO`
and metrics, which was wrong and is corrected here. Do not infer from this table that a name is
live in production: the table fixes *spelling*, not reachability. `slave_repl_offset_slot` follows
the existing `last_apply_slot()` / `link_up_slot()` convention.

**`ReplicaRegistry` restructure (plan 05):**

```rust
pub struct ReplicaEntry {
    pub addr: Option<String>,
    tx: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
    pub ack_offset: std::sync::atomic::AtomicU64,
    pub last_ack_unix: std::sync::atomic::AtomicI64,
}
```

`register` returns `Arc<ReplicaEntry>` so `serve_replica` updates acks directly without an id
lookup. `len()`, `addrs()`, `is_empty()`, and `broadcast()` keep their current signatures and
behavior — several call sites depend on them (`INFO`, `metrics::refresh_sampled_gauges`).
`last_ack_unix` is `0` for a replica that has never acked.

New registry methods:
- `pub fn good_replicas(&self, max_lag: std::time::Duration) -> usize` — replicas whose
  `last_ack_unix` is within `max_lag` of now. A replica that has never acked is **not** good.
- `pub fn states(&self) -> Vec<ReplicaState>` — a plain snapshot struct
  (`addr`, `ack_offset`, `last_ack_unix`) for `INFO` and metrics to render without holding the lock.

**`INFO REPLICATION` keys:**

| Role | Keys |
|---|---|
| Leader | `master_repl_offset:<n>`; each slave line becomes `slave{i}:ip={ip},port={port},state=online,offset={ack},lag={secs}` |
| Follower | `slave_repl_offset:<n>` **and** `master_repl_offset:<n>` with the same value (Redis does this; it is the follower's own processed position) |

`lag` is whole seconds since `last_ack_unix`; a replica that has never acked reports
`offset=0,lag=-1`. `-1` means "unknown", never "zero lag".

> **Trap for plan 03 — the generation race changes character once you add the per-frame advance.**
> Found by plan 02's final review. `sync_once` re-checks the generation counter before loading the
> snapshot, but passing that check does not *pin* the generation: a newer `REPLICAOF` can bump it
> immediately afterwards, leaving the stale task to seed `slave_offset` and set `link_up` anyway.
> **Today that is benign** — all follower tasks share one atomic (`slave_repl_offset_slot()` hands
> out an `Arc::clone` of the single field), so the newer task's own seed simply overwrites the
> stale one, and the same window already exposes the entire store contents to a far more
> consequential clobber. **After plan 03 it is no longer benign:** a wrong seed becomes a wrong
> *base* that per-frame `fetch_add`s accumulate on top of, so the offset stays wrong until the next
> full resync instead of self-correcting at the next seed. Plan 03's author must decide whether to
> re-check the generation immediately before the store, or hold the seed until after the check. The
> fix belongs in plan 03, with the code that makes it matter — do not "fix" it in plan 02.

> **Trap for plan 03 — read this before writing the follower's `INFO` branch.** The follower emits
> **both** `slave_repl_offset:<n>` and `master_repl_offset:<n>` with the same value. Both must be
> rendered from `slave_repl_offset()`. Do **not** render the mirrored `master_repl_offset` line
> from `master_repl_offset()` — that counter only advances for writes dispatched *on this node*,
> so on a follower it is stale or zero, and the mirrored line would silently report garbage. Found
> during plan 01's final review.

> **Note for plan 11's runbook.** `master_repl_offset` is **not monotonic across a promotion.** A
> follower promoted with `REPLICAOF NO ONE` becomes a leader whose counter starts near 0 rather
> than continuing from the stream position it had reached as a follower. This is consistent with
> the full-resync design (§2.1) but will look like a counter reset to anyone watching the metric,
> so the runbook must say it out loud rather than let an operator read it as data loss.

**Config fields:**

| Field | Type | Default | Env | CLI |
|---|---|---|---|---|
| `min_replicas_to_write` | `u64` | `0` (disabled) | `ROCKET_MEM_MIN_REPLICAS_TO_WRITE` | `--min-replicas-to-write` |
| `min_replicas_max_lag_secs` | `u64` | `10` | `ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS` | `--min-replicas-max-lag-secs` |
| `cluster_probe_interval_secs` | `u64` | `1` | `ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS` | `--cluster-probe-interval-secs` |
| `cluster_node_timeout_secs` | `u64` | `15` | `ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS` | `--cluster-node-timeout-secs` |

All four are numeric, so all four use the **manual** `if let Some(v) = cli.field` pattern in
`cli_overrides`, not `set!` (§1.9).

**Error string:** `"NOREPLICAS Not enough good replicas to write."` — matches Redis exactly and
follows the house style of `READONLY`/`WRONGPASS` (all-caps prefix, space, capitalized sentence,
terminal period).

**Metric names** (all `rocket_mem_`-prefixed, all gauges except where noted):

| Metric | Meaning |
|---|---|
| `rocket_mem_master_repl_offset` | Leader's stream position |
| `rocket_mem_slave_repl_offset` | This node's applied position (follower) |
| `rocket_mem_replica_min_ack_offset` | Furthest-behind connected replica's acked offset |
| `rocket_mem_good_replicas` | Replicas acked within `min_replicas_max_lag_secs` |
| `rocket_mem_writes_rejected_no_replicas_total` | **Counter.** Writes refused by fencing |
| `rocket_mem_cluster_peers_reachable` | Peers currently probing OK |
| `rocket_mem_cluster_peers_unreachable` | Peers currently marked failed |

**No per-replica metric labels.** Replica addresses are unbounded-cardinality; aggregate gauges
only. Per-replica detail belongs in `INFO`.

### 2.5 Fencing semantics

A write is refused with `NOREPLICAS` when **all** of:
- `min_replicas_to_write > 0`, and
- this node is not a replica (a replica returns `READONLY` first — the existing gate wins), and
- `extract_write_command_name(&frame).is_some()`, and
- `registry.good_replicas(max_lag) < min_replicas_to_write`.

The check goes in `dispatch_and_log_inner` **immediately after the READONLY gate** (§1.7) and
before the write path takes the AOF ordering lock — a rejected write must never touch that lock.

The thresholds live on `ReplicationHandle` (via `with_min_replicas`), because
`dispatch_and_log_inner` receives `&ReplicationHandle` and has no access to `Config`.

`min_replicas_to_write = 0` disables fencing entirely and is the default: **every existing
deployment must be unaffected by these plans until it opts in.**

Validation rule: `min_replicas_to_write > 0` together with `min_replicas_max_lag_secs == 0` is
rejected at startup — no replica could ever qualify, so it is a permanent, silent write outage
spelled as a config typo.

### 2.6 Cluster health honesty (chain D)

The prober is **observational only**. It marks peers reachable or not and makes `CLUSTER *`
replies tell the truth. It **must not** promote anything, rewrite `cluster.conf`, or change
routing — `cluster_redirect` keeps redirecting to the configured owner even when that owner is
known-dead, because the alternative is inventing a topology decision this project has no
mechanism to agree on. The spec is explicit that reconciliation is out of scope; honest reporting
is the whole deliverable.

Peer health lives in a new `Arc<PeerHealth>` alongside `ClusterConfig` on `ReplicationHandle`,
**not** inside `ClusterConfig` — that type's immutability-for-process-lifetime guarantee is
depended on elsewhere and must not be weakened.

State mapping, following Redis's vocabulary:
- Reachable → `connected` in `CLUSTER NODES`, `health: online` in `CLUSTER SHARDS`.
- No successful probe within `cluster_node_timeout_secs` → flag `master,fail?` (pfail) and
  `health: failed`.
- A node's own entry (`myself`) is always reachable — it is answering the command.
- `replication-offset` in `CLUSTER SHARDS` reports the real `master_repl_offset` for `myself`, and
  stays `0` for peers (their offset is genuinely unknown; no cluster bus carries it).

#### pfail vs fail: `cluster_slots_fail` is structurally always `0`

Redis distinguishes **pfail** (`fail?` — *this* node suspects a peer is down) from **fail** (a
majority of nodes agreed, over the cluster bus, that it is down). rocket-mem has no cluster bus and
no quorum mechanism, so **a suspicion here can never be promoted to an agreed failure.** That fixes
the field mapping, and an earlier draft of this contract got it wrong by asking for both at once:

| Field | Value | Why |
|---|---|---|
| `CLUSTER NODES` flag | `master,fail?` | Single-observer suspicion is exactly what pfail means. |
| `cluster_slots_pfail` | the failed node's slot span | This node's own observation. |
| `cluster_slots_fail` | **always `0`** | Nothing has been agreed, and structurally nothing ever can be. |
| `cluster_slots_ok` | `SLOT_COUNT` minus the pfail span | Subtract the suspected span once. |
| `cluster_state` | `fail` when any peer is pfail | This node's operational verdict: it cannot serve the whole keyspace. |

**Do not inflate `cluster_slots_fail` to match `cluster_state`.** `cluster_slots_fail > 0` asserts a
consensus that does not exist in this system, and this entire spec exists because the cluster
commands were reporting confident falsehoods. `cluster_state:fail` beside `cluster_slots_fail:0`
looks odd on first read, but every field is individually true and the doc comment explains it —
which is strictly better than one field lying to make the set look tidy.

**Wire-compatibility divergence to document, not hide:** in real Redis `cluster_state:fail` also
means the node *refuses to serve*. Here it is report-only — `cluster_redirect` keeps routing
normally. A cluster-aware client that gates on `cluster_state` may behave unexpectedly. Say so in
the doc comment, `docs/config-reference.md`, and the manual-testing section.

**The probe treats any RESP reply as alive, including `-NOAUTH`.** The prober deliberately never
authenticates, so requiring a literal `+PONG` would mark every node of an ACL-protected cluster
permanently failed. A node that answers at all is up; that is the whole question being asked.

**Chain D's plan 13 depends on chain A's plan 01** for the real `master_repl_offset` in
`CLUSTER SHARDS`. The chain table in §0 lists the chains in priority order, not as independent
tracks — A must land before 13's offset task.

---

## 3. Conventions every plan in this folder follows

- **TDD, strictly.** Write the failing test, run it, watch it fail for the stated reason,
  implement, watch it pass, commit. A plan step that says "implement X and test it" is malformed.
- **Max 3 tasks per plan.** Each task ends with an independently testable, committed deliverable.
- **Every plan ends with a `## Next plan` section** naming the next plan file by relative path, so
  execution can chain without a human picking the next step.
- **The three CI gates must be clean before every commit:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
  Clippy is strict (`-D warnings`) and lints test code too — a dead-code warning fails CI.
- **Comment style** (from the project's `CLAUDE.md`): short, easy, full sentences ending in a
  punctuation mark. No emojis.
- **Do not weaken an existing test to make a new feature pass.** If extending `INFO`'s slave line
  breaks `dispatcher.rs:5142`, update that test to assert the new correct format — in the same
  commit as the change that broke it.

## 4. Known flaky test

`ttls_set_before_the_kill_come_back_as_absolute_deadlines_not_restarted_countdowns` in
`crates/server/tests/kill_and_recover.rs` is timing-sensitive and fails intermittently under
parallel load. It is **pre-existing and unrelated** to any plan here. If it fails, re-run the
suite, or confirm with `cargo test --workspace -- --test-threads=1`. Do not "fix" it as part of
these plans, and do not treat it as a regression you caused.

---

## 5. Known hazard: the replica output channel is unbounded

Each replica's outbound queue is a `tokio::sync::mpsc::UnboundedSender<Bytes>`
(`replication.rs:37`). The leader pushes every replicated frame into it and never waits. If a
replica's socket stalls — a TCP zero-window from a paused or swapping follower, not a
disconnect — nothing drains that queue and it grows without limit until the **leader** runs out
of memory. A slow follower can therefore kill the node it replicates from.

Plan 06's periodic ack made this hazard easier to reach: a follower that is alive enough to hold
its socket open but too slow to drain it now stays attached, where before it was more likely to
be dropped.

**No plan in this folder fixes it, deliberately.** The fix is not "add a bound" — a bounded
channel would make the leader's write path block on its slowest follower, converting a replica
problem into a total write outage, which is strictly worse. The correct shape is Redis's
`client-output-buffer-limit` for replicas: track each replica's queued bytes, and **disconnect
the replica** that exceeds a hard limit (or stays over a soft limit for too long). That is a new
eviction policy with its own config surface and its own tests, and it is orthogonal to failover
safety.

Recorded here so no later plan mistakes the unbounded channel for a considered choice. Note the
interaction with §2.5: fencing counts a replica as "good" from its **ack recency**, and a replica
whose queue is exploding is still acking, so fencing will not detect or protect against this.

---

## Next plan

Start with [`01-leader-replication-offset.md`](01-leader-replication-offset.md).
