# Replica Registry & Leader Fan-Out Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a leader accepts `PSYNC` connections, sends each one a consistent snapshot, and fans every subsequent write out to all of them, using the exact already-rewritten bytes the AOF already logs.

**Architecture:** `ReplicaRegistry` (skeleton from `03-replication-handle-and-save.md`) gains `register`/`broadcast` over an unbounded `mpsc` channel per replica. `aof.rs` splits `append` into `encode_frame`/`append_encoded` so `dispatch_and_log` can encode a write once and reuse the bytes for both the AOF and the fan-out. `connection.rs` intercepts `PSYNC` before it reaches `dispatch_and_log` (it needs the raw socket, which no dispatcher function has), snapshotting and registering the replica inside one `AofWriter::lock_for_ordering()` critical section so no write is ever lost or duplicated across that boundary.

**Tech Stack:** `tokio::sync::mpsc` (unbounded), `tokio::io::AsyncWriteExt` (already a workspace dependency via `tokio`'s `io-util` feature).

**Spec:** `../../specs/2026-08-30-sprint-5-spec.md` — "replication transport reuses the RESP port..." is authoritative for this plan. Depends on `01-snapshot-serialization.md` (`Engine::snapshot`) and `03-replication-handle-and-save.md` (`ReplicationHandle`, `ReplicaRegistry` skeleton).

## Global Constraints

- The workspace's `tokio` dependency (`Cargo.toml` root, `[workspace.dependencies]`) currently enables `["rt-multi-thread", "macros", "net", "io-util", "time"]` — no `"sync"`. `tokio::sync::mpsc`, which this plan introduces (`ReplicaRegistry`), is compile-blocked without it. Task 1's Step 1 below adds it before anything else in this plan.
- Lock ordering is fixed: `AofWriter::lock_for_ordering()` is always acquired *before* `ReplicaRegistry`'s own internal mutex, never the reverse — this holds in both places this plan takes both locks (`serve_replica`'s snapshot-and-register section, and `dispatch_and_log`'s fan-out hook), so there is no path to a lock-ordering inversion.
- The replica fan-out channel is unbounded, deliberately: `broadcast` runs inside `lock_for_ordering()`'s critical section, so a bounded channel filled by one stalled replica would block every write on the leader. A stalled replica instead grows the leader's memory, invisibly to `MAXMEMORY` accounting — a known, documented limit of this sprint's replication, not an oversight.
- `PSYNC` never reaches `dispatch` or `dispatch_and_log` — it's intercepted in `connection.rs::handle_connection` itself, since serving it means taking ownership of the whole socket, which no dispatcher function has access to.

---

### Task 1: `ReplicaRegistry::register`/`broadcast`

**Files:**
- Modify: `Cargo.toml` (workspace root — adds tokio's `sync` feature)
- Modify: `crates/server/src/replication.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `ReplicaRegistry::register(&self, sender: mpsc::UnboundedSender<Bytes>)` and `ReplicaRegistry::broadcast(&self, bytes: Bytes)`, both `pub`, consumed by Task 3 (fan-out hook) and Task 4 (`serve_replica`).

- [ ] **Step 1: Enable tokio's `sync` feature**

```toml
# Cargo.toml (workspace root) — add "sync" to tokio's existing feature list
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "io-util", "time", "sync"] }
```

- [ ] **Step 2: Write the failing tests**

```rust
// crates/server/src/replication.rs — add to the existing tests module
#[test]
fn broadcast_delivers_to_every_registered_sender() {
    let registry = ReplicaRegistry::default();
    let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
    let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
    registry.register(tx1);
    registry.register(tx2);

    registry.broadcast(bytes::Bytes::from_static(b"hello"));

    assert_eq!(rx1.try_recv().unwrap().as_ref(), b"hello");
    assert_eq!(rx2.try_recv().unwrap().as_ref(), b"hello");
}

#[test]
fn broadcast_prunes_a_sender_whose_receiver_was_dropped() {
    let registry = ReplicaRegistry::default();
    let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
    drop(rx1); // simulate a dead replica connection
    let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
    registry.register(tx1);
    registry.register(tx2);

    registry.broadcast(bytes::Bytes::from_static(b"a"));
    registry.broadcast(bytes::Bytes::from_static(b"b")); // the dead sender must be pruned by now

    // rx2 saw both broadcasts; nothing panicked or errored over rx1's drop
    assert_eq!(rx2.try_recv().unwrap().as_ref(), b"a");
    assert_eq!(rx2.try_recv().unwrap().as_ref(), b"b");
}

#[test]
fn broadcast_with_no_registered_replicas_does_nothing() {
    let registry = ReplicaRegistry::default();
    registry.broadcast(bytes::Bytes::from_static(b"hello")); // must not panic
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem replication::tests::broadcast`
Expected: FAIL with "no field `senders`"/"no method named `register`/`broadcast`" — `ReplicaRegistry` is still the empty skeleton from `03-replication-handle-and-save.md`

- [ ] **Step 4: Implement `register`/`broadcast`**

```rust
// crates/server/src/replication.rs — replaces the `#[derive(Default)] pub struct ReplicaRegistry {}` skeleton
/// Holds one outbound channel per connected replica. `senders` is a plain `std::sync::Mutex`,
/// not `tokio::sync::Mutex`: every access is a quick, synchronous push/retain, never held
/// across an `.await`, so the lighter std lock is the right tool — matching `AofWriter::order`'s
/// existing choice for the same reason.
#[derive(Default)]
pub struct ReplicaRegistry {
    senders: std::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<bytes::Bytes>>>,
}

impl ReplicaRegistry {
    /// Registers a newly-synced replica's outbound channel. Called only from `serve_replica`
    /// (Task 4), while it still holds `AofWriter::lock_for_ordering()` — see this plan's
    /// Global Constraints for why registration must happen inside that same critical section
    /// as the snapshot walk, not after it.
    pub fn register(&self, sender: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>) {
        self.senders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(sender);
    }

    /// Fans `bytes` out to every registered replica, pruning any whose receiver has been
    /// dropped (the replica connection died). Never itself returns an error: a delivery
    /// failure to one dead replica must not affect delivery to the others, and must never
    /// roll back the write that already committed on the leader.
    pub fn broadcast(&self, bytes: bytes::Bytes) {
        let mut senders = self.senders.lock().unwrap_or_else(|e| e.into_inner());
        senders.retain(|tx| tx.send(bytes.clone()).is_ok());
    }
}
```

Both methods recover from a poisoned lock rather than propagate the panic — this mutex can be reached from arbitrary dispatch work (`dispatch_and_log`'s fan-out hook), so a panicking holder elsewhere in the process must not turn into a permanent, server-wide replication outage, matching `AofWriter::order`'s established convention (Sprint 4).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem replication::tests`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/server/src/replication.rs
git commit -m "feat(server): add ReplicaRegistry::register and broadcast"
```

---

### Task 2: `AofWriter::encode_frame`/`append_encoded`

**Files:**
- Modify: `crates/server/src/aof.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn encode_frame(frame: &Frame) -> std::io::Result<Vec<u8>>` (free function) and `AofWriter::append_encoded(&self, bytes: Vec<u8>) -> std::io::Result<()>`, both `pub`, consumed by Task 3.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/aof.rs — add to the existing tests module
#[test]
fn encode_frame_matches_append_s_existing_wire_format() {
    let encoded = encode_frame(&frame(&[b"SET", b"k", b"v"])).unwrap();
    assert_eq!(encoded, b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
}

#[test]
fn append_encoded_writes_pre_encoded_bytes_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
    writer.append_encoded(b"raw bytes, not even valid RESP".to_vec()).unwrap();
    writer.fsync().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"raw bytes, not even valid RESP");
}

#[test]
fn append_still_produces_the_same_output_as_before_the_split() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
    writer.append(frame(&[b"SET", b"k", b"v"])).unwrap();
    writer.fsync().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem aof::tests::encode_frame aof::tests::append_encoded`
Expected: FAIL with "cannot find function `encode_frame`"/"no method named `append_encoded`" — `append_still_produces_the_same_output_as_before_the_split` already passes against the current `append`, since it's not new behavior, only a regression guard for the refactor

- [ ] **Step 3: Split `append`**

Replace the existing `append` method with three pieces:

```rust
// crates/server/src/aof.rs
/// Encodes `frame` in RESP wire format. A free function, not a method, so `dispatch_and_log`
/// can call it once per write and reuse the same bytes for both `append_encoded` and a
/// replica broadcast — see the sprint-5 spec's fan-out hook decision for why.
pub fn encode_frame(frame: &Frame) -> std::io::Result<Vec<u8>> {
    let mut buf = bytes::BytesMut::new();
    protocol::codec::RespCodec::default().encode(frame.clone(), &mut buf)?;
    Ok(buf.to_vec())
}

impl AofWriter {
    // ... existing methods unchanged, except `append` below ...

    /// Sends already-encoded bytes to the writer thread — the part of the old `append` that
    /// wasn't encoding. See `append`'s own doc comment for the `Always`/`EverySecond`/`Never`
    /// blocking behavior, which is unchanged by this split.
    pub fn append_encoded(&self, bytes: Vec<u8>) -> std::io::Result<()> {
        if self.policy == FsyncPolicy::Always {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            self.send(AofMsg::AppendAndFsync(bytes, ack_tx))?;
            ack_rx.recv().map_err(writer_gone)?
        } else {
            self.send(AofMsg::Append(bytes))
        }
    }

    /// Encodes `frame` and sends it to the dedicated writer thread — now a thin wrapper over
    /// `encode_frame` + `append_encoded`, kept for the ~15 existing call sites (tests and
    /// `main.rs`... actually there are none in `main.rs`; check `crates/server` for
    /// `\.append\(` to confirm the exact count before assuming) that pass an owned `Frame`
    /// rather than pre-encoded bytes.
    pub fn append(&self, frame: Frame) -> std::io::Result<()> {
        self.append_encoded(encode_frame(&frame)?)
    }
}
```

Fold this into the existing `impl AofWriter` block (don't create a second one) — the snippet above shows `append_encoded` and the new `append` body inside their own `impl` fence only for clarity in this plan; in the actual file, edit the existing single `impl AofWriter { ... }` block in place, and add the free `encode_frame` function outside it, near the top of the file below `WRITE_COMMANDS` (or any existing free-function location in the file — check `aof.rs`'s current layout before choosing a spot).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem aof::tests`
Expected: PASS, all of `aof.rs`'s tests including the 3 new ones and every pre-existing `append`-based test

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/aof.rs
git commit -m "refactor(server): split AofWriter::append into encode_frame + append_encoded"
```

---

### Task 3: the `dispatch_and_log` fan-out hook

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `crate::aof::encode_frame`/`AofWriter::append_encoded` (Task 2), `ReplicaRegistry::broadcast` (Task 1, via `ReplicationHandle::registry`, already threaded through `dispatch_and_log` by `03-replication-handle-and-save.md`).
- Produces: every write command a leader executes is fanned out to registered replicas; nothing new consumed by later tasks in this plan.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn dispatch_and_log_fans_out_a_write_command_to_registered_replicas() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), dir.path().join("unused.snapshot"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    replication.registry.register(tx);

    dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);

    let received = rx.try_recv().unwrap();
    assert_eq!(received.as_ref(), b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
}

#[test]
fn dispatch_and_log_fans_out_spops_rewrite_not_the_original_command() {
    let engine = std::sync::Arc::new(Engine::new());
    dispatch(&engine, cmd(&[b"SADD", b"s", b"only-member"]), &mut Protocol::default(), 1);
    let (dir, aof) = test_aof();
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), dir.path().join("unused.snapshot"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    replication.registry.register(tx);

    dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SPOP", b"s"]), &mut Protocol::default(), 1);

    let received = rx.try_recv().unwrap();
    assert_eq!(received.as_ref(), b"*3\r\n$4\r\nSREM\r\n$1\r\ns\r\n$11\r\nonly-member\r\n");
}

#[test]
fn dispatch_and_log_with_no_registered_replicas_still_succeeds() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), dir.path().join("unused.snapshot"));
    let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(reply, Frame::Simple("OK".into()));
}

#[test]
fn dispatch_and_log_broadcasts_even_when_the_read_only_command_is_not_a_write() {
    // a read-only command has no to_log entries at all -- broadcast must simply not be
    // reached for it, not error
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), dir.path().join("unused.snapshot"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    replication.registry.register(tx);

    dispatch_and_log(&engine, &aof, &replication, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1);

    assert!(rx.try_recv().is_err()); // nothing was broadcast for a read
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::dispatch_and_log_fans_out dispatcher::tests::dispatch_and_log_broadcasts`
Expected: FAIL — `rx.try_recv()` finds nothing, since `dispatch_and_log` doesn't broadcast anything yet

- [ ] **Step 3: Implement the fan-out hook**

Replace the existing `for frame_to_log in to_log { ... }` loop body (the one that currently only calls `aof.append(frame_to_log)`) with:

```rust
// crates/server/src/dispatcher.rs — inside dispatch_and_log, replacing the existing loop
let mut aof_failed = false;
for frame_to_log in to_log {
    let encoded = match crate::aof::encode_frame(&frame_to_log) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("aof encode failed: {e}");
            aof_failed = true;
            continue; // nothing to append or broadcast without a successful encode
        }
    };
    // One clone: `append_encoded` needs an owned `Vec<u8>` for the writer-thread channel
    // (AofMsg's existing shape, unchanged this sprint), while `broadcast` needs its own
    // `Bytes` handle. A small, accepted per-write-command cost rather than widening AofMsg's
    // channel type just for this sprint.
    if let Err(e) = aof.append_encoded(encoded.clone()) {
        eprintln!("aof append failed: {e}");
        aof_failed = true;
    }
    // Broadcast regardless of the append's result: the engine mutation already committed, so
    // a leader that fails to log a write locally must not also withhold it from its
    // replicas — that would diverge them permanently over a purely local disk problem.
    replication.registry.broadcast(bytes::Bytes::from(encoded));
}
```

This whole loop still runs inside the existing `_order_guard` (from `extract_write_command_name`'s `lock_for_ordering()` acquisition earlier in the function, unchanged from Sprint 4) — nothing about the locking scope changes, only what happens inside it.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, every test in the module including the 4 new ones

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): fan out write commands to registered replicas"
```

---

### Task 4: `PSYNC` and `serve_replica`

**Files:**
- Modify: `crates/server/src/connection.rs`

**Interfaces:**
- Consumes: `AofWriter::lock_for_ordering` (existing), `ReplicationHandle::engine`/`registry` (`03`), `Engine::snapshot` (`01`), `ReplicaRegistry::register` (Task 1).
- Produces: `PSYNC` as a working leader-side handshake; consumed end-to-end by `05-replicaof-and-follower-apply-loop.md`'s follower and `06-replication-integration-tests.md`'s integration tests (neither of which this task's own unit tests can exercise alone, since a full round trip needs a real follower client — Task 4's own tests below cover the leader side in isolation).

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/connection.rs — add to the existing tests module
#[tokio::test]
async fn psync_sends_a_length_prefixed_snapshot_then_streams_subsequent_writes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(Engine::new());
    engine.set(Bytes::from_static(b"k"), engine::Value::String(Bytes::from_static(b"v")));
    let (_dir, aof) = test_aof();
    let replication = Arc::new(crate::replication::ReplicationHandle::new(
        Arc::clone(&engine),
        std::env::temp_dir().join("psync-test-unused.snapshot"),
    ));
    tokio::spawn(serve(listener, Arc::clone(&engine), Arc::clone(&aof), Arc::clone(&replication)));

    let stream = TcpStream::connect(addr).await.unwrap();
    let mut framed = Framed::new(stream, RespCodec::default());
    framed
        .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PSYNC"))]))
        .await
        .unwrap();
    let mut parts = framed.into_parts();

    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 8];
    parts.io.read_exact(&mut len_buf).await.unwrap();
    let len = u64::from_le_bytes(len_buf) as usize;
    let mut blob = vec![0u8; len];
    parts.io.read_exact(&mut blob).await.unwrap();

    let loaded = Engine::new();
    loaded.load_snapshot(&blob).unwrap();
    assert_eq!(loaded.get(b"k"), Some(engine::Value::String(Bytes::from_static(b"v"))));

    // now drive a write through the *real* engine via a second, ordinary client connection,
    // and prove it arrives on the replica connection's raw socket
    let mut client = Framed::new(TcpStream::connect(addr).await.unwrap(), RespCodec::default());
    client
        .send(Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Bulk(Bytes::from_static(b"new")),
            Frame::Bulk(Bytes::from_static(b"value")),
        ]))
        .await
        .unwrap();
    assert_eq!(client.next().await.unwrap().unwrap(), Frame::Simple("OK".into()));

    let mut streamed = vec![0u8; b"*3\r\n$3\r\nSET\r\n$3\r\nnew\r\n$5\r\nvalue\r\n".len()];
    parts.io.read_exact(&mut streamed).await.unwrap();
    assert_eq!(streamed, b"*3\r\n$3\r\nSET\r\n$3\r\nnew\r\n$5\r\nvalue\r\n");
}

#[tokio::test]
async fn a_registered_replica_is_pruned_after_its_connection_drops() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = Arc::new(crate::replication::ReplicationHandle::new(
        Arc::clone(&engine),
        std::env::temp_dir().join("psync-test-unused-2.snapshot"),
    ));
    tokio::spawn(serve(listener, Arc::clone(&engine), Arc::clone(&aof), Arc::clone(&replication)));

    let stream = TcpStream::connect(addr).await.unwrap();
    let mut framed = Framed::new(stream, RespCodec::default());
    framed
        .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PSYNC"))]))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await; // let serve_replica register
    drop(framed); // disconnect the replica

    // two broadcasts: the first send after a drop can still succeed on some platforms before
    // the OS notices the close, so prune is only guaranteed observable after a second attempt
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
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // no assertion beyond "the server is still alive and answering" -- proves broadcast's
    // retain-based pruning didn't panic or wedge on the dropped connection
    let mut ping = Framed::new(TcpStream::connect(addr).await.unwrap(), RespCodec::default());
    ping.send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))])).await.unwrap();
    assert_eq!(ping.next().await.unwrap().unwrap(), Frame::Simple("PONG".into()));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem connection::tests::psync connection::tests::a_registered_replica`
Expected: FAIL — `PSYNC` currently falls through to `dispatch_and_log`'s unknown-command path, so no snapshot blob is ever sent and the test hangs on `read_exact` until it times out; if this happens, that confirms the pre-implementation failure mode rather than indicating a broken test

- [ ] **Step 3: Implement `PSYNC` interception and `serve_replica`**

In `handle_connection`'s loop, intercept before dispatching:

```rust
// crates/server/src/connection.rs — inside handle_connection's loop, right after decoding `frame`
if is_psync_command(&frame) {
    serve_replica(framed, &aof, &replication).await;
    return; // serve_replica never returns until the replica connection dies
}
```

Add the helper and `serve_replica` itself, near `handle_connection`:

```rust
// crates/server/src/connection.rs
fn is_psync_command(frame: &protocol::Frame) -> bool {
    let protocol::Frame::Array(items) = frame else { return false };
    let Some(protocol::Frame::Bulk(name)) = items.first() else { return false };
    name.eq_ignore_ascii_case(b"PSYNC")
}

/// Takes ownership of `framed`'s underlying socket and never returns until the replica
/// connection dies. `PSYNC` has no reply frame of its own — the length-prefixed snapshot blob
/// (not a RESP value) stands in for one.
async fn serve_replica(
    framed: Framed<tokio::net::TcpStream, RespCodec>,
    aof: &AofWriter,
    replication: &crate::replication::ReplicationHandle,
) {
    use tokio::io::AsyncWriteExt;

    // ONE critical section: snapshot + register, so no write can slip between them. Taken
    // separately, a write committing after the snapshot walk but before registration would
    // reach neither the blob nor the stream -- lost permanently, unrepairable by reconnect,
    // since a reconnect just snapshots a leader that has already moved past it. Lock
    // ordering: lock_for_ordering() before the registry's own mutex, matching this plan's
    // Global Constraints and the fan-out hook in dispatcher.rs, the only other place both
    // are taken.
    let (snapshot_bytes, mut rx) = {
        let _order_guard = aof.lock_for_ordering();
        let bytes = replication.engine().snapshot(0); // 0: a follower keeps no AOF, so the header is moot
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        replication.registry.register(tx);
        (bytes, rx)
    };

    // Reclaim the raw socket. Any bytes already buffered for a reply this connection never
    // got to send (there shouldn't be any at this point -- PSYNC is answered with the blob
    // below, not a normal `feed`/`flush` reply -- but flushing defensively costs nothing) are
    // written out first so nothing already-queued is silently dropped.
    let mut parts = framed.into_parts();
    if !parts.write_buf.is_empty() {
        if parts.io.write_all(&parts.write_buf).await.is_err() {
            return;
        }
    }
    let io = &mut parts.io;

    if io.write_all(&(snapshot_bytes.len() as u64).to_le_bytes()).await.is_err() {
        return;
    }
    if io.write_all(&snapshot_bytes).await.is_err() {
        return;
    }

    // Drain replicated writes onto the raw socket forever -- this connection never reads
    // again once PSYNC has been handled. A closed channel (this task's own sender side was
    // dropped, e.g. the process is shutting down) ends the loop cleanly; a write error means
    // the replica disconnected, which `ReplicaRegistry::broadcast`'s retain-based pruning
    // already handles from the registry's side on its next send -- this loop returning is
    // this connection's own half of that same cleanup.
    while let Some(bytes) = rx.recv().await {
        if io.write_all(&bytes).await.is_err() {
            return;
        }
    }
}
```

`Framed<tokio::net::TcpStream, RespCodec>` needs `tokio::net::TcpStream` in scope — `connection.rs` already imports it (`use tokio::net::TcpListener;` is present; add `use tokio::net::TcpStream;` alongside it if not already implicitly available via the existing `tokio::net::TcpStream` fully-qualified uses elsewhere in the file — check before assuming).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem connection::tests`
Expected: PASS, every test in the module including the 2 new ones

- [ ] **Step 5: Run the full workspace verification**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/connection.rs
git commit -m "feat(server): add PSYNC and serve_replica to the leader's accept loop"
```
