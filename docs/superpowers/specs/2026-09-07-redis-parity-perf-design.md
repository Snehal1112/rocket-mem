# Blocking-I/O Fix and Read/Write Performance Parity: Spec & Design

**Date:** 2026-09-07
**Status:** Approved
**Scope:** `crates/server/src/aof.rs`, `crates/server/src/connection.rs`, `crates/server/src/dispatcher.rs`, `crates/server/src/slowlog.rs`, `crates/server/src/replication.rs`, `crates/engine/src/shard.rs` (read-only investigation unless profiling says otherwise), `docs/benchmarks/`, `README.md`'s Performance table. No wire-protocol change, no durability-guarantee weakening, no command-semantics change — every fix in this spec is an internal implementation detail.

**Goal:** eliminate the periodic 100–280ms request stalls found in manual benchmarking, then close the throughput/latency gap to `redis-server` across the full 8-row benchmark matrix in `docs/benchmarks/2026-08-30-redis-benchmark.md`, under matched durability settings (`appendonly yes`, `appendfsync everysec` on both servers), to parity or a specific documented remaining cause.

## Problem

### The stall bug

Manual `redis-benchmark -t set,get -n 100000 -c 50` runs (session transcript, 2026-09-07) show rocket-mem's `SET`/`GET` p50/p95/p99 latencies at or better than `redis-server`'s, but with `max` latency spiking to 213–495ms across three separate runs, against `redis-server`'s 4–7ms max on the same runs. The cumulative-distribution data shows this isn't gradual degradation — it's a small fraction of requests (roughly 1 in 1,000–2,000) stalling severely while the rest are unaffected.

Root cause, traced by file:line:

- `periodic_fsync_loop` (`crates/server/src/connection.rs:65-75`) is a plain `tokio::spawn`ed async task, firing every 1 second under `FsyncPolicy::EverySecond` (the server's default, set in `main.rs:155`). It calls `aof.fsync()`.
- `AofWriter::fsync()` (`crates/server/src/aof.rs:189`) sends a message to the dedicated AOF-writer OS thread, then calls `ack_rx.recv()` — a **blocking** `std::sync::mpsc::Receiver::recv()`, not an `.await`. It waits for that thread's real `flush()` + `sync_data()` (fdatasync) syscall to complete.
- Because this blocking call isn't wrapped in `tokio::task::spawn_blocking`, it freezes the entire Tokio worker OS thread that happens to be running it, for the syscall's real duration — not just the one task.
- It compounds: every write command's `AofWriter::append_encoded()` (`crates/server/src/aof.rs`, called from `dispatcher.rs:2615`) sends to the *same* single writer thread via a bounded `mpsc::SyncSender<AofMsg>` channel (`AOF_QUEUE_CAPACITY = 1024`, `aof.rs:38`). While the writer thread is stuck inside the periodic fsync's syscall, it can't drain queued `Append` messages either, so under concurrent write load the channel fills, and every subsequent write's own blocking `send()` call — also not offloaded — freezes *its* worker thread too.
- Net effect: whenever the underlying disk's fdatasync call is slow (not unusual under I/O contention), a cluster of Tokio worker threads can freeze simultaneously — for writes, directly (queue backpressure); for reads, indirectly (fewer worker threads available for the runtime's work-stealing scheduler to run *any* task on, including unrelated `GET`s).

### The performance gap

`docs/benchmarks/2026-08-30-redis-benchmark.md` records rocket-mem 1.03x–2.39x slower than `redis-server` on 7 of 8 rows (roughly proportional to dispatcher/AOF-encode overhead per the existing analysis) and one anomalous row 58.30x slower (pipelined 1KB `GET`, unexplained — the accompanying flamegraph notes could not resolve kernel call stacks because `kptr_restrict=1`/`perf_event_paranoid=4` blocked symbol resolution in that environment, and remain so in the current one).

That same flamegraph profile (`docs/benchmarks/2026-08-30-flamegraph-notes.md`) recorded two leads never acted on:
- A `std::sync::Mutex::lock_contended` frame at 1.96% self-time — the single largest *named* self-time frame in the whole profile, larger than the shard's `parking_lot::RwLock` contention (0.01%, confirmed *not* the bottleneck). Three candidates share this mutex type on the hot path: `AofWriter::lock_for_ordering` (`aof.rs:164`), `SlowLog`'s `entries` mutex (`slowlog.rs:4`), `ReplicaRegistry::senders` (`replication.rs:24`). Never attributed to one specifically.
- `dispatch_and_log`'s `metric_label` function still allocates 2–3 `String`s per command via `to_ascii_lowercase()` and `.clone()` — flagged as a known, unfixed opportunity in the AOF-compaction notes.

## Decision: fix the stall bug first, independent of the profiling work

`AofWriter::fsync()`'s `ack_rx.recv()` and `AofWriter::append_encoded()`'s `self.send()` both move inside `tokio::task::spawn_blocking`, called from an `.await`ed wrapper so the calling task suspends cooperatively instead of blocking its worker thread. `periodic_fsync_loop` and every write-command call site keep their current signatures; only the internals of these two `AofWriter` methods change from "blocking call directly in async context" to "blocking call handed to the blocking-task pool."

This is intentionally sequenced first and separately from the profiling work in the next section: it's already root-caused and doesn't need a flamegraph to justify, and fixing it changes the *baseline* every subsequent benchmark run measures against.

TDD approach: a test that starts a slow/artificially-delayed fsync (e.g. a test-only `AofWriter` variant or a way to inject latency into the writer thread's `sync_data` call) concurrently with unrelated engine reads on other tasks, asserting the reads complete promptly instead of blocking on the slow fsync. Written to fail against the current code, then made to pass by the fix.

## Decision: profile with resolved kernel symbols, three separate recordings

The prior flamegraph run recorded three `redis-benchmark` phases (unpipelined 3B, pipelined 3B, pipelined 1KB) as one continuous `perf.data`, which its own notes flag as the reason samples couldn't be attributed to the 58x anomaly specifically. This time: three separate `cargo flamegraph` invocations, one per phase, each against a freshly started server, with `kernel.kptr_restrict=0` and `kernel.perf_event_paranoid=-1` (set by the user via `sudo` before this phase runs) so kernel/socket call stacks resolve instead of bottoming out in `[unknown]`.

Each recording is analyzed for:
1. Which of the three `std::sync::Mutex` candidates the 1.96% contention frame actually belongs to.
2. Whether the pipelined-1KB-GET recording shows a distinct hot path the other two don't (the prior profile's leading hypothesis was TCP/socket write-path behavior under large pipelined responses, unconfirmed).
3. Confirmation of the already-known allocation costs (`metric_label`, `encoded.clone()`, `BytesMut` growth) so their real weight is measured, not assumed.

## Decision: fix in self-time order, one TDD cycle per fix

Fixes are not pre-committed to a list — Phase 2's profile decides priority and existence. The two currently-known, not-yet-fixed candidates most likely to make the cut:

- **`metric_label` allocation removal**: replace the per-command `to_ascii_lowercase()` + `.clone()` pair with a lookup table keyed the same way `KNOWN_COMMANDS`/`CommandName` already is (mirroring the Sprint 6 fix that removed the four uppercase-allocation sites). Purely internal to `dispatcher.rs`.
- **Mutex contention resolution**: once attributed to one of the three candidates, the fix depends on which one — e.g. if it's `SlowLog`'s mutex, consider `parking_lot::Mutex` (already a project dependency) in place of `std::sync::Mutex` for its lower uncontended overhead and short critical section; if it's `AofWriter::lock_for_ordering`, examine whether its critical section can shrink without breaking the ordering guarantee its doc comment describes.

Each fix: reproduce/measure in isolation where practical, implement, `cargo test --workspace` for regressions, re-run the relevant benchmark row(s) to confirm the fix moved the number it was meant to move.

## Decision: fair baseline and iteration loop

Before Phase 2's profiling and after every fix in Phase 3, re-run the full 8-row matrix with **both servers** configured `appendonly yes`, `appendfsync everysec`, RDB auto-save off (`docs/benchmarks/2026-08-30-redis-benchmark.md`'s own methodology, but this time actually applied to `redis-server` too — the last several manual runs left `redis-server`'s `appendonly` at `no`, which the session already flagged as not apples-to-apples). Stop iterating when rocket-mem matches or beats `redis-server` on all 8 rows, or when a specific row's remaining gap is traced to a specific, profiled cause rather than left as "still slower."

## Out of scope

- Any change to RESP/RMP wire behavior, command semantics, or durability guarantees (a write must still be at least as durable after this work as before).
- Lock-free shard rewrite — the existing flamegraph data already shows shard-lock contention is *not* the bottleneck at this concurrency (0.01% self-time); revisiting that is explicitly deferred per `docs/design/sharding-decision.md`'s own reasoning, unless Phase 2's fresh profile contradicts the old one.
- Cluster-mode or replication-path benchmarking — the matrix being targeted is single-node only, matching the existing `docs/benchmarks/` scope.
- Root-causing the 58x anomaly is a *goal*, not a guaranteed outcome of this spec — if Phase 2's improved profiling still can't attribute it (e.g. if the true cause is outside rocket-mem, such as kernel TCP buffer tuning on this specific host), that becomes a documented, specific finding, not a blocker to closing this work.

## Testing strategy

`cargo test --workspace` (773 tests as of this spec) is the regression gate for every phase. `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` gate every commit, matching this project's CI (`.github/workflows/ci.yml`) and the standing instruction to verify build+lint before considering work done. New tests accompany the stall-bug fix (Phase 0) and any Phase 3 fix that has an isolatable regression risk (e.g. a concurrency test for the mutex-contention fix, if reproducible without flakiness).
