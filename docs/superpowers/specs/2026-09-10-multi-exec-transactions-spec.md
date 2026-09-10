# MULTI / EXEC Transactions — Spec & Design

**Date:** 2026-09-10
**Status:** Approved
**Scope:** `crates/server` only — `dispatcher.rs` (new `Session` state, new command handling,
AOF/replication batching), `aof.rs` (replay grouping). No `engine.rs` or `protocol` change.
**Goal:** `MULTI`/`EXEC`/`DISCARD`, with the same queue-then-batch semantics real Redis clients
already expect, without regressing the non-transaction hot path `redis-benchmark` exercises.

This is Phase 5 of the post-v1 roadmap (the unscoped "Phase 5" follow-on list in
`docs/rocket-mem-production-plan.md`, prioritized 2026-09-10: 5=transactions, 6=pub/sub,
7=Lua `EVAL`, 8=streams, 9=live resharding). `WATCH`/`UNWATCH` and true read isolation are
explicitly deferred — see "Out of scope" below.

## Problem

Every command dispatched today runs immediately: `dispatch_and_log_inner` (`dispatcher.rs:3366`)
takes a single `Frame`, runs it through the auth/cluster/READONLY/fencing gates, executes it
against the engine, and logs it — one command, one round trip, no notion of "queue this, run it
later as a unit." Real Redis clients that use `MULTI`/`EXEC` (most connection-pooling client
libraries touch this path at least incidentally) get `ERR unknown command 'MULTI'` today.

Building it is not just "add three commands" — it interacts with three things rocket-mem's
concurrent, sharded-lock design makes non-trivial, none of which Redis's single-threaded design
has to solve at all:

1. **What does "atomic" mean** when 16 shard locks and a task-per-connection model mean another
   client's write could otherwise run between two of a transaction's queued commands.
2. **What does "one crash-safe unit" mean** for the AOF/replication stream, so a `kill -9`
   mid-`EXEC` never replays half a transaction.
3. **How to add this without slowing down every other command**, since `redis-benchmark`'s
   default `SET`/`GET` workload never touches `MULTI` at all and must not pay for it.

## Decision: writers-only isolation, reusing the existing AOF ordering guard

`dispatch_and_log_inner` already takes `aof.lock_shards(&shards)` (`dispatcher.rs:3487-3497`) —
a guard scoped to exactly the engine shards a write command's keys hash to
(`engine.shard_index`), held across "mutate the engine, then AOF-append" for every ordinary
write, so two concurrent writers' AOF order can never disagree with their mutation order.

`EXEC` reuses this same guard, widened to span the whole batch:

```rust
let shards: Vec<usize> = queued_frames
    .iter()
    .flat_map(|f| command_keys(f))
    .map(|k| engine.shard_index(k))
    .collect(); // dedup via a HashSet before sorting for lock_shards
let _order_guard = aof.lock_shards(&shards);
for frame in queued_frames {
    // run each command's existing execution + per-command log-rewrite path,
    // still inside this one guard scope
}
```

This blocks any other **write** touching an overlapping shard until the transaction finishes —
the same guarantee ordinary writes already give each other, just held longer. **Reads stay fully
concurrent**, exactly as they are today (`dispatch_and_log_inner`'s guard is `write_name`-gated
only) — a concurrent `GET` could observe the transaction's intermediate state partway through the
batch. Real Redis cannot expose this (single-threaded), but closing that gap means holding each
touched shard's actual data `RwLock` for the whole batch, which needs `engine.rs`'s
`with_mut`/`with_ref` reworked to run against an already-held guard instead of re-locking
(`parking_lot::RwLock` isn't reentrant). Explicitly deferred — see "Out of scope."

### Rejected: per-command atomicity only (no batch-wide guard)

Simplest to build — `EXEC` would just replay each queued command through today's unchanged
per-command path. Rejected because it gives up the one property `MULTI`/`EXEC` exists for: two
queued commands in the same transaction could have an unrelated client's write land between
them, which is indistinguishable from `MULTI`/`EXEC` not doing anything at all from a
correctness standpoint.

### Rejected: full read+write isolation now

Would need `engine.rs`'s locking API reworked to accept a pre-acquired shard guard so command
functions (`commands::string::get`, etc.) don't try to re-lock a shard `EXEC` already holds.
That's a real change to the engine's core locking primitive, touching every command function
indirectly, for a guarantee (`GET` never observes a partial transaction) real usage may not need
in a mostly-cache workload. Revisit if a real workload demonstrates torn reads matter.

## Session state & command handling

`Session` (`dispatcher.rs:24`) gains:

```rust
in_transaction: std::sync::atomic::AtomicBool,   // fast-path flag, see Performance below
tx: std::sync::Mutex<TransactionState>,
```
```rust
enum TransactionState {
    Idle,
    Queuing { commands: Vec<Frame>, dirty: bool },
}
```

- **`MULTI`**: `Idle → Queuing { commands: vec![], dirty: false }`, sets `in_transaction = true`,
  replies `+OK`. Already `Queuing` → replies `ERR MULTI calls can not be nested` and leaves the
  existing queue untouched.
- **While `Queuing`, any other command** is intercepted before `dispatch_and_log_inner`'s normal
  gates run: unknown command or wrong arity → reply `-ERR` immediately, set `dirty = true` (queue
  keeps growing so `DISCARD`/`EXEC` still see accurate state); otherwise append the raw `Frame`
  and reply `+QUEUED`. No gate (auth/cluster/READONLY/fencing) runs yet — see "Gate timing"
  below.
- **`EXEC`**: `dirty == true` → reply `EXECABORT Transaction discarded because of previous
  errors`, reset to `Idle`, run nothing. Otherwise run the batch (next section), reply with a
  RESP array of each queued command's own reply, reset to `Idle`.
- **`DISCARD`**: `Idle` → `ERR DISCARD without MULTI`; `Queuing` → reset to `Idle`, reply `+OK`.
- **`EXEC`/`DISCARD` with no `MULTI`** in effect both reply their respective `ERR ... without
  MULTI`, matching real Redis.

### Per-command runtime errors don't abort the batch

A queued command's *runtime* error (`WRONGTYPE`, a gate rejection, etc.) becomes that command's
own `Frame::Error` entry in `EXEC`'s reply array — it does not stop or roll back later commands
in the same batch. Only a *queue-time* arity/unknown-command error (caught before `EXEC` even
runs) triggers `EXECABORT`. This matches real Redis's own split between "the command couldn't
even be queued" and "the command ran and failed."

### Gate timing

Auth, cluster `MOVED`/`CROSSSLOT`, `READONLY`, and min-replicas fencing all re-run **per queued
command, at `EXEC` time** — not at queue time, since the environment (replica status, fencing
state, cluster ownership) can change in the gap between `MULTI` and `EXEC`. A gate rejection
becomes that command's error entry per the rule above, reusing the exact gate functions
`dispatch_and_log_inner` already calls per-command today (`auth_gate`, `cluster_redirect`, the
inline `READONLY` check, the inline fencing check) — no new gate logic, just called once per
queued frame inside the batch loop instead of once per top-level dispatch.

## AOF & replication as one unit

Under the same `_order_guard` scope: append a `MULTI` frame, then each queued write's existing
per-command log-rewrite output (`dispatch_and_log_inner`'s `to_log` logic, called once per queued
command, unchanged), then an `EXEC` frame — one contiguous append sequence — then broadcast that
same sequence to replicas before releasing the guard. This is the existing single-command
append-then-broadcast-under-guard pattern, just looped across the batch before the guard drops.

**Replay** (`aof.rs`) and the **follower apply loop** both need a small addition: buffer frames
between a replayed `MULTI` and `EXEC` and apply them as one unit. A truncated/corrupt tail that
stops mid-transaction is discarded — same spirit as today's corrupt-last-line handling, just
scoped to "last transaction" instead of "last line" when a `MULTI` was seen without a matching
`EXEC`.

## Performance: the non-transaction hot path pays almost nothing

`redis-benchmark`'s default workload (plain `SET`/`GET`, no `MULTI`) must not regress — this
project already has an open, unresolved perf gap against real Redis (shared atomic clock, no
`TCP_NODELAY`, dispatcher overhead — specced and planned separately as of 2026-09-08), so nothing
here should widen it further.

- Every dispatched command checks `session.in_transaction.load(Ordering::Relaxed)` **first**, at
  the very top of `dispatch_and_log_inner` — a single atomic load, the same cost class as the
  existing `replication.is_replica` check already on this hot path
  (`dispatcher.rs`, READONLY gate). Only when it's `true` does the code touch `session.tx`'s
  `Mutex` at all. An ordinary connection that never sends `MULTI` never locks that mutex, ever.
- The `Vec<Frame>` queue is allocated only inside `MULTI`'s own handler — zero allocation on the
  non-transaction path.
- `aof.lock_shards` for a whole `EXEC` batch is the same primitive already used per single write;
  no new lock type, no polling.
- **Before/after verification (required before calling this done):** run `scripts/benchmark.sh`
  (this project's established matched-durability benchmark methodology — a manual run against
  the system Redis flatters it, since rocket-mem can't disable its AOF) against plain `SET`/`GET`
  before and after this change lands, and confirm no regression beyond noise. Add one new benchmark scenario
  exercising `MULTI`/`EXEC` batches (e.g. 5 and 20 commands per transaction) and record its
  throughput/latency in a short note under `docs/` — following this repo's own established
  convention (Week 12 / Phase 3) of documenting benchmark results honestly, including anywhere
  it's slower and why, rather than omitting an unflattering number.
- If profiling later shows `lock_shards`-held-across-a-batch causing real contention under a
  transaction-heavy workload, that is a documented tradeoff of the writers-only isolation choice
  above — not something to silently work around by weakening the guarantee.

## Logging

Consistent with this project's existing redaction policy (`CLAUDE.md`: engine/protocol log key
names and byte lengths only, never value contents) and the existing "command dispatched"
`debug!` event's shape (`dispatcher.rs:3355-3359`, which records `elapsed_us` and `reply_kind`):

- `MULTI` → `tracing::debug!(client_id, "transaction started")`.
- Each queued command → `tracing::trace!` (not `debug!`, to avoid per-command noise on a busy
  connection) with the command name only — no arguments, no values.
- A queue-time arity/unknown-command rejection → `tracing::debug!(command = name, "transaction
  marked dirty")`.
- `EXEC` → one `tracing::debug!` with `queued_count`, `shard_count`, `elapsed_us`, and whether it
  committed or hit `EXECABORT` — mirrors the existing per-command dispatch log, scoped to the
  whole batch.
- `DISCARD` → `tracing::debug!(queued_count, "transaction discarded")`.

All of these are cheap enough at `debug!` (the project's default level) to leave on by default,
giving an operator transaction boundaries and timing without needing `trace!`'s value-logging
tier.

## Testing strategy

- Queue/state-machine unit tests: nested `MULTI`, `EXEC`/`DISCARD` without `MULTI`, arity error
  during queuing → `EXECABORT`, a runtime `WRONGTYPE` inside a batch not aborting later commands.
- Isolation test: spawn two connections, start a transaction touching shard S on connection A,
  hold it mid-batch (a test hook or a deliberately multi-command batch), assert a concurrent
  write from connection B to a key in shard S blocks until A's `EXEC` completes, while a
  concurrent *read* from B is not blocked.
- AOF/replication: kill `-9` mid-`EXEC` (same style as the Week 8 AOF kill-and-recover test),
  confirm the partial transaction never replays; a follower receiving a full `MULTI…EXEC`
  sequence applies all of it or (on a truncated stream) none of it.
- Benchmark verification: the before/after `scripts/benchmark.sh` run described above, checked
  into CI or at minimum recorded in the PR/commit.

## Out of scope

- **`WATCH`/`UNWATCH`** (optimistic locking). Needs a per-key version/change-tracking primitive
  that doesn't exist in `shard.rs`'s `Entry` today — a separable follow-up increment once that
  primitive is designed on its own, per the Phase 5-9 roadmap ordering noted above.
- **True read isolation** (holding each touched shard's real data lock for the whole batch).
  Deferred pending `engine.rs` locking-API changes — see "Rejected: full read+write isolation
  now" above.
- **RMP support.** This spec is RESP-only for v1, matching how every other command family landed
  in RESP first.
- **Lua scripting (`EVAL`).** A separate, later phase (Phase 7 in the roadmap ordering above) —
  no shared code with this spec beyond both being forms of server-side batching.
