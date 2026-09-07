# Closing the Throughput Gap to redis-server: Spec & Design

**Date:** 2026-09-07
**Status:** Approved
**Scope:** `crates/engine/src/store.rs`, `crates/engine/src/shard.rs`, `crates/server/src/connection.rs`, `crates/server/src/rmp_connection.rs`, `crates/server/src/aof.rs`, `crates/server/src/dispatcher.rs`. No wire-protocol change, no durability-guarantee weakening, no command-semantics change, no change to `maxmemory` eviction's observable behavior (approximate-LRU stays approximate-LRU).

**Goal:** close (or flip) rocket-mem's remaining throughput/latency gap to `redis-server`, currently 5.7–8.1% on raw throughput (rocket-mem already wins p99 tail latency by 27–29% on the same apples-to-apples run — this is about per-request overhead, not the tail-stall class of bug the prior effort fixed).

## Problem

Four causes identified, two independently (a concurrent investigation converged on the clock and `TCP_NODELAY` findings separately from this project's own flamegraph work — corroborating evidence, not just one analysis):

1. **A single shared `AtomicU64` clock defeats 16-way sharding.** `Store::clock` (`store.rs:8`) is passed by reference into every `Shard::get`/`set`/`with_ref` call (`store.rs:28,31,37,55`), and `Shard::get`/`set` call `clock.fetch_add(1, Ordering::Relaxed)` on it (`shard.rs:45,71`) to stamp each entry's `last_touched`. A `fetch_add` is a read-modify-write — it requires exclusive ownership of that cache line on whichever core executes it, so every GET/SET across all 16 shards, on every core, serializes on this one line. This is real, unconditional overhead on every command, not just writes, and it directly defeats the reason 16 shards exist (see `docs/design/sharding-decision.md`).

2. **No `TCP_NODELAY` anywhere.** Absent from `crates/server/src/connection.rs`, the TLS accept path, and `crates/server/src/rmp_connection.rs` (confirmed by grep). Nagle's algorithm interacting with unpipelined request/response traffic (exactly `redis-benchmark -c 50`'s default shape) adds real per-round-trip latency.

3. **`AofWriter::lock_for_ordering` contention on the write path.** `docs/benchmarks/2026-09-07-flamegraph-notes.md` source-confirmed this as the sole remaining candidate after eliminating `SlowLog`'s and `ReplicaRegistry`'s mutexes (both structurally incapable of being contended on this path). The guard is bound at `dispatcher.rs:2537` and held across mutate → encode → append → **broadcast to replicas**, though the invariant it exists to protect (AOF write order matches mutation-commit order, per its own doc comment) only requires covering mutate → encode → append.

4. **Dispatcher per-command overhead**, three independent parts:
   - `dispatch_and_log_inner` unconditionally clones the whole frame (`dispatcher.rs:2530`, `let original_frame = frame.clone();`) *before* checking whether the command is even a write command (`extract_write_command_name` is called on the clone one line later). The clone is only ever used inside the write-command branch (`original_frame` next appears at `:2552,2579,2581,2583`, all inside `if write_name.is_some()`'s body) — every read command pays for a clone it never uses.
   - `metric_label` (`dispatcher.rs:2410-2416`) allocates one `String` via `to_ascii_lowercase()`, and its caller (`dispatcher.rs:2444-2453`) then calls `.clone()` on it twice more (once per `metrics::counter!`/`metrics::histogram!` macro invocation) — three heap allocations on the common (non-error) path of every single command.
   - Eleven sequential checks (`cluster_redirect`, the replica/`extract_write_command_name` READONLY gate, `handle_auth`, `handle_acl`, `is_save_command`, `is_bgrewriteaof_command`, `handle_replicaof`, `handle_cluster`, `handle_info`, `handle_hello`, `handle_slowlog`) run before every command reaches real dispatch, each independently inspecting the frame's command name (`dispatcher.rs:2485-2525`).

## Decision: replace the shared clock with a periodic coarse clock, read not incremented

`active_expire_loop` (`connection.rs:49-56`) already ticks every 100ms. It gains one more responsibility: `store()` (not `fetch_add`) a shared `AtomicU64` representing "current approximate time" (a simple incrementing generation counter is sufficient — this never needs to be wall-clock-accurate, only monotonically non-decreasing at ~100ms resolution). `Shard::get`/`set` change from `clock.fetch_add(1, Ordering::Relaxed)` to `clock.load(Ordering::Relaxed)`.

This is deliberately the same design real Redis uses (`server.lruclock`, updated by a ~100ms cron, read on every access) — a proven pattern, not a novel one. It preserves `sample_for_eviction`'s cross-shard comparability exactly as today (the clock is still one value, shared and monotonic across all 16 shards) — the only change is that reading it no longer requires exclusive cache-line ownership. `Store::clock`'s type (`AtomicU64`) and `Shard::get`/`set`'s signatures (`clock: &AtomicU64`) are unchanged; only the one line inside each that touches it changes from `fetch_add` to `load`.

**Resolution tradeoff, stated plainly:** two entries touched within the same ~100ms window now get identical `last_touched` timestamps, where today they'd differ by however many operations happened between them. `sample_for_eviction`'s job is picking an approximately-least-recently-used candidate from a sample for `maxmemory` eviction, not exact LRU ordering — Redis's own eviction has run on exactly this resolution in production for over a decade. Existing eviction tests (`engine.rs`'s `with_maxmemory_evicts_the_least_recently_touched_key_first` and friends) assert *that* the least-recently-touched key gets evicted first among keys touched at clearly-separated times, not that ties within one clock tick resolve a specific way — they should need no changes, but must still pass.

## Decision: `TCP_NODELAY` on every accepted stream

`.set_nodelay(true)` on the `TcpStream` in `connection.rs`'s plaintext accept loop, in the TLS accept path (also `connection.rs`, after the TLS handshake completes — `set_nodelay` operates on the underlying `TcpStream`, unaffected by the TLS layer wrapping it), and in `rmp_connection.rs`'s accept loop. Three call sites, no design decision — this is a well-understood, standard setting for low-latency request/response protocols. A failure to set it is logged and non-fatal (matching how other non-critical socket options would be handled) — never a reason to drop an otherwise-good connection.

## Decision: narrow `lock_for_ordering`'s critical section, not its type

The guard at `dispatcher.rs:2537` currently spans mutate → encode → append → broadcast. Splits into two phases: everything through the AOF append stays under the lock (preserving the write-order-matches-commit-order invariant the lock exists for); the replication broadcast (`dispatcher.rs:2622`, `replication.registry.broadcast(...)`) moves to after the guard is dropped. Broadcast order to replicas was never part of the invariant this lock documents (replicas apply whatever they receive, in receipt order, independent of any local lock) — only the *local AOF's* order relative to *local mutations* was. Locking primitive stays `std::sync::Mutex` — the previous perf work already established (via `run_blocking`'s design) that swapping primitives isn't needed when the actual problem is section duration, not per-acquisition cost.

## Decision: three independent dispatcher micro-fixes

- **Frame clone**: call `extract_write_command_name(&frame)` first (needs only `&Frame`, no clone); clone into `original_frame` only inside the `if write_name.is_some()` branch, immediately before its first use. Read commands (the majority of most workloads) pay zero clones.
- **`metric_label` allocations**: add a `KNOWN_COMMANDS_LOWER: &[&str]` static array, index-parallel to the existing sorted `KNOWN_COMMANDS: &[&str]` (`dispatcher.rs:1168`). `metric_label` becomes `fn metric_label(name: &str) -> &'static str`, using `KNOWN_COMMANDS.binary_search(&name)`'s returned index (on `Ok`) to index into the lowercase array, falling back to `"other"` on `Err` — mirrors the existing binary-search-then-index pattern already used elsewhere in this file. `&'static str` is `Copy`, so the caller's two `.clone()` calls disappear entirely (`Copy` values don't need cloning) — this is the same shape of fix Sprint 6 already applied to command-name uppercasing (`CommandName`), applied to the lowercase metrics-label path that fix didn't cover.
- **Sequential routing checks**: out of scope for this spec. Reordering or restructuring 11 independent gate functions each with their own documented ordering rationale (see the doc comments at `dispatcher.rs:2474-2525`, several of which explicitly justify *why* one gate precedes another — e.g. "MOVED-beats-READONLY precedence") is a higher-risk change for uncertain payoff. Flagged as a candidate for a future, narrowly-scoped follow-up if profiling after the other three fixes still shows this as a measurable cost — not committed to here.

## Out of scope

- Tokio per-connection task scheduling overhead (item 5 from the investigation) — architectural, not a bug; accepted as a ceiling, not a target.
- The pipelined-1KB-GET anomaly's root cause — `TCP_NODELAY` is this spec's leading candidate fix for it, but confirming causation is a benchmark-and-profile activity for after this lands, not a design decision here.
- Any change to `Value`'s internal collection types, shard count, or the AOF pipeline's overall architecture — none of the four causes call for it.

## Testing strategy

`cargo test --workspace` (778 tests as of this spec) is the regression gate throughout. `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` gate every commit. New/adjusted tests per decision:
- Clock: a test proving `Store::sample_for_eviction`'s cross-shard comparison still works correctly with a coarse, externally-driven clock (not tied to a real 100ms wall-clock wait in the test itself — inject/advance the clock directly, the same way `Shard`/`Store`'s existing tests already construct clocks by hand).
- `TCP_NODELAY`: a test asserting the accepted stream has `nodelay() == true` after connection setup, for both the plaintext and RMP listeners at minimum (TLS path, if practically testable without a real handshake in a unit test — otherwise covered by existing integration-level TLS tests plus manual verification, noted honestly rather than skipped silently).
- `lock_for_ordering` narrowing: existing AOF-ordering tests must keep passing unchanged (they assert order, not lock span) — no new test needed unless the narrowing reveals a gap the existing suite didn't cover, in which case add one.
- Dispatcher fixes: existing `metric_label_lowercases_known_commands_and_collapses_the_rest` test (`dispatcher.rs:3580-3587`) continues to pass against the new signature (return-type change from `String` to `&'static str` doesn't change any assertion in that test, since `assert_eq!` compares `&str` either way). Frame-clone reordering needs a test confirming read commands never reach the clone path (e.g. instrument or reason about it structurally) plus confirmation existing write-path tests (already extensive per `dispatcher.rs`'s test module) still pass unchanged.
