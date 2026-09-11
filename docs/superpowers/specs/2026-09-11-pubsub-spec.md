# Pub/Sub — Spec & Design

**Date:** 2026-09-11
**Status:** Approved
**Scope:** `crates/server` only — a new `pubsub.rs` module, `dispatcher.rs` (new `Session`
state, new command handling), `connection.rs` (the RESP read loop gains a second event source),
`replication.rs` (forward `PUBLISH` to followers). `crates/protocol` gains one new `Frame`
variant (`Push`). No `crates/engine` change.
**Goal:** `SUBSCRIBE`, `UNSUBSCRIBE`, `PSUBSCRIBE`, `PUNSUBSCRIBE`, `PUBLISH`, `PUBSUB`
(`CHANNELS`/`NUMSUB`/`NUMPAT`), matching real Redis's per-node message-delivery semantics,
without regressing the non-pubsub hot path `redis-benchmark` exercises.

This is Phase 6 of the post-v1 roadmap (prioritized 2026-09-10: 5=transactions [done], 6=pub/sub,
7=Lua `EVAL`, 8=streams, 9=live resharding).

## Problem

rocket-mem has no cross-connection communication mechanism today for ordinary clients. The
closest existing thing is `ReplicaRegistry` (`replication.rs`) — a registry of `mpsc` senders
broadcast writes to — but it exists only for replica connections, which hijack the whole TCP
connection into a one-way byte stream via `PSYNC` and never read another command again
(`connection.rs`'s `serve_replica`). `CLIENT LIST` itself replies `ERR ... no live connection
registry exists` (`dispatcher.rs:2450`) — there is no existing per-client registry to build on
for ordinary RESP connections.

Building pub/sub requires three things this project has never needed before:

1. **A registry mapping channel/pattern names to live subscriber connections**, distinct from
   `ReplicaRegistry` (keyed differently, and a subscriber connection must keep reading/replying
   to ordinary commands, unlike a replica connection).
2. **A way to push a message to a connection asynchronously**, interleaved with that same
   connection's normal read-dispatch-write loop — `handle_connection` (`connection.rs:262`)
   today only ever awaits `framed.next()`; it has no second event source.
3. **RESP3's `Push` wire type**, which real RESP3 client libraries specifically watch for to
   route pub/sub messages out-of-band from command replies — `protocol::Frame` has no such
   variant today (only `Simple`/`Error`/`Integer`/`Bulk`/`Null`/`Array`/`Map`).

## Decision: single-node delivery, a new `PubSubRegistry`, `tokio::select!` in the read loop

### Cluster scope: single-node only

`PUBLISH` on one node delivers only to subscribers connected to that same node. No cross-shard
forwarding, no gossip. rocket-mem's cluster mode has fixed hash-slot ownership per shard, and
building cross-node fanout would need a new inter-node message channel shards don't have today
(they only ever talk to each other through client-driven `MOVED` redirects). Real Redis's
cluster-wide pub/sub relies on its gossip protocol, which this project doesn't have an
equivalent of. Deferred — see "Out of scope."

### `PubSubRegistry`

A new type in `pubsub.rs`, structurally the same "register a sender, broadcast prunes dead ones"
shape `ReplicaRegistry` already uses, keyed by channel/pattern instead of by replica address:

```rust
pub(crate) struct Subscriber {
    client_id: u64,
    tx: tokio::sync::mpsc::UnboundedSender<protocol::Frame>,
}

pub(crate) struct PubSubRegistry {
    channels: std::sync::Mutex<std::collections::HashMap<Bytes, Vec<Subscriber>>>,
    patterns: std::sync::Mutex<std::collections::HashMap<Bytes, Vec<Subscriber>>>,
}
```

- `subscribe(channel, client_id, tx)` / `unsubscribe(channel, client_id)` and the `p`-prefixed
  pattern equivalents mutate the relevant map, returning the caller's new subscription count for
  the reply.
- `publish(channel, message) -> usize` locks `channels`, sends to every exact-match subscriber;
  locks `patterns`, sends to every pattern whose `engine::glob::glob_match(pattern, channel)` is
  `true`. A `tx.send` failure (receiver dropped) removes that subscriber from the map then and
  there — same "prune on send failure" pattern `ReplicaRegistry::broadcast` already uses. Returns
  the total number of sends that succeeded.
- `channels() -> Vec<Bytes>`, `num_sub(channels: &[Bytes]) -> Vec<(Bytes, usize)>`,
  `num_pat() -> usize` back `PUBSUB CHANNELS`/`NUMSUB`/`NUMPAT` as pure reads.

One `PubSubRegistry` lives on `ReplicationHandle` alongside the existing `ReplicaRegistry` (both
are per-process singletons reachable from `dispatcher.rs`), rather than inventing a second
top-level handle type threaded through the same call sites `ReplicationHandle` already reaches.

### Interception point: inside `dispatch_and_log_gated`, not `dispatch_and_log_inner`

`dispatch_and_log_inner` calls `intercept_for_transaction` (handling `MULTI`/`EXEC`/`DISCARD`
and per-command queuing) *before* calling `dispatch_and_log_gated`, which is where
`cluster_redirect`/`READONLY`/fencing run and where `handle_client` is already intercepted
(`dispatcher.rs:3821`, inside `dispatch_and_log_gated`'s body) — every ordinary command, `CLIENT`
included, is reachable both from top-level dispatch and from `EXEC`'s per-queued-frame replay
loop, since that loop calls `dispatch_and_log_gated` directly (`take_own_guard: false`), never
`dispatch_and_log_inner` again.

A new `intercept_for_pubsub`, handling `SUBSCRIBE`/`UNSUBSCRIBE`/`PSUBSCRIBE`/`PUNSUBSCRIBE`/
`PUBLISH`/`PUBSUB` plus the RESP2 restricted-mode check, is called from the same place
`handle_client` is — inside `dispatch_and_log_gated`, after the cluster/READONLY/fencing gates
(which all no-op for these commands: none of them appear in the write-command table or carry a
keyspace key, so `cluster_redirect` and the `READONLY` check pass through untouched, matching
real Redis, where `PUBLISH` and `SUBSCRIBE` both work against a read-only replica). This placement
means `PUBLISH` queued inside a `MULTI` batch is replayed correctly at `EXEC` time through the
exact same code path as top-level `PUBLISH` — no separate transaction-aware copy needed.

### Interaction with `MULTI`/`EXEC`

Real Redis queues `PUBLISH` inside a transaction like any other command (it runs at `EXEC` time,
its delivery timing deferred along with everything else in the batch) but rejects
`SUBSCRIBE`/`UNSUBSCRIBE`/`PSUBSCRIBE`/`PUNSUBSCRIBE` at **queue time**, since a connection's
subscription state is not a deferrable/replayable batch operation. This spec matches that:
`intercept_for_transaction`'s per-command queuing branch gets one new check, alongside its
existing arity/unknown-command check — a `SUBSCRIBE`-family command name immediately replies
`-ERR SUBSCRIBE is not allowed in transactions` (substituting the actual command name) and marks
the transaction `dirty`, exactly like a queue-time arity error does, rather than being appended to
the queue.

### `Session` additions

```rust
subscriptions: std::sync::Mutex<std::collections::HashSet<Bytes>>,
psubscriptions: std::sync::Mutex<std::collections::HashSet<Bytes>>,
subscription_count: std::sync::atomic::AtomicUsize, // fast path, see Performance below
push_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Frame>>>,
```

`push_rx` starts `None`; the first `SUBSCRIBE`/`PSUBSCRIBE` on a connection creates the
`mpsc::unbounded_channel`, registers the sender half in `PubSubRegistry`, and stores the receiver
half here for `connection.rs`'s read loop to pick up (see below). `subscriptions`/
`psubscriptions` track exactly what this connection is currently subscribed to, for
no-argument `UNSUBSCRIBE`/`PUNSUBSCRIBE` and for the RESP2 restricted-mode check.
`subscription_count` is the sum of both sets' sizes, checked with a relaxed load before either
mutex is touched — same fast-path shape as `in_transaction` (see the transactions spec).

### Connection loop: `tokio::select!` over two event sources

`handle_connection`'s loop (`connection.rs:262`) currently does:

```rust
let next = match pending.take() {
    Some(n) => n,
    None => framed.next().await,
};
```

Once `session.push_rx` is `Some(_)` (i.e. the connection has subscribed at least once), the loop
instead does:

```rust
let next = match pending.take() {
    Some(n) => n,
    None => {
        let mut rx_guard = session.push_rx.lock().unwrap_or_else(|e| e.into_inner());
        match rx_guard.as_mut() {
            Some(rx) => {
                tokio::select! {
                    frame = framed.next() => frame,
                    Some(push) = rx.recv() => {
                        drop(rx_guard);
                        if framed.send(push).await.is_err() { return; }
                        continue;
                    }
                }
            }
            None => framed.next().await,
        }
    }
};
```

A pushed message is sent immediately (`framed.send`, which flushes) rather than buffered with
`feed`, since it isn't part of a request/reply exchange the client is about to follow with
another read — there's no next write in the same batch to piggyback the flush onto. `pending`'s
existing pipelining-peek behavior is untouched; it only ever holds a value already pulled from
`framed.next()`, never from `rx`.

### `Frame::Push`

```rust
pub enum Frame {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Bytes),
    Null,
    Array(Vec<Frame>),
    Map(Vec<(Frame, Frame)>),
    Push(Vec<Frame>),
}
```

Encodes as RESP3's `>` type under `Protocol::Resp3`, and falls back to a plain `*` array under
`Protocol::Resp2` — the same per-protocol dispatch `RespCodec`'s encoder already does for `Map`
(which is `%` under RESP3, flattened to `*` under RESP2). `kind()` returns `"push"`; `log_len()`
returns element count, matching `Array`'s and `Map`'s existing convention. This is the only
`protocol` crate change in this spec.

## Command semantics

- **`SUBSCRIBE ch [ch...]`**: for each channel (processed in argument order), lazily create
  `push_rx`/register with `PubSubRegistry` if this is the connection's first subscription of
  either kind, add to `session.subscriptions`, reply with one `Frame::Push(["subscribe", channel,
  count])` per channel — never one batched reply, matching real Redis's per-channel reply
  sequence.
- **`UNSUBSCRIBE [ch...]`**: no arguments unsubscribes from every channel in
  `session.subscriptions` (patterns are untouched — `PUNSUBSCRIBE`'s job); explicit arguments
  remove just those. Each removal (including of a channel never subscribed to, which still
  replies once, matching real Redis) replies `Frame::Push(["unsubscribe", channel, count])`. If
  `count` reaches 0 and `push_rx` is now unused (no patterns left either), it stays allocated for
  the connection's lifetime rather than being torn down — a later re-`SUBSCRIBE` reuses it. This
  is a deliberate simplification: tearing down and re-creating the channel on every 0-crossing
  buys nothing (the registry entry is already gone) and adds a "does this need lazy-recreate too"
  edge case for no benefit.
- **`PSUBSCRIBE`/`PUNSUBSCRIBE`**: identical shape against `psubscriptions`/`patterns`,
  `psubscribe`/`punsubscribe` message names.
- **`PUBLISH channel message`**: handled by `intercept_for_pubsub`, called from inside
  `dispatch_and_log_gated` alongside `handle_client` (which already has `engine`, `aof`,
  `replication`, `session`, `client_id` in scope — see "Interception point" above). **Never
  AOF-appended** (skips the
  lock-shards/mutate/append sequence entirely — there's no engine mutation and nothing to make
  crash-safe). Calls `replication.pubsub.publish(channel, message)` for local delivery, then
  separately re-encodes the raw `PUBLISH` frame and broadcasts it via
  `replication.registry.broadcast` (the existing `ReplicaRegistry`) so followers receive it too.
  Replies `Frame::Integer(delivered_count)` (the local delivery count only — a follower's own
  local delivery count is invisible to the leader, matching real Redis, which also only reports
  the publishing node's own subscriber count). This intentionally never reaches bare `dispatch()`
  — see "Why not `dispatch()`" below.
- **`PUBSUB CHANNELS [pattern]`**: `registry.channels()`, optionally filtered through
  `engine::glob::glob_match` against `pattern`. **`PUBSUB NUMSUB [ch...]`**:
  `registry.num_sub(...)`, replied as a flat `channel1 count1 channel2 count2...` array (real
  Redis's own shape). **`PUBSUB NUMPAT`**: `registry.num_pat()`.
- **RESP2 restricted mode**: while `session.subscription_count.load(Relaxed) > 0` and
  `session.protocol() == Protocol::Resp2`, any command other than `SUBSCRIBE`/`UNSUBSCRIBE`/
  `PSUBSCRIBE`/`PUNSUBSCRIBE`/`PING`/`QUIT`/`RESET` is rejected with real Redis's own message:
  `-ERR only (P)SUBSCRIBE / (P)UNSUBSCRIBE / PING / QUIT / RESET are allowed in this context`.
  RESP3 connections skip this check entirely (checked via `session.protocol()`, already read
  every dispatch for the existing RESP2/RESP3 encoding split).
- **Disconnect**: no explicit cleanup command exists for this — `Session` (and its `push_rx`,
  `subscriptions`, `psubscriptions`) is simply dropped when `handle_connection` returns, but the
  registry entries it registered under `client_id` would leak (`Vec<Subscriber>` per channel
  never gets pruned by a clean disconnect, only by a failed `send`). `handle_connection` must
  call a new `registry.remove_all(client_id)` right before returning, for every exit path
  (`return` on decode error, clean disconnect, PSYNC handoff never applies here since a
  subscribing connection can't also `PSYNC`). This is the one piece of real cleanup this feature
  needs beyond "let it drop."

## AOF & replication

- **`PUBLISH` is never written to the AOF.** Not a keyspace mutation — nothing for
  `replay_with_stats` to replay.
- **`PUBLISH` is forwarded to followers** over the existing replication broadcast
  (`replication.registry.broadcast`, the `ReplicaRegistry` one) as a plain
  `Frame::Array([Bulk("PUBLISH"), Bulk(channel), Bulk(message)])` — no new wire format, riding
  the same byte-stream every replicated write already does.
- **Subscriptions themselves are never replicated or persisted.** `SUBSCRIBE`/`PSUBSCRIBE` are
  pure connection-local state, gone on disconnect — exactly like real Redis.

### Why not `dispatch()`

`replication.rs`'s follower-apply loop (`sync_once`, around line 1241) calls
`crate::dispatcher::dispatch(engine, buffered, &mut protocol, 0)` for every buffered frame —
today, always a write command being replayed against the engine. `dispatch()`'s signature is
`fn dispatch(engine: &Engine, frame: Frame, protocol: &mut Protocol, client_id: u64) -> Frame` —
no `replication` parameter, so it has no way to reach a `PubSubRegistry` even if one existed
inside it. Widening `dispatch()`'s signature to thread `replication` through it would touch every
call site, including `aof.rs`'s replay loop (which will never see a `PUBLISH` frame, since
`PUBLISH` is never AOF-logged) — unjustified blast radius for one command.

Instead, `sync_once`'s loop gets one new branch, checked before its existing call to `dispatch()`:
a buffered frame whose command name is `PUBLISH` is intercepted right there — extract
`channel`/`message` and call `replication.pubsub.publish(channel, message)` directly, then
`continue` to the next buffered frame without ever calling `dispatch()`. This mirrors the same
"intercept before the shared function" shape `TransactionGrouper` already uses in this exact loop
(buffering `MULTI`/`EXEC` markers before frames reach `dispatch()`), and the same shape
`connection.rs` uses to intercept `PSYNC` before `dispatch_and_log`. A follower's locally
subscribed clients receive the message exactly as if `PUBLISH` had been called on the follower
directly; the follower never touches its engine for this frame, matching the leader's own
skip-the-AOF-and-engine handling of `PUBLISH`.

## Performance

- `session.subscription_count.load(Ordering::Relaxed)` at the top of the RESP2-restricted-mode
  check: one atomic load, same cost class as the existing `in_transaction` check, `0` for every
  connection that never subscribes.
- `session.push_rx` starts `None`; the `tokio::select!` branch in `connection.rs` is only reached
  once it's `Some`, so a connection that never subscribes pays one `Mutex::lock` +
  `Option::is_none` check per loop iteration in place of the old bare `framed.next().await` — this
  is the one unavoidable added cost on the hot path, since the loop has to check whether to
  `select!` at all. Measured, not assumed: covered by the required before/after benchmark below.
- `PubSubRegistry`'s two `Mutex<HashMap>`s are only ever touched by the six new commands — never
  by `SET`/`GET`/etc.
- **Before/after verification (required before calling this done):** run `scripts/benchmark.sh`
  against plain `SET`/`GET` before and after this change, confirm no regression beyond noise
  (same `<=2%` gate methodology as the transactions series — see
  `docs/benchmarks/2026-09-11-post-transactions-final.md` for the established gate rows/means).
  Add a new `scripts/benchmark-pubsub.sh` measuring `PUBLISH`-to-delivery throughput as a
  first-time reference number (nothing to compare it to yet).

## Logging

Consistent with the existing redaction policy (engine/protocol log key names and byte lengths,
never value contents — channel/pattern names are identifiers, not values, so they're safe to log
in full):

- `SUBSCRIBE`/`UNSUBSCRIBE`/`PSUBSCRIBE`/`PUNSUBSCRIBE` →
  `tracing::debug!(client_id, channel = %name, count, "subscription changed")`.
- `PUBLISH` → `tracing::debug!(channel = %name, delivered_count, elapsed_us, "message
  published")` — never the message body.

## Testing strategy

- `PubSubRegistry` unit tests: subscribe/publish/prune-on-dead-receiver (mirroring
  `replication.rs`'s existing `ReplicaRegistry` test shapes), exact-channel vs. pattern matching,
  `channels()`/`num_sub()`/`num_pat()` correctness.
- Queue/state-machine unit tests: `UNSUBSCRIBE` with no active subscriptions, `UNSUBSCRIBE` of a
  channel never subscribed to (replies once, doesn't error), nested nonsensical sequences.
- RESP2 restricted-mode test: a RESP2 connection with one active subscription rejects `GET`; a
  RESP3 connection with the same subscription does not.
- Integration (two live connections over `serve`): connection A `SUBSCRIBE`s, connection B
  `PUBLISH`es, assert A receives a `Push` frame shaped `["message", channel, payload]`; same for
  `PSUBSCRIBE`/`pmessage`.
- Disconnect cleanup: a subscribed connection disconnecting stops appearing in
  `PUBSUB NUMSUB`/`CHANNELS`.
- Replication: a follower's locally-subscribed client receives a message `PUBLISH`ed on the
  leader (same mock-leader harness pattern `replication.rs`'s existing `sync_once` tests use).
- Benchmark verification: the before/after `scripts/benchmark.sh` gate, plus the new
  `benchmark-pubsub.sh` reference number, recorded under `docs/benchmarks/`.

## Out of scope (v1)

- **Cluster-wide fanout.** `PUBLISH` only reaches subscribers on the same node. Needs an
  inter-node message channel this project doesn't have; a separable follow-up.
- **Sharded pub/sub (`SPUBLISH`/`SSUBSCRIBE`).** Not applicable without cluster-wide fanout.
- **RMP protocol support.** RESP-only for v1, same precedent as the transactions spec.
- **Backpressure / `CLIENT KILL` on a slow subscriber.** `push_rx`'s channel is unbounded, so a
  subscriber that never reads can grow memory unboundedly under sustained publishing — the same
  accepted tradeoff `ReplicaRegistry` already makes for replica connections. Flagged as a known
  limitation, not solved here.
