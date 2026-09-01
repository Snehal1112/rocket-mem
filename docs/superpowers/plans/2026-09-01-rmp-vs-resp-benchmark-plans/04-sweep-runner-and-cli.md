# Sweep Runner and CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the three drivers into a runnable `rocket-mem-bench` binary that seeds the keyspace, runs all 54 cells sequentially, and prints a table.

**Architecture:** A `runner` module owning one cell's lifecycle (fresh connection, warmup, measured run) and a `main.rs` that enumerates the sweep and formats results. Cells run strictly one at a time — concurrent cells would contend for the same cores and shards.

**Tech Stack:** Rust 2021, `tokio` (`rt-multi-thread`, `macros`), `clap` (derive), plus the plan 01–03 modules.

**Spec:** [`../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md`](../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md)

**Depends on:** [`01-crate-foundation.md`](01-crate-foundation.md), [`02-resp-batch-driver.md`](02-resp-batch-driver.md), [`03-rmp-drivers.md`](03-rmp-drivers.md).

## Global Constraints

- **Sequential cells, never concurrent.** Running two cells at once would have them contend for the same cores and shards.
- **Fixed sweep defaults:** depths `1,2,4,8,16,32,64,128,256`; 1,000 keys; 64-byte values; 10,000 warmup ops; 200,000 measured ops. Depth stops at 256 because that is the server's `MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION`.
- **A fresh connection per cell**, so one cell's socket state never carries into the next.
- **CI gates:** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- **Comment style:** short, plain full sentences ending in a punctuation mark. No emojis.

---

### Task 1: Seed the keyspace

**Files:**
- Create: `crates/bench/src/seed.rs`
- Modify: `crates/bench/src/lib.rs`

**Interfaces:**
- Consumes: `BenchError`, `Workload`.
- Produces: `pub async fn seed(addr: std::net::SocketAddr, workload: &Workload) -> Result<(), BenchError>` — pipelines one `SET` per key over RESP and verifies every reply is `+OK`.

**Why RESP and not RMP:** either would work, since both write the same keyspace through the same dispatcher. RESP is chosen because a seeding failure then surfaces on the protocol the existing `docs/benchmarks/` figures already use, making it easier to reproduce with `redis-cli`.

- [ ] **Step 1: Write the failing test**

Create `crates/bench/src/seed.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::{OpKind, Workload};
    use futures_util::{SinkExt, StreamExt};
    use protocol::codec::RespCodec;
    use protocol::Frame;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio_util::codec::Framed;

    async fn spawn_server(reply: Frame) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let server_seen = Arc::clone(&seen);
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut framed = Framed::new(socket, RespCodec::default());
            while let Some(Ok(_)) = framed.next().await {
                server_seen.fetch_add(1, Ordering::SeqCst);
                if framed.send(reply.clone()).await.is_err() {
                    break;
                }
            }
        });
        (addr, seen)
    }

    #[tokio::test]
    async fn seed_writes_every_key_once() {
        let (addr, seen) = spawn_server(Frame::Simple("OK".to_string())).await;
        // A GET workload still seeds with SETs, so its reads are hits.
        let workload = Workload::new(OpKind::Get, 25, 64);

        seed(addr, &workload).await.unwrap();

        assert_eq!(seen.load(Ordering::SeqCst), 25);
    }

    #[tokio::test]
    async fn seed_fails_loudly_when_a_write_is_rejected() {
        let (addr, _seen) = spawn_server(Frame::Error("ERR no space".to_string())).await;
        let workload = Workload::new(OpKind::Get, 10, 64);

        let err = seed(addr, &workload).await.unwrap_err();
        assert!(matches!(err, BenchError::UnexpectedReply { .. }));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem-bench seed`
Expected: FAIL to compile — `seed` is not defined.

- [ ] **Step 3: Write the minimal implementation**

Add `pub mod seed;` to `crates/bench/src/lib.rs`, then prepend to `crates/bench/src/seed.rs`:

```rust
use crate::error::BenchError;
use crate::workload::Workload;
use futures_util::{SinkExt, StreamExt};
use protocol::codec::RespCodec;
use protocol::Frame;
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

/// Writes every key in the workload once, so a GET cell measures hits rather than the cheaper
/// miss path. Runs over RESP because a failure here is then reproducible with redis-cli.
///
/// The whole keyspace is pipelined in one batch. Seeding is setup, not measurement, so its
/// throughput does not matter -- only that it finishes and that every write was accepted.
pub async fn seed(addr: SocketAddr, workload: &Workload) -> Result<(), BenchError> {
    let socket = TcpStream::connect(addr).await?;
    let mut framed = Framed::new(socket, RespCodec::default());

    let requests = workload.seed_requests();
    let expected = requests.len();

    for args in requests {
        let frame = Frame::Array(args.into_iter().map(Frame::Bulk).collect());
        framed.feed(frame).await?;
    }
    framed.flush().await?;

    for _ in 0..expected {
        let reply = match framed.next().await {
            Some(Ok(frame)) => frame,
            Some(Err(e)) => return Err(BenchError::Io(e)),
            None => return Err(BenchError::ConnectionClosed),
        };
        match reply {
            Frame::Simple(ref s) if s == "OK" => {}
            other => {
                return Err(BenchError::UnexpectedReply {
                    expected: "+OK".to_string(),
                    got: other,
                })
            }
        }
    }

    Ok(())
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem-bench seed`
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
test(bench): add keyspace seeding over RESP

Writes every key in the workload once before any cell runs, so GET cells
measure hits. A miss returns Null without touching a value and is a
cheaper, different path.

Seeding runs over RESP rather than RMP so that a failure is reproducible
with redis-cli against the same port the existing docs/benchmarks
figures used. Both protocols write the same keyspace through the same
dispatcher, so the choice does not affect what is measured.

Every reply is checked. A silently rejected seed would leave GET cells
measuring misses for the rest of the sweep.
EOF
)"
```

---

### Task 2: The cell runner

**Files:**
- Create: `crates/bench/src/runner.rs`
- Modify: `crates/bench/src/lib.rs`

**Interfaces:**
- Consumes: all three drivers, `Workload`, `Samples`, `BenchError`.
- Produces:
  - `pub enum DriverKind { RespBatch, RmpBatch, RmpWindow }` with `DriverKind::label(&self) -> &'static str` and `DriverKind::ALL: [DriverKind; 3]`.
  - `pub struct CellResult { pub driver: DriverKind, pub command: OpKind, pub depth: usize, pub ops: usize, pub elapsed: std::time::Duration, pub p50: std::time::Duration, pub p99: std::time::Duration }` with `CellResult::ops_per_sec(&self) -> f64`.
  - `pub async fn run_cell(kind: DriverKind, resp_addr: SocketAddr, rmp_addr: SocketAddr, workload: &Workload, warmup: usize, ops: usize, depth: usize) -> Result<CellResult, BenchError>`.

- [ ] **Step 1: Write the failing test**

Create `crates/bench/src/runner.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::{OpKind, Workload};
    use futures_util::{SinkExt, StreamExt};
    use protocol::codec::RespCodec;
    use protocol::rmp::{MsgType, RmpCodec, RmpMessage};
    use protocol::Frame;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio_util::codec::Framed;

    async fn spawn_resp_server() -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let server_seen = Arc::clone(&seen);
        tokio::spawn(async move {
            // One connection per cell, so keep accepting.
            while let Ok((socket, _)) = listener.accept().await {
                let conn_seen = Arc::clone(&server_seen);
                tokio::spawn(async move {
                    let mut framed = Framed::new(socket, RespCodec::default());
                    while let Some(Ok(_)) = framed.next().await {
                        conn_seen.fetch_add(1, Ordering::SeqCst);
                        if framed.send(Frame::Simple("OK".to_string())).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        (addr, seen)
    }

    async fn spawn_rmp_server() -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let server_seen = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let conn_seen = Arc::clone(&server_seen);
                tokio::spawn(async move {
                    let mut framed = Framed::new(socket, RmpCodec);
                    while let Some(Ok(req)) = framed.next().await {
                        conn_seen.fetch_add(1, Ordering::SeqCst);
                        let msg = RmpMessage {
                            request_id: req.request_id,
                            msg_type: MsgType::Response,
                            frame: Frame::Simple("OK".to_string()),
                        };
                        if framed.send(msg).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        (addr, seen)
    }

    #[tokio::test]
    async fn a_cell_runs_warmup_plus_measured_ops_and_reports_only_the_measured_ones() {
        let (resp_addr, resp_seen) = spawn_resp_server().await;
        let (rmp_addr, _rmp_seen) = spawn_rmp_server().await;
        let workload = Workload::new(OpKind::Set, 8, 8);

        let cell = run_cell(DriverKind::RespBatch, resp_addr, rmp_addr, &workload, 10, 40, 4)
            .await
            .unwrap();

        // Warmup traffic reaches the server but must not be counted.
        assert_eq!(resp_seen.load(Ordering::SeqCst), 50);
        assert_eq!(cell.ops, 40);
        assert_eq!(cell.depth, 4);
        assert!(cell.ops_per_sec() > 0.0);
    }

    #[tokio::test]
    async fn each_driver_kind_routes_to_its_own_port() {
        let (resp_addr, resp_seen) = spawn_resp_server().await;
        let (rmp_addr, rmp_seen) = spawn_rmp_server().await;
        let workload = Workload::new(OpKind::Set, 4, 8);

        run_cell(DriverKind::RmpBatch, resp_addr, rmp_addr, &workload, 0, 8, 2)
            .await
            .unwrap();
        run_cell(DriverKind::RmpWindow, resp_addr, rmp_addr, &workload, 0, 8, 2)
            .await
            .unwrap();

        // Both RMP kinds must use the RMP port and leave the RESP port untouched.
        assert_eq!(rmp_seen.load(Ordering::SeqCst), 16);
        assert_eq!(resp_seen.load(Ordering::SeqCst), 0);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem-bench runner`
Expected: FAIL to compile — `run_cell`, `DriverKind`, and `CellResult` are not defined.

- [ ] **Step 3: Write the minimal implementation**

Add `pub mod runner;` to `crates/bench/src/lib.rs`, then prepend to `crates/bench/src/runner.rs`:

```rust
use crate::driver::Driver;
use crate::error::BenchError;
use crate::resp::RespDriver;
use crate::rmp::{RmpDriver, RmpWindowDriver};
use crate::stats::Samples;
use crate::workload::{OpKind, Workload};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverKind {
    RespBatch,
    RmpBatch,
    RmpWindow,
}

impl DriverKind {
    pub const ALL: [DriverKind; 3] = [
        DriverKind::RespBatch,
        DriverKind::RmpBatch,
        DriverKind::RmpWindow,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            DriverKind::RespBatch => "RESP-batch",
            DriverKind::RmpBatch => "RMP-batch",
            DriverKind::RmpWindow => "RMP-window",
        }
    }

    /// What one latency sample means for this driver. The batch drivers time a whole round;
    /// the window driver times a single operation. The two are not comparable, so the report
    /// must keep them in separate columns.
    pub fn latency_unit(&self) -> &'static str {
        match self {
            DriverKind::RespBatch | DriverKind::RmpBatch => "round",
            DriverKind::RmpWindow => "op",
        }
    }
}

pub struct CellResult {
    pub driver: DriverKind,
    pub command: OpKind,
    pub depth: usize,
    pub ops: usize,
    pub elapsed: Duration,
    pub p50: Duration,
    pub p99: Duration,
}

impl CellResult {
    pub fn ops_per_sec(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs <= 0.0 {
            return 0.0;
        }
        self.ops as f64 / secs
    }
}

/// Runs one cell end to end on a fresh connection: warm up, discard those samples, then time
/// the measured run. A fresh connection per cell keeps one cell's socket state out of the next.
pub async fn run_cell(
    kind: DriverKind,
    resp_addr: SocketAddr,
    rmp_addr: SocketAddr,
    workload: &Workload,
    warmup: usize,
    ops: usize,
    depth: usize,
) -> Result<CellResult, BenchError> {
    // Each arm is spelled out rather than boxed, because `Driver` uses RPITIT and so is not
    // object-safe. Three arms is a small price for keeping the trait lint-free.
    let (elapsed, mut samples) = match kind {
        DriverKind::RespBatch => {
            let mut d = RespDriver::connect(resp_addr).await?;
            measure(&mut d, workload, warmup, ops, depth).await?
        }
        DriverKind::RmpBatch => {
            let mut d = RmpDriver::connect(rmp_addr).await?;
            measure(&mut d, workload, warmup, ops, depth).await?
        }
        DriverKind::RmpWindow => {
            let mut d = RmpWindowDriver::connect(rmp_addr).await?;
            measure(&mut d, workload, warmup, ops, depth).await?
        }
    };

    Ok(CellResult {
        driver: kind,
        command: workload.kind(),
        depth,
        ops,
        elapsed,
        p50: samples.percentile(50.0),
        p99: samples.percentile(99.0),
    })
}

/// Warms up, throws those samples away, then times the measured run.
async fn measure<D: Driver>(
    driver: &mut D,
    workload: &Workload,
    warmup: usize,
    ops: usize,
    depth: usize,
) -> Result<(Duration, Samples), BenchError> {
    if warmup > 0 {
        let _ = driver.run(workload, warmup, depth).await?;
    }
    let started = Instant::now();
    let samples = driver.run(workload, ops, depth).await?;
    Ok((started.elapsed(), samples))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem-bench runner`
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
test(bench): add the per-cell runner

run_cell owns one cell's whole lifecycle: fresh connection, warmup whose
samples are discarded, then the timed measured run. A fresh connection
per cell keeps one cell's socket state from carrying into the next.

DriverKind records which latency unit each driver produces -- a round for
the batch drivers, an operation for the window driver -- so the reporting
layer cannot accidentally put the two in one column, where a round would
look N times worse for a purely definitional reason.

The match arms are spelled out rather than boxed because Driver uses
RPITIT and is therefore not object-safe. Three arms is cheaper than
making the trait dyn-compatible.
EOF
)"
```

---

### Task 3: CLI and table output

**Files:**
- Create: `crates/bench/src/main.rs`
- Modify: `crates/bench/Cargo.toml` (declare the binary explicitly)

**Interfaces:**
- Consumes: `seed`, `run_cell`, `DriverKind`, `CellResult`, `Workload`, `OpKind`.
- Produces: the `rocket-mem-bench` binary.

- [ ] **Step 1: Declare the binary**

Append to `crates/bench/Cargo.toml`:

```toml
[lib]
name = "rocket_mem_bench"
path = "src/lib.rs"

[[bin]]
name = "rocket-mem-bench"
path = "src/main.rs"
```

- [ ] **Step 2: Write `main.rs`**

Create `crates/bench/src/main.rs`:

```rust
use clap::Parser;
use rocket_mem_bench::runner::{run_cell, CellResult, DriverKind};
use rocket_mem_bench::seed::seed;
use rocket_mem_bench::workload::{OpKind, Workload};
use std::net::SocketAddr;

/// Sweeps in-flight depth for RESP pipelining against RMP multiplexing on one connection.
#[derive(Parser, Debug)]
#[command(about, long_about = None)]
struct Args {
    /// The server's RESP port.
    #[arg(long, default_value = "127.0.0.1:6379")]
    resp_addr: SocketAddr,

    /// The server's RMP port.
    #[arg(long, default_value = "127.0.0.1:6380")]
    rmp_addr: SocketAddr,

    /// In-flight depths to sweep. Stops at 256, the server's per-connection cap.
    #[arg(long, value_delimiter = ',', default_value = "1,2,4,8,16,32,64,128,256")]
    depths: Vec<usize>,

    /// Measured operations per cell.
    #[arg(long, default_value_t = 200_000)]
    ops: usize,

    /// Warmup operations per cell, discarded before timing starts.
    #[arg(long, default_value_t = 10_000)]
    warmup: usize,

    /// Distinct keys. Spread matters: one key would serialise every request on one shard.
    #[arg(long, default_value_t = 1_000)]
    keys: usize,

    /// Value size in bytes.
    #[arg(long, default_value_t = 64)]
    value_len: usize,
}

fn print_row(cell: &CellResult) {
    println!(
        "{:<12} {:<4} {:>5} {:>14.0} {:>12.3} {:>12.3}  {}",
        cell.driver.label(),
        cell.command.as_str(),
        cell.depth,
        cell.ops_per_sec(),
        cell.p50.as_secs_f64() * 1000.0,
        cell.p99.as_secs_f64() * 1000.0,
        cell.driver.latency_unit(),
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Seed once. The keyspace is fixed and SET only overwrites, so it never grows -- which
    // matters because rocket-mem has no FLUSHALL to reset between cells.
    let seed_workload = Workload::new(OpKind::Set, args.keys, args.value_len);
    seed(args.resp_addr, &seed_workload).await?;
    println!("seeded {} keys of {} bytes\n", args.keys, args.value_len);

    println!(
        "{:<12} {:<4} {:>5} {:>14} {:>12} {:>12}  {}",
        "driver", "cmd", "depth", "ops/sec", "p50 (ms)", "p99 (ms)", "latency unit"
    );

    // Strictly sequential. Two cells at once would contend for the same cores and shards.
    for command in [OpKind::Get, OpKind::Set] {
        let workload = Workload::new(command, args.keys, args.value_len);
        for kind in DriverKind::ALL {
            for depth in &args.depths {
                let cell = run_cell(
                    kind,
                    args.resp_addr,
                    args.rmp_addr,
                    &workload,
                    args.warmup,
                    args.ops,
                    *depth,
                )
                .await?;
                print_row(&cell);
            }
        }
    }

    Ok(())
}
```

- [ ] **Step 3: Build and check the CLI**

```bash
cargo build --workspace
cargo run -p rocket-mem-bench -- --help
```
Expected: the help text lists every flag above with its default.

- [ ] **Step 4: Smoke-test against a real server**

```bash
cargo build --release --workspace
WORK=$(mktemp -d)
ROCKET_MEM_AOF_PATH="$WORK/a.aof" ROCKET_MEM_SNAPSHOT_PATH="$WORK/a.snap" \
  ./target/release/rocket-mem &
SERVER=$!
sleep 1
./target/release/rocket-mem-bench --ops 2000 --warmup 100 --keys 100 --depths 1,8
kill $SERVER; rm -rf "$WORK"
```
Expected: a seeding line, a header, and 12 rows (2 commands × 3 drivers × 2 depths), every `ops/sec` greater than zero. Any `UnexpectedReply` error means the correctness gate caught a real mismatch — investigate rather than working around it.

- [ ] **Step 5: Run the full CI gate**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
Expected: all clean.

- [ ] **Step 6: Commit**

```bash
git add crates/bench/
git commit -m "$(cat <<'EOF'
feat(bench): add the sweep CLI

Seeds the keyspace once, then walks every cell strictly sequentially --
two cells at once would contend for the same cores and shards and
measure neither.

Seeding happens once rather than per cell because rocket-mem has no
FLUSHALL to reset with. That is harmless here: the key count is fixed and
SET only overwrites, so the keyspace never grows across a run.

Each row carries the latency unit it was measured in, so a round latency
from a batch driver is never silently read alongside a per-op latency
from the window driver.
EOF
)"
```

---

## Definition of Done

- [ ] `seed`, `run_cell`, `DriverKind`, and `CellResult` exist with the interfaces above.
- [ ] `cargo run -p rocket-mem-bench -- --help` prints every flag.
- [ ] The smoke test against a real `rocket-mem` produces 12 rows with non-zero throughput.
- [ ] 4 new tests pass (2 in `seed.rs`, 2 in `runner.rs`).
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` are all clean.
- [ ] Three commits, one per task.
