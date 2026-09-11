# Peer-Probe Connection Log Level Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the cluster peer-liveness prober's own TCP probe connections from producing an unconditional `info`-level `connection accepted`/`connection closed` pair on the probed peer every round, which currently defeats the prober's own "log only on reachability change" design and floods logs at `cluster_probe_interval_secs` cadence.

**Architecture:** The prober (`cluster_health.rs::probe_once`/`probe_ping`) already sends a bare `PING` over a real TCP connection to check liveness. Change it to send `PING <PROBE_MARKER>` — a fixed, shared byte-string argument — instead. `connection.rs::handle_connection` reads its first frame *before* logging "connection accepted" (instead of unconditionally at entry), recognizes that exact marker, and logs both the accept and close events for that one connection at `debug` instead of `info`. No wire-protocol change, no new listener, no ACL/dispatcher change — `PING` already accepts an optional message argument and echoes it back unchanged, so a probed peer still replies correctly to a marked probe.

**Tech Stack:** Rust, `tokio`, `tracing`, existing `Framed`/`RespCodec` (RESP protocol crate).

**Spec:** No standalone spec doc — this plan is scoped narrowly from a live debugging session (see `docs/superpowers/specs/2026-09-09-verbose-logging-design.md` for the pre-existing level taxonomy this plan must stay consistent with: `info` = milestones, `debug` = per-request activity).

## Global Constraints

- No `format!` outside a log macro's argument list (hot-path guardrail from the verbose-logging spec — this plan's new code sits on the connection-accept hot path).
- `Bytes`/byte slices are never logged via `Debug` — unaffected here since no new field is added to any log line, only the level of two pre-existing events changes conditionally.
- Every existing test in the workspace must keep passing unchanged, and `cargo clippy --workspace --all-targets -- -D warnings` must stay clean, per this repo's standing bar (`CLAUDE.md`'s Commands section).
- `PROBE_MARKER` classification is by exact byte match only — never treat this as an authentication or security boundary. It only controls a log line's verbosity; a client that happens to send `PING __rocket_mem_peer_probe__` by coincidence is harmless (it just gets a quieter log entry for that one connection), and this must not be built into anything ACL- or auth-adjacent later.

---

### Task 1: Send a self-identifying marker on every peer-liveness probe

**Files:**
- Modify: `crates/server/src/cluster_health.rs:178-198` (the `probe_ping` function) and add a new `pub(crate) const` near it.
- Test: `crates/server/src/cluster_health.rs`'s existing `#[cfg(test)] mod tests` (starts around line 336).

**Interfaces:**
- Produces: `pub(crate) const PROBE_MARKER: &[u8]` — the exact RESP argument bytes every probe's `PING` carries. Task 2 imports this as `crate::cluster_health::PROBE_MARKER` to recognize a probe connection on the receiving side.
- Consumes: nothing new; `probe_ping`'s existing signature (`async fn probe_ping<S: AsyncRead + AsyncWrite + Unpin>(socket: S) -> Option<()>`) is unchanged.

Current code (for reference — this is what step 3 replaces):

```rust
async fn probe_ping<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut socket: S,
) -> Option<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    socket.write_all(b"*1\r\n$4\r\nPING\r\n").await.ok()?;
    let mut buf = [0u8; 32];
    let read = socket.read(&mut buf).await.ok()?;
    let answered = read > 0 && (buf[0] == b'+' || buf[0] == b'-');
    let _ = socket.shutdown().await;
    answered.then_some(())
}
```

- [ ] **Step 1: Write the failing test**

Add to `crates/server/src/cluster_health.rs`'s `mod tests`, near `spawn_ping_responder`/`a_probe_of_a_live_node_succeeds`:

```rust
    /// Like `spawn_ping_responder`, but captures the raw bytes the prober actually sent instead
    /// of blindly replying — this is what proves `probe_ping` sends the marker, not just that
    /// probing still works. Replies with a `PING`-style Bulk echo of whatever second argument it
    /// parsed out of the request, mirroring what a real rocket-mem peer's `PING <msg>` handling
    /// does, so the round trip stays representative.
    async fn spawn_capturing_responder() -> (String, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 128];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let _ = tx.send(buf[..n].to_vec());
                let _ = socket
                    .write_all(b"$25\r\n__rocket_mem_peer_probe__\r\n")
                    .await;
            }
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn probe_ping_sends_the_ping_marker_as_a_two_element_array() {
        let (addr, rx) = spawn_capturing_responder().await;
        assert!(probe_once(&addr, Duration::from_secs(1), None).await);
        let sent = rx.await.unwrap();
        let expected = b"*2\r\n$4\r\nPING\r\n$25\r\n__rocket_mem_peer_probe__\r\n";
        assert_eq!(
            sent, expected,
            "expected the exact PING <PROBE_MARKER> wire encoding, got: {}",
            String::from_utf8_lossy(&sent)
        );
    }

    #[test]
    fn probe_marker_is_the_length_the_wire_encoding_assumes() {
        // The test above hardcodes `$25\r\n` for the marker's RESP bulk-length prefix; this
        // guards that assumption so a future edit to PROBE_MARKER's text fails loudly here
        // instead of silently breaking the wire-format assertion above.
        assert_eq!(PROBE_MARKER.len(), 25);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem --lib cluster_health::tests::probe_ping_sends_the_ping_marker -- --exact`
Expected: FAIL — compile error, `PROBE_MARKER` does not exist yet; once that's stubbed in, a second run should fail on the byte-string assertion because `probe_ping` still sends bare `PING`.

- [ ] **Step 3: Implement**

Replace the `probe_ping` function body and add the constant above it:

```rust
/// The RESP argument every peer-liveness probe's `PING` carries, so the probed peer's
/// `connection.rs` can recognize this connection as this node's own internal health check
/// (never a real client) and log its accept/close pair at `debug` instead of `info`. The sender
/// (`probe_ping`, below) and the recognizer (`connection::is_probe_ping`) must agree on this
/// exact byte string — it is a log-verbosity signal only, never an authentication or security
/// boundary: a client that happens to send this by coincidence just gets a quieter log line for
/// that one connection, nothing more.
pub(crate) const PROBE_MARKER: &[u8] = b"__rocket_mem_peer_probe__";

async fn probe_ping<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut socket: S,
) -> Option<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut request = Vec::with_capacity(16 + PROBE_MARKER.len());
    request.extend_from_slice(b"*2\r\n$4\r\nPING\r\n$");
    request.extend_from_slice(PROBE_MARKER.len().to_string().as_bytes());
    request.extend_from_slice(b"\r\n");
    request.extend_from_slice(PROBE_MARKER);
    request.extend_from_slice(b"\r\n");
    socket.write_all(&request).await.ok()?;
    let mut buf = [0u8; 64];
    let read = socket.read(&mut buf).await.ok()?;
    // `PING <msg>` replies with a Bulk string (`$...`), not the Simple-string `+PONG` a bare
    // PING gets — probe_ping now always sends the two-argument form, so `$` is the expected
    // success prefix. `+`/`-` stay accepted too, for robustness against a peer that doesn't
    // recognize rocket-mem's own marker at all (a plain RESP server, or a future protocol
    // change) and just answers with an ordinary PONG or an error — either still proves the peer
    // is alive and speaking RESP, which is all this check is for.
    let answered = read > 0 && matches!(buf[0], b'+' | b'-' | b'$');
    let _ = socket.shutdown().await;
    answered.then_some(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem --lib cluster_health::tests:: 2>&1 | tail -40`
Expected: every test in `cluster_health.rs` passes, including the two new ones and all pre-existing ones (`a_probe_of_a_live_node_succeeds` etc. — `spawn_ping_responder`'s dumb `+PONG\r\n` reply still counts as "answered" under the updated `+`/`-`/`$` check, so no existing test needs to change).

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill for the commit message (this project's standing convention for Superpowers-driven commits).

```bash
git add crates/server/src/cluster_health.rs
```

---

### Task 2: Recognize the marker on the receiving side and downgrade that connection's log level

**Files:**
- Modify: `crates/server/src/connection.rs:215-334` (`ConnectionStats` struct/impl and the top of `handle_connection`).
- Test: `crates/server/src/connection.rs`'s existing `#[cfg(test)] mod tests` (starts at line 642).

**Interfaces:**
- Consumes: `crate::cluster_health::PROBE_MARKER` from Task 1.
- Produces: `fn is_probe_ping(frame: &protocol::Frame) -> bool` — a pure, synchronous predicate. Task 3's integration test calls the same marker constant (not this function directly) to build a probe-shaped frame from the outside.

Current code (for reference):

```rust
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

and, inside `handle_connection` (after the `#[tracing::instrument]` header):

```rust
    tracing::info!("connection accepted");
    replication.connection_opened();
    let _client_guard = ClientGuard(Arc::clone(&replication), client_id);
    let mut conn_stats = ConnectionStats::new();
    let mut framed = Framed::new(socket, RespCodec::default());
    let session = dispatcher::Session::with_peer_addr(peer);
    // Carries a frame pulled ahead by the pipelining peek below, so it isn't re-read.
    let mut pending: Option<Option<std::io::Result<protocol::Frame>>> = None;
    loop {
        let next = match pending.take() {
            Some(n) => n,
            None => {
                // ... unchanged select! block ...
            }
        };
        let frame = match next {
            Some(Ok(frame)) => frame,
            Some(Err(e)) => {
                tracing::warn!(%peer, error = %e, "connection closed: decode error");
                return;
            }
            None => return, // client disconnected cleanly — not worth logging
        };
        // ... rest of loop unchanged ...
```

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/connection.rs`'s `mod tests`, near the top (these are pure/sync, no networking needed):

```rust
    #[test]
    fn is_probe_ping_recognizes_the_exact_marker() {
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"PING")),
            Frame::Bulk(Bytes::from_static(crate::cluster_health::PROBE_MARKER)),
        ]);
        assert!(is_probe_ping(&frame));
    }

    #[test]
    fn is_probe_ping_is_case_insensitive_on_the_command_name() {
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"ping")),
            Frame::Bulk(Bytes::from_static(crate::cluster_health::PROBE_MARKER)),
        ]);
        assert!(is_probe_ping(&frame));
    }

    #[test]
    fn is_probe_ping_rejects_a_bare_ping() {
        let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))]);
        assert!(!is_probe_ping(&frame));
    }

    #[test]
    fn is_probe_ping_rejects_an_unrelated_ping_message() {
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"PING")),
            Frame::Bulk(Bytes::from_static(b"hello")),
        ]);
        assert!(!is_probe_ping(&frame));
    }

    #[test]
    fn is_probe_ping_rejects_a_non_ping_command_even_with_the_marker_as_an_argument() {
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"ECHO")),
            Frame::Bulk(Bytes::from_static(crate::cluster_health::PROBE_MARKER)),
        ]);
        assert!(!is_probe_ping(&frame));
    }

    #[test]
    fn is_probe_ping_rejects_a_non_array_frame() {
        assert!(!is_probe_ping(&Frame::Simple("PING".into())));
    }
```

The two tests below use `crate::logging::test_support::CapturedLogs` directly with
`tracing::subscriber::set_default` (a guard kept alive across `.await` points), rather than
`dispatcher.rs`'s `capture_logs_at` helper: that helper wraps `tracing::subscriber::with_default`,
which only covers its synchronous closure's duration and does not follow execution across a
`tokio::spawn`ed task's `.await` points — fine for `dispatcher.rs`'s synchronous call sites, wrong
tool here since `handle_connection` runs inside a spawned task this test must observe for tens of
milliseconds after `serve()` returns.

```rust
    use crate::logging::test_support::CapturedLogs;

    /// Reproduces the exact bug this plan fixes: before it, `handle_connection` logged
    /// "connection accepted"/"connection closed" at `info` unconditionally, so every peer-probe
    /// round (a real, if synthetic, TCP connection) produced two `info` lines regardless of
    /// `cluster_health.rs`'s own "log only on reachability change" design. `#[tokio::test]`
    /// defaults to a current-thread runtime, so the `tokio::spawn`ed connection task below runs
    /// on the same OS thread this test's `tracing::subscriber::set_default` guard covers —
    /// unlike `capture_logs_at` above (fine for the synchronous `is_probe_ping` unit tests,
    /// wrong tool for anything spanning a `.await` across a spawned task).
    #[tokio::test]
    async fn a_probe_connections_accept_and_close_log_at_debug_not_info() {
        let writer = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("probe-log-level-test-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            Arc::from("test-node"),
        ));

        let mut probe = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        probe
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"PING")),
                Frame::Bulk(Bytes::from_static(crate::cluster_health::PROBE_MARKER)),
            ]))
            .await
            .unwrap();
        assert_eq!(
            probe.next().await.unwrap().unwrap(),
            Frame::Bulk(Bytes::from_static(crate::cluster_health::PROBE_MARKER))
        );
        drop(probe);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let full_log = writer.text();
        assert!(
            full_log.contains("connection accepted") && full_log.contains("connection closed"),
            "sanity check: the events must exist at debug, got: {full_log}"
        );

        drop(_guard);
        let writer_info = CapturedLogs::default();
        let subscriber_info = tracing_subscriber::fmt()
            .with_writer(writer_info.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _guard2 = tracing::subscriber::set_default(subscriber_info);

        let listener2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr2 = listener2.local_addr().unwrap();
        tokio::spawn(serve(
            listener2,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            Arc::from("test-node"),
        ));
        let mut probe2 = Framed::new(
            TcpStream::connect(addr2).await.unwrap(),
            RespCodec::default(),
        );
        probe2
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"PING")),
                Frame::Bulk(Bytes::from_static(crate::cluster_health::PROBE_MARKER)),
            ]))
            .await
            .unwrap();
        probe2.next().await.unwrap().unwrap();
        drop(probe2);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let info_log = writer_info.text();
        assert!(
            !info_log.contains("connection accepted") && !info_log.contains("connection closed"),
            "a probe connection's accept/close pair must not appear at info, got: {info_log}"
        );
    }

    /// Regression guard for real clients: an ordinary bare `PING` (no marker) must keep logging
    /// its accept/close pair at `info`, exactly as before this plan.
    #[tokio::test]
    async fn an_ordinary_connections_accept_and_close_log_at_info() {
        let writer = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("probe-log-level-test-unused-2.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            Arc::from("test-node"),
        ));

        let mut client = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        client
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))]))
            .await
            .unwrap();
        client.next().await.unwrap().unwrap();
        drop(client);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let log = writer.text();
        assert!(
            log.contains("connection accepted") && log.contains("connection closed"),
            "an ordinary client connection must still log at info, got: {log}"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem --lib connection::tests::is_probe_ping -- --exact 2>&1 | tail -20`
Expected: FAIL — compile error, `is_probe_ping` does not exist yet.

- [ ] **Step 3: Implement**

Replace `ConnectionStats` and the top of `handle_connection` as follows.

`ConnectionStats`:

```rust
struct ConnectionStats {
    started_at: std::time::Instant,
    commands_served: u64,
    is_probe: bool,
}

impl ConnectionStats {
    fn new(is_probe: bool) -> Self {
        Self {
            started_at: std::time::Instant::now(),
            commands_served: 0,
            is_probe,
        }
    }

    fn record_command(&mut self) {
        self.commands_served += 1;
    }
}

impl Drop for ConnectionStats {
    fn drop(&mut self) {
        let elapsed_us = self.started_at.elapsed().as_micros().min(u64::MAX as u128) as u64;
        // See `is_probe_ping`'s doc comment: a probe connection's accept/close pair moves to
        // `debug` so `cluster_probe_interval_secs` no longer dictates how many `info` lines a
        // healthy, unchanging cluster produces every round.
        if self.is_probe {
            tracing::debug!(elapsed_us, commands_served = self.commands_served, "connection closed");
        } else {
            tracing::info!(elapsed_us, commands_served = self.commands_served, "connection closed");
        }
    }
}

/// True when `frame` is exactly the two-argument `PING <PROBE_MARKER>` this node's own
/// cluster-peer-liveness prober sends (`cluster_health::probe_ping`) — never a bare `PING` or
/// any other real command. Used only to pick the log level for one connection's accept/close
/// pair; matching by exact bytes is not, and must never become, an authentication or security
/// boundary — see `cluster_health::PROBE_MARKER`'s own doc comment.
fn is_probe_ping(frame: &protocol::Frame) -> bool {
    let protocol::Frame::Array(items) = frame else {
        return false;
    };
    let [protocol::Frame::Bulk(name), protocol::Frame::Bulk(arg)] = items.as_slice() else {
        return false;
    };
    name.eq_ignore_ascii_case(b"PING") && arg.as_ref() == crate::cluster_health::PROBE_MARKER
}
```

Top of `handle_connection` (everything from the `#[tracing::instrument]` header's closing brace down to the `loop {` line):

```rust
{
    replication.connection_opened();
    let _client_guard = ClientGuard(Arc::clone(&replication), client_id);
    let mut framed = Framed::new(socket, RespCodec::default());
    let session = dispatcher::Session::with_peer_addr(peer);
    // Read the first frame before deciding "connection accepted"'s log level, so a peer-probe
    // connection (see `is_probe_ping`) can log at `debug` instead of `info` — this is the one
    // behavior change for every OTHER connection too: a connection that disconnects (EOF or a
    // decode error) before ever sending a valid frame no longer gets an "connection accepted"
    // line at all, since there was never anything to inspect. `replication.connection_opened()`
    // and `_client_guard` above are unaffected — they still fire immediately and unconditionally,
    // so the connected-clients gauge and pub/sub cleanup keep their exact existing timing.
    let first = framed.next().await;
    let is_probe = matches!(&first, Some(Ok(frame)) if is_probe_ping(frame));
    if is_probe {
        tracing::debug!("connection accepted");
    } else {
        tracing::info!("connection accepted");
    }
    let mut conn_stats = ConnectionStats::new(is_probe);
    // Carries a frame pulled ahead — either the pre-read `first` above, or the pipelining peek
    // further down — so it isn't re-read.
    let mut pending: Option<Option<std::io::Result<protocol::Frame>>> = Some(first);
    loop {
        let next = match pending.take() {
            Some(n) => n,
            None => {
                // ... unchanged select! block ...
```

Everything from that `select!` block through the end of the function body is **unchanged** — do not touch it. Only the lines shown above move/change.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem --lib connection::tests:: 2>&1 | tail -60`
Expected: every test in `connection.rs` passes, including the 8 new ones. Pay particular attention to any pre-existing test that asserted on "connection accepted" timing or count — none currently do (confirm by reading the full failure list if anything unexpected breaks; if a pre-existing test relies on the log firing before the first frame is read, that test's assumption is exactly what this plan intentionally changes, and the test itself should be updated to match, not worked around).

Then run the full workspace bar:

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace 2>&1 | tail -80`
Expected: clean format, zero clippy warnings, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/connection.rs
```

Commit via the `1-git-commit` skill.

---

### Task 3: Live two-node verification and doc update

**Files:**
- No new source files. This task re-runs the same manual verification method already used to diagnose the bug (an isolated 2-node cluster with output redirected to files), against the built fix, and updates the one doc paragraph that currently promises quieter behavior than the code delivered.
- Modify: `docs/config-reference.md`'s "Cluster peer health is reported, never acted on" section (the paragraph beginning "In cluster mode, `rocket-mem` probes every other node...").

**Interfaces:**
- Consumes: the built `rocket-mem` binary from Tasks 1-2 (`cargo build --release -p rocket-mem`).
- Produces: nothing new for later tasks — this is the plan's final verification and doc-accuracy step.

- [ ] **Step 1: Build the release binary**

Run: `cargo build --release -p rocket-mem`
Expected: builds clean, no warnings (clippy already gated this in Task 2's step 4).

- [ ] **Step 2: Set up an isolated 2-node test cluster**

```bash
SCRATCH=$(mktemp -d)
cat > "$SCRATCH/cluster.conf" <<'EOF'
test-a 127.0.0.1:27101 0 8191
test-b 127.0.0.1:27102 8192 16383
EOF
cat > "$SCRATCH/node-a.toml" <<EOF
addr = "127.0.0.1:27101"
rmp_addr = "127.0.0.1:27201"
metrics_addr = "127.0.0.1:27301"
aof_path = "$SCRATCH/a.aof"
snapshot_path = "$SCRATCH/a.snapshot"
log_level = "info"
cluster_config = "$SCRATCH/cluster.conf"
cluster_node_id = "test-a"
cluster_probe_interval_secs = 2
cluster_node_timeout_secs = 30
EOF
cat > "$SCRATCH/node-b.toml" <<EOF
addr = "127.0.0.1:27102"
rmp_addr = "127.0.0.1:27202"
metrics_addr = "127.0.0.1:27302"
aof_path = "$SCRATCH/b.aof"
snapshot_path = "$SCRATCH/b.snapshot"
log_level = "info"
cluster_config = "$SCRATCH/cluster.conf"
cluster_node_id = "test-b"
cluster_probe_interval_secs = 2
cluster_node_timeout_secs = 30
EOF
cd "$SCRATCH"
./target/release/rocket-mem --config node-a.toml > a.log 2>&1 &
./target/release/rocket-mem --config node-b.toml > b.log 2>&1 &
```

Wait ~10 seconds (5 probe rounds at the configured 2s interval), then check:

```bash
grep -c "connection accepted" a.log b.log
```

Expected: **`0`** for both files at the `info` default — this is the direct before/after contrast with the diagnosis session, which showed a `connection accepted`/`connection closed` pair every `cluster_probe_interval_secs` seconds at `info` before this fix.

- [ ] **Step 3: Confirm the events still exist at debug, just quieter**

Restart both nodes with `log_level = "debug"` in place of `"info"` in both TOML files, wait ~10 seconds, then:

```bash
grep -c "connection accepted" a.log b.log
```

Expected: non-zero on both — proves the fix downgrades the level rather than deleting the event outright (an operator explicitly asking for `debug` still sees probe traffic, matching this repo's `debug` = "per-request activity" taxonomy).

Kill both test processes:

```bash
kill %1 %2
```

- [ ] **Step 4: Update the doc paragraph**

In `docs/config-reference.md`, the "Cluster peer health is reported, never acted on" section currently doesn't mention the accept/close side effect at all — add one sentence after the paragraph ending "...each state change is logged once — once per change, not once per probe.":

```markdown
Each probe round is also a real TCP connection to the peer, whose own `connection.rs` accept
loop would otherwise log a `connection accepted`/`connection closed` pair at `info` every
round regardless of this section's "once per change" behavior — rocket-mem recognizes its own
probe traffic and logs that pair at `debug` instead, so a healthy, unchanging cluster stays
quiet at the `info` default. See `crates/server/src/cluster_health.rs`'s `PROBE_MARKER`.
```

- [ ] **Step 5: Commit**

```bash
git add docs/config-reference.md
```

Commit via the `1-git-commit` skill. Suggested subject line for this task's commit: `Document that peer-probe connections log at debug, not info`.

---

## Next plan

`docs/superpowers/plans/2026-09-12-cluster-timer-rollout.md` (not yet written) — the operational follow-up identified during diagnosis, independent of this code fix: (1) correct the backwards `cluster_node_timeout_secs (15) < cluster_probe_interval_secs (20)` ordering in the three production `.toml` files (`rocket-mem.toml`, `rocket-mem-shard-b.toml`, `rocket-mem-shard-c.toml`), (2) rebuild and restart all six live hand-started processes so every one picks up both this plan's fix and the corrected timers (two of the six — shard-b's primary and shard-a's replica — were also confirmed running on a stale on-disk config as of this diagnosis), (3) re-run this plan's Task 3 verification method against the real `cluster.conf`/production topology instead of the synthetic 2-node one, to close the loop on the original symptom report.
