# RESP Batch Driver Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `RespDriver` that runs a `Workload` over RESP2, pipelining `depth` requests per round exactly as `redis-benchmark -P N` does, verifying every reply.

**Architecture:** Wraps `Framed<TcpStream, protocol::codec::RespCodec>`. A round is `feed` × N, one `flush`, then read exactly N replies in order. Tests run against mock RESP servers built from the same codec, following the pattern in `crates/rmp-client/src/lib.rs`'s tests.

**Tech Stack:** Rust 2021, `tokio`, `tokio-util::codec::Framed`, `futures-util` (`SinkExt`/`StreamExt`), `protocol`.

**Spec:** [`../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md`](../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md)

**Depends on:** [`01-crate-foundation.md`](01-crate-foundation.md) — `BenchError`, `Workload`, `Samples`.

## Global Constraints

- **No changes to engine, dispatcher, protocol, or server code.**
- **CI gates:** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- **RESP2 only.** `RespCodec::default()` is `Protocol::Resp2`, which is what `redis-benchmark` speaks. Do not send `HELLO`.
- **Requests are `Frame::Array` of `Frame::Bulk`** — the standard RESP command encoding.
- **Comment style:** short, plain full sentences ending in a punctuation mark. No emojis.

---

### Task 1: `RespDriver` connect and run at depth 1

**Files:**
- Create: `crates/bench/src/resp.rs`
- Modify: `crates/bench/src/lib.rs`

**Interfaces:**
- Consumes: `crate::error::BenchError`, `crate::workload::{Workload, OpKind}`, `crate::stats::Samples`.
- Produces:
  - `pub struct RespDriver`
  - `RespDriver::connect(addr: std::net::SocketAddr) -> Result<RespDriver, BenchError>` (async)
  - `RespDriver::run(&mut self, workload: &Workload, ops: usize, depth: usize) -> Result<Samples, BenchError>` (async)

- [ ] **Step 1: Write the failing test**

Create `crates/bench/src/resp.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::{OpKind, Workload};
    use futures_util::{SinkExt, StreamExt};
    use protocol::codec::RespCodec;
    use protocol::Frame;
    use tokio::net::TcpListener;
    use tokio_util::codec::Framed;

    /// A mock RESP server that answers every request with `reply`, one at a time.
    async fn spawn_echo_server(reply: Frame) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut framed = Framed::new(socket, RespCodec::default());
            while let Some(Ok(_req)) = framed.next().await {
                if framed.send(reply.clone()).await.is_err() {
                    break;
                }
            }
        });
        addr
    }

    #[tokio::test]
    async fn run_at_depth_one_completes_every_operation() {
        let addr = spawn_echo_server(Frame::Simple("OK".to_string())).await;
        let mut driver = RespDriver::connect(addr).await.unwrap();
        let workload = Workload::new(OpKind::Set, 4, 8);

        let samples = driver.run(&workload, 10, 1).await.unwrap();

        // Depth 1 means one round per operation, so one sample per operation.
        assert_eq!(samples.len(), 10);
    }

    #[tokio::test]
    async fn run_returns_connection_closed_when_the_server_disappears() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket); // Disconnect without ever replying.
        });

        let mut driver = RespDriver::connect(addr).await.unwrap();
        let workload = Workload::new(OpKind::Set, 1, 8);
        let err = driver.run(&workload, 1, 1).await.unwrap_err();
        assert!(matches!(err, BenchError::ConnectionClosed | BenchError::Io(_)));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem-bench resp`
Expected: FAIL to compile — `RespDriver` is not defined.

- [ ] **Step 3: Write the minimal implementation**

Add `pub mod resp;` to `crates/bench/src/lib.rs`, then prepend to `crates/bench/src/resp.rs`:

```rust
use crate::error::BenchError;
use crate::stats::Samples;
use crate::workload::Workload;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use protocol::codec::RespCodec;
use protocol::Frame;
use std::net::SocketAddr;
use std::time::Instant;
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

/// Drives load over RESP2 with client-side pipelining: `depth` requests are written in one
/// batch, then exactly `depth` replies are read back in order. This is what
/// `redis-benchmark -P N` does, and it is the only concurrency RESP structurally offers --
/// the server dispatches a pipelined batch serially, one command at a time.
pub struct RespDriver {
    framed: Framed<TcpStream, RespCodec>,
}

/// Turns command args into the RESP array-of-bulk-strings a server expects.
fn to_frame(args: Vec<Bytes>) -> Frame {
    Frame::Array(args.into_iter().map(Frame::Bulk).collect())
}

impl RespDriver {
    pub async fn connect(addr: SocketAddr) -> Result<Self, BenchError> {
        let socket = TcpStream::connect(addr).await?;
        // RespCodec::default() is RESP2. No HELLO is sent, matching redis-benchmark.
        Ok(RespDriver {
            framed: Framed::new(socket, RespCodec::default()),
        })
    }

    pub async fn run(
        &mut self,
        workload: &Workload,
        ops: usize,
        depth: usize,
    ) -> Result<Samples, BenchError> {
        let mut samples = Samples::with_capacity(ops.div_ceil(depth.max(1)));
        let mut issued = 0usize;

        while issued < ops {
            let batch = depth.min(ops - issued);
            let started = Instant::now();

            // Buffer the whole batch, then flush once. Flushing per request would turn
            // pipelining into a syscall per command and measure the wrong thing.
            for i in 0..batch {
                let frame = to_frame(workload.request(issued + i));
                self.framed.feed(frame).await?;
            }
            self.framed.flush().await?;

            // Read exactly as many replies as were written. RESP guarantees they come back
            // in request order, so no correlation is needed.
            for _ in 0..batch {
                let reply = match self.framed.next().await {
                    Some(Ok(frame)) => frame,
                    Some(Err(e)) => return Err(BenchError::Io(e)),
                    None => return Err(BenchError::ConnectionClosed),
                };
                workload.verify(&reply)?;
            }

            samples.push(started.elapsed());
            issued += batch;
        }

        Ok(samples)
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem-bench resp`
Expected: PASS, 2 tests.

- [ ] **Step 5: Run the full CI gate**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
Expected: all clean.

- [ ] **Step 6: Commit**

```bash
git add crates/bench/
git commit -m "$(cat <<'EOF'
test(bench): add RespDriver running a workload over RESP2

Wraps Framed<TcpStream, RespCodec> and runs a workload as batches of
`depth` requests: buffer the batch with feed(), flush once, then read
back exactly that many replies in order.

Flushing once per batch rather than per request is the point. A flush is
a write syscall, and flushing per command would turn pipelining into the
regression the server's own connection.rs comments describe, measuring
syscall overhead instead of protocol behaviour.

No HELLO is sent, so the connection stays RESP2 -- the same dialect
redis-benchmark uses, keeping this comparable to the existing
docs/benchmarks figures.
EOF
)"
```

---

### Task 2: Prove the driver genuinely pipelines

**Files:**
- Modify: `crates/bench/src/resp.rs` (tests only)

**Interfaces:**
- Consumes: `RespDriver::run` from Task 1.
- Produces: no new public API. This task is a proof, not a feature.

**Why this task exists:** if `run` accidentally serialized — flushing and reading one reply before writing the next — every RESP number would be a round-trip-latency measurement rather than a pipelining measurement, understating RESP and manufacturing a fake RMP win. The bug is invisible without a test that asserts on *arrival order at the server*.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/bench/src/resp.rs`:

```rust
/// A mock server that refuses to reply until `expect` requests have arrived. If the driver
/// waits for a reply before sending its next request, both sides block and the test times out.
async fn spawn_batch_asserting_server(expect: usize, reply: Frame) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(socket, RespCodec::default());
        loop {
            // Collect a whole batch before answering any of it.
            let mut received = 0usize;
            while received < expect {
                match framed.next().await {
                    Some(Ok(_)) => received += 1,
                    _ => return,
                }
            }
            for _ in 0..expect {
                if framed.send(reply.clone()).await.is_err() {
                    return;
                }
            }
        }
    });
    addr
}

#[tokio::test]
async fn run_writes_the_whole_batch_before_reading_any_reply() {
    let addr = spawn_batch_asserting_server(8, Frame::Simple("OK".to_string())).await;
    let mut driver = RespDriver::connect(addr).await.unwrap();
    let workload = Workload::new(OpKind::Set, 8, 8);

    // Bounded: a driver that serializes round-trips deadlocks against this server, and the
    // failure must be a test failure rather than a hung suite.
    let samples = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        driver.run(&workload, 16, 8),
    )
    .await
    .expect("driver did not pipeline -- it waited for a reply before sending the next request")
    .unwrap();

    // 16 operations at depth 8 is exactly 2 rounds, so 2 samples.
    assert_eq!(samples.len(), 2);
}

#[tokio::test]
async fn a_final_short_batch_is_still_written_as_one_flush() {
    // 10 ops at depth 4 is two full rounds plus a final round of 2. The short round must
    // still be flushed, or the run hangs waiting for replies it never asked for.
    let addr = spawn_echo_server(Frame::Simple("OK".to_string())).await;
    let mut driver = RespDriver::connect(addr).await.unwrap();
    let workload = Workload::new(OpKind::Set, 4, 8);

    let samples = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        driver.run(&workload, 10, 4),
    )
    .await
    .expect("the final short batch was never flushed")
    .unwrap();

    assert_eq!(samples.len(), 3);
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p rocket-mem-bench resp`
Expected: PASS, 4 tests. Task 1's implementation already flushes per batch, so these should pass without changes — that is the point of writing them, to lock the behaviour in.

If `run_writes_the_whole_batch_before_reading_any_reply` times out, `run` is flushing or reading inside the write loop. Move the `flush()` outside the `for i in 0..batch` loop.

- [ ] **Step 3: Verify the test actually detects the bug**

Temporarily break the implementation by moving `self.framed.flush().await?;` inside the write loop and adding a read after it, or more simply replace `feed` with `send` (which flushes each time) — `send` alone still pipelines at the protocol level, so instead deliberately serialize:

```rust
// TEMPORARY -- revert after this step.
for i in 0..batch {
    self.framed.send(to_frame(workload.request(issued + i))).await?;
    let _ = self.framed.next().await;
}
```

Run: `cargo test -p rocket-mem-bench resp`
Expected: `run_writes_the_whole_batch_before_reading_any_reply` FAILS with the "did not pipeline" message.

Revert the change and re-run. Expected: PASS, 4 tests.

- [ ] **Step 4: Run the full CI gate**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
Expected: all clean.

- [ ] **Step 5: Commit**

```bash
git add crates/bench/
git commit -m "$(cat <<'EOF'
test(bench): assert RespDriver pipelines rather than round-trips

Adds a mock server that withholds every reply until a full batch has
arrived, so a driver that waits for reply N before sending request N+1
deadlocks and fails the test on a timeout instead of passing quietly.

This guards the measurement, not the feature. A silently serializing
driver still produces plausible numbers -- they would just be
round-trip latency, understating RESP and manufacturing an RMP win that
is not real.

Also covers the final short batch, where ops is not a multiple of depth,
since failing to flush it hangs the run waiting on replies that were
never requested.
EOF
)"
```

---

### Task 3: Prove the correctness gate aborts the run

**Files:**
- Modify: `crates/bench/src/resp.rs` (tests only)

**Interfaces:**
- Consumes: `RespDriver::run`, `Workload::verify`.
- Produces: no new public API.

**Why this task exists:** the spec calls the correctness gate the single most important guard in the design. `Workload::verify` is unit-tested in plan 01, but nothing yet proves `RespDriver` actually *calls* it. A driver that discards replies would report the highest throughput in the whole sweep.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/bench/src/resp.rs`:

```rust
#[tokio::test]
async fn a_wrong_reply_aborts_the_run_instead_of_counting_it() {
    // The server answers a SET workload with an error, which is exactly the fast path that
    // would look like a throughput win if replies went unchecked.
    let addr = spawn_echo_server(Frame::Error("ERR unknown command".to_string())).await;
    let mut driver = RespDriver::connect(addr).await.unwrap();
    let workload = Workload::new(OpKind::Set, 4, 8);

    let err = driver.run(&workload, 100, 4).await.unwrap_err();
    assert!(
        matches!(err, BenchError::UnexpectedReply { .. }),
        "expected the gate to reject the error reply, got {err:?}"
    );
}

#[tokio::test]
async fn a_get_miss_aborts_the_run() {
    // A miss returns Null without touching a value. It is cheaper than a hit, so counting it
    // would silently measure the wrong path.
    let addr = spawn_echo_server(Frame::Null).await;
    let mut driver = RespDriver::connect(addr).await.unwrap();
    let workload = Workload::new(OpKind::Get, 4, 8);

    let err = driver.run(&workload, 100, 4).await.unwrap_err();
    assert!(matches!(err, BenchError::UnexpectedReply { .. }));
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p rocket-mem-bench resp`
Expected: PASS, 6 tests. Task 1 already calls `workload.verify(&reply)?`, so these lock in existing behaviour.

If they fail, `run` is discarding replies — add `workload.verify(&reply)?;` after each read.

- [ ] **Step 3: Verify the tests actually detect the bug**

Temporarily comment out `workload.verify(&reply)?;` in `run`.

Run: `cargo test -p rocket-mem-bench resp`
Expected: both new tests FAIL (the run succeeds where it should have errored).

Restore the line and re-run. Expected: PASS, 6 tests.

- [ ] **Step 4: Run the full CI gate**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
Expected: all clean.

- [ ] **Step 5: Commit**

```bash
git add crates/bench/
git commit -m "$(cat <<'EOF'
test(bench): assert RespDriver enforces the reply gate

Workload::verify is unit-tested already, but nothing proved the driver
calls it. These tests answer an error reply and a GET miss and require
the run to abort.

Both are fast paths: an error never reaches the engine, and a miss
returns Null without touching a value. A driver that discarded replies
would post the best throughput figures in the sweep while measuring
neither GET nor SET.
EOF
)"
```

---

## Definition of Done

- [ ] `RespDriver::connect` and `RespDriver::run` exist with the interfaces above.
- [ ] 6 tests pass in `crates/bench/src/resp.rs`, including the two that were manually verified to fail when their guard is removed.
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` are all clean.
- [ ] Three commits, one per task.
