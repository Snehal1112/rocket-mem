# RMP vs RESP — Controlled Protocol Benchmark: Spec & Design

**Goal:** answer one question with evidence — *does RMP's request multiplexing actually beat RESP's
pipelining, and if so, at what in-flight depth does it start to?*

**Scope:** a new dev-only load generator (`crates/bench`) that speaks both wire protocols from one
process, a driver script, and a committed report. This is a measurement project: no engine,
dispatcher, or protocol code changes. Existing benchmark work it builds on lives in
`../../benchmarks/2026-08-30-redis-benchmark.md` (rocket-mem vs real Redis over RESP, Sprint 6) and
`../../benchmarks/2026-08-30-flamegraph-notes.md`.

## Why this needs its own tool

No off-the-shelf load generator speaks RMP, so RMP has never been benchmarked — every number in
`docs/benchmarks/` is RESP-only. The obvious shortcut, `redis-benchmark` for RESP against a new
Rust tool for RMP, is not viable: `redis-benchmark` is mature hand-tuned C, and any measured
difference would conflate *protocol* with *client implementation quality*. The comparison is only
meaningful if the same client code, on the same runtime, drives both sides.

## The hypothesis being tested

The two protocols differ **structurally at the server**, not merely in framing. This is the finding
the whole design exists to measure:

- **RESP** (`crates/server/src/connection.rs:183`) dispatches **inline and serially** in the read
  loop. Its pipelining support (the `feed`/`flush` peek at `connection.rs:189-196`) batches *write
  syscalls* only — it never parallelizes execution. One RESP connection executes strictly one
  command at a time, on one core, no matter how deep the client's pipeline is.
- **RMP** (`crates/server/src/rmp_connection.rs:147`) spawns **a task per request**, bounded by
  `MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION = 256`. One RMP connection can execute up to 256 commands
  concurrently across the Tokio worker pool and all 16 engine shards.

Two falsifiable predictions follow:

1. **At depth 1, RMP is slightly *slower*.** It pays a `tokio::spawn` plus a bounded-channel hop per
   request that RESP's inline dispatch does not. If RMP wins at depth 1, the model above is wrong.
2. **As depth rises on a single connection, RMP pulls ahead**, because RESP cannot use more than one
   core per connection. The crossover depth is the number worth reporting.

A result showing RMP flat or slower at every depth would be a real finding, not a failed run — it
would mean the per-request spawn costs more than the shard parallelism it buys.

## Global Constraints

- **The client is held constant.** One binary, one Tokio runtime, one measurement loop, three
  drivers. Anything that optimizes one driver must be applied to the others or not at all.
- **No changes to engine, dispatcher, protocol, or server code.** `crates/bench` is additive and
  depends on `protocol` and `rmp-client` as they already exist; the only edit outside the new crate
  is adding it to the root `Cargo.toml`'s `workspace.members`. If a fair benchmark turns out to
  require a change to either dependency, that is a finding to report, not a change to slip in.
- **One server process serves both protocols.** rocket-mem binds RESP (`127.0.0.1:6379`) and RMP
  (`127.0.0.1:6380`) in the same process, so both sides of the comparison share one engine, one
  config, one AOF, and one durability policy by construction. Nothing needs to be matched by hand.
- **Sequential cells, never concurrent.** Only one (protocol, command, depth) cell runs at a time.
  Running RESP and RMP simultaneously would have them contend for the same cores and shards.
- **No new runtime dependency in any shipped crate.** `crates/bench` is a workspace member but is
  not published and is not part of the release artifact.

---

## Decision: crate layout

**`crates/bench`**, package name `rocket-mem-bench`, binary only.

Dependencies, all already in `workspace.dependencies`: `protocol`, `rmp-client`, `tokio`, `bytes`,
`tokio-util`, `futures-util`, `clap`.

Rejected alternatives:

- **A `--bench` binary inside `rmp-client`.** Cheaper, but wrong layering: `rmp-client` is the RMP
  client library, and adding a RESP client would put a second wire protocol into the dependency
  surface of every consumer of it.
- **Criterion benches.** Wrong tool. Criterion is built for in-process microbenchmarks with
  statistical resampling; it has no natural way to express a network concurrency window and does not
  produce sustained-throughput numbers.

## Decision: three drivers, one trait

```rust
/// Run exactly `ops` operations drawn from `workload`, holding `depth` requests in flight,
/// verifying every reply. Returns the latency samples the caller turns into p50/p99.
///
/// The unit of work is the whole cell, not one round, because the sliding-window driver
/// has no rounds to expose -- see the latency note under "measurement and reporting".
trait Driver {
    async fn run(
        &mut self,
        workload: &Workload,
        ops: usize,
        depth: usize,
    ) -> Result<Samples, BenchError>;
}
```

The trait stays **private to the crate**. `async fn` in a *publicly reachable* trait triggers
rustc's warn-by-default `async_fn_in_trait` lint, and CI runs
`cargo clippy --workspace --all-targets -- -D warnings`, which promotes that to a build failure.
Keeping the trait private avoids it; the alternative, desugaring to
`fn run(&mut self, ...) -> impl Future<Output = ...> + Send`, is available if the trait ever needs
to be public. Only static dispatch is needed here, so nothing requires `dyn`.

- **`RespDriver`** wraps `Framed<TcpStream, protocol::codec::RespCodec>` (`RespCodec::default()` is
  RESP2, which is what `redis-benchmark` speaks). A round is `feed` for each request, one `flush`,
  then read exactly `batch.len()` replies in order. This is exactly what `redis-benchmark -P N` does.
- **`RmpDriver`** wraps `rmp_client::RmpClient`. A round is `batch.len()` concurrent `call` futures
  collected with `join_all`. `RmpClient::call` takes `&self`, so this needs no client changes.
- **`RmpWindowDriver`** wraps the same `RmpClient`, but keeps a **sliding window** of N outstanding
  calls: as each reply lands, another request is issued immediately, so exactly N are in flight at
  all times with no barrier between rounds.

**The first two drivers share batch semantics — fire N, await all N, repeat.** That is a deliberate
fairness decision, not an implementation convenience: holding the client's *issuing pattern*
constant leaves the server's execution model as the only variable, which is precisely the
hypothesis.

But batch semantics also under-serve RMP. Fire-N-wait-all imposes a barrier at the end of every
round — the whole batch waits on its slowest member — and RMP is structurally able to avoid that
while RESP, whose replies must be read in order, is not. Measuring only the batched mode would
answer "does RMP's execution model help?" while leaving "how fast can RMP actually go?" unasked.

Hence the third driver rather than a choice between the two. The three pairings answer three
distinct questions from one run:

| Comparison | Question answered |
|---|---|
| `RespDriver` vs `RmpDriver` | Does RMP's server-side execution model help, client pattern held constant? |
| `RmpDriver` vs `RmpWindowDriver` | What does the fire-N-wait-all barrier itself cost? |
| `RespDriver` vs `RmpWindowDriver` | Each protocol at its best — the practical "should I use RMP?" |

The controlled comparison is preserved intact; the window mode is additive. Note that the third
pairing is deliberately *not* client-pattern-controlled — it is the one comparison here that mixes
two variables on purpose, and the report must label it as such rather than presenting it alongside
the first as though they were the same kind of measurement.

## Decision: keyspace, and why it is load-bearing

**1,000 pre-seeded keys, 64-byte values**, named `bench:key:{0..999}`, which
`DefaultHasher(key) % 16` spreads across all 16 shards.

This is the design's most consequential detail. The Sprint 6 run used a single key throughout — its
own `/metrics` sample recorded `rocket_mem_keys 1`, because `redis-benchmark` was never passed `-r`.
Repeating that here would invalidate the entire experiment: with one key, all in-flight RMP tasks
contend on one shard's `RwLock`, erasing the shard parallelism that is the only reason RMP could
win. The key spread is what makes the hypothesis testable at all.

GET is measured against **pre-seeded keys so it is a hit, not a miss** — a miss returns `Null`
without touching a value and would measure a different, cheaper path.

## Decision: the sweep

| Dimension | Values |
|---|---|
| Driver | RESP-batch, RMP-batch, RMP-window |
| Command | GET, SET |
| In-flight depth | 1, 2, 4, 8, 16, 32, 64, 128, 256 |
| Connections | 1 (fixed) |
| Payload | 64 B (fixed) |

54 cells. Connections are pinned at 1 deliberately: the structural difference is *per connection*,
and adding connections lets RESP reach the same core count by a different route, which would mask
the effect rather than measure it. Depth stops at 256 because that is
`MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION` — beyond it the server's semaphore, not the protocol, is
what throttles.

At depth 1 all three drivers degenerate to the same thing — one request, one reply, no batching and
no window — so those three cells are a built-in sanity check: RMP-batch and RMP-window must agree
within noise, and any gap between them at depth 1 is a bug in the harness, not a finding.

## Decision: measurement and reporting

Per cell: **10,000 warmup operations, discarded**, then **200,000 measured operations**, timed end
to end. Throughput is `total_ops / elapsed`. Both counts are fixed across every cell so cells are
directly comparable; 200,000 matches the order of magnitude `scripts/benchmark.sh` already uses
(`redis-benchmark -n 100000`) while keeping the deepest cells from finishing too fast to time well.

**Throughput is comparable across all 54 cells.** Latency is not, and the report must not pretend
otherwise — the three drivers measure two different quantities:

- **RESP-batch and RMP-batch** report **round** latency (p50/p99): the time from firing a batch of N
  to its last reply. Under batch semantics a per-operation latency is not separable on the RESP
  side, where replies are read in order from one buffer, so a per-op figure there would be false
  precision. Both batch drivers define a round identically, so their round latencies compare
  directly.
- **RMP-window** reports **per-operation** latency (p50/p99), which it genuinely can measure — each
  `call` future completes independently, so every operation has its own start and end. It has no
  rounds to report.

These two are not interchangeable and must never be placed in one column: at depth N a round
latency is roughly N operations' worth of work, so it will look ~N× worse than a per-op latency
measuring the same server at the same speed. Latency is therefore compared **within** a mode across
depths, and **between** the two batch drivers — never between a batch driver and the window driver.
Throughput carries the cross-mode comparison.

**Correctness gate.** Every reply is asserted, never discarded: GET must return the seeded value and
SET must return `+OK`. Any mismatch aborts the cell. This is the single most important guard in the
design — an error path returning fast (a `WRONGTYPE`, an auth failure, a malformed frame) reads as a
throughput *win* to a generator that only counts replies. Nothing here is trustworthy without it.

## Decision: harness and output

**`scripts/rmp-vs-resp.sh`**, following the shape of the existing `scripts/benchmark.sh`: build
release, start one rocket-mem into a `mktemp -d` working directory with AOF enabled and an
`everysec` fsync policy, wait for both ports to answer, run the sweep, sample `/metrics`, tear down
via an `EXIT` trap.

AOF stays **on**, matching `scripts/benchmark.sh` and real deployment. It is identical for both
protocols (same process, same AOF), so it cannot bias the comparison — though it does mean SET
carries an AOF encode-and-channel-send that GET does not, which is expected to show up as a
consistent SET/GET gap on *both* protocols.

Note that rocket-mem has **no `FLUSHALL`** (verified against
`crates/server/src/dispatcher.rs`), so the harness cannot reset the keyspace between cells. With a
fixed 1,000-key working set that is harmless — the keyspace is seeded once and never grows — but it
is why the seeded-key count is fixed rather than per-cell.

Report committed to **`docs/benchmarks/2026-09-01-rmp-vs-resp.md`**, carrying: host/version/date
provenance, the full 54-cell table, the crossover depth, whether each of the two predictions held,
each of the three pairings reported separately with its own question stated, and an honest "what
this does not measure" section.

## Decision: testing the generator

A benchmark that is wrong is worse than no benchmark, so the generator is tested like production
code, against mock servers in the style `crates/rmp-client/src/lib.rs`'s own tests already use:

- **`RespDriver` genuinely pipelines** — a mock server asserts that N requests arrive before it
  sends the first reply, proving `feed`/`flush` batching works and the driver is not accidentally
  serializing round-trips (which would understate RESP and fake an RMP win).
- **`RmpDriver` genuinely multiplexes** — a mock server asserts N requests arrive before it replies,
  and replies out of order, proving depth N means N actually in flight.
- **`RmpWindowDriver` holds the window full** — a mock server records the number of outstanding
  requests over time and asserts it reaches N and stays there, rather than sawtoothing between N and
  0. A window that silently drains to empty between refills is just the batch driver wearing a
  different name, and would make the two RMP modes look identical for a reason that has nothing to
  do with the server.
- **All three drivers issue exactly `ops` operations** — the counts must match across modes or the
  throughput comparison is meaningless, and a window driver is easy to get subtly wrong here by
  over- or under-issuing at the tail as the window drains.
- **The correctness gate fires** — a mock server returning a wrong value must abort the cell rather
  than count it.

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` must all pass, per `CLAUDE.md`; clippy lints the new crate's test code too.

## What this explicitly does not measure

TLS, authentication or ACL overhead, cluster routing, multi-connection scaling curves, command
families beyond GET/SET, payload sizes other than 64 B, replication lag, and RESP3 (the driver
negotiates RESP2). Cross-protocol comparison against *real Redis* is also out of scope — Redis does
not speak RMP, and `../../benchmarks/2026-08-30-redis-benchmark.md` already covers the RESP side of
that question.

Results will be single-sample on one shared machine. Per the Sprint 6 report's own conclusion, a few
percent of movement in either direction is within run-to-run noise; only differences well outside
that band will be claimed as real.
