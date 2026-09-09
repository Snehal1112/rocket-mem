# Verbose Logging Plan 17: Replication Events

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Instrument `crates/server/src/replication.rs` and the leader-side `serve_replica` in `crates/server/src/connection.rs` with the `repl` span and the replication event catalogue: PSYNC handshake steps (debug), replica register/prune with addr (info), offset progress (trace), and per-command apply (debug).

**Architecture:** The `repl` span (field `host_port`) is the correlation backbone: it wraps the follower-side `replication_client_loop` and the leader-side `serve_replica`, so every event this plan adds — and the pre-existing `tracing::warn!`/`tracing::error!` calls at `replication.rs:524`, `:526`, and `:724` — inherits `host_port` for free, without touching those three existing call sites. This plan does Task 1 (the span) first, deliberately: every later task in this plan depends on it for correlation, and adding events before the span exists would mean re-touching every one of them later to thread `host_port` through by hand.

**Tech Stack:** Rust 2021, `tracing 0.1` (`#[tracing::instrument]`), `tracing-subscriber 0.3` (test-only capture, already a `crates/server` dependency).

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the Event catalogue's "Replication" row and the "three spans" decision's `repl` row.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting. The load-bearing ones here: `cargo clippy --workspace --all-targets -- -D warnings` must pass, every pre-existing test must pass unchanged, no `format!` outside a log macro's argument list, all fields use `%`/`?` so formatting stays lazy, `Bytes` is never logged via `Debug`, and no new atomic counters on the hot path (this plan's `frames_applied` counter in Task 3 is a plain function-local `u64`, not shared or atomic, for exactly this reason).

**Note on test infrastructure:** no log-capture test harness existed anywhere in `crates/server` before this plan (confirmed by a repo-wide search for `tracing_subscriber`, `set_default`, `MakeWriter`, and any custom `Layer` impl — there were none outside `main.rs`'s production logger init and plan 09's `capture_at` helper in `crates/server/tests/logging.rs`, which cannot be reused here: files under `tests/` compile as separate crates, so a helper defined there is not importable from a `#[cfg(test)] mod tests` inside `src/`). Task 1 of this plan defines the shared helper once, in `crates/server/src/logging.rs` under `#[cfg(test)] pub(crate) mod test_support`, using only crates already in `crates/server`'s `Cargo.toml` (`tracing-subscriber` is already a dependency) — a `tracing_subscriber::fmt` writer backed by a shared buffer, via `tracing-subscriber`'s own public `MakeWriter` trait, not a new dependency. Every task below that needs to assert on rendered log text imports it with `use crate::logging::test_support::CapturedLogs;` rather than redefining it.

---

### Task 1: Open the `repl` span on both replication call sites

**Files:**
- Modify: `crates/server/src/replication.rs` — `replication_client_loop` (signature at lines 496–504), its `#[cfg(test)] mod tests` (starts line 732)
- Modify: `crates/server/src/connection.rs` — `serve_replica` (signature at lines 287–293), its `#[cfg(test)] mod tests` (starts line 348)

**Interfaces:**
- Consumes: `tracing::instrument` (part of `tracing`'s default features, already a dependency of `crates/server`).
- Produces: a `repl` span, field `host_port`, wrapping the whole body of both functions. Tasks 2 and 3 of this plan, and every pre-existing `tracing::warn!`/`tracing::error!` call already inside these two functions (`replication.rs:524`, `:526`, `:724`), inherit this span's `host_port` field automatically — none of those three existing lines are edited by this plan.

- [ ] **Step 1: Add the shared log-capture test helper, then write the failing test for the follower-side span**

First, append the following to the END of `crates/server/src/logging.rs`, OUTSIDE that file's existing `#[cfg(test)] mod tests` block. This is the one, shared `CapturedLogs` test-capture helper that this plan's remaining tests, and plans 18 and 19, all import rather than redefining:

```rust
/// A log-capture harness for unit tests that need to assert on rendered log text.
///
/// Lives here, in the crate's own `src`, rather than in `tests/logging.rs`: Rust compiles each
/// file under `tests/` as a separate crate, so a helper there is not importable from a
/// `#[cfg(test)] mod tests` inside `src/`. Plan 09's `capture_at` serves the integration tests;
/// this serves the unit tests, and the two cannot be merged.
///
/// `#[cfg(test)]` and introduced at its first use (plan 17) rather than alongside `fmt_value`,
/// because an unused test helper would trip `clippy -D warnings`' dead-code lint.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    /// A `tracing_subscriber::fmt` writer backed by a shared buffer. Uses `tracing-subscriber`'s
    /// own public `MakeWriter` trait -- no new dependency.
    #[derive(Clone, Default)]
    pub(crate) struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            // `unwrap_or_else(|e| e.into_inner())` rather than `unwrap()`: a test that panics
            // while holding this lock would otherwise poison it and turn one real failure into
            // a cascade of unrelated ones.
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedLogs;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl CapturedLogs {
        /// The log text captured so far.
        pub(crate) fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
        }
    }
}
```

Then add to the existing `mod tests` in `crates/server/src/replication.rs` (after the last existing test, before the closing `}`), pulling in that shared helper via `use crate::logging::test_support::CapturedLogs;`:

```rust
    use crate::logging::test_support::CapturedLogs;

    #[tokio::test]
    async fn replication_client_loop_opens_a_repl_span_naming_the_leader_host_port() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let host_port = addr.to_string();

        let fake_leader = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut psync_bytes = [0u8; 15];
            socket.read_exact(&mut psync_bytes).await.unwrap();
            let blob = engine::Engine::new().snapshot(0);
            socket
                .write_all(&(blob.len() as u64).to_le_bytes())
                .await
                .unwrap();
            socket.write_all(&blob).await.unwrap();
            // keep the socket open long enough for the span to still be active when this test
            // samples the captured output below
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        });

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let engine = Arc::new(Engine::new());
        let generation = Arc::new(AtomicU64::new(0));
        let task = tokio::spawn(replication_client_loop(
            host_port.clone(),
            engine,
            Generation {
                counter: Arc::clone(&generation),
                mine: 0,
            },
            None,
            FollowerHandles {
                last_apply: Arc::new(AtomicI64::new(0)),
                link_up: Arc::new(AtomicBool::new(false)),
            },
            None,
            FollowerIdentity::default(),
        ));

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        task.abort();
        fake_leader.abort();
        drop(_guard);

        let text = captured.text();
        assert!(
            text.contains("repl") && text.contains("host_port") && text.contains(&host_port),
            "expected a new `repl` span carrying host_port={host_port:?}, got:\n{text}"
        );
    }
```

The `crates/server/src/logging.rs` change above is committed together with the rest of this task's changes in Step 10 below — add `crates/server/src/logging.rs` to that step's `git add`.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem replication::tests::replication_client_loop_opens_a_repl_span
```

Expected: FAIL — `text.contains("repl")` and `text.contains("host_port")` are both false, since `replication_client_loop` opens no span at all yet. The assertion message prints the (empty, or unrelated) captured text.

- [ ] **Step 3: Add the span to `replication_client_loop`**

In `crates/server/src/replication.rs`, directly above the function's doc comment (line 490) and signature (line 496):

```rust
/// The `repl` span is the correlation backbone for every replication log line on the follower
/// side: it wraps this whole function, so the pre-existing `tracing::warn!` reconnect-logging
/// calls inside the loop below, and every event this plan's later tasks add, inherit
/// `host_port` for free. Named `"repl"` explicitly (rather than the default, the function's own
/// name) to match the leader-side span opened in `connection.rs`'s `serve_replica` — one name,
/// grep-able from either end of a replication link. See
/// ../../docs/superpowers/specs/2026-09-09-verbose-logging-design.md's span table.
#[tracing::instrument(name = "repl", skip_all, fields(host_port = %host_port))]
async fn replication_client_loop(
    host_port: String,
    engine: Arc<Engine>,
    generation: Generation,
    aof: Option<Arc<AofWriter>>,
    handles: FollowerHandles,
    tls_client_config: Option<Arc<rustls::ClientConfig>>,
    identity: FollowerIdentity,
) {
```

Nothing else in the function body changes — `#[instrument]`'s `fields(host_port = %host_port)` records the field by borrowing `host_port` at span-creation time; the parameter itself is untouched and remains usable exactly as before (it already is, at the `%host_port` sites in the pre-existing `warn!` calls at lines 524 and 526).

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p rocket-mem replication::tests::replication_client_loop_opens_a_repl_span
```

Expected: PASS.

- [ ] **Step 5: Write the failing test for the leader-side span**

Add to the existing `mod tests` in `crates/server/src/connection.rs` (after the last existing test, before the closing `}`):

```rust
    use crate::logging::test_support::CapturedLogs;

    #[tokio::test]
    async fn serve_replica_opens_a_repl_span_naming_the_advertised_host_port() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("repl-span-test.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec::default());
        framed
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"PSYNC")),
                Frame::Bulk(Bytes::from_static(b"127.0.0.1:9999")),
            ]))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await; // let serve_replica open its span

        drop(_guard);
        let text = captured.text();
        assert!(
            text.contains("repl") && text.contains("host_port") && text.contains("127.0.0.1:9999"),
            "expected a new `repl` span carrying host_port=\"127.0.0.1:9999\", got:\n{text}"
        );
    }
```

- [ ] **Step 6: Run the test to verify it fails**

```bash
cargo test -p rocket-mem connection::tests::serve_replica_opens_a_repl_span
```

Expected: FAIL — `serve_replica` opens no span yet, so none of the three substrings are present in the (empty) captured text.

- [ ] **Step 7: Add the span to `serve_replica`**

In `crates/server/src/connection.rs`, directly above the function's doc comment (line 284) and signature (line 287):

```rust
/// The `repl` span is the correlation backbone for every replication log line on the leader
/// side, matching `replication.rs`'s follower-side span of the same name. `host_port` is the
/// address this replica advertised in its own `PSYNC <addr>` frame, or the fixed sentinel
/// `"unknown"` for a bare `PSYNC` (an old client, or a test) — see `psync_advertised_addr`'s
/// doc comment. See ../../docs/superpowers/specs/2026-09-09-verbose-logging-design.md's span
/// table.
#[tracing::instrument(
    name = "repl",
    skip_all,
    fields(host_port = %advertised_addr.clone().unwrap_or_else(|| "unknown".to_string()))
)]
async fn serve_replica<S>(
    framed: Framed<S, RespCodec>,
    aof: &AofWriter,
    replication: &crate::replication::ReplicationHandle,
    advertised_addr: Option<String>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
```

`advertised_addr` is only cloned here (never moved), so it remains available, untouched, for the registration call later in the body (and for Task 2 of this plan, which reads it again).

- [ ] **Step 8: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem connection::tests::serve_replica_opens_a_repl_span
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: both new tests PASS, fmt clean, clippy clean, and every pre-existing test — including `crates/server/tests/replication.rs`'s full leader/follower integration suite — still passes unchanged.

- [ ] **Step 9: Manual check**

Follow `.claude/manual-testing.md`'s "Replication (`REPLICAOF`)" section to bring up a leader/follower pair, then re-run it with `RUST_LOG=info` set on both processes. Confirm every log line printed by either process while replicating carries `repl{host_port="..."}` in its prefix, and that the two processes show consistent addresses for each other.

- [ ] **Step 10: Commit**

```bash
git add crates/server/src/replication.rs crates/server/src/connection.rs crates/server/src/logging.rs
git commit -m "feat(logging): open the repl span on both replication call sites"
```

---

### Task 2: PSYNC handshake steps and replica register/prune events

**Files:**
- Modify: `crates/server/src/replication.rs` — `sync_once` (body starts line 594; AUTH block lines 613–634; PSYNC send at line 650; blob read completing at line 689; `load_snapshot` call at line 694–696)
- Modify: `crates/server/src/connection.rs` — `serve_replica`'s registration block (lines 306–312)
- Modify: `crates/server/src/replication.rs` — `ReplicaRegistry::broadcast` (lines 55–58)

**Interfaces:**
- Consumes: the `repl` span from Task 1 (already wraps `sync_once` via `replication_client_loop` → `connect_and_sync` → `sync_once`, and wraps `serve_replica` directly), so none of the calls below need to pass `host_port` explicitly.
- Produces: nothing consumed by a later plan — these are leaf log lines.

- [ ] **Step 1: Write the failing test for PSYNC handshake debug lines**

Add to the existing `mod tests` in `crates/server/src/replication.rs`:

```rust
    #[tokio::test]
    async fn sync_once_logs_each_psync_handshake_step_at_debug() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut psync_bytes = [0u8; 15];
            socket.read_exact(&mut psync_bytes).await.unwrap();
            let blob = engine::Engine::new().snapshot(0);
            socket
                .write_all(&(blob.len() as u64).to_le_bytes())
                .await
                .unwrap();
            socket.write_all(&blob).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        });

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let engine = engine::Engine::new();
        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
        let stream = tokio::net::TcpStream::connect(&host_port).await.unwrap();
        let sync_task = tokio::spawn(async move {
            sync_once(
                stream,
                &engine,
                &generation,
                0,
                None,
                FollowerStatus {
                    last_apply: &AtomicI64::new(0),
                    link_up: &AtomicBool::new(false),
                },
                &FollowerIdentity::default(),
            )
            .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        sync_task.abort();
        fake_leader.abort();
        drop(_guard);

        let text = captured.text();
        assert!(text.contains("sending PSYNC to leader"), "{text}");
        assert!(text.contains("received snapshot blob from leader"), "{text}");
        assert!(text.contains("snapshot loaded"), "{text}");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem replication::tests::sync_once_logs_each_psync_handshake_step
```

Expected: FAIL — none of the three debug messages exist yet, so all three `assert!`s fail on the (empty) captured text.

- [ ] **Step 3: Add the PSYNC handshake debug lines to `sync_once`**

In `crates/server/src/replication.rs`, inside the AUTH block (around lines 613–634), add a debug line before sending AUTH and one in the success arm:

```rust
    if let Some((username, password)) = &identity.auth {
        tracing::debug!("sending AUTH to leader");
        framed
            .send(protocol::Frame::Array(vec![
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"AUTH")),
                protocol::Frame::Bulk(bytes::Bytes::copy_from_slice(username.as_bytes())),
                protocol::Frame::Bulk(bytes::Bytes::copy_from_slice(password.as_bytes())),
            ]))
            .await?;
        match framed.next().await {
            Some(Ok(protocol::Frame::Error(e))) => {
                return Err(std::io::Error::other(format!("leader rejected AUTH: {e}")))
            }
            Some(Ok(_)) => tracing::debug!("leader accepted AUTH"), // +OK -- proceed to PSYNC
            Some(Err(e)) => return Err(e),
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "leader closed the connection during AUTH",
                ))
            }
        }
    }
```

Directly before `framed.send(psync_frame).await?;` (line 650):

```rust
    tracing::debug!("sending PSYNC to leader");
    framed.send(psync_frame).await?;
```

Directly after `parts.io.read_exact(&mut blob).await?;` (line 689):

```rust
    parts.io.read_exact(&mut blob).await?;
    tracing::debug!(bytes = len, "received snapshot blob from leader");
```

Directly after the `load_snapshot` call succeeds (lines 694–696):

```rust
    engine
        .load_snapshot(&blob)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    tracing::debug!("snapshot loaded");
    status.link_up.store(true, Ordering::Relaxed);
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p rocket-mem replication::tests::sync_once_logs_each_psync_handshake_step
```

Expected: PASS.

- [ ] **Step 5: Write the failing test for replica register/prune info lines**

Add to the existing `mod tests` in `crates/server/src/connection.rs`:

```rust
    #[tokio::test]
    async fn a_replica_registering_and_being_pruned_are_both_logged_at_info() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("repl-register-prune-test.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let framed = Framed::new(TcpStream::connect(addr).await.unwrap(), RespCodec::default());
        let mut framed = framed;
        framed
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"PSYNC")),
                Frame::Bulk(Bytes::from_static(b"127.0.0.1:6480")),
            ]))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await; // let serve_replica register
        drop(framed); // disconnect the replica

        // two broadcasts, matching the existing pruning test's own reasoning: the first send
        // after a drop can still succeed on some platforms before the OS notices the close.
        let mut client = Framed::new(TcpStream::connect(addr).await.unwrap(), RespCodec::default());
        for _ in 0..2 {
            client
                .send(Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"SET")),
                    Frame::Bulk(Bytes::from_static(b"k")),
                    Frame::Bulk(Bytes::from_static(b"v")),
                ]))
                .await
                .unwrap();
            client.next().await.unwrap().unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        drop(_guard);
        let text = captured.text();
        assert!(
            text.contains("replica registered") && text.contains("127.0.0.1:6480"),
            "expected a registration log naming the advertised address:\n{text}"
        );
        assert!(
            text.contains("replica pruned") && text.contains("127.0.0.1:6480"),
            "expected a prune log naming the same address:\n{text}"
        );
    }
```

- [ ] **Step 6: Run the test to verify it fails**

```bash
cargo test -p rocket-mem connection::tests::a_replica_registering_and_being_pruned
```

Expected: FAIL — neither "replica registered" nor "replica pruned" exists yet.

- [ ] **Step 7: Add the registration log to `serve_replica`**

In `crates/server/src/connection.rs`, inside the critical section (lines 306–312):

```rust
    let (snapshot_bytes, mut rx) = {
        let _order_guard = aof.lock_all_shards();
        let bytes = replication.engine().snapshot(0); // 0: a follower keeps no AOF, so the header is moot
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        let host_port = advertised_addr.clone().unwrap_or_else(|| "unknown".to_string());
        replication.registry.register(advertised_addr, tx);
        tracing::info!(host_port = %host_port, "replica registered");
        (bytes, rx)
    };
```

`host_port` is cloned from `advertised_addr` before `register` moves it — the same pattern the `#[instrument]` field expression on this function already uses, just captured into a local so it survives the move.

- [ ] **Step 8: Add the prune log to `ReplicaRegistry::broadcast`**

In `crates/server/src/replication.rs` (lines 55–58):

```rust
    pub fn broadcast(&self, bytes: bytes::Bytes) {
        let mut replicas = self.replicas.lock().unwrap_or_else(|e| e.into_inner());
        replicas.retain(|(addr, tx)| {
            let alive = tx.send(bytes.clone()).is_ok();
            if !alive {
                tracing::info!(
                    host_port = %addr.clone().unwrap_or_else(|| "unknown".to_string()),
                    "replica pruned"
                );
            }
            alive
        });
    }
```

This fires only for a replica actually being removed (the rare case), not on every fan-out — the common all-alive case pays one extra `bool` check per replica and no logging call at all.

- [ ] **Step 9: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem connection::tests::a_replica_registering_and_being_pruned
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: both new tests from this task PASS, fmt clean, clippy clean, full workspace suite green — including `crates/server/tests/replication.rs`, which exercises both registration and the pruning path already (see its comments about `serve_replica`'s snapshot+register critical section).

- [ ] **Step 10: Manual check**

Per `.claude/manual-testing.md`'s Replication section, start a leader and a follower with `RUST_LOG=debug`. On the follower's log, confirm `sending PSYNC to leader`, `received snapshot blob from leader`, and `snapshot loaded` appear in order inside a `repl{host_port=...}` span. On the leader's log, confirm `replica registered` appears once the follower connects, and `replica pruned` appears after `kill`ing the follower and issuing one more write on the leader.

- [ ] **Step 11: Commit**

```bash
git add crates/server/src/replication.rs crates/server/src/connection.rs
git commit -m "feat(logging): add PSYNC handshake and replica register/prune events"
```

---

### Task 3: Replication stream offset progress and per-command apply events

**Files:**
- Modify: `crates/server/src/replication.rs` — `sync_once`'s frame-apply loop (lines 702–727)

**Interfaces:**
- Consumes: the `repl` span from Task 1 (wraps this loop already, via `replication_client_loop` → `connect_and_sync` → `sync_once`).
- Produces: nothing consumed by a later plan.

- [ ] **Step 1: Write the failing test**

Add to the existing `mod tests` in `crates/server/src/replication.rs`. This extends the existing `sync_once_loads_the_snapshot_then_applies_streamed_frames` fixture (same fake-leader shape: a snapshot blob, then one streamed `SET`) with log capture:

```rust
    #[tokio::test]
    async fn sync_once_logs_stream_offset_and_the_applied_command_name() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut psync_bytes = [0u8; 15];
            socket.read_exact(&mut psync_bytes).await.unwrap();

            let snapshot_engine = engine::Engine::new();
            let blob = snapshot_engine.snapshot(0);
            socket
                .write_all(&(blob.len() as u64).to_le_bytes())
                .await
                .unwrap();
            socket.write_all(&blob).await.unwrap();

            socket
                .write_all(b"*3\r\n$3\r\nSET\r\n$11\r\nfrom-stream\r\n$1\r\nv\r\n")
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let engine = std::sync::Arc::new(engine::Engine::new());
        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
        let sync_task = {
            let engine = std::sync::Arc::clone(&engine);
            tokio::spawn(async move {
                let stream = tokio::net::TcpStream::connect(&host_port).await.unwrap();
                sync_once(
                    stream,
                    &engine,
                    &generation,
                    0,
                    None,
                    FollowerStatus {
                        last_apply: &AtomicI64::new(0),
                        link_up: &AtomicBool::new(false),
                    },
                    &FollowerIdentity::default(),
                )
                .await
            })
        };

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        sync_task.abort();
        fake_leader.abort();
        drop(_guard);

        let text = captured.text();
        assert!(
            text.contains("replication stream advanced") && text.contains("offset"),
            "expected an offset-progress trace line:\n{text}"
        );
        assert!(
            text.contains("applied replicated command") && text.contains("SET"),
            "expected a per-command apply debug line naming SET:\n{text}"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem replication::tests::sync_once_logs_stream_offset_and_the_applied_command_name
```

Expected: FAIL — neither "replication stream advanced" nor "applied replicated command" exists yet.

- [ ] **Step 3: Add a local command-name helper**

`sync_once` has no access to `dispatcher::command_name_upper` (private to `dispatcher.rs`). Add a small local helper to `crates/server/src/replication.rs`, directly above `sync_once`, mirroring `connection.rs`'s own `is_psync_command`/`psync_advertised_addr`, which face the identical problem for PSYNC detection:

```rust
/// Uppercased command name from a replicated frame's first element, for the per-command apply
/// debug log. `sync_once` has no access to `dispatcher::command_name_upper` (private to that
/// module); duplicating this tiny extraction locally matches `connection.rs`'s own
/// `is_psync_command`/`psync_advertised_addr`, which solve the same cross-module-visibility
/// problem for PSYNC detection. Not on the client dispatch hot path the 2% benchmark gate
/// covers -- this runs once per replicated frame on the follower's own apply loop, which
/// already pays for a full `dispatch` call per frame.
fn replicated_command_name(frame: &protocol::Frame) -> String {
    let protocol::Frame::Array(items) = frame else {
        return "?".to_string();
    };
    let Some(protocol::Frame::Bulk(name)) = items.first() else {
        return "?".to_string();
    };
    String::from_utf8_lossy(name).to_uppercase()
}
```

- [ ] **Step 4: Add the offset-progress and per-command apply logs**

In `crates/server/src/replication.rs`, replace the frame-apply loop (lines 702–727):

```rust
    let mut framed = tokio_util::codec::Framed::from_parts(parts);
    let mut frames_applied: u64 = 0; // per-session counter, not persisted or shared -- see Global Constraints
    while let Some(result) = framed.next().await {
        if generation.load(Ordering::SeqCst) != my_generation {
            return Ok(()); // superseded -- stop applying frames to state a newer task now owns
        }
        let frame = result?;
        frames_applied += 1;
        tracing::trace!(offset = frames_applied, "replication stream advanced");
        let name = replicated_command_name(&frame);
        let mut protocol = protocol::codec::Protocol::default();
        let _order_guard = aof.map(|a| a.lock_all_shards());
        let reply = crate::dispatcher::dispatch(engine, frame, &mut protocol, 0);
        tracing::debug!(cmd = %name, "applied replicated command");
        if let protocol::Frame::Error(e) = reply {
            tracing::error!(error = %e, "failed to apply replicated command");
        }
        status.last_apply.store(unix_now_secs(), Ordering::Relaxed);
    }
    Ok(())
```

`name` is computed from `&frame` before `frame` is moved into `dispatch`, matching the ordering constraint `dispatch(engine, frame, ...)`'s by-value signature imposes. `frames_applied` is a plain local `u64`, scoped to this one sync session (reset to 0 on every reconnect) — not a `ReplicationHandle` field, not atomic, not shared: exactly what the Global Constraints' "no new atomic counters on the hot path" is protecting against introducing.

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem replication::tests::sync_once_logs_stream_offset_and_the_applied_command_name
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: the new test PASSes, fmt clean, clippy clean, full workspace suite green — including the "torn snapshot" stress test (`a_save_racing_the_apply_loop_never_observes_a_half_applied_multi_key_write`), which drives this exact loop under load and must keep passing unchanged.

- [ ] **Step 6: Benchmark gate**

This plan's changes are all inside the follower's replication apply path, not the client command-dispatch path the 2% gate covers, so no `scripts/benchmark.sh` re-run is required for this plan specifically. Confirm this reasoning still holds by checking that none of Tasks 1–3 touched `dispatcher.rs`'s `dispatch_and_log` or any code on its call path.

- [ ] **Step 7: Manual check**

Per `.claude/manual-testing.md`'s Replication section, start a leader and follower with `RUST_LOG=trace` on the follower. Issue several `SET`s on the leader and confirm the follower's log shows one `replication stream advanced` trace line per write with a strictly increasing `offset`, each immediately followed by an `applied replicated command` debug line naming `SET`.

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "feat(logging): add replication offset progress and per-command apply events"
```

---

## Next plan

[`18-cluster-events.md`](18-cluster-events.md) — logs the cluster topology once at startup and a `MOVED` redirect's key/slot/target node on the dispatch hot path.
