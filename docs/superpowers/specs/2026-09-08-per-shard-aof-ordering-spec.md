# Per-Shard AOF Ordering Locks: Spec & Design

**Date:** 2026-09-08
**Status:** Implemented — 2026-09-09. The per-shard locking mechanism (`Vec<Mutex<()>>`,
`lock_shards`, `shard_index`, and every call site) landed before this plan was written; Tasks 1-4
added the tests proving each of its three guarantees, and Task 5 measures the result below.
**Scope:** `crates/server/src/aof.rs`, `crates/server/src/dispatcher.rs`,
`crates/server/src/connection.rs`, `crates/server/src/replication.rs`,
`crates/engine/src/store.rs`, `crates/engine/src/engine.rs`.
No wire-protocol change, no command-semantics change, no weakening of any durability or
replication guarantee.

**Goal:** stop every write command in the process serialising on one mutex, without losing any of
the three guarantees that mutex currently provides.

Supersedes item 3 of [`2026-09-07-throughput-parity-design.md`](2026-09-07-throughput-parity-design.md),
whose proposed fix — moving the replica broadcast outside the guard — was implemented and measured
to change nothing. See "Why the earlier fix failed" below.

## Problem

`dispatcher::dispatch_and_log_inner` acquires `aof.lock_for_ordering()` *before* calling
`dispatch`, and holds it across the engine mutation, the AOF encode, and the AOF append. It is a
single process-wide `Mutex<()>`. Every write command in the server therefore executes one at a
time, which is why the engine's 16 independently-locked shards buy nothing on write workloads.

### Evidence

A CPU profile of pipelined 3B `SET` (`perf record -F 997 -g`, steady state, 20K samples) puts lock
contention at the top of the profile:

| Symbol | Self time |
|---|---:|
| `native_queued_spin_lock_slowpath` (kernel) | 4.65% |
| `std::sys::sync::mutex::futex::Mutex::lock_contended` | 4.58% |

Those two account for ~9.2% of all CPU cycles, and that figure *understates* the cost: it counts
only cycles spent spinning, not time parked. The same profile taken against pipelined `GET`, which
never acquires the lock, contains neither symbol anywhere in its top 30.

Removing the lock entirely (a diagnostic — it breaks the ordering guarantee and is not a candidate
fix) gives the size of the prize, measured on `SET -d 3 -P 16 -c 50`, median of four runs:

| Keyspace | with the lock | lock removed | ratio |
|---|---:|---:|---:|
| one key | 214,983 | 562,335 | 2.62x |
| 1,000,000 keys | 161,171 | 499,719 | 3.10x |

Pipelined `GET` did not move in the same experiment, which is the control: reads never take the
lock.

Against `redis-server` 8.10.1 under the multi-key harness, the two pipelined `SET` rows are the
only ones materially behind — 2.18x and 2.78x, against 0.90x–1.15x everywhere else.

### Why the earlier fix failed

The 09-07 spec identified this lock but proposed narrowing it by moving
`replication.registry.broadcast(...)` outside the guard. That was implemented; contention did not
move (`lock_contended` 4.58% → 4.91%, kernel spinlock 4.65% → 4.65%). The reason is that the
benchmark runs with no replicas attached, so `broadcast` was already walking an empty registry —
a no-op moved out of a lock is still a no-op. The narrowing is retained because it genuinely helps
a leader that *does* have replicas, but it was never going to address this.

The mistake worth not repeating: the earlier spec reasoned about where the guard *ended* and never
checked where it *began*.

## What the lock actually guarantees

Three distinct jobs, all of which a replacement must preserve. The third is easy to miss.

1. **AOF append order equals mutation-commit order** (`dispatch_and_log_inner`). Without it,
   replaying two writes to the same key can reproduce a different final value than the one that
   was committed. Note this matters *per key*: two commands touching disjoint keys may be appended
   in either order and replay identically.

2. **Snapshot cuts are consistent.** `handle_save` (`dispatcher.rs`), `handle_bgrewriteaof`, and
   `serve_replica` (`connection.rs`) hold the guard across "read the AOF offset, then walk the
   keyspace" so the resulting `(snapshot, offset)` pair is a point-in-time cut. Lose this and
   recovery re-applies commands the snapshot already contains — harmless for `SET`, wrong for
   `INCR`, `LPUSH`, `SADD` and every other non-idempotent command.

3. **Multi-key commands are atomic with respect to a snapshot walk.** The follower apply loop
   (`replication.rs`) takes the guard around its `dispatch` call specifically so that
   `Store::snapshot_entries`' shard-by-shard walk cannot observe an `MSET`/`RENAME`/`SINTERSTORE`
   half-applied across shards. `serve_replica` takes it so no write can slip between its snapshot
   and its registration.

## Decision: one ordering guard per shard

`AofWriter`'s `order: Mutex<()>` becomes `order: Vec<Mutex<()>>`, one entry per engine shard,
sized from the engine's shard count rather than a second hardcoded 16.

**A write command locks exactly the shards its keys live in.** Single-key writes — the common case
and where the contention is — take one guard. Concurrent writes to keys in different shards no
longer block each other, which is the entire point.

**Acquisition is always in ascending shard index**, after deduplication. This is the only rule
preventing deadlock between two multi-key commands whose key sets overlap in different orders, and
it must hold at every acquisition site without exception.

### Enumerating a command's keys

`dispatcher::command_keys(&Frame) -> Vec<&Bytes>` already does this, built for cluster `CROSSSLOT`
enforcement. It becomes a second consumer rather than new logic. Mapping a key to a shard index
needs a new engine method: `Store::shard_for` currently computes the index and immediately returns
`&Shard`, so the index computation is extracted into `Store::shard_index(key) -> usize` with
`shard_for` calling it, and `Engine::shard_index` exposed as a thin facade in the usual style.

**Fallback:** if `command_keys` returns empty for a frame that `extract_write_command_name`
accepted as a write, the command locks *all* shards. This is the safe direction — it degrades to
today's behaviour rather than skipping ordering — and it must be a fallback rather than an
assumption that the two functions always agree, because they derive their answers independently
(`key_spec` vs the write-command table) and nothing structurally forces them to stay in step.

### The snapshot paths take every guard

`handle_save`, `handle_bgrewriteaof`, `serve_replica`, and the follower apply loop acquire **all**
guards, ascending. These are rare operations — a snapshot walk already costs orders of magnitude
more than the acquisitions — so there is no reason to be clever here, and being clever is how job 2
or job 3 gets silently broken.

The follower apply loop is the subtle one: it must take all guards, not just the applied command's
shards, because its job is mutual exclusion against a concurrent `SAVE` on the same node, and a
`SAVE` holding all 16 must be excluded by anything that mutates.

### What does not change

- The locking primitive stays `std::sync::Mutex`, and poison is still recovered from rather than
  propagated, for the reason the current code documents: the guard is held across arbitrary
  command dispatch, so a panicking handler must not become a permanent server-wide write outage.
- `#[must_use]` stays on the accessor, for the same reason it is there now.
- The engine's own per-shard `RwLock`s are untouched. These ordering guards sit above them and
  are a separate concern.

## Expected result

Recovering most of the 3.10x measured on many-key writes would move pipelined 3B `SET` from 2.78x
to roughly 1.0–1.3x of `redis-server`, and pipelined 1KB `SET` from 2.18x similarly. It will
**not** improve a single-key benchmark: one key is one shard, so sixteen guards and one guard are
the same guard. Anyone re-measuring must use `scripts/benchmark.sh`, which passes `-r`.

## Measured result

Measured 2026-09-09 with `./scripts/benchmark.sh` (the only script this spec's methodology
sanctions — see "Expected result" above), median of four runs, on the same host used for the
original evidence. The host was not idle: a 3-node rocket-mem cluster was running throughout, and
`uptime` load average was 1.36, 2.44, 2.00 immediately before the first run and 2.30, 2.53, 2.08
immediately after the fourth. Because of that, the ratio against `redis-server` (both sides
measured back-to-back on the same loaded host) is the primary result below; the absolute
requests-per-second figures are secondary and would be higher on an idle machine.

| Row | prior ratio (redis/rocket) | measured ratio (redis/rocket) | redis-server (median rps) | rocket-mem (median rps) |
|---|---:|---:|---:|---:|
| pipelined 3B `SET` | 2.78x | **1.01x** | 709,256 | 702,455 |
| pipelined 1KB `SET` | 2.18x | **0.75x** | 350,168 | 465,116 |
| pipelined 3B `GET` (control) | ~0.90–1.15x | 1.23x | 1,324,561 | 1,078,386 |
| pipelined 1KB `GET` (control) | ~0.90–1.15x | 1.14x | 754,610 | 662,513 |

Ratio convention matches the rest of this spec: `redis-server` throughput divided by rocket-mem
throughput, so above 1x means redis-server is faster and below 1x means rocket-mem is faster.

**The 1.0–1.3x prediction held for pipelined 3B `SET`:** 1.01x, at the low end of the predicted
band.

**It did not hold for pipelined 1KB `SET` — rocket-mem overshot the prediction, in the good
direction.** The ratio moved to 0.75x: rocket-mem's pipelined 1KB `SET` throughput is now measurably
*higher* than redis-server's (about 1.33x), not merely at parity with it. That is better than the
top of the predicted 1.0–1.3x band, not worse, so it should be reported plainly as a prediction miss
rather than folded into "the prediction held": the predicted floor was 1.0x and the measured value
is 0.75x, roughly 25 percentage points past even the optimistic edge of the prediction. This result
was consistent in all four individual runs (rocket-mem's 1KB pipelined `SET` beat redis-server's in
every run, not just in the median), so it is not an artifact of taking the median.

**Control (`GET`, which never takes the ordering guard):** 3B moved to 1.23x and 1KB to 1.14x,
against the prior general band of roughly 0.90x–1.15x for non-`SET` rows. The 1KB row sits inside
that band; the 3B row sits slightly above it. Neither shows anything close to the multi-x swing the
`SET` rows show, which is consistent with the mechanism affecting only writes — but the 3B row's
modest rise is a reminder that these are not clean-room numbers, measured as they were on a loaded
host.

## Out of scope

- The remaining per-command dispatcher overhead (the `metrics` crate's per-command registry
  lookup, ~4.7% of CPU on the `GET` profile, is the largest single item left). Separate work.
- Any change to the AOF writer thread, `AofMsg`, or the channel between them.
- Making single-key pipelined writes competitive. For one key, correct ordering *requires*
  serialisation; the gap there is the cost of a contended futex plus a channel send and a
  cross-thread wakeup versus Redis's memcpy into a buffer, and closing it means restructuring how
  the writer is fed. Not this change.

## Testing strategy

`cargo test --workspace` (792 tests as of this spec), `cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets -- -D warnings` gate every commit, per `CLAUDE.md`.

New tests, one per guarantee above:

1. **Per-key ordering survives.** Concurrent writers hammering the *same* key, then assert the AOF
   replays to the value the last committed write set. `aof.rs`'s existing
   `lock_for_ordering_serializes_concurrent_holders` is the model.
2. **Disjoint keys actually proceed concurrently** — the change is worthless if they do not. Assert
   two writers on keys in different shards can both be inside their critical sections at once
   (the inverse of the existing serialisation test), so a future refactor that reintroduces a
   global guard fails loudly rather than silently costing 3x.
3. **Snapshot consistency.** A `SAVE` concurrent with a write load, then assert
   snapshot-plus-AOF-tail replays to the same state as the AOF alone. This is the test that would
   catch job 2 regressing, and it does not exist today.
4. **Multi-key atomicity.** An `MSET` spanning shards concurrent with a `SAVE`, asserting the
   snapshot never contains a partial application. Directly covers job 3, which today is protected
   only by a comment.
5. **Deadlock.** Two multi-key commands with overlapping key sets in opposite orders, run
   concurrently under a timeout. Ascending-index acquisition makes this impossible; the test is
   what proves the rule is actually followed at every site.

Existing replication and `kill -9` durability tests must pass unchanged.

## Risks

- **Lock-ordering discipline is now load-bearing.** One site acquiring out of order deadlocks the
  server under concurrency, and may not reproduce on a developer machine. Test 5 exists for this;
  a helper that takes a key set and does the sorting/deduplication/acquisition in one place —
  rather than each call site rolling its own — is the structural mitigation and should be the only
  way guards are acquired.
- **Sixteen mutexes cost more memory and more acquisitions for multi-key commands.** A command
  touching keys in eight shards takes eight locks where it took one. Multi-key commands are rare
  relative to single-key ones, but a workload dominated by wide `MSET`s could regress. Worth
  measuring rather than assuming.
- **`command_keys` disagreeing with `extract_write_command_name`** would silently skip ordering for
  some command. The all-shards fallback contains this, and a test asserting every write command in
  `KNOWN_COMMANDS` yields at least one key (or is explicitly listed as taking all shards) would
  turn it into a compile-time-ish guarantee.
