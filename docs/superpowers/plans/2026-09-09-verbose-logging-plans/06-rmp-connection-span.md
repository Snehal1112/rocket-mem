# Verbose Logging Plan 06: RMP `conn` Span & Connection-Closed Event

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open the spec's `conn` tracing span around `rmp_connection.rs`'s `handle_connection`, carrying `conn_id`, `peer`, `protocol = "rmp"`, and `tls`, and add a connection-closed `info!` event reporting how long the RMP connection was open and how many requests it served.

**Architecture:** The same `conn` span design plan 05 applied to the RESP path, applied here to the RMP one — `#[instrument(skip_all, fields(...))]` on `handle_connection` so `protocol = "rmp"` (rather than `"resp"`) and the connection's other fields are inherited by every nested log line, including the existing `rmp decode error` `warn!`.

The connection-closed event needs the same care as plan 05's, but RMP's concurrency shape is different and changes where the count is taken: `handle_connection`'s `while let Some(next) = stream.next().await` loop reads one request at a time, but each request is handed to its own `tokio::spawn`ed task and dispatched concurrently — up to `MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION` (256) at once — while the read loop immediately goes back to decoding the next one. Counting on *completion* (inside each spawned task, after its own `dispatch_and_log` call) would need a counter shared across every one of those tasks, which without an atomic — ruled out by this series' hot-path constraint — would need a lock taken on every single request. Counting at *read* time avoids that entirely: only the single sequential read loop ever touches the counter, so a plain (non-atomic) local `u64` is correct with no synchronization at all. This is a deliberate simplification, not an oversight: it counts requests taken off the socket, not confirmed-replied requests, but by the time the connection closes (the function returns only after `writer.await` drains every in-flight reply) the two numbers coincide for all but a request still in flight at the exact instant of a mid-stream abort.

`handle_connection` has a single normal exit — after `drop(tx); let _ = writer.await;` at the end of the function — reached from every one of the read loop's `break` arms (a stray non-`Request` message, a decode error, or the semaphore closing), so a `Drop`-based guard is not strictly load-bearing the way plan 05's is for RESP's several early `return`s, but this plan uses the same `ConnectionStats` shape anyway: it keeps the two files' logging code recognizably parallel, and it costs nothing to make future changes that add an early `return` here safe by construction rather than by convention.

**Tech Stack:** Rust 2021, `tokio`, `tracing 0.1`'s `#[instrument]` macro. No new dependency.

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see "Decision: three spans, not per-function instrumentation" and the Connection row of the "Event catalogue" table.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting. The load-bearing ones here: `cargo clippy --workspace --all-targets -- -D warnings` must pass; every pre-existing test must pass **unchanged**, including `rmp_connection.rs`'s existing `serve_tracks_connected_rmp_clients_and_drops_the_count_on_disconnect` and the in-flight-cap tests; throughput at default `info` level must stay within 2% of the plan-01 baseline; and **no new atomic counters on the hot path** — this plan's Architecture section above explains why counting at request-read time keeps the counter a plain, unshared `u64`.

---

### Task 1: Open the `conn` span on `handle_connection`

**Files:**
- Modify: `crates/server/src/rmp_connection.rs:99` (the `handle_connection` function signature) and `:110` (its first log line)

**Interfaces:**
- Consumes: `client_id: u64`, `peer: std::net::SocketAddr`, `tls: bool` — all already `handle_connection` parameters.
- Produces: a `conn` span (named `handle_connection`, `#[instrument]`'s default) wrapping the function's whole body, carrying `conn_id`, `peer`, `protocol = "rmp"`, `tls`. The existing `rmp decode error` `warn!` at line 142 (and any future log line added inside this function) inherits these fields once a subscriber is installed, without needing to repeat them.

- [ ] **Step 1: Note why this task has no new automated test, and confirm the pre-change baseline**

As in plan 05's Task 1: `#[instrument]` is a declarative, control-flow-free change, and asserting the span's actual fields would need a custom capturing `tracing::Subscriber` — the heavyweight capture infrastructure this series' testing note says to avoid for a lifecycle-log change. The check here is that every pre-existing test keeps passing unchanged, most directly the one that drives a real connection through this exact function:

```bash
cargo test -p rocket-mem rmp_connection::tests::serve_tracks_connected_rmp_clients_and_drops_the_count_on_disconnect
```

Expected: PASS (the pre-change baseline the next steps must not break).

- [ ] **Step 2: Add the span and collapse the now-redundant fields**

In `crates/server/src/rmp_connection.rs`, change:

```rust
async fn handle_connection<S>(
    socket: S,
    peer: std::net::SocketAddr,
    tls: bool,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tracing::info!(%peer, protocol = "rmp", tls, "connection accepted");
```

to:

```rust
#[tracing::instrument(skip_all, fields(conn_id = client_id, %peer, protocol = "rmp", tls))]
async fn handle_connection<S>(
    socket: S,
    peer: std::net::SocketAddr,
    tls: bool,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tracing::info!("connection accepted");
```

As in plan 05: `skip_all` is required, not optional — `socket: S` has no `Debug` bound, so `#[instrument]`'s default per-argument recording would fail to compile against it. The `%peer`/`protocol`/`tls` fields move onto the span so nested log lines inherit them instead of repeating them.

- [ ] **Step 3: Verify nothing broke**

```bash
cargo test -p rocket-mem rmp_connection::tests::serve_tracks_connected_rmp_clients_and_drops_the_count_on_disconnect
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: every test still PASSES — in particular the full `rmp_connection::tests` module, including `more_than_the_in_flight_cap_concurrent_requests_all_still_succeed` and `an_rmp_connection_saturated_with_slow_requests_eventually_serves_every_reply`, which exercise this exact function under concurrency and would surface any span-related deadlock or panic — `fmt` and `clippy` clean.

- [ ] **Step 4: Manual check**

```bash
RUST_LOG=debug cargo run -p rocket-mem
```

In another terminal, using `rmp-client` or any RMP-speaking client to connect and issue a request (a raw RESP `redis-cli` will not speak RMP; use the workspace's own `rmp-client` crate or an existing manual-testing script — see `.claude/manual-testing.md`), then send a malformed frame to trigger the `rmp decode error` `warn!`.

Expected: the `connection accepted` line, and the `rmp decode error` line if triggered, both carry `conn_id`, `peer`, `protocol="rmp"`, and `tls` fields.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/rmp_connection.rs
git commit -m "feat(logging): open a conn span on the RMP handle_connection"
```

---

### Task 2: Emit a connection-closed event with duration and request count

**Files:**
- Modify: `crates/server/src/rmp_connection.rs` (add a `ConnectionStats` guard above `handle_connection` at line 99; wire it into the function body between lines 99–182)
- Test: `crates/server/src/rmp_connection.rs`'s existing `#[cfg(test)] mod tests` (starts at line 184)

**Interfaces:**
- Consumes: nothing new from outside the file.
- Produces: a private `ConnectionStats` type with `new()` and `record_command(&mut self)`, and a `Drop` impl that emits a `connection closed` `info!` event carrying `elapsed_us` and `commands_served`. Structurally identical to plan 05's type of the same name in `connection.rs` — the two are independent private items in separate modules, so there is no naming conflict — but this one is incremented once per request read off the socket rather than once per completed dispatch, per this plan's Architecture section.

- [ ] **Step 1: Write a failing test for the pure counting logic**

Add the stub type just above `handle_connection` in `crates/server/src/rmp_connection.rs`:

```rust
/// Tracks one RMP connection's lifetime state for the connection-closed log event: how long it
/// was open and how many requests it read off the socket. Emits that event from `Drop`, mirroring
/// `connection.rs`'s `ConnectionStats` for the RESP path.
///
/// Counted at read time, before a request's per-request task is spawned -- not on completion.
/// Counting on completion would need a counter shared with every spawned task, which without an
/// atomic (this series' hot-path constraint rules that out) would need a lock on every single
/// request; counting at read time needs neither, since only this connection's sequential read
/// loop ever touches the counter.
struct ConnectionStats {
    started_at: std::time::Instant,
    commands_served: u64,
}

impl ConnectionStats {
    fn new() -> Self {
        Self {
            started_at: std::time::Instant::now(),
            commands_served: 0,
        }
    }

    fn record_command(&mut self) {
        // Filled in once the failing test below demonstrates it's needed.
    }
}
```

Add this test to the existing `mod tests` block at the bottom of `crates/server/src/rmp_connection.rs`:

```rust
    #[test]
    fn connection_stats_counts_each_recorded_command() {
        let mut stats = ConnectionStats::new();
        stats.record_command();
        stats.record_command();
        stats.record_command();
        assert_eq!(stats.commands_served, 3);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem rmp_connection::tests::connection_stats_counts_each_recorded_command
```

Expected: FAIL —

```
assertion `left == right` failed
  left: 0
 right: 3
```

- [ ] **Step 3: Implement counting and the closed-event `Drop` impl**

Replace the stub `impl ConnectionStats` block with:

```rust
impl ConnectionStats {
    fn new() -> Self {
        Self {
            started_at: std::time::Instant::now(),
            commands_served: 0,
        }
    }

    fn record_command(&mut self) {
        self.commands_served += 1;
    }
}

impl Drop for ConnectionStats {
    fn drop(&mut self) {
        tracing::info!(
            elapsed_us = self.started_at.elapsed().as_micros().min(u64::MAX as u128) as u64,
            commands_served = self.commands_served,
            "connection closed"
        );
    }
}
```

(Same clamp-before-cast as plan 05, matching `slowlog.rs:81`'s existing pattern.)

Now wire it into `handle_connection`. Change:

```rust
    tracing::info!("connection accepted");
    replication.connection_opened();
    let _client_guard = ClientGuard(Arc::clone(&replication));
    let framed = Framed::new(socket, RmpCodec);
```

to:

```rust
    tracing::info!("connection accepted");
    replication.connection_opened();
    let _client_guard = ClientGuard(Arc::clone(&replication));
    let mut conn_stats = ConnectionStats::new();
    let framed = Framed::new(socket, RmpCodec);
```

and change the read loop to count each request as it's decoded, before the permit is acquired and the per-request task is spawned:

```rust
    while let Some(next) = stream.next().await {
        let request = match next {
            Ok(msg) if msg.msg_type == MsgType::Request => msg,
            Ok(_) => break, // a stray Response from a misbehaving client
            Err(e) => {
                tracing::warn!(%peer, error = %e, "rmp decode error");
                break;
            }
        };
        // Blocks once MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION tasks are already mid-dispatch --
        // that's the backpressure: the read loop stops pulling more requests off the socket
        // until one finishes and its permit is released.
        let permit = match Arc::clone(&semaphore).acquire_owned().await {
```

to:

```rust
    while let Some(next) = stream.next().await {
        let request = match next {
            Ok(msg) if msg.msg_type == MsgType::Request => msg,
            Ok(_) => break, // a stray Response from a misbehaving client
            Err(e) => {
                tracing::warn!(%peer, error = %e, "rmp decode error");
                break;
            }
        };
        conn_stats.record_command();
        // Blocks once MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION tasks are already mid-dispatch --
        // that's the backpressure: the read loop stops pulling more requests off the socket
        // until one finishes and its permit is released.
        let permit = match Arc::clone(&semaphore).acquire_owned().await {
```

No other change is needed: `conn_stats` is a local variable of `handle_connection`, and every path out of the function — the two `break` arms above, the `Err(_) => break` on a closed semaphore, and the normal loop exhaustion once `stream.next()` returns `None` — all fall through to the same `drop(tx); let _ = writer.await;` tail, after which `conn_stats` is dropped and the event fires exactly once.

- [ ] **Step 4: Run the test to verify it passes, then the full suite**

```bash
cargo test -p rocket-mem rmp_connection::tests::connection_stats_counts_each_recorded_command
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: the new test PASSES, every pre-existing test still PASSES unchanged — including the concurrency-heavy `more_than_the_in_flight_cap_concurrent_requests_all_still_succeed` and `an_rmp_connection_saturated_with_slow_requests_eventually_serves_every_reply`, which prove the plain `u64` counter causes no contention or ordering issue since only the read loop ever touches it — `fmt` and `clippy` clean.

- [ ] **Step 5: Manual check**

As in plan 05, the actual log line is verified by eye rather than by capture, per this series' testing note:

```bash
RUST_LOG=info cargo run -p rocket-mem
```

In another terminal, connect with an RMP client (see `.claude/manual-testing.md`), send a couple of requests, then disconnect.

Expected: on disconnect, a `connection closed` line appears carrying `elapsed_us` (a small positive number) and `commands_served` (matching however many requests that connection sent), plus the `conn_id`/`peer`/`protocol="rmp"`/`tls` fields inherited from Task 1's span.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/rmp_connection.rs
git commit -m "feat(logging): log RMP connection duration and request count on close"
```

---

## Next plan

[`07-command-span.md`](07-command-span.md) — opens the `cmd` span inside `dispatcher.rs`'s `dispatch_and_log`, the span every per-command `debug`/`trace` log line in later plans nests under, using the `client_id`, `name`, `first_key`, and `arg_count` that function already computes for the metrics and slowlog paths.
