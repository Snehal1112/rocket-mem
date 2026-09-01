# RMP Drivers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The `Driver` trait plus both RMP drivers — `RmpDriver` (batch, matching `RespDriver`'s issuing pattern) and `RmpWindowDriver` (sliding window, RMP at its best).

**Architecture:** Both wrap `rmp_client::RmpClient`, whose `call` takes `&self` and so already supports concurrent in-flight requests. The batch driver uses `join_all`; the window driver uses `FuturesUnordered`, refilling on each completion so exactly `depth` requests are outstanding at all times. `RespDriver` is refactored to implement the same trait.

**Tech Stack:** Rust 2021, `tokio`, `futures-util` (`join_all`, `FuturesUnordered`), `rmp-client`, `protocol`.

**Spec:** [`../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md`](../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md)

**Depends on:** [`01-crate-foundation.md`](01-crate-foundation.md), [`02-resp-batch-driver.md`](02-resp-batch-driver.md).

## Global Constraints

- **No changes to engine, dispatcher, protocol, server, or `rmp-client` code.** If a fair benchmark appears to need a change to `rmp-client`, stop and report it rather than making it.
- **CI gates:** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- **The `Driver` trait is `pub` and uses RPITIT**, not `async fn`. A `pub(crate)` trait in a lib target is dead code under `-D warnings`, and a `pub` trait with `async fn` trips the `async_fn_in_trait` lint. `-> impl Future<Output = ...> + Send` avoids both.
- **Depth 1 must behave identically across all three drivers.** At depth 1 there is no batch and no window, so the three are the same thing; any divergence there is a harness bug.
- **Comment style:** short, plain full sentences ending in a punctuation mark. No emojis.

---

### Task 1: The `Driver` trait and `RmpDriver` (batch mode)

**Files:**
- Create: `crates/bench/src/driver.rs`
- Create: `crates/bench/src/rmp.rs`
- Modify: `crates/bench/src/lib.rs`
- Modify: `crates/bench/src/resp.rs` (move `run` into an `impl Driver for RespDriver` block)

**Interfaces:**
- Consumes: `BenchError`, `Workload`, `Samples`, `RespDriver`.
- Produces:
  - `pub trait Driver { fn run(&mut self, workload: &Workload, ops: usize, depth: usize) -> impl std::future::Future<Output = Result<Samples, BenchError>> + Send; }`
  - `impl Driver for RespDriver`
  - `pub struct RmpDriver` with `RmpDriver::connect(addr: std::net::SocketAddr) -> Result<RmpDriver, BenchError>` (async) and `impl Driver for RmpDriver`.

- [ ] **Step 1: Write the failing test**

Create `crates/bench/src/rmp.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::{OpKind, Workload};
    use futures_util::{SinkExt, StreamExt};
    use protocol::rmp::{MsgType, RmpCodec, RmpMessage};
    use protocol::Frame;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio_util::codec::Framed;

    /// A mock RMP server that answers every request with `reply`, counting how many it saw.
    async fn spawn_counting_server(reply: Frame) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let server_seen = Arc::clone(&seen);
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut framed = Framed::new(socket, RmpCodec);
            while let Some(Ok(req)) = framed.next().await {
                server_seen.fetch_add(1, Ordering::SeqCst);
                let msg = RmpMessage {
                    request_id: req.request_id,
                    msg_type: MsgType::Response,
                    frame: reply.clone(),
                };
                if framed.send(msg).await.is_err() {
                    break;
                }
            }
        });
        (addr, seen)
    }

    #[tokio::test]
    async fn batch_driver_runs_every_operation_and_records_one_sample_per_round() {
        let (addr, seen) = spawn_counting_server(Frame::Simple("OK".to_string())).await;
        let mut driver = RmpDriver::connect(addr).await.unwrap();
        let workload = Workload::new(OpKind::Set, 8, 8);

        let samples = driver.run(&workload, 16, 8).await.unwrap();

        assert_eq!(seen.load(Ordering::SeqCst), 16, "every operation must reach the server");
        assert_eq!(samples.len(), 2, "16 ops at depth 8 is two rounds");
    }

    #[tokio::test]
    async fn batch_driver_enforces_the_reply_gate() {
        let (addr, _seen) = spawn_counting_server(Frame::Error("ERR nope".to_string())).await;
        let mut driver = RmpDriver::connect(addr).await.unwrap();
        let workload = Workload::new(OpKind::Set, 4, 8);

        let err = driver.run(&workload, 100, 4).await.unwrap_err();
        assert!(matches!(err, BenchError::UnexpectedReply { .. }));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem-bench rmp`
Expected: FAIL to compile — `RmpDriver` and `Driver` are not defined.

- [ ] **Step 3: Write the `Driver` trait**

Create `crates/bench/src/driver.rs`:

```rust
use crate::error::BenchError;
use crate::stats::Samples;
use crate::workload::Workload;
use std::future::Future;

/// Runs exactly `ops` operations from `workload` while holding `depth` requests in flight,
/// verifying every reply.
///
/// The unit of work is the whole cell rather than one round, because the sliding-window
/// driver has no rounds to expose.
///
/// Written as `-> impl Future + Send` rather than `async fn` on purpose: this trait is `pub`
/// (a `pub(crate)` trait in a lib target is dead code under CI's `-D warnings`), and `async fn`
/// in a public trait triggers the `async_fn_in_trait` lint, which `-D warnings` turns into a
/// build failure. The desugared form is equivalent and lint-free.
pub trait Driver {
    fn run(
        &mut self,
        workload: &Workload,
        ops: usize,
        depth: usize,
    ) -> impl Future<Output = Result<Samples, BenchError>> + Send;
}
```

Add to `crates/bench/src/lib.rs`:

```rust
pub mod driver;
pub mod rmp;
```

- [ ] **Step 4: Move `RespDriver::run` into a trait impl**

In `crates/bench/src/resp.rs`, add `use crate::driver::Driver;` and change the inherent `impl RespDriver` block so `connect` stays inherent while `run` moves:

```rust
impl RespDriver {
    pub async fn connect(addr: SocketAddr) -> Result<Self, BenchError> {
        let socket = TcpStream::connect(addr).await?;
        // RespCodec::default() is RESP2. No HELLO is sent, matching redis-benchmark.
        Ok(RespDriver {
            framed: Framed::new(socket, RespCodec::default()),
        })
    }
}

impl Driver for RespDriver {
    async fn run(
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

The body is byte-for-byte what plan 02 task 1 wrote; only its enclosing `impl` block changed. Add `use crate::driver::Driver;` to the `tests` module too, since the tests call `run`.

- [ ] **Step 5: Write `RmpDriver`**

Prepend to `crates/bench/src/rmp.rs`:

```rust
use crate::driver::Driver;
use crate::error::BenchError;
use crate::stats::Samples;
use crate::workload::Workload;
use bytes::Bytes;
use futures_util::future::join_all;
use protocol::Frame;
use rmp_client::{RmpClient, RmpError};
use std::net::SocketAddr;
use std::time::Instant;

/// Maps the client's error into the benchmark's, so a driver failure reads the same whichever
/// protocol produced it.
fn map_rmp_error(e: RmpError) -> BenchError {
    match e {
        RmpError::Io(io) => BenchError::Io(io),
        RmpError::ConnectionClosed => BenchError::ConnectionClosed,
        other => BenchError::Rmp(other.to_string()),
    }
}

/// Drives load over RMP using the same batch semantics as `RespDriver`: fire `depth` requests,
/// wait for all of them, repeat. Holding the issuing pattern constant is what leaves the
/// server's execution model as the only variable between the two.
pub struct RmpDriver {
    client: RmpClient,
}

impl RmpDriver {
    pub async fn connect(addr: SocketAddr) -> Result<Self, BenchError> {
        let client = RmpClient::connect(addr).await.map_err(map_rmp_error)?;
        Ok(RmpDriver { client })
    }
}

impl Driver for RmpDriver {
    async fn run(
        &mut self,
        workload: &Workload,
        ops: usize,
        depth: usize,
    ) -> Result<Samples, BenchError> {
        let mut samples = Samples::with_capacity(ops.div_ceil(depth.max(1)));
        let mut issued = 0usize;

        while issued < ops {
            let batch = depth.min(ops - issued);
            let requests: Vec<Vec<Bytes>> =
                (0..batch).map(|i| workload.request(issued + i)).collect();

            let started = Instant::now();
            // All `batch` calls go out before any is awaited, so they are genuinely in flight
            // together rather than issued one at a time.
            let replies = join_all(requests.into_iter().map(|args| self.client.call(args))).await;
            let elapsed = started.elapsed();

            for reply in replies {
                let frame: Frame = reply.map_err(map_rmp_error)?;
                workload.verify(&frame)?;
            }

            samples.push(elapsed);
            issued += batch;
        }

        Ok(samples)
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem-bench`
Expected: PASS — 6 RESP tests plus 2 RMP tests, and everything from plan 01.

- [ ] **Step 7: Run the full CI gate**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
Expected: all clean. If clippy reports `async_fn_in_trait`, the trait was written with `async fn` instead of `-> impl Future + Send`.

- [ ] **Step 8: Commit**

```bash
git add crates/bench/
git commit -m "$(cat <<'EOF'
test(bench): add the Driver trait and RmpDriver batch mode

Introduces the Driver trait now that there are two implementors to
abstract over, and moves RespDriver::run onto it unchanged.

The trait is pub with a desugared `-> impl Future + Send` return rather
than `async fn`. A pub(crate) trait in a lib target is dead code under
CI's -D warnings, while a pub trait using async fn trips
async_fn_in_trait, which the same flag turns into a build failure. The
desugared form sidesteps both without changing semantics.

RmpDriver deliberately copies RespDriver's batch semantics -- fire N,
await all N, repeat. Holding the client's issuing pattern constant is
what leaves the server's execution model as the only variable between
the two protocols.
EOF
)"
```

---

### Task 2: `RmpWindowDriver` (sliding window)

**Files:**
- Modify: `crates/bench/src/rmp.rs`

**Interfaces:**
- Consumes: `Driver`, `BenchError`, `Workload`, `Samples`, `map_rmp_error`.
- Produces: `pub struct RmpWindowDriver` with `RmpWindowDriver::connect(addr: std::net::SocketAddr) -> Result<RmpWindowDriver, BenchError>` (async) and `impl Driver for RmpWindowDriver`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/bench/src/rmp.rs`:

```rust
#[tokio::test]
async fn window_driver_runs_every_operation_and_samples_per_operation() {
    let (addr, seen) = spawn_counting_server(Frame::Simple("OK".to_string())).await;
    let mut driver = RmpWindowDriver::connect(addr).await.unwrap();
    let workload = Workload::new(OpKind::Set, 8, 8);

    let samples = driver.run(&workload, 16, 8).await.unwrap();

    assert_eq!(seen.load(Ordering::SeqCst), 16);
    // The window driver times each call independently, so one sample per operation.
    assert_eq!(samples.len(), 16);
}

#[tokio::test]
async fn window_driver_enforces_the_reply_gate() {
    let (addr, _seen) = spawn_counting_server(Frame::Error("ERR nope".to_string())).await;
    let mut driver = RmpWindowDriver::connect(addr).await.unwrap();
    let workload = Workload::new(OpKind::Get, 4, 8);

    let err = driver.run(&workload, 100, 4).await.unwrap_err();
    assert!(matches!(err, BenchError::UnexpectedReply { .. }));
}

#[tokio::test]
async fn at_depth_one_both_rmp_drivers_behave_identically() {
    // With no batch and no window, the two modes are the same thing. A divergence here is a
    // harness bug, and the spec relies on this as a built-in sanity check.
    let (addr_a, seen_a) = spawn_counting_server(Frame::Simple("OK".to_string())).await;
    let mut batch = RmpDriver::connect(addr_a).await.unwrap();
    let (addr_b, seen_b) = spawn_counting_server(Frame::Simple("OK".to_string())).await;
    let mut window = RmpWindowDriver::connect(addr_b).await.unwrap();
    let workload = Workload::new(OpKind::Set, 4, 8);

    let a = batch.run(&workload, 12, 1).await.unwrap();
    let b = window.run(&workload, 12, 1).await.unwrap();

    assert_eq!(a.len(), 12);
    assert_eq!(b.len(), 12);
    assert_eq!(seen_a.load(Ordering::SeqCst), seen_b.load(Ordering::SeqCst));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem-bench rmp`
Expected: FAIL to compile — `RmpWindowDriver` is not defined.

- [ ] **Step 3: Write the minimal implementation**

Add to the imports at the top of `crates/bench/src/rmp.rs`:

```rust
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::time::Duration;
```

Then append:

```rust
/// Drives load over RMP with a sliding window: `depth` requests stay outstanding at all times,
/// and a replacement is issued the moment any reply lands. Unlike the batch drivers there is no
/// barrier at the end of a round, so the whole window never waits on its slowest member.
///
/// This is RMP at its best, and it is deliberately not client-pattern-matched against
/// `RespDriver` -- RESP cannot express this, since its replies must be read in order.
pub struct RmpWindowDriver {
    client: RmpClient,
}

impl RmpWindowDriver {
    pub async fn connect(addr: SocketAddr) -> Result<Self, BenchError> {
        let client = RmpClient::connect(addr).await.map_err(map_rmp_error)?;
        Ok(RmpWindowDriver { client })
    }
}

/// One timed call. Returns the reply alongside how long that single operation took, which the
/// batch drivers cannot measure but this one can.
async fn timed_call(
    client: &RmpClient,
    args: Vec<Bytes>,
) -> (Result<Frame, RmpError>, Duration) {
    let started = Instant::now();
    let reply = client.call(args).await;
    (reply, started.elapsed())
}

impl Driver for RmpWindowDriver {
    async fn run(
        &mut self,
        workload: &Workload,
        ops: usize,
        depth: usize,
    ) -> Result<Samples, BenchError> {
        let client = &self.client;
        let mut samples = Samples::with_capacity(ops);
        let mut in_flight = FuturesUnordered::new();
        let mut issued = 0usize;

        // Prime the window, capped at `ops` so a short run never over-issues.
        while issued < ops && in_flight.len() < depth.max(1) {
            in_flight.push(timed_call(client, workload.request(issued)));
            issued += 1;
        }

        while let Some((reply, elapsed)) = in_flight.next().await {
            let frame = reply.map_err(map_rmp_error)?;
            workload.verify(&frame)?;
            samples.push(elapsed);

            // Refill immediately, so the window stays full instead of draining to empty and
            // refilling in a batch -- that would just be the batch driver under another name.
            if issued < ops {
                in_flight.push(timed_call(client, workload.request(issued)));
                issued += 1;
            }
        }

        Ok(samples)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem-bench rmp`
Expected: PASS, 5 tests.

If borrow-checker errors appear on `self.client`, ensure `let client = &self.client;` is bound once at the top and used everywhere, rather than writing `self.client` inside the loop.

- [ ] **Step 5: Run the full CI gate**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
Expected: all clean.

- [ ] **Step 6: Commit**

```bash
git add crates/bench/
git commit -m "$(cat <<'EOF'
test(bench): add RmpWindowDriver holding a sliding window

Keeps `depth` requests outstanding at all times using FuturesUnordered,
issuing a replacement the moment any reply lands. There is no barrier at
the end of a round, so the window never waits on its slowest member the
way the batch drivers must.

This is the mode that measures RMP at its best, and it is deliberately
not client-pattern-matched against RespDriver: RESP cannot express a
sliding window at all, because its replies must be read in order.

It also samples per operation rather than per round, which it genuinely
can do since every call future completes independently. The batch
drivers cannot, which is why the report keeps the two latency columns
apart.
EOF
)"
```

---

### Task 3: Prove both RMP drivers behave as claimed

**Files:**
- Modify: `crates/bench/src/rmp.rs` (tests only)

**Interfaces:**
- Consumes: all three drivers.
- Produces: no new public API.

**Why this task exists:** three separate ways the RMP side could measure nothing while still producing plausible numbers.

1. If `RmpDriver` issued its batch one call at a time, "depth N" would mean one in flight, and the RESP-vs-RMP comparison would be round-trip latency on both sides.
2. If `RmpWindowDriver` drained to empty between refills, it would behaviourally *be* the batch driver — the two RMP modes would report identical numbers for a reason having nothing to do with the server, silently voiding the spec's middle comparison.
3. If the drivers issued different operation counts, throughput would not be comparable across modes at all.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/bench/src/rmp.rs`:

```rust
/// A mock server that withholds every reply until `expect` requests have arrived, then answers
/// them in reverse order. A driver that issues its batch one call at a time deadlocks here.
/// Replying backwards also proves the client correlates by request id rather than by arrival
/// order -- if it assumed order, the correctness gate would see mismatched replies.
async fn spawn_multiplex_asserting_server(expect: usize, reply: Frame) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(socket, RmpCodec);
        loop {
            let mut ids = Vec::new();
            while ids.len() < expect {
                match framed.next().await {
                    Some(Ok(req)) => ids.push(req.request_id),
                    _ => return,
                }
            }
            ids.reverse();
            for id in ids {
                let msg = RmpMessage {
                    request_id: id,
                    msg_type: MsgType::Response,
                    frame: reply.clone(),
                };
                if framed.send(msg).await.is_err() {
                    return;
                }
            }
        }
    });
    addr
}

#[tokio::test]
async fn batch_driver_puts_the_whole_batch_in_flight_before_awaiting_any_of_it() {
    let addr = spawn_multiplex_asserting_server(8, Frame::Simple("OK".to_string())).await;
    let mut driver = RmpDriver::connect(addr).await.unwrap();
    let workload = Workload::new(OpKind::Set, 8, 8);

    // Bounded: a driver issuing one call at a time deadlocks against this server, and that
    // must fail the test rather than hang the suite.
    let samples = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        driver.run(&workload, 16, 8),
    )
    .await
    .expect("depth 8 did not mean 8 in flight -- the driver awaited each call before the next")
    .unwrap();

    assert_eq!(samples.len(), 2);
}

/// A mock server that primes with `depth` requests, answers exactly one, and records whether a
/// replacement arrives promptly. A sliding window refills the instant a reply lands; a batch
/// driver sends nothing until all `depth` replies are in, so the probe times out and the flag
/// stays false.
///
/// After the probe it drains normally, so the driver always finishes rather than hanging --
/// the assertion is on the recorded flag, not on a timeout.
async fn spawn_refill_probe_server(
    depth: usize,
    reply: Frame,
) -> (std::net::SocketAddr, Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::AtomicBool;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let refilled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&refilled);
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(socket, RmpCodec);
        let mut pending: Vec<u64> = Vec::new();

        // Prime the window.
        while pending.len() < depth {
            match framed.next().await {
                Some(Ok(req)) => pending.push(req.request_id),
                _ => return,
            }
        }

        // Answer exactly one, then watch for an immediate replacement.
        if let Some(id) = pending.pop() {
            let msg = RmpMessage {
                request_id: id,
                msg_type: MsgType::Response,
                frame: reply.clone(),
            };
            if framed.send(msg).await.is_err() {
                return;
            }
        }
        if let Ok(Some(Ok(req))) =
            tokio::time::timeout(std::time::Duration::from_secs(2), framed.next()).await
        {
            flag.store(true, Ordering::SeqCst);
            pending.push(req.request_id);
        }

        // Drain normally from here so the run always completes.
        loop {
            while let Some(id) = pending.pop() {
                let msg = RmpMessage {
                    request_id: id,
                    msg_type: MsgType::Response,
                    frame: reply.clone(),
                };
                if framed.send(msg).await.is_err() {
                    return;
                }
            }
            match framed.next().await {
                Some(Ok(req)) => pending.push(req.request_id),
                _ => return, // The client finished and dropped the connection.
            }
        }
    });
    (addr, refilled)
}

#[tokio::test]
async fn the_window_refills_on_each_completion_rather_than_draining_first() {
    let (addr, refilled) = spawn_refill_probe_server(8, Frame::Simple("OK".to_string())).await;
    let mut driver = RmpWindowDriver::connect(addr).await.unwrap();
    let workload = Workload::new(OpKind::Set, 8, 8);

    let samples = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        driver.run(&workload, 40, 8),
    )
    .await
    .expect("the run hung")
    .unwrap();

    assert_eq!(samples.len(), 40);
    assert!(
        refilled.load(Ordering::SeqCst),
        "the window drained instead of refilling on each completion"
    );
}

#[tokio::test]
async fn every_driver_issues_exactly_ops_requests() {
    // The counts must match across modes or the throughput comparison means nothing. A window
    // driver is easy to get wrong at the tail, over- or under-issuing as it drains.
    let workload = Workload::new(OpKind::Set, 7, 8);
    let ops = 25; // Deliberately not a multiple of the depth below.
    let depth = 4;

    let (addr_batch, seen_batch) = spawn_counting_server(Frame::Simple("OK".to_string())).await;
    let mut batch = RmpDriver::connect(addr_batch).await.unwrap();
    batch.run(&workload, ops, depth).await.unwrap();

    let (addr_window, seen_window) = spawn_counting_server(Frame::Simple("OK".to_string())).await;
    let mut window = RmpWindowDriver::connect(addr_window).await.unwrap();
    window.run(&workload, ops, depth).await.unwrap();

    assert_eq!(seen_batch.load(Ordering::SeqCst), ops, "batch driver issued the wrong count");
    assert_eq!(seen_window.load(Ordering::SeqCst), ops, "window driver issued the wrong count");
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p rocket-mem-bench rmp`
Expected: PASS, 8 tests. Tasks 1 and 2 already issue whole batches and refill inside the completion loop, so these lock the behaviour in rather than driving new code.

Note the module now needs `use std::sync::atomic::AtomicBool;` reachable — it is imported inside `spawn_refill_probe_server` rather than at module scope, so no other test is affected.

- [ ] **Step 3: Verify the window test actually detects the bug**

Temporarily rewrite `RmpWindowDriver::run`'s loop to drain before refilling:

```rust
// TEMPORARY -- revert after this step.
while issued < ops || !in_flight.is_empty() {
    while let Some((reply, elapsed)) = in_flight.next().await {
        let frame = reply.map_err(map_rmp_error)?;
        workload.verify(&frame)?;
        samples.push(elapsed);
    }
    while issued < ops && in_flight.len() < depth.max(1) {
        in_flight.push(timed_call(client, workload.request(issued)));
        issued += 1;
    }
}
```

Run: `cargo test -p rocket-mem-bench the_window_refills`
Expected: FAIL with "the window drained instead of refilling on each completion". The run itself still completes — the probe server drains after its measurement — so this is a clean assertion failure, not a timeout.

Revert to Task 2's implementation and re-run. Expected: PASS.

Then verify the multiplexing test the same way. Temporarily rewrite `RmpDriver::run`'s batch to await each call as it is issued:

```rust
// TEMPORARY -- revert after this step.
let started = Instant::now();
let mut replies = Vec::with_capacity(batch);
for args in requests {
    replies.push(self.client.call(args).await);
}
let elapsed = started.elapsed();
```

Run: `cargo test -p rocket-mem-bench batch_driver_puts_the_whole_batch`
Expected: FAIL with "depth 8 did not mean 8 in flight".

Revert to Task 1's `join_all` implementation and re-run. Expected: PASS.

- [ ] **Step 4: Run the full CI gate**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
Expected: all clean.

- [ ] **Step 5: Commit**

```bash
git add crates/bench/
git commit -m "$(cat <<'EOF'
test(bench): assert both RMP drivers behave as claimed

Three ways the RMP side could measure nothing while still producing
plausible numbers, each now pinned by a test.

A batch driver issuing one call at a time would make depth N mean one in
flight, reducing the whole comparison to round-trip latency. A mock
server that withholds replies until a full batch arrives deadlocks such
a driver, and answering in reverse order also proves the client
correlates by request id rather than arrival order.

A window that drains to empty between refills is behaviourally the batch
driver, which would have the two RMP modes report identical numbers for
a reason unrelated to the server and silently void the middle
comparison. The probe server answers exactly one request and records
whether a replacement arrives, then drains normally so the assertion
lands on the recorded flag rather than on a timeout.

Op counts are pinned across both modes with a count that is not a
multiple of the depth, since a window driver is easy to get wrong at the
tail as it drains.
EOF
)"
```

---

## Definition of Done

- [ ] `Driver`, `RmpDriver`, and `RmpWindowDriver` exist with the interfaces above; `RespDriver` implements `Driver`.
- [ ] 8 tests pass in `crates/bench/src/rmp.rs`; the RESP tests still pass after the trait refactor.
- [ ] The window-refill test was manually verified to fail against a draining implementation.
- [ ] The multiplexing test was manually verified to fail against a serially-awaiting implementation.
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` are all clean.
- [ ] Three commits, one per task.
