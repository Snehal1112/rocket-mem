# Architecture

This document pulls together `rocket-mem`'s three-layer design, its concurrency model, and
the `Session`/auth boundary added in Sprint 8. It assumes you've skimmed the README's
"Status" section for the sprint-by-sprint feature history; this document's job is the
cross-cutting shape, not a rehash of each sprint's own decisions — those live in
`docs/superpowers/specs/`, linked throughout and tabulated at the end.

## 1. The three layers

Three layers, fixed in Sprint 1 and respected throughout every sprint since:

```
┌─────────────────────────────────────────┐
│  Protocol Layer (RESP2/RESP3, RMP)       │
├─────────────────────────────────────────┤
│  Command Dispatcher (maps commands →     │
│  engine calls, arg validation)           │
├─────────────────────────────────────────┤
│  Storage Engine (data structures,        │
│  persistence, expiry, protocol-agnostic) │
└─────────────────────────────────────────┘
```

A client speaks either wire protocol to the Protocol Layer, which turns bytes into a
protocol-agnostic command shape. The Command Dispatcher matches that shape against a command
name, validates arguments, and calls into the Storage Engine, which holds the actual data
and knows nothing about either protocol. Below is what each layer looks like one level
deeper than the README.

### Protocol layer

Two wire protocols, both producing and consuming the same `protocol::Frame` value model
(`crates/protocol/src/frame.rs`): `Simple`, `Error`, `Integer`, `Bulk`, `Null`, `Array`, and
RESP3's `Map`.

- **RESP2/RESP3** — `protocol::codec::RespCodec` (`crates/protocol/src/codec.rs`), a Tokio
  `Decoder`/`Encoder` that both parses RESP's line-oriented wire format and encodes replies
  back into it (RESP2 or RESP3 depending on the connection's negotiated `Protocol`), handling
  split-read reassembly across TCP reads.
- **RMP** — `protocol::rmp` (`crates/protocol/src/rmp.rs`), a hand-rolled binary framing
  (magic bytes, version, a 16-byte envelope carrying a `request_id`, length-prefixed values)
  that reuses `Frame` as its own value model rather than inventing a second one. Its codec
  enforces `MAX_RMP_FRAME_LEN` (64 MiB) and a max nesting depth (32) against any
  length/count field decoded from the wire, before allocating anything sized by it.

Both codecs turn their wire bytes into the *same* `Frame::Array(Vec<Frame::Bulk>)` command
shape the dispatcher expects, and turn the dispatcher's `Frame` reply back into their own
wire format. Nothing about the dispatcher or the engine below it is RESP-specific or
RMP-specific — a third protocol could be added the same way, as a new `protocol::` codec plus
a new connection-handling module, without touching the dispatcher.

### Command dispatcher

Lives in `crates/server/src/dispatcher.rs`, and is really two functions with a specific
division of labor:

- **`dispatch`** (`pub fn dispatch(engine: &Engine, frame: Frame, ...) -> Frame`) is pure and
  protocol-agnostic: it matches a command name, validates arguments, and calls into
  `engine::commands::*`. It is deliberately the *only* thing AOF replay
  (`crates/server/src/aof.rs`) and the follower apply loop (`crates/server/src/replication.rs`)
  call — see "Durability & replication recap" below for why that matters.
- **`dispatch_and_log`** is the full pipeline every live client command goes through. It's
  split into a thin instrumented wrapper (metrics counters/histograms, slow-log recording)
  around `dispatch_and_log_inner`, which runs, in order:
  1. `auth_gate` — NOAUTH/NOPERM enforcement (see §3 below).
  2. `cluster_redirect` — `-MOVED`/`-CROSSSLOT` hash-slot routing.
  3. The `-READONLY` gate — rejects a client-originated write on a replica.
  4. `handle_auth`/`handle_acl` — `AUTH` and the `ACL` command family.
  5. `is_save_command`/`handle_replicaof`/`handle_cluster`/`handle_info`/`handle_hello`/`handle_slowlog`
     — interceptions for `SAVE`, `REPLICAOF`, `CLUSTER`, `INFO`, `HELLO`, and `SLOWLOG`.
  6. Falls through to `dispatch` itself, then — for a successful write command — appends the
     (possibly rewritten, e.g. `SPOP`→`SREM`, `EXPIRE`-family→absolute `PEXPIREAT`) command to
     the AOF and fans it out to registered replicas.

This sprint's own finding, worth stating explicitly: **`dispatch_and_log` is the single place
command behavior lives.** That property is what let RMP (Sprint 7) reach nearly the entire
command set — `INFO`, `CLUSTER`, `SAVE`, `REPLICAOF`, `SLOWLOG` included — by calling this
one unmodified function, and what let ACL enforcement (Sprint 8) be added as exactly one new
interception (`auth_gate`) at the top of the same pipeline, rather than by touching
`connection.rs`, `rmp_connection.rs`, or `dispatch` itself. `PSYNC` is the one command that sits
*above* this pipeline rather than inside it — it's intercepted in `connection.rs` before
`dispatch_and_log` is ever called, because serving a replica means handing over the raw socket
for a length-prefixed snapshot blob, which has no `Frame` reply to give back. RMP's connection
handler has no equivalent interception, so `PSYNC` sent over RMP falls through to `dispatch`'s
ordinary "unknown command" error.

### Storage engine

`crates/engine` — `Value`, `Store`, and `Engine`, unchanged in shape by every protocol- or
auth-layer sprint since Sprint 1. Read `value.rs` → `shard.rs` → `store.rs` → `engine.rs` →
`commands/` in that order:

- **`value.rs`** — the `Value` enum (`String`/`List`/`Hash`/`Set`/`SortedSet`).
- **`shard.rs`** — `Shard`, an `RwLock<HashMap<Bytes, Entry>>` (`Entry` wraps a `Value` with
  an optional TTL and an LRU recency tick) plus a byte-usage counter.
- **`store.rs`** — `Store`, a fixed array of shards a key routes into by hash (§2 below).
- **`engine.rs`** — `Engine`, the public facade over `Store` that every command function and
  the dispatcher call through.
- **`commands/{string,hash,list,set,sorted_set,keys}.rs`** — one free function per command,
  `fn(&Engine, ...) -> Result<T, common::EngineError>`.

That "protocol-agnostic engine" claim isn't aspirational; it's traceable through the sprints
that most plausibly would have broken it: Sprint 2 added RESP on top with zero engine changes;
Sprint 5's replication reuses the exact same `dispatch` function AOF replay already used,
rather than a parallel apply path; Sprint 7 added RMP as a second protocol layer without
touching `crates/engine` at all; Sprint 8 added ACL/TLS as a dispatcher- and connection-layer
concern, again with no engine change. `crates/engine`'s own commit history since Sprint 1 is
additive-only (new commands, TTL/eviction state in `Entry`, snapshotting) — never a change
forced by what protocol or auth model sits above it.

## 2. Concurrency model

One Tokio task per client connection, keyspace split into 16 shards each behind its own
lock — any task can read/write any key by acquiring that key's shard lock.

- **16 shards.** `Engine::new()` hardcodes `Store::new(16)`; a key routes to shard
  `DefaultHasher(key) % 16`. `docs/design/sharding-decision.md` has the rationale in full: 16
  was a reasonable starting point with no load testing behind it at the time, chosen because
  it's cheap to change later (nothing outside `Store::new()` knows the count) and
  `DefaultHasher` was picked because it's already in `std`, fast, and shard assignment needs
  only a reasonably even spread, not cryptographic properties. Sprint 6's flamegraph pass
  revisited this with real contention data: at `-c 50` concurrent clients, the shard lock's
  contended slow path showed up at only 0.01% self CPU time under `Shard::get`/`Shard::set`
  combined — contention is real but small at this concurrency, so 16 shards stayed unchanged.
  The escape hatch the production plan's Architecture Decision Record documented (swapping
  each shard's internals for a lock-free structure) remains open and unexercised.
- **One Tokio task per RESP connection.** `connection::serve`'s accept loop spawns
  `handle_connection` once per accepted socket; that task owns the connection's `Framed`
  stream and its `Session` for the connection's full lifetime, processing frames strictly in
  arrival order (RESP has no request-identifying field, so replies are one-in-one-out).
- **One task per in-flight RMP request, sharing one `Session` per connection.**
  `rmp_connection::serve`'s accept loop spawns one task per *connection* that owns the read
  loop, a `Semaphore`-bounded (256) writer channel, and an `Arc<Session>`; that read loop then
  spawns a *second*, independent task per decoded request, cloning the `Arc<Session>` and a
  `Sender` handle into it. Because each request runs on its own spawned task, commands sent
  back-to-back on one RMP connection may execute out of order relative to each other, not just
  reply out of order. Once 256 requests are mid-dispatch, the read loop's next
  `semaphore.acquire_owned().await` blocks, which stops it pulling more requests off the
  socket — ordinary TCP backpressure, not an unbounded task/queue growth risk.

## 3. The `Session` / auth boundary

Through Sprint 7, `dispatch_and_log` took a bare `protocol: &mut Protocol` parameter — enough
state for RESP2/RESP3 negotiation, which only RESP needed. Sprint 8 replaced it with
`session: &Session` (`crates/server/src/dispatcher.rs`):

```rust
pub struct Session {
    protocol: std::sync::Mutex<Protocol>,
    authenticated_user: std::sync::Mutex<Option<std::sync::Arc<crate::acl::AclUser>>>,
}
```

The forcing problem was RMP, not RESP: RESP's connection loop owns one task for the
connection's whole life, so a bare `&mut Protocol` local variable would have worked fine for
it alone. RMP's per-request spawn model (§2 above) means each request runs on its *own* task —
a bare `&mut Protocol` held by the read loop could never be handed, exclusively, to several
concurrently-spawned request tasks at once. `Session` is shared instead: its two fields are
`Mutex`-wrapped (not `Cell`-wrapped — `Cell<T>` is never `Sync`, which would make
`Arc<Session>: !Send` and break `tokio::spawn`), so `&Session` — a shared reference — can be
cloned as `Arc<Session>` into every spawned RMP request task for one connection. That's what
lets `AUTH` sent as one RMP request stay visible to a later, independently-spawned request on
the *same* connection: RESP's connection loop owns one `Session` and passes `&session` each
iteration; RMP's connection handler owns one `Arc<Session>` per accepted connection and clones
it into every spawned per-request task.

`auth_gate` is the first check `dispatch_and_log_inner` runs — ahead of `cluster_redirect` and
the `-READONLY` gate, matching real Redis's own auth-before-everything ordering: an
unauthenticated or unauthorized client must not learn cluster topology or reach any other
gate by way of its rejection. Concretely, the ordering inside `dispatch_and_log_inner` is:

1. `auth_gate` (NOAUTH / NOPERM)
2. `cluster_redirect` (`-MOVED` / `-CROSSSLOT`)
3. the `-READONLY` replica gate
4. `handle_auth` / `handle_acl`, then the `SAVE`/`REPLICAOF`/`CLUSTER`/`INFO`/`HELLO`/`SLOWLOG`
   interceptions, then `dispatch` itself

`auth_gate` exempts only `AUTH` and `HELLO` (matching real Redis's `CMD_NO_AUTH` set) — `HELLO`
is exempted because RESP3 clients commonly authenticate inline as
`HELLO <ver> AUTH <user> <pass>` rather than a separate `AUTH` call, so blocking `HELLO` at the
gate would make that form unreachable. `ACL` itself is *not* exempted: an unauthenticated
`ACL SETUSER ... allcommands allkeys` followed by `AUTH` would otherwise be a full
authentication bypass. When ACL users exist at all, `auth_gate` re-resolves the *live* current
version of the connection's authenticated user by username on every call, rather than trusting
the snapshot cached at `AUTH` time — so an admin's `ACL SETUSER`/`DELUSER` against an
already-authenticated connection's username takes effect on its very next command, not only
after a reconnect. When no ACL user has ever been configured, `auth_gate` returns `None`
immediately (the fast path), preserving Sprint 8's zero-config-zero-behavior-change guarantee
for every deployment and test that predates it.

`PSYNC` sits outside this entirely, in `connection.rs`, ahead of the `dispatch_and_log` call —
its own auth check duplicates `auth_gate`'s ACL-configured condition by hand, because an
unauthenticated client sending `PSYNC` would otherwise receive a full keyspace snapshot plus a
live write stream, bypassing the gate that guards every other command.

TLS is additive to this same model, not a replacement for it: `connection::serve_tls` and
`rmp_connection::serve_tls` are second accept loops (bound only when `tls_resp_addr`/
`tls_rmp_addr` plus a cert/key pair are configured) that wrap each accepted socket in a
`tokio_rustls` handshake before handing it to the *same* `handle_connection` the plaintext
listener uses — the `Session`/auth pipeline above runs identically either way.

## 4. Durability & replication recap

AOF persistence (Sprint 4) and snapshotting/replication (Sprint 5) are covered in full in
their own specs — this section only states the cross-cutting shape those two features share
with the dispatcher split above. Both depend on the same fact §1 makes about `dispatch`: it is
pure, protocol-agnostic, and untouched by `dispatch_and_log`'s auth/cluster/read-only gates.
AOF replay (`aof::replay`) and the follower apply loop (`replication`'s sync-apply path) both
call `dispatch` directly rather than `dispatch_and_log` — deliberately, and not an oversight.
A replica applying its leader's already-authorized, already-cluster-routed write stream must
not be re-subjected to the auth gate (there is no client connection or `Session` to check),
nor to cluster redirection (the write is arriving because this node already owns it, from the
leader's perspective), nor to the read-only gate (that gate exists specifically to stop
*client-originated* writes on a replica, not the replication stream itself, which is the one
legitimate way a replica's data changes at all). Routing a leader-applied write back through
`dispatch_and_log` would also double-log it to the replica's own AOF and re-broadcast it to
that replica's own (nonexistent, Sprint-5-scoped-out) sub-replicas. See
`docs/superpowers/specs/2026-08-30-sprint-4-spec.md` for the AOF wire format, write-command
classification, and replay/corrupt-tail handling, and
`docs/superpowers/specs/2026-08-30-sprint-5-spec.md` for the snapshot format, the hybrid
snapshot+tail recovery offset scheme, and the full-resync-only replication transport.

## 5. Where to go deeper

| Spec | What it decided |
|---|---|
| [`2026-08-28-sprint-1-spec.md`](superpowers/specs/2026-08-28-sprint-1-spec.md) | Workspace/crate layout, the `Value` type, and the sharding scheme for the protocol-agnostic engine core. |
| [`2026-08-29-sprint-2-spec.md`](superpowers/specs/2026-08-29-sprint-2-spec.md) | The `Frame` type, RESP2 wire format, the dispatcher shape, and rejecting RESP3/`HELLO` for that sprint. |
| [`2026-08-29-resp3-design.md`](superpowers/specs/2026-08-29-resp3-design.md) | Reverses Sprint 2's RESP3 rejection: `HELLO`-negotiated RESP2/RESP3 support, once a real client library was found to hard-fail without it. |
| [`2026-08-29-sprint-3-spec.md`](superpowers/specs/2026-08-29-sprint-3-spec.md) | The `EXPIRE`-family stub-for-now call, the glob-matching feature set, the `SCAN` cursor design, and the sorted-set data structure. |
| [`2026-08-30-sprint-4-spec.md`](superpowers/specs/2026-08-30-sprint-4-spec.md) | The `Entry`/TTL data model, the AOF wire format and write-command classification, replay, and the LRU-approximation eviction design. |
| [`2026-08-30-sprint-5-spec.md`](superpowers/specs/2026-08-30-sprint-5-spec.md) | The snapshot on-disk format, the hybrid snapshot+AOF-tail recovery offset scheme, and the full-resync-only replication transport. |
| [`2026-08-30-tech-debt-cleanup-spec.md`](superpowers/specs/2026-08-30-tech-debt-cleanup-spec.md) | Three deliberately-deferred debt items closed out, including extending `KEYS` glob support beyond Sprint 3's original scope-down. |
| [`2026-08-30-with-mut-delta-extension-spec.md`](superpowers/specs/2026-08-30-with-mut-delta-extension-spec.md) | Fixes O(n²) `LPUSH` growth by extending `with_mut`'s re-accounting to a delta-based update instead of a full rescan per mutation. |
| [`2026-08-30-sprint-6-spec.md`](superpowers/specs/2026-08-30-sprint-6-spec.md) | The hash-slot algorithm and static cluster config format, `-MOVED`-before-`-READONLY` precedence, Prometheus metric names, `INFO` fields, and the slow log. |
| [`2026-08-31-sprint-7-spec.md`](superpowers/specs/2026-08-31-sprint-7-spec.md) | RMP's wire format (envelope + value encoding), its reuse of `dispatch_and_log` unmodified, and its per-request-task concurrency model. |
| [`2026-08-31-sprint-8-spec.md`](superpowers/specs/2026-08-31-sprint-8-spec.md) | The ACL data model and its `dispatch_and_log_inner` interception point, the `Session` type replacing the bare `Protocol` parameter, TLS listener wiring, and config-file layering. |

For the architecture-level case for sharded locks and task-per-connection over the
alternatives (single-thread, thread-per-core, lock-free, proxy-based), see the Architecture
Decision Record in [`docs/rocket-mem-production-plan.md`](rocket-mem-production-plan.md#architecture-decision-record).
For the shard-count/hash-choice rationale specifically, see
[`docs/design/sharding-decision.md`](design/sharding-decision.md).
