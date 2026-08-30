# REPLICAOF & Follower Apply Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `REPLICAOF host port` turns a node into a read-only follower that syncs from a leader in the background and keeps itself in sync forever (reconnecting on any failure); `REPLICAOF NO ONE` turns it back into a normal, writable node.

**Architecture:** `ReplicationHandle` gains `start_replicating`/`stop_replicating`, both serialized under the existing `follower_task` mutex so concurrent `REPLICAOF` calls can't race two apply loops into the same `Engine`. A new `replication_client_loop` (spawned, cancelled via `JoinHandle::abort()` — never a cooperative flag, since the only await points in its inner loop are socket reads) connects, `PSYNC`s, loads the snapshot, then applies every subsequent frame via plain `dispatch()` — the same function AOF replay already uses, so no new command-application path exists anywhere in this project. `dispatch_and_log` gains a `-READONLY` gate ahead of everything else in its body, so a rejected write never touches the AOF ordering lock.

**Tech Stack:** `tokio::task::JoinHandle`, `tokio_util::codec::{Framed, FramedParts}` (all already workspace dependencies).

**Spec:** `../../specs/2026-08-30-sprint-5-spec.md` — "a follower keeps no AOF; it applies the leader's stream via plain `dispatch()`..." and "read-only enforcement is one `AtomicBool` role flag..." are authoritative for this plan. Depends on `04-replica-registry-and-leader-fanout.md`'s `PSYNC`/`serve_replica` (the leader side this plan's follower connects to).

## Global Constraints

- A follower keeps no AOF of its own this sprint. `replication_client_loop` applies every frame via plain `dispatch()`, never `dispatch_and_log` — replicated writes are never logged, never re-broadcast, and never subject to the `-READONLY` gate (which only exists to reject *client*-originated writes).
- Cancellation is always `JoinHandle::abort()`, never a cooperative flag — the follower loop's only await points are socket reads, so a flag would leave a stale task parked indefinitely against an idle stream, still applying writes after an operator believes the node is standalone again.
- Reconnect is always a fresh, full resync — there is no code-level distinction between "first sync" and "resync after disconnect." An applied frame that itself returns `Frame::Error` is logged and the loop continues; it is not treated as a reason to disconnect and resync.

---

### Task 1: `replication_client_loop` and `ReplicationHandle::start_replicating`/`stop_replicating`

**Files:**
- Modify: `crates/server/src/replication.rs`

**Interfaces:**
- Consumes: `Engine::load_snapshot` (`01-snapshot-serialization.md`), `dispatcher::dispatch` (existing), the leader-side `PSYNC` handling this connects to (`04-replica-registry-and-leader-fanout.md`).
- Produces: `ReplicationHandle::start_replicating(&self, host_port: String)` and `ReplicationHandle::stop_replicating(&self)`, both `pub`, consumed by Task 2 below.

- [x] **Step 1: Write the failing test**

```rust
// crates/server/src/replication.rs — add to the existing tests module
#[tokio::test]
async fn sync_once_loads_the_snapshot_then_applies_streamed_frames() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let fake_leader = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // consume the PSYNC frame the follower sends: `*1\r\n$5\r\nPSYNC\r\n` is exactly 15 bytes
        let mut psync_bytes = [0u8; 15];
        socket.read_exact(&mut psync_bytes).await.unwrap();

        let snapshot_engine = engine::Engine::new();
        snapshot_engine.set(
            bytes::Bytes::from_static(b"from-snapshot"),
            engine::Value::String(bytes::Bytes::from_static(b"v")),
        );
        let blob = snapshot_engine.snapshot(0);
        socket.write_all(&(blob.len() as u64).to_le_bytes()).await.unwrap();
        socket.write_all(&blob).await.unwrap();

        socket
            .write_all(b"*3\r\n$3\r\nSET\r\n$11\r\nfrom-stream\r\n$1\r\nv\r\n")
            .await
            .unwrap();
        // keep the socket open long enough for the follower to read and apply that frame
        // before this task (and its socket) drops
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    });

    let engine = std::sync::Arc::new(engine::Engine::new());
    let host_port = addr.to_string();
    let sync_task = {
        let engine = std::sync::Arc::clone(&engine);
        tokio::spawn(async move { sync_once(&host_port, &engine).await })
    };

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    sync_task.abort();
    fake_leader.await.unwrap();

    assert_eq!(
        engine.get(b"from-snapshot"),
        Some(engine::Value::String(bytes::Bytes::from_static(b"v")))
    );
    assert_eq!(
        engine.get(b"from-stream"),
        Some(engine::Value::String(bytes::Bytes::from_static(b"v")))
    );
}

#[tokio::test]
async fn start_replicating_sets_is_replica_and_stop_replicating_clears_it() {
    let handle = ReplicationHandle::new(std::sync::Arc::new(engine::Engine::new()), "/tmp/unused.snapshot".into());
    assert!(!handle.is_replica.load(std::sync::atomic::Ordering::Relaxed));

    // "127.0.0.1:1" is a real address nothing listens on — start_replicating doesn't wait
    // for the connection to succeed, so this returns immediately regardless
    handle.start_replicating("127.0.0.1:1".to_string());
    assert!(handle.is_replica.load(std::sync::atomic::Ordering::Relaxed));

    handle.stop_replicating();
    assert!(!handle.is_replica.load(std::sync::atomic::Ordering::Relaxed));
}

#[tokio::test]
async fn start_replicating_twice_cancels_the_first_task_before_starting_the_second() {
    let handle = ReplicationHandle::new(std::sync::Arc::new(engine::Engine::new()), "/tmp/unused.snapshot".into());
    handle.start_replicating("127.0.0.1:1".to_string());
    handle.start_replicating("127.0.0.1:2".to_string()); // must not panic or leave two tasks running
    assert!(handle.is_replica.load(std::sync::atomic::Ordering::Relaxed));
    handle.stop_replicating();
}
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem replication::tests::sync_once replication::tests::start_replicating`
Expected: FAIL — `sync_once`, `start_replicating`, `stop_replicating` don't exist yet

- [x] **Step 3: Implement**

```rust
// crates/server/src/replication.rs — add near the bottom, above the tests module
use futures_util::{SinkExt, StreamExt};

impl ReplicationHandle {
    /// Cancels any currently-running replication task, then spawns a new one against
    /// `host_port` and sets `is_replica`. The whole sequence — abort old, spawn new, store,
    /// set the flag — happens under `follower_task`'s mutex, so two clients issuing
    /// `REPLICAOF` concurrently can only serialize, never leave two apply loops racing into
    /// the same `Engine`.
    pub fn start_replicating(&self, host_port: String) {
        let mut task = self.follower_task.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = task.take() {
            old.abort();
        }
        let engine = Arc::clone(&self.engine);
        *task = Some(tokio::spawn(replication_client_loop(host_port, engine)));
        self.is_replica.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Cancels the running replication task (if any) and returns this node to normal,
    /// writable operation.
    pub fn stop_replicating(&self) {
        let mut task = self.follower_task.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = task.take() {
            old.abort();
        }
        self.is_replica.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Connects to `host_port`, syncs, applies the leader's stream forever, and reconnects (after
/// a fixed ~1s backoff) on any failure — including the leader simply closing the connection.
/// There is no distinction between "first sync" and "resync after disconnect": both run this
/// same loop body.
async fn replication_client_loop(host_port: String, engine: Arc<Engine>) {
    loop {
        match sync_once(&host_port, &engine).await {
            Ok(()) => eprintln!("replication: connection to {host_port} closed; reconnecting"),
            Err(e) => eprintln!("replication: lost connection to {host_port}: {e}; reconnecting"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// One full sync: connect, `PSYNC`, load the snapshot, then apply every subsequent frame
/// until the connection ends (cleanly or with an error). Never called `dispatch_and_log` —
/// see this plan's Global Constraints.
async fn sync_once(host_port: &str, engine: &Engine) -> std::io::Result<()> {
    let stream = tokio::net::TcpStream::connect(host_port).await?;
    let mut framed = tokio_util::codec::Framed::new(stream, protocol::codec::RespCodec::default());
    framed
        .send(protocol::Frame::Array(vec![protocol::Frame::Bulk(
            bytes::Bytes::from_static(b"PSYNC"),
        )]))
        .await?;

    // Reclaim the raw socket to read the length-prefixed snapshot blob, which is NOT a RESP
    // frame — decoding it through RespCodec would desync the stream entirely. `read_buf` is
    // guaranteed empty here: this Framed has never had `next()`/`decode` called on it, only
    // `send()`, so nothing has been read from the socket yet on the codec side.
    let mut parts = framed.into_parts();
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 8];
    parts.io.read_exact(&mut len_buf).await?;
    let len = u64::from_le_bytes(len_buf) as usize;
    let mut blob = vec![0u8; len];
    parts.io.read_exact(&mut blob).await?;
    engine
        .load_snapshot(&blob)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    // From here on the leader sends plain RESP frames, byte-for-byte what its own AOF
    // received — rebuild a Framed over the same socket (whose read position is exactly past
    // the blob) to resume decoding normally.
    let mut framed = tokio_util::codec::Framed::from_parts(parts);
    while let Some(result) = framed.next().await {
        let frame = result?;
        let mut protocol = protocol::codec::Protocol::default();
        let reply = crate::dispatcher::dispatch(engine, frame, &mut protocol, 0);
        // A leader only ever fans out a command whose local execution already succeeded, so
        // an error applying it here means the two sides have genuinely diverged (a bug, or
        // version skew) — logged and skipped, not a reason to tear down and resync, which
        // would just reproduce the same error against the same divergence.
        if let protocol::Frame::Error(e) = reply {
            eprintln!("replication: applying a replicated command failed: {e}");
        }
    }
    Ok(())
}
```

Add `use std::sync::Arc;` to `replication.rs`'s existing imports if not already present from earlier plans in this sprint.

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem replication::tests`
Expected: PASS

- [x] **Step 5: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "feat(server): add replication_client_loop and start/stop_replicating"
```

---

### Task 2: `REPLICAOF`/`REPLICAOF NO ONE` interception

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `ReplicationHandle::start_replicating`/`stop_replicating` (Task 1).
- Produces: `REPLICAOF`/`REPLICAOF NO ONE` as working commands, reachable over RESP.

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[tokio::test]
async fn replicaof_with_host_and_port_returns_ok_and_marks_the_node_a_replica() {
    let engine = std::sync::Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), "/tmp/unused.snapshot".into());

    let reply = dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"REPLICAOF", b"127.0.0.1", b"1"]), // port 1: nothing listens there, connection attempt fails harmlessly in the background
        &mut Protocol::default(),
        1,
    );
    assert_eq!(reply, Frame::Simple("OK".into()));
    assert!(replication.is_replica.load(std::sync::atomic::Ordering::Relaxed));
    replication.stop_replicating(); // clean up the background task this test started
}

#[tokio::test]
async fn replicaof_no_one_returns_ok_and_clears_replica_status() {
    let engine = std::sync::Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), "/tmp/unused.snapshot".into());
    replication.start_replicating("127.0.0.1:1".to_string());

    let reply = dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"REPLICAOF", b"NO", b"ONE"]),
        &mut Protocol::default(),
        1,
    );
    assert_eq!(reply, Frame::Simple("OK".into()));
    assert!(!replication.is_replica.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn replicaof_with_the_wrong_number_of_arguments_is_a_resp_error() {
    let engine = std::sync::Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), "/tmp/unused.snapshot".into());
    let reply = dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"REPLICAOF", b"onlyhost"]),
        &mut Protocol::default(),
        1,
    );
    assert_eq!(
        reply,
        Frame::Error("ERR wrong number of arguments for 'replicaof' command".into())
    );
}
```

Note the first two tests are `#[tokio::test]`, not plain `#[test]` — `start_replicating` calls `tokio::spawn`, which panics outside a running runtime.

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::replicaof`
Expected: FAIL — `REPLICAOF` currently falls through to `dispatch`'s unknown-command path

- [x] **Step 3: Implement the interception**

Add to `dispatch_and_log`'s body, right after the existing `SAVE` interception from `03-replication-handle-and-save.md`:

```rust
// crates/server/src/dispatcher.rs — inside dispatch_and_log
if is_save_command(&frame) {
    return handle_save(aof, replication);
}
if let Some(reply) = handle_replicaof(&frame, replication) {
    return reply;
}
```

Add the new function near `handle_save`:

```rust
// crates/server/src/dispatcher.rs
/// Returns `Some(reply)` if `frame` was `REPLICAOF` (in either form) — handled entirely here,
/// never reaching `dispatch` — or `None` if `frame` was some other command, in which case the
/// caller falls through to its normal handling.
fn handle_replicaof(frame: &Frame, replication: &crate::replication::ReplicationHandle) -> Option<Frame> {
    let Frame::Array(items) = frame else { return None };
    let Some(Frame::Bulk(name)) = items.first() else { return None };
    if !name.eq_ignore_ascii_case(b"REPLICAOF") {
        return None;
    }
    if items.len() != 3 {
        return Some(Frame::Error(
            "ERR wrong number of arguments for 'replicaof' command".into(),
        ));
    }
    let (Frame::Bulk(a), Frame::Bulk(b)) = (&items[1], &items[2]) else {
        return Some(Frame::Error(
            "ERR wrong number of arguments for 'replicaof' command".into(),
        ));
    };

    if a.eq_ignore_ascii_case(b"NO") && b.eq_ignore_ascii_case(b"ONE") {
        replication.stop_replicating();
    } else {
        let host = String::from_utf8_lossy(a);
        let port = String::from_utf8_lossy(b);
        replication.start_replicating(format!("{host}:{port}"));
    }
    Some(Frame::Simple("OK".into()))
}
```

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests::replicaof`
Expected: PASS

- [x] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): add REPLICAOF and REPLICAOF NO ONE"
```

---

### Task 3: the `-READONLY` gate

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `ReplicationHandle::is_replica` (`03`), `extract_write_command_name` (existing), `crate::aof::WRITE_COMMANDS` (existing).
- Produces: a write command from a normal client is rejected on a replica node; nothing new consumed by later plans.

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn a_write_command_on_a_replica_is_rejected_with_readonly() {
    let engine = std::sync::Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), "/tmp/unused.snapshot".into());
    replication.is_replica.store(true, std::sync::atomic::Ordering::Relaxed);

    let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(
        reply,
        Frame::Error("READONLY You can't write against a read only replica.".into())
    );
    assert_eq!(engine.get(b"k"), None); // the write must never have reached the engine
}

#[test]
fn a_read_command_on_a_replica_is_not_gated() {
    let engine = std::sync::Arc::new(Engine::new());
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    let (_dir, aof) = test_aof();
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), "/tmp/unused.snapshot".into());
    replication.is_replica.store(true, std::sync::atomic::Ordering::Relaxed);

    let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1);
    assert_eq!(reply, Frame::Bulk(Bytes::from_static(b"v")));
}

#[test]
fn save_is_not_gated_on_a_replica() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path);
    replication.is_replica.store(true, std::sync::atomic::Ordering::Relaxed);

    let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SAVE"]), &mut Protocol::default(), 1);
    assert_eq!(reply, Frame::Simple("OK".into()));
}

#[test]
fn a_write_command_when_not_a_replica_is_unaffected() {
    let engine = std::sync::Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), "/tmp/unused.snapshot".into());

    let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(reply, Frame::Simple("OK".into()));
}
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::a_write_command_on_a_replica dispatcher::tests::a_read_command_on_a_replica dispatcher::tests::save_is_not_gated`
Expected: FAIL — `a_write_command_on_a_replica_is_rejected_with_readonly` fails because the write currently succeeds; the other two pass already (nothing gates them yet), which is expected and fine — they exist as regression guards for Step 3

- [x] **Step 3: Implement the gate**

Add at the very top of `dispatch_and_log`'s body, before the `SAVE`/`REPLICAOF` interceptions from `03`/this plan's Task 2:

```rust
// crates/server/src/dispatcher.rs — the new first lines of dispatch_and_log's body
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
    // Checked before anything else in this function, including the SAVE/REPLICAOF
    // interceptions below (both are no-ops against WRITE_COMMANDS so ordering relative to
    // them doesn't matter) and extract_write_command_name's own later call further down (so
    // a rejected write never touches the AOF ordering lock).
    if replication.is_replica.load(std::sync::atomic::Ordering::Relaxed) {
        if extract_write_command_name(&frame).is_some() {
            return Frame::Error("READONLY You can't write against a read only replica.".into());
        }
    }

    if is_save_command(&frame) {
        return handle_save(aof, replication);
    }
    if let Some(reply) = handle_replicaof(&frame, replication) {
        return reply;
    }

    let original_frame = frame.clone();
    // ... rest of the existing body, unchanged
```

`Ordering::Relaxed` is deliberate: the flag guards nothing but itself, and a client whose write races the exact instant of a role change may legitimately land on either side of it — there's no stronger ordering requirement to uphold here.

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, every test in the module

- [x] **Step 5: Run the full workspace verification**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean/green

- [x] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): reject client writes on a read-only replica"
```
