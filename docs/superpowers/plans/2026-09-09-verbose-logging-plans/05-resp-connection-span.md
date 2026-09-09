# Verbose Logging Plan 05: RESP `conn` Span & Connection-Closed Event

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open the spec's `conn` tracing span around `connection.rs`'s `handle_connection`, carrying `conn_id`, `peer`, `protocol`, and `tls`, and add a connection-closed `info!` event reporting how long the RESP connection was open and how many commands it served.

**Architecture:** This is the first of the spec's three named spans (`conn`, `cmd`, `repl` — see the spec's "Decision: three spans, not per-function instrumentation"). `#[instrument(skip_all, fields(...))]` goes directly on `handle_connection`, so every log line the function's body already emits when a subscriber is active — the accepted event itself, and every `warn!` inside the frame loop for a decode error — inherits `conn_id`/`peer`/`protocol`/`tls` for free, without each call site repeating them. `skip_all` is required, not just a performance choice: `handle_connection`'s `socket: S` parameter has no `Debug` bound (only `AsyncRead + AsyncWrite + Unpin + Send + 'static`), so `#[instrument]`'s default behavior of recording every argument via `Debug` would fail to compile against it.

The connection-closed event needs a duration and a served-command count at every one of `handle_connection`'s several exit points (decode error, clean EOF, feed failure, flush failure, and the `serve_replica` hand-off, which never returns normally once entered). Rather than duplicating the log call at each `return`, a small `ConnectionStats` guard is created once per connection and emits the event from its `Drop` impl, mirroring the existing `ClientGuard` pattern directly above `handle_connection` in the same file, which already solves the identical "many return paths, one thing that must always happen" problem for the connected-clients counter.

This plan touches only `connection.rs`'s RESP path. The `cmd` span inside `dispatcher.rs`'s `dispatch_and_log`, and the RMP equivalent of this plan's two changes, are separate plans (06 and 07).

**Tech Stack:** Rust 2021, `tokio`, `tracing 0.1`'s `#[instrument]` macro. `tracing` is already a dependency of `crates/server` (unlike `engine`/`protocol`, which plan 01 added it to) — this plan adds no new dependency.

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see "Decision: three spans, not per-function instrumentation" and the Connection row of the "Event catalogue" table.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting. The load-bearing ones here: `cargo clippy --workspace --all-targets -- -D warnings` must pass; every pre-existing test must pass **unchanged**, including `connection.rs`'s existing `serve_tracks_connected_clients_and_drops_the_count_on_disconnect` test; throughput at default `info` level must stay within 2% of the plan-01 baseline; and **no new atomic counters on the hot path** — the command count this plan adds is a plain (non-atomic) `u64` local to one connection's own task, never shared across threads, so it does not need one.

---

### Task 1: Open the `conn` span on `handle_connection`

**Files:**
- Modify: `crates/server/src/connection.rs:188` (the `handle_connection` function signature) and `:199` (its first log line)

**Interfaces:**
- Consumes: `client_id: u64`, `peer: std::net::SocketAddr`, `tls: bool` — all already `handle_connection` parameters.
- Produces: a `conn` span (named `handle_connection` — `#[instrument]`'s default span name is the function name) wrapping the function's whole body, carrying `conn_id`, `peer`, `protocol = "resp"`, `tls`. Every `tracing::warn!`/`error!`/`info!` call already inside the function (the decode-error `warn!` at line 214, `disable_nagle`'s caller-side context, etc.) is emitted as a child of this span once a subscriber is installed, without those call sites changing.

- [ ] **Step 1: Note why this task has no new automated test, and confirm the pre-change baseline**

Adding `#[instrument]` is a declarative, control-flow-free change: it does not introduce a new pure function or a new branch to characterize with an assertion, and asserting the *span's fields* would need a custom `tracing::Subscriber` capturing and parsing rendered output — exactly the "heavyweight capture infrastructure" this series' testing note says to avoid inventing for a lifecycle log change, in favor of `cargo test --workspace` staying green plus a manual check. This task follows that: the "test" is that every pre-existing test — most directly `serve_tracks_connected_clients_and_drops_the_count_on_disconnect`, which drives a real connection through `handle_connection` via `serve` — keeps passing unchanged.

Confirm the baseline before touching anything:

```bash
cargo test -p rocket-mem connection::tests::serve_tracks_connected_clients_and_drops_the_count_on_disconnect
```

Expected: PASS (this establishes the pre-change baseline the next steps must not break).

- [ ] **Step 2: Add the span and collapse the now-redundant fields**

In `crates/server/src/connection.rs`, change:

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
    tracing::info!(%peer, protocol = "resp", tls, "connection accepted");
```

to:

```rust
#[tracing::instrument(skip_all, fields(conn_id = client_id, %peer, protocol = "resp", tls))]
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

The `%peer`/`protocol`/`tls` fields move onto the span so they are attached once, at connection-open, and inherited by everything nested inside — repeating them on the `info!` line itself would duplicate what the span already carries.

- [ ] **Step 3: Verify nothing broke**

```bash
cargo test -p rocket-mem connection::tests::serve_tracks_connected_clients_and_drops_the_count_on_disconnect
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: every test still PASSES (in particular, the whole `connection::tests` module — the span change touches nothing these tests assert on), `fmt` and `clippy` clean.

- [ ] **Step 4: Manual check**

```bash
RUST_LOG=debug cargo run -p rocket-mem
```

In another terminal:

```bash
redis-cli -p 6379 PING
redis-cli -p 6379 EXEC   # sent outside a MULTI -- triggers a client-caused warn!, useful here
```

Expected: the `connection accepted` line, and any subsequent `warn!`/`error!` line logged while that connection is open, carry `conn_id`, `peer`, `protocol="resp"`, and `tls` fields (exact rendering depends on `tracing_subscriber`'s formatter, but all four fields must be present on every line from that connection).

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/connection.rs
git commit -m "feat(logging): open a conn span on the RESP handle_connection"
```

---

### Task 2: Emit a connection-closed event with duration and command count

**Files:**
- Modify: `crates/server/src/connection.rs` (add a `ConnectionStats` guard near `ClientGuard`, defined just above `handle_connection` at line 180; wire it into `handle_connection`'s body between lines 188–258)
- Test: `crates/server/src/connection.rs`'s existing `#[cfg(test)] mod tests` (starts at line 348)

**Interfaces:**
- Consumes: nothing new from outside the file.
- Produces: a private `ConnectionStats` type with `new()` and `record_command(&mut self)`, and a `Drop` impl that emits a `connection closed` `info!` event carrying `elapsed_us` and `commands_served`. Nothing outside this file depends on it — it exists purely to guarantee the event fires exactly once, however the connection ends.

- [ ] **Step 1: Write a failing test for the pure counting logic**

The count and elapsed-time bookkeeping is real, isolable logic (unlike the span in Task 1); the log line it eventually feeds is not independently assertable without the same heavyweight-capture problem noted in Task 1, so only the counting itself is unit-tested here. Add the stub type just above `handle_connection` (right after `ClientGuard`'s `impl Drop` block, which ends at line 186) in `crates/server/src/connection.rs`:

```rust
/// Tracks one RESP connection's lifetime state for the connection-closed log event: how long
/// it was open and how many commands it served. Emits that event from `Drop` so every one of
/// `handle_connection`'s several return paths -- decode error, clean EOF, feed failure, flush
/// failure, and the `serve_replica` hand-off, which never returns normally -- gets it exactly
/// once, without each of them having to remember. Mirrors `ClientGuard` just above, which
/// solves the identical problem for the connected-clients counter.
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

Add this test to the existing `mod tests` block at the bottom of `crates/server/src/connection.rs`:

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
cargo test -p rocket-mem connection::tests::connection_stats_counts_each_recorded_command
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

(The `.min(u64::MAX as u128) as u64` clamp before the cast matches `slowlog.rs:81`'s existing pattern for turning a `Duration`'s `u128` micros into a `u64` field without a truncation footgun on an implausibly long-lived connection.)

Now wire it into `handle_connection`. Change:

```rust
    tracing::info!("connection accepted");
    replication.connection_opened();
    let _client_guard = ClientGuard(Arc::clone(&replication));
    let mut framed = Framed::new(socket, RespCodec::default());
```

to:

```rust
    tracing::info!("connection accepted");
    replication.connection_opened();
    let _client_guard = ClientGuard(Arc::clone(&replication));
    let mut conn_stats = ConnectionStats::new();
    let mut framed = Framed::new(socket, RespCodec::default());
```

and change the dispatch call:

```rust
        let response =
            dispatcher::dispatch_and_log(&engine, &aof, &replication, frame, &session, client_id);
        framed.codec_mut().protocol = session.protocol(); // sync BEFORE sending this reply
```

to:

```rust
        let response =
            dispatcher::dispatch_and_log(&engine, &aof, &replication, frame, &session, client_id);
        conn_stats.record_command();
        framed.codec_mut().protocol = session.protocol(); // sync BEFORE sending this reply
```

No other change is needed: `conn_stats` is a local variable of `handle_connection`, so every `return` in the function — the decode-error return at line 216, the clean-EOF return at line 217, the PSYNC hand-off's `return` at line 234 (after `serve_replica` completes), the feed-failure return at line 244, and the flush-failure return at line 253 — drops it on the way out and fires the event exactly once. PSYNC itself is deliberately not counted as a served command: it never reaches `dispatch_and_log`, and once `serve_replica` is entered the connection stops serving ordinary commands entirely, so the count that matters is however many ordinary commands preceded it.

- [ ] **Step 4: Run the test to verify it passes, then the full suite**

```bash
cargo test -p rocket-mem connection::tests::connection_stats_counts_each_recorded_command
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: the new test PASSES, every pre-existing test still PASSES unchanged, `fmt` and `clippy` clean.

- [ ] **Step 5: Manual check**

The `connection closed` event's actual log line — as opposed to the counting logic behind it, which Step 4 already verifies — is exactly the kind of lifecycle log this series' testing note says to verify by eye rather than by capture:

```bash
RUST_LOG=info cargo run -p rocket-mem
```

In another terminal:

```bash
redis-cli -p 6379 SET a 1
redis-cli -p 6379 SET b 2
redis-cli -p 6379 -x QUIT < /dev/null  # or simply Ctrl-C a `redis-cli` session mid-connection
```

Expected: on that connection's disconnect, a `connection closed` line appears carrying `elapsed_us` (a small positive number) and `commands_served` (matching however many commands that particular connection sent — e.g. `2` for a `redis-cli` session that ran two `SET`s before quitting), plus the `conn_id`/`peer`/`protocol`/`tls` fields inherited from Task 1's span.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/connection.rs
git commit -m "feat(logging): log RESP connection duration and command count on close"
```

---

## Next plan

[`06-rmp-connection-span.md`](06-rmp-connection-span.md) — adds the same `conn` span and connection-closed event to `rmp_connection.rs`'s `handle_connection`, with `protocol = "rmp"` and a count taken at request-read time rather than per-dispatch, since RMP dispatches each request on its own spawned task.
