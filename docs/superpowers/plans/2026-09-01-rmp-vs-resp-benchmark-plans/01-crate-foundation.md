# Crate Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up `crates/bench` as a workspace member with the three foundation types every driver and the runner depend on: `BenchError`, `Workload`, and `Samples`.

**Architecture:** A new lib + bin crate. The lib holds all logic (so `pub` items are never dead code under CI's `-D warnings`); the bin is a thin wrapper added in plan 04. No existing crate is modified except the root `Cargo.toml` members list.

**Tech Stack:** Rust 2021, `bytes`, `protocol` (for `Frame`), `tokio`, `clap`. All already in `workspace.dependencies`.

**Spec:** [`../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md`](../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md)

## Global Constraints

- **No changes to engine, dispatcher, protocol, or server code.** The only edit outside `crates/bench` is adding it to the root `Cargo.toml`'s `workspace.members`.
- **CI gates, all three must pass before every commit:** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Clippy is strict and lints test code too.
- **No new entry in `workspace.dependencies`.** `crates/bench` may only use crates already listed there.
- **Keyspace is fixed:** 1,000 keys named `bench:key:{0..999}`, 64-byte values.
- **Comment style:** short, plain full sentences ending in a punctuation mark. No emojis.

---

### Task 1: Crate scaffold and `BenchError`

**Files:**
- Create: `crates/bench/Cargo.toml`
- Create: `crates/bench/src/lib.rs`
- Create: `crates/bench/src/error.rs`
- Modify: `Cargo.toml` (root, `workspace.members`)

**Interfaces:**
- Consumes: nothing.
- Produces: `bench::error::BenchError` with variants `Io(std::io::Error)`, `ConnectionClosed`, `UnexpectedReply { expected: String, got: protocol::Frame }`, `Rmp(String)`; implements `Display`, `std::error::Error`, and `From<std::io::Error>`.

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, add `"crates/bench"` to `workspace.members`:

```toml
members = ["crates/bench", "crates/common", "crates/engine", "crates/protocol", "crates/rmp-client", "crates/server"]
```

- [ ] **Step 2: Write `crates/bench/Cargo.toml`**

```toml
[package]
name = "rocket-mem-bench"
edition.workspace = true
version.workspace = true
publish = false

[dependencies]
protocol = { path = "../protocol" }
rmp-client = { path = "../rmp-client" }
bytes.workspace = true
tokio.workspace = true
tokio-util.workspace = true
futures-util.workspace = true
clap.workspace = true
```

`publish = false` because this is a development tool, not part of the release artifact.

- [ ] **Step 3: Write the failing test**

Create `crates/bench/src/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Frame;

    #[test]
    fn unexpected_reply_names_both_what_was_wanted_and_what_arrived() {
        let err = BenchError::UnexpectedReply {
            expected: "+OK".to_string(),
            got: Frame::Integer(3),
        };
        let msg = err.to_string();
        assert!(msg.contains("+OK"), "message should name the expectation: {msg}");
        assert!(msg.contains("Integer(3)"), "message should show the reply: {msg}");
    }

    #[test]
    fn an_io_error_converts_into_a_bench_error() {
        let io = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone");
        let err: BenchError = io.into();
        assert!(matches!(err, BenchError::Io(_)));
    }
}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test -p rocket-mem-bench`
Expected: FAIL to compile — `BenchError` is not defined, and `lib.rs` does not exist yet.

- [ ] **Step 5: Write the minimal implementation**

Create `crates/bench/src/lib.rs`:

```rust
pub mod error;
```

Prepend to `crates/bench/src/error.rs`:

```rust
use protocol::Frame;

/// Everything that can go wrong while driving load at the server. `UnexpectedReply` is the
/// important one: it carries both halves of the mismatch so a failed correctness gate says
/// what it wanted and what it got, not just that something was wrong.
#[derive(Debug)]
pub enum BenchError {
    Io(std::io::Error),
    ConnectionClosed,
    UnexpectedReply { expected: String, got: Frame },
    Rmp(String),
}

impl std::fmt::Display for BenchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BenchError::Io(e) => write!(f, "io error: {e}"),
            BenchError::ConnectionClosed => write!(f, "connection closed"),
            BenchError::UnexpectedReply { expected, got } => {
                write!(f, "expected {expected}, got {got:?}")
            }
            BenchError::Rmp(msg) => write!(f, "rmp client error: {msg}"),
        }
    }
}

impl std::error::Error for BenchError {}

impl From<std::io::Error> for BenchError {
    fn from(e: std::io::Error) -> Self {
        BenchError::Io(e)
    }
}
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p rocket-mem-bench`
Expected: PASS, 2 tests.

- [ ] **Step 7: Run the full CI gate**

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all clean. If clippy reports `BenchError` as dead code, `lib.rs` is missing `pub mod error;` or the type is missing `pub`.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/bench/
git commit -m "$(cat <<'EOF'
test(bench): scaffold the benchmark crate with BenchError

Adds crates/bench as a workspace member, a lib+bin crate holding the
RMP-vs-RESP load generator. It is lib-first so that pub items are never
dead code under CI's -D warnings, which would otherwise force each task
to wire new types into main.rs before they are needed.

BenchError::UnexpectedReply carries both the expectation and the actual
frame, so a failed correctness gate reports the mismatch rather than
just the fact of one.
EOF
)"
```

---

### Task 2: `Workload` — keyspace and request building

**Files:**
- Create: `crates/bench/src/workload.rs`
- Modify: `crates/bench/src/lib.rs`

**Interfaces:**
- Consumes: `BenchError` (not directly used here, but same crate).
- Produces:
  - `pub enum OpKind { Get, Set }`
  - `pub struct Workload`
  - `Workload::new(kind: OpKind, key_count: usize, value_len: usize) -> Workload`
  - `Workload::request(&self, i: usize) -> Vec<bytes::Bytes>` — args for operation `i`, cycling over the keys.
  - `Workload::verify(&self, reply: &protocol::Frame) -> Result<(), BenchError>` — the correctness gate.
  - `Workload::seed_requests(&self) -> Vec<Vec<bytes::Bytes>>` — one `SET` per key, used by the runner to pre-seed.
  - `Workload::kind(&self) -> OpKind`

- [ ] **Step 1: Write the failing test**

Create `crates/bench/src/workload.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use protocol::Frame;

    #[test]
    fn get_requests_cycle_over_the_whole_keyspace() {
        let w = Workload::new(OpKind::Get, 3, 8);
        assert_eq!(w.request(0), vec![Bytes::from_static(b"GET"), Bytes::from_static(b"bench:key:0")]);
        assert_eq!(w.request(1), vec![Bytes::from_static(b"GET"), Bytes::from_static(b"bench:key:1")]);
        assert_eq!(w.request(2), vec![Bytes::from_static(b"GET"), Bytes::from_static(b"bench:key:2")]);
        // Wraps, so any op count works against any key count.
        assert_eq!(w.request(3), vec![Bytes::from_static(b"GET"), Bytes::from_static(b"bench:key:0")]);
    }

    #[test]
    fn set_requests_carry_a_value_of_the_requested_length() {
        let w = Workload::new(OpKind::Set, 2, 64);
        let req = w.request(0);
        assert_eq!(req.len(), 3);
        assert_eq!(req[0], Bytes::from_static(b"SET"));
        assert_eq!(req[1], Bytes::from_static(b"bench:key:0"));
        assert_eq!(req[2].len(), 64);
    }

    #[test]
    fn seed_requests_write_every_key_exactly_once() {
        let w = Workload::new(OpKind::Get, 4, 16);
        let seeds = w.seed_requests();
        assert_eq!(seeds.len(), 4);
        for (i, seed) in seeds.iter().enumerate() {
            assert_eq!(seed[0], Bytes::from_static(b"SET"));
            assert_eq!(seed[1], Bytes::from(format!("bench:key:{i}")));
            assert_eq!(seed[2].len(), 16);
        }
    }

    #[test]
    fn a_get_workload_accepts_the_seeded_value_and_rejects_anything_else() {
        let w = Workload::new(OpKind::Get, 1, 8);
        let value = w.request(0); // GET has no value, so build the expectation from a seed.
        assert_eq!(value.len(), 2);
        let seeded = w.seed_requests()[0][2].clone();
        assert!(w.verify(&Frame::Bulk(seeded)).is_ok());
        // A miss must fail the gate -- benchmarking a miss measures a cheaper path.
        assert!(w.verify(&Frame::Null).is_err());
        assert!(w.verify(&Frame::Bulk(Bytes::from_static(b"wrong"))).is_err());
    }

    #[test]
    fn a_set_workload_accepts_only_a_simple_ok() {
        let w = Workload::new(OpKind::Set, 1, 8);
        assert!(w.verify(&Frame::Simple("OK".to_string())).is_ok());
        assert!(w.verify(&Frame::Error("ERR nope".to_string())).is_err());
        // An error reply returns fast and would read as a throughput win without this.
        assert!(w.verify(&Frame::Integer(1)).is_err());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem-bench workload`
Expected: FAIL to compile — `Workload` and `OpKind` are not defined.

- [ ] **Step 3: Write the minimal implementation**

Add `pub mod workload;` to `crates/bench/src/lib.rs`, then prepend to `crates/bench/src/workload.rs`:

```rust
use crate::error::BenchError;
use bytes::Bytes;
use protocol::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Get,
    Set,
}

impl OpKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OpKind::Get => "GET",
            OpKind::Set => "SET",
        }
    }
}

/// The fixed keyspace every cell runs against, plus the request shape for one command.
///
/// Keys are spread over `key_count` distinct names on purpose. A single key would put every
/// in-flight request on one shard's lock, which erases the shard parallelism this benchmark
/// exists to measure. See the spec's "keyspace, and why it is load-bearing".
pub struct Workload {
    kind: OpKind,
    keys: Vec<Bytes>,
    value: Bytes,
}

impl Workload {
    pub fn new(kind: OpKind, key_count: usize, value_len: usize) -> Self {
        let keys = (0..key_count)
            .map(|i| Bytes::from(format!("bench:key:{i}")))
            .collect();
        // A fixed, non-empty filler byte. The contents never matter, only the length.
        let value = Bytes::from(vec![b'v'; value_len]);
        Workload { kind, keys, value }
    }

    pub fn kind(&self) -> OpKind {
        self.kind
    }

    /// Args for operation `i`, wrapping around the keyspace so any op count works.
    pub fn request(&self, i: usize) -> Vec<Bytes> {
        let key = self.keys[i % self.keys.len()].clone();
        match self.kind {
            OpKind::Get => vec![Bytes::from_static(b"GET"), key],
            OpKind::Set => vec![Bytes::from_static(b"SET"), key, self.value.clone()],
        }
    }

    /// One SET per key, so a GET workload measures hits rather than the cheaper miss path.
    pub fn seed_requests(&self) -> Vec<Vec<Bytes>> {
        self.keys
            .iter()
            .map(|k| {
                vec![
                    Bytes::from_static(b"SET"),
                    k.clone(),
                    self.value.clone(),
                ]
            })
            .collect()
    }

    /// The correctness gate. Without it an error reply -- which returns faster than real work --
    /// would be counted as throughput.
    pub fn verify(&self, reply: &Frame) -> Result<(), BenchError> {
        match (self.kind, reply) {
            (OpKind::Get, Frame::Bulk(b)) if *b == self.value => Ok(()),
            (OpKind::Set, Frame::Simple(s)) if s == "OK" => Ok(()),
            (kind, got) => Err(BenchError::UnexpectedReply {
                expected: match kind {
                    OpKind::Get => "a bulk reply holding the seeded value".to_string(),
                    OpKind::Set => "+OK".to_string(),
                },
                got: got.clone(),
            }),
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem-bench workload`
Expected: PASS, 5 tests.

- [ ] **Step 5: Run the full CI gate**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
Expected: all clean.

- [ ] **Step 6: Commit**

```bash
git add crates/bench/
git commit -m "$(cat <<'EOF'
test(bench): add Workload with the reply correctness gate

Workload owns the fixed keyspace (bench:key:{0..N}) and builds request
args for GET and SET, cycling over keys so any op count runs against any
key count.

The keys are deliberately spread rather than reusing one name: a single
key puts every in-flight request on one shard lock, erasing the shard
parallelism the benchmark measures.

verify() is the correctness gate -- a GET must return the seeded value
and a SET must return +OK. An error or miss reply returns faster than
real work, so without this gate a broken run would report as a
throughput win.
EOF
)"
```

---

### Task 3: `Samples` — latency collection and percentiles

**Files:**
- Create: `crates/bench/src/stats.rs`
- Modify: `crates/bench/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct Samples`
  - `Samples::with_capacity(n: usize) -> Samples`
  - `Samples::push(&mut self, d: std::time::Duration)`
  - `Samples::len(&self) -> usize`, `Samples::is_empty(&self) -> bool`
  - `Samples::percentile(&mut self, p: f64) -> std::time::Duration` — nearest-rank, sorts in place.

- [ ] **Step 1: Write the failing test**

Create `crates/bench/src/stats.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn samples_of_millis(values: &[u64]) -> Samples {
        let mut s = Samples::with_capacity(values.len());
        for v in values {
            s.push(Duration::from_millis(*v));
        }
        s
    }

    #[test]
    fn percentile_uses_nearest_rank_and_ignores_insertion_order() {
        // Deliberately unsorted -- percentile must sort, not assume.
        let mut s = samples_of_millis(&[50, 10, 100, 40, 20, 90, 30, 80, 60, 70]);
        assert_eq!(s.percentile(50.0), Duration::from_millis(50));
        assert_eq!(s.percentile(99.0), Duration::from_millis(100));
        assert_eq!(s.percentile(100.0), Duration::from_millis(100));
    }

    #[test]
    fn a_single_sample_is_every_percentile() {
        let mut s = samples_of_millis(&[7]);
        assert_eq!(s.percentile(50.0), Duration::from_millis(7));
        assert_eq!(s.percentile(99.0), Duration::from_millis(7));
    }

    #[test]
    fn an_empty_sample_set_reports_zero_rather_than_panicking() {
        let mut s = Samples::with_capacity(0);
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert_eq!(s.percentile(99.0), Duration::ZERO);
    }

    #[test]
    fn len_counts_every_pushed_sample() {
        let s = samples_of_millis(&[1, 2, 3]);
        assert_eq!(s.len(), 3);
        assert!(!s.is_empty());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem-bench stats`
Expected: FAIL to compile — `Samples` is not defined.

- [ ] **Step 3: Write the minimal implementation**

Add `pub mod stats;` to `crates/bench/src/lib.rs`, then prepend to `crates/bench/src/stats.rs`:

```rust
use std::time::Duration;

/// Latency samples for one cell. What one sample represents depends on the driver: the batch
/// drivers push one sample per round, the window driver one per operation. The two are not
/// comparable, which is why the report keeps them in separate columns -- see the spec's
/// "measurement and reporting".
pub struct Samples {
    durations: Vec<Duration>,
    sorted: bool,
}

impl Samples {
    pub fn with_capacity(n: usize) -> Self {
        Samples {
            durations: Vec::with_capacity(n),
            sorted: false,
        }
    }

    pub fn push(&mut self, d: Duration) {
        self.durations.push(d);
        self.sorted = false;
    }

    pub fn len(&self) -> usize {
        self.durations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.durations.is_empty()
    }

    /// Nearest-rank percentile. Sorts on first use and remembers it, so asking for p50 and then
    /// p99 sorts once rather than twice.
    pub fn percentile(&mut self, p: f64) -> Duration {
        if self.durations.is_empty() {
            return Duration::ZERO;
        }
        if !self.sorted {
            self.durations.sort_unstable();
            self.sorted = true;
        }
        let rank = (p / 100.0 * self.durations.len() as f64).ceil() as usize;
        let index = rank.saturating_sub(1).min(self.durations.len() - 1);
        self.durations[index]
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem-bench stats`
Expected: PASS, 4 tests.

- [ ] **Step 5: Run the full CI gate**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
Expected: all clean. Clippy's `len_without_is_empty` is satisfied because `is_empty` is defined.

- [ ] **Step 6: Commit**

```bash
git add crates/bench/
git commit -m "$(cat <<'EOF'
test(bench): add Samples with nearest-rank percentiles

Collects per-cell latency samples and reports p50/p99. Sorting is
deferred to the first percentile call and cached, so asking for p50 then
p99 sorts once.

An empty sample set returns zero rather than panicking, so a cell that
failed before recording anything reports cleanly instead of taking the
whole sweep down.

What one sample means is driver-dependent -- a round for the batch
drivers, an operation for the window driver -- which is why the report
keeps the two in separate columns.
EOF
)"
```

---

## Definition of Done

- [ ] `crates/bench` is a workspace member and `cargo build --workspace` succeeds.
- [ ] `BenchError`, `Workload`, and `Samples` all exist with the interfaces above.
- [ ] 11 tests pass in `rocket-mem-bench`.
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` are all clean.
- [ ] Three commits, one per task.
