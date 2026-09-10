# Bidirectional Replica Connection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the leader's `PSYNC` connection bidirectional. Today `serve_replica` is provably write-only — it calls `framed.into_parts()` and then only ever `write_all`s, and its own source comment says *"this connection never reads again once PSYNC has been handled."* A `REPLCONF ACK` sent by a follower is received by the kernel and never read. This plan restructures that function so the leader **reads** the replica connection, while streaming exactly as before. It deliberately does **not** interpret acks yet: an inbound frame is logged at `debug` and dropped. `05-replica-ack-tracking.md` replaces the drop with ack parsing.

**Architecture:** `tokio::io::split` the reclaimed socket into a `ReadHalf<S>`/`WriteHalf<S>` pair. The write half keeps doing exactly what `parts.io` did: flush any queued `write_buf`, write the 8-byte length prefix, write the snapshot blob, then `write_all` every broadcast frame. The read half is wrapped in a `FramedRead<_, RespCodec>` whose read buffer is **seeded with `parts.read_buf`**, so any bytes the codec had already read ahead of `PSYNC` are not silently lost. The outbound channel and the inbound frame stream are then driven by one `tokio::select!` loop with all four outcomes handled explicitly. No new dependency, no wire-format change, no new public API.

**Tech Stack:** Rust, tokio (`io-util` for `split`, `macros` for `select!`), tokio-util (`codec`), futures-util (`StreamExt`).

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md) — "Decision: v1 is not failover", step 1 ("Replication offsets ... echoed by each follower via a periodic `REPLCONF ACK <offset>`-equivalent frame"), is authoritative for why this exists. Nothing here promotes anything or changes routing.

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) in full before writing any code.** It is normative: every name, type, and semantic decision below is fixed there. Where this plan and the contract disagree, the contract wins and the disagreement is a bug worth reporting before you write code. §1.3 (`serve_replica` as it exists today) and §2.3 (`REPLCONF ACK` on the existing socket) govern this plan directly.
- **Contract §2.3's snippet does not compile as written, and this plan corrects it.** It shows `FramedRead::from_parts(read_parts)`. `tokio_util::codec::FramedRead` has `into_parts` but **no** `from_parts` (verified against tokio-util 0.7.19 — only `Framed` has `from_parts`). The working form is:
  ```rust
  let rd = std::io::Cursor::new(parts.read_buf).chain(rd);
  let mut inbound = FramedRead::new(rd, RespCodec::default());
  ```
  > **Superseded twice — corrected 2026-09-10.** This plan originally prescribed
  > `*inbound.read_buffer_mut() = parts.read_buf;` and shipped it. That compiles and preserves the
  > bytes but **leaves them undecoded**: `FramedRead::new` starts with `is_readable: false`, and
  > `poll_next` decodes the buffer only once that flag is set, which happens after a socket read.
  > A pipelined ack therefore sat unseen until the follower sent something else or hung up. Plan
  > 05's mandatory deliberate-break verification caught it. See contract §2.3's full note; use the
  > chained-cursor form above.
  The *requirement* (`parts.read_buf` must be carried into the inbound reader, never dropped) is unchanged and is non-negotiable — dropping it silently loses whatever the follower pipelined behind its `PSYNC`, which is exactly the bug plan 05's ack test would then hit.
- **`serve_replica` is generic and serves both plaintext and TLS.** `S` is a plain `TcpStream` on the `serve` path and a `tokio_rustls::server::TlsStream<TcpStream>` on the `serve_tls` path. `tokio::io::split` works for both. The bound gains `+ Send`: `ReadHalf<S>: Send` only when `S: Send`, and `handle_connection` (this function's only caller) already declares `S: ... + Send + 'static`, so adding it costs nothing and makes the requirement explicit rather than inferred. Task 3 proves the TLS monomorphization really works end to end.
- **`write_all` on a `WriteHalf<S>` needs `tokio::io::AsyncWriteExt` in scope** — the existing `use tokio::io::AsyncWriteExt;` at the top of the function body already provides it. Keep it.
- **All four `select!` outcomes must be handled explicitly.** Outbound bytes, inbound frame, outbound channel closed, inbound stream ended. Getting any of them wrong parks a replica connection forever. Never write a catch-all arm.
- **An inbound frame must never draw a reply and must never cost the follower its connection.** A follower that never acks at all (an older build) must keep working as a replica with no ack information. See contract §2.3.
- **`framed.next()`/`rx.recv()` are both cancel-safe**, which is what makes them legal `select!` branches. The `write_all` calls stay in the branch *bodies*, never in the branch futures.
- **Timing tests are bounded polls with a deadline**, never a bare `sleep` + assert. Copy the shape of `wait_for` at `crates/server/tests/replication.rs:42-60`.
- **The three CI gates must be clean before every commit:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
  Clippy is strict (`-D warnings`) and lints test code too; a dead-code warning fails CI.
- **Known flaky test:** `ttls_set_before_the_kill_come_back_as_absolute_deadlines_not_restarted_countdowns` in `crates/server/tests/kill_and_recover.rs` is timing-sensitive and pre-existing. If it fails, re-run, or confirm with `cargo test --workspace -- --test-threads=1`. Do not "fix" it here, and do not treat it as a regression you caused.
- **Comment style:** short, easy, full sentences ending in a punctuation mark. No emojis.

---

### Task 1: split the replica socket and read from it

**Files:**
- Modify: `crates/server/src/connection.rs:9` (the `tokio_util::codec` import)
- Modify: `crates/server/src/connection.rs:302-364` (`serve_replica`, whole function)
- Test: `crates/server/src/connection.rs` (existing `#[cfg(test)] mod tests`, after `a_registered_replica_is_pruned_after_its_connection_drops`, which ends at `:833`)

**Interfaces:**
- Consumes:
  - `ReplicationHandle::master_repl_offset(&self) -> u64` — plan 01's leader offset; plan 02 already wired the call into `serve_replica`'s snapshot capture as `let handoff_offset = replication.master_repl_offset();`. This plan preserves that line verbatim.
  - `ReplicaRegistry::register(&self, addr: Option<String>, sender: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>)` — unchanged in this plan. (Plan 05 changes its return type; this plan must not.)
  - `AofWriter::lock_all_shards(&self) -> Vec<std::sync::MutexGuard<'_, ()>>`
  - `ReplicationHandle::connected_clients(&self) -> usize` — for the test only.
- Produces:
  - `async fn serve_replica<S>(framed: Framed<S, RespCodec>, aof: &AofWriter, replication: &crate::replication::ReplicationHandle, advertised_addr: Option<String>) where S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send` — same signature plus the `Send` bound, now bidirectional. Consumed by `05-replica-ack-tracking.md`, which replaces the inbound `debug!` with `REPLCONF ACK` parsing.
  - The behavioral guarantee that a replica connection is released as soon as the follower disconnects, without waiting for a broadcast to fail.

- [ ] **Step 1: Write the failing test**

Append to `crates/server/src/connection.rs`'s test module, after `a_registered_replica_is_pruned_after_its_connection_drops`:

```rust
    /// The one thing a write-only replica connection cannot do: notice its follower is gone.
    /// Before this plan, `serve_replica` parked on `rx.recv()` forever after handing over the
    /// snapshot, so a departed replica's connection task stayed alive -- and its `ClientGuard`
    /// undropped -- until some later broadcast happened to fail on the dead socket. That is why
    /// `a_registered_replica_is_pruned_after_its_connection_drops` has to drive two writes to
    /// observe pruning at all. Reading the connection makes the disconnect observable with no
    /// write traffic whatsoever, which is what this test pins.
    #[tokio::test]
    async fn a_replica_connection_is_released_as_soon_as_the_follower_disconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-disconnect-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec::default());
        framed
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(
                b"PSYNC",
            ))]))
            .await
            .unwrap();
        let mut parts = framed.into_parts();

        // Drain the whole handshake first, so `serve_replica` is provably past its three
        // `write_all` calls and parked in its main loop. Dropping the socket before that could
        // end the connection through a failed write instead, which would prove nothing.
        use tokio::io::AsyncReadExt;
        let mut len_buf = [0u8; 8];
        parts.io.read_exact(&mut len_buf).await.unwrap();
        let mut blob = vec![0u8; u64::from_le_bytes(len_buf) as usize];
        parts.io.read_exact(&mut blob).await.unwrap();
        assert_eq!(replication.connected_clients(), 1);

        drop(parts.io); // the follower goes away, and nothing is ever written to it again

        // Bounded poll, not a fixed sleep: the deadline is what makes the old write-only
        // behavior (which never notices, ever) a failure rather than a slow pass.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while replication.connected_clients() != 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the leader never released the replica connection after the follower \
                 disconnected; it is still write-only"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem connection::tests::a_replica_connection_is_released_as_soon_as_the_follower_disconnects`

Expected: FAIL with the panic `the leader never released the replica connection after the follower disconnected; it is still write-only`. That is the correct failure — `serve_replica` never reads, so the closed socket is invisible to it and `connected_clients()` stays `1` for the full two seconds.

- [ ] **Step 3: Add `FramedRead` to the module's imports**

```rust
// crates/server/src/connection.rs — replace the import at :9
use tokio_util::codec::{Framed, FramedRead};
```

- [ ] **Step 4: Rewrite `serve_replica`**

Replace the whole function (`crates/server/src/connection.rs:302-364`, from its doc comment through its closing brace) with:

```rust
/// Takes ownership of `framed`'s underlying socket and never returns until the replica
/// connection dies. `PSYNC` has no reply frame of its own — the length-prefixed snapshot blob
/// (not a RESP value) stands in for one.
///
/// The connection is bidirectional. The leader streams replicated writes down it and reads
/// frames back up it, which is what lets a follower report how caught up it is via `REPLCONF
/// ACK <offset>` on this same socket -- no second port and no second connection. See the
/// failover-safety design contract's §2.3.
async fn serve_replica<S>(
    framed: Framed<S, RespCodec>,
    aof: &AofWriter,
    replication: &crate::replication::ReplicationHandle,
    advertised_addr: Option<String>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    use tokio::io::AsyncWriteExt;

    // ONE critical section: snapshot + register, so no write can slip between them. Taken
    // separately, a write committing after the snapshot walk but before registration would
    // reach neither the blob nor the stream -- lost permanently, unrepairable by reconnect,
    // since a reconnect just snapshots a leader that has already moved past it. Lock
    // ordering: lock_for_ordering() before the registry's own mutex, matching this plan's
    // Global Constraints and the fan-out hook in dispatcher.rs, the only other place both are
    // taken -- there, the order guard for a write's shard(s) is held across both the AOF
    // append and the registry broadcast, for the same reason: neither critical section may
    // release the order guard before it has finished touching the registry.
    let (snapshot_bytes, mut rx) = {
        let _order_guard = aof.lock_all_shards();
        // The header's stream position, for a PSYNC image, is the leader's replication offset --
        // not an AOF length. See `Engine::snapshot`'s doc comment for the parameter's two
        // meanings.
        let handoff_offset = replication.master_repl_offset();
        let bytes = replication.engine().snapshot(handoff_offset);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        replication.registry.register(advertised_addr, tx);
        (bytes, rx)
    };

    // Reclaim the raw socket and split it, because this connection now reads as well as
    // writes. The write half does exactly what the single `parts.io` handle used to; the read
    // half becomes a RESP frame stream.
    let parts = framed.into_parts();
    let (rd, mut wr) = tokio::io::split(parts.io);
    // Replay whatever the codec had already read ahead of `PSYNC` before touching the socket
    // again. Those bytes are gone from the socket, so reading it straight away would silently
    // swallow anything the follower pipelined behind its PSYNC -- its first ack, most likely.
    // Chained in front of the read half rather than poked into `FramedRead`'s own buffer: see
    // contract §2.3's second correction for why `read_buffer_mut` leaves them undecoded.
    let rd = std::io::Cursor::new(parts.read_buf).chain(rd);
    let mut inbound = FramedRead::new(rd, RespCodec::default());

    // Any bytes already buffered for a reply this connection never got to send (there
    // shouldn't be any at this point -- PSYNC is answered with the blob below, not a normal
    // `feed`/`flush` reply -- but flushing defensively costs nothing) are written out first so
    // nothing already-queued is silently dropped.
    if !parts.write_buf.is_empty() && wr.write_all(&parts.write_buf).await.is_err() {
        return;
    }

    if wr
        .write_all(&(snapshot_bytes.len() as u64).to_le_bytes())
        .await
        .is_err()
    {
        return;
    }
    if wr.write_all(&snapshot_bytes).await.is_err() {
        return;
    }

    // Drain replicated writes onto the socket while simultaneously reading whatever the
    // follower sends back. Both branch futures are cancel-safe, which is what makes them legal
    // `select!` arms; the writes themselves happen in the arm bodies, after the other future
    // has been dropped.
    loop {
        tokio::select! {
            outbound = rx.recv() => match outbound {
                Some(bytes) => {
                    // A write error means the replica disconnected, which
                    // `ReplicaRegistry::broadcast`'s retain-based pruning already handles from
                    // the registry's side on its next send -- this loop returning is this
                    // connection's own half of that same cleanup.
                    if wr.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                // Every sender for this replica has been dropped: the registry pruned this
                // entry, or the process is shutting down. Nothing will ever be streamed here
                // again, so end the connection rather than parking on it forever.
                None => return,
            },
            incoming = inbound.next() => match incoming {
                // Nothing interprets inbound frames yet. Logging and dropping is deliberate:
                // an unrecognised frame must never draw an error reply and must never cost the
                // follower its connection. `05-replica-ack-tracking.md` replaces this arm's
                // body with `REPLCONF ACK` parsing.
                Some(Ok(frame)) => {
                    tracing::debug!(?frame, "ignoring inbound frame from a replica");
                }
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "replica connection decode error");
                    return;
                }
                // The follower closed its side. Return so this task's `ClientGuard` drops and
                // its sender goes with it, instead of waiting for some later broadcast to fail.
                None => return,
            },
        }
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p rocket-mem connection::tests::a_replica_connection_is_released_as_soon_as_the_follower_disconnects`

Expected: PASS.

- [ ] **Step 6: Run the existing replica-path tests to prove no regression**

Run: `cargo test -p rocket-mem connection::tests::psync && cargo test -p rocket-mem connection::tests::a_registered_replica_is_pruned_after_its_connection_drops && cargo test -p rocket-mem --test replication`

Expected: all PASS. `psync_sends_a_length_prefixed_snapshot_then_streams_subsequent_writes` in particular must still assert the exact streamed bytes — it is the proof the write side is byte-for-byte unchanged.

- [ ] **Step 7: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

Expected: all clean/green.

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/connection.rs
git commit -m "Make the leader's replica connection bidirectional"
```

---

### Task 2: unparseable inbound bytes must not cost a replica its write stream

**Files:**
- Modify: `crates/server/src/connection.rs` (`serve_replica`'s `select!` loop only — the loop Task 1 added)
- Test: `crates/server/src/connection.rs` (existing `#[cfg(test)] mod tests`, after the test Task 1 added)

**Interfaces:**
- Consumes: `serve_replica` as Task 1 left it.
- Produces: the invariant that a replica which sends a well-formed frame the leader does not understand, **or bytes the RESP codec cannot parse at all**, keeps receiving the replication stream. Consumed by `05-replica-ack-tracking.md`, whose ack parsing relies on "not an ack" being a no-op rather than a teardown.

**Why this needs its own task:** Task 1 returns on `Some(Err(_))`, which drops the replica over a single unparseable byte. That violates contract §2.3 ("a malformed or unknown inbound frame is logged at `debug` and ignored — never an error reply, never a disconnect"). It cannot simply be turned into `continue` either: `RespCodec::decode` returns `Err` on an unknown type byte **without consuming anything** (`crates/protocol/src/codec.rs`), so the reader can never resynchronise, and `FramedRead` fuses after a decoder error — the poll following an `Err` yields `None`, which this loop correctly treats as end-of-stream. So `continue` drops the replica anyway, just less obviously. (**Corrected 2026-09-10:** this said "errors forever in a hot loop", which is false; see contract §2.3's correction note. The conclusion is unchanged.) The correct resolution is to stop reading that connection while leaving the write stream fully intact — the leader loses this follower's ack information, exactly as it would for a follower that never acks, and nothing else changes.

- [ ] **Step 1: Write the failing test**

Append to `crates/server/src/connection.rs`'s test module, after the test Task 1 added:

```rust
    /// A follower is allowed to say things this leader does not understand. A well-formed frame
    /// with no handler is ignored, and bytes the RESP codec cannot parse at all cost the leader
    /// this follower's ack information -- nothing more. Neither may draw a reply, and neither
    /// may take the replication stream down: an older follower build that speaks a dialect this
    /// leader has never heard of must keep replicating.
    #[tokio::test]
    async fn a_replica_sending_unparseable_bytes_keeps_receiving_the_write_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-garbage-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec::default());
        framed
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(
                b"PSYNC",
            ))]))
            .await
            .unwrap();
        let mut parts = framed.into_parts();

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut len_buf = [0u8; 8];
        parts.io.read_exact(&mut len_buf).await.unwrap();
        let mut blob = vec![0u8; u64::from_le_bytes(len_buf) as usize];
        parts.io.read_exact(&mut blob).await.unwrap();

        // A well-formed frame with no handler, then bytes that are not RESP at all. `g` is not
        // a RESP type byte, so `parse_frame` errors on it and consumes nothing.
        parts
            .io
            .write_all(b"*1\r\n$4\r\nPING\r\ngarbage\r\n")
            .await
            .unwrap();

        // Drive a write through an ordinary client. It must arrive on the replica socket, and
        // it must be the very next bytes on it -- proving the leader neither replied to
        // anything above nor tore the connection down over it.
        let mut client = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        client
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"new")),
                Frame::Bulk(Bytes::from_static(b"value")),
            ]))
            .await
            .unwrap();
        assert_eq!(
            client.next().await.unwrap().unwrap(),
            Frame::Simple("OK".into())
        );

        let expected = b"*3\r\n$3\r\nSET\r\n$3\r\nnew\r\n$5\r\nvalue\r\n";
        let mut streamed = vec![0u8; expected.len()];
        parts
            .io
            .read_exact(&mut streamed)
            .await
            .expect("the replica must still be receiving the write stream");
        assert_eq!(streamed, expected);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem connection::tests::a_replica_sending_unparseable_bytes_keeps_receiving_the_write_stream`

Expected: FAIL with `the replica must still be receiving the write stream: Custom { kind: UnexpectedEof, ... }` (an `early eof` from `read_exact`). That is the correct failure — Task 1's `Some(Err(_))` arm returns from `serve_replica`, closing the socket, so the streamed `SET` never arrives.

- [ ] **Step 3: Gate the inbound branch on a flag that a decode error clears**

Replace the `loop { tokio::select! { ... } }` block at the end of `serve_replica` with:

```rust
    // Cleared by a decode error, which disables the inbound branch for the rest of this
    // connection's life. The write stream is untouched either way.
    let mut inbound_open = true;
    loop {
        tokio::select! {
            outbound = rx.recv() => match outbound {
                Some(bytes) => {
                    // A write error means the replica disconnected, which
                    // `ReplicaRegistry::broadcast`'s retain-based pruning already handles from
                    // the registry's side on its next send -- this loop returning is this
                    // connection's own half of that same cleanup.
                    if wr.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                // Every sender for this replica has been dropped: the registry pruned this
                // entry, or the process is shutting down. Nothing will ever be streamed here
                // again, so end the connection rather than parking on it forever.
                None => return,
            },
            incoming = inbound.next(), if inbound_open => match incoming {
                // Nothing interprets inbound frames yet. Logging and dropping is deliberate:
                // an unrecognised frame must never draw an error reply and must never cost the
                // follower its connection. `05-replica-ack-tracking.md` replaces this arm's
                // body with `REPLCONF ACK` parsing.
                Some(Ok(frame)) => {
                    // Corrected 2026-09-10: `?frame` Debug-renders client bytes, which
                    // `logging.rs` forbids. Log the kind and length only.
                    tracing::debug!(
                        kind = frame.kind(),
                        len = frame.log_len(),
                        "ignoring inbound frame from a replica"
                    );
                }
                // The follower sent bytes this codec cannot parse, so the read side is desynced
                // and can never resynchronise -- `RespCodec::decode` errors on an unknown type
                // byte without consuming it, so that byte stays at the head of the buffer.
                // Clearing the flag is not the same as `continue`: `FramedRead` fuses after a
                // decoder error, so the next poll would yield `None`, and `None` is handled
                // below as end-of-stream. Continuing would therefore drop this replica by a
                // roundabout route, which is the one thing this arm exists to prevent.
                // Stop reading and keep streaming: the leader loses this follower's ack
                // information, exactly as it would for a follower that never acks, and the
                // follower keeps replicating. Dropping the connection instead would punish a
                // replica for speaking a dialect this leader has not learned yet. The cost is
                // that this connection also stops noticing a disconnect promptly, falling back
                // to the write-failure pruning that predated the bidirectional split.
                Some(Err(e)) => {
                    tracing::debug!(
                        error = %e,
                        "unparseable inbound bytes from a replica; no longer reading this \
                         connection, but still streaming to it"
                    );
                    inbound_open = false;
                }
                // The follower closed its side. Return so this task's `ClientGuard` drops and
                // its sender goes with it, instead of waiting for some later broadcast to fail.
                None => return,
            },
        }
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem connection::tests::a_replica_sending_unparseable_bytes_keeps_receiving_the_write_stream`

Expected: PASS.

- [ ] **Step 5: Re-run Task 1's test to prove the disconnect path still works**

Run: `cargo test -p rocket-mem connection::tests::a_replica_connection_is_released_as_soon_as_the_follower_disconnects`

Expected: PASS. `inbound_open` must not swallow end-of-stream — `None` still returns.

- [ ] **Step 6: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

Expected: all clean/green.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/connection.rs
git commit -m "Keep streaming to a replica that sends unparseable bytes"
```

---

### Task 3: prove the split works on the TLS replication path too

**Files:**
- Test: `crates/server/tests/replication.rs` (append after `a_follower_never_resyncs_when_pinned_to_the_wrong_certificate`, which ends at `:556`)

**Interfaces:**
- Consumes: `serve_replica` as Tasks 1 and 2 left it, through the `S = tokio_rustls::server::TlsStream<TcpStream>` monomorphization; `rocket_mem::serve_tls`; `rocket_mem::serve`; `rocket_mem::tls::load_server_config`/`load_client_config`; `ReplicationHandle::with_replication_tls_client_config`; the `fixture` and `wait_for` helpers already in this file.
- Produces: no code. A regression guard that `tokio::io::split` and the `select!` loop behave identically over a TLS stream, closing the gap the existing TLS test names in its own comment ("not the plaintext streamed-frame path, which this test never exercises").

**Why this needs its own task:** the existing TLS replication test only proves the *snapshot blob* crosses a TLS socket. Everything Tasks 1 and 2 changed is on the streaming side, which no TLS test covers. `WriteHalf<TlsStream<..>>` and `ReadHalf<TlsStream<..>>` share one underlying stream through tokio's bilock, so a TLS record being written while the read half is parked is precisely the interleaving this restructure introduced and nothing previously exercised.

- [ ] **Step 1: Write the failing test**

Append to `crates/server/tests/replication.rs`:

```rust
/// The TLS test above proves a follower can read the *snapshot blob* over TLS. It says so in its
/// own comment, and it stops there. Everything the bidirectional restructure of `serve_replica`
/// touched is on the other side of that handshake: the split socket, and the `select!` loop that
/// writes streamed frames while a read is parked on the same TLS stream. This test drives a write
/// *after* the follower has synced and requires it to arrive, which is the only way that
/// interleaving gets exercised over `TlsStream` rather than only over a plain `TcpStream`.
///
/// The leader binds two listeners over one shared engine/AOF/handle: a TLS one the follower
/// PSYNCs to, and a plaintext one the ordinary client below writes through. Both feed the same
/// `ReplicaRegistry`, so the write fans out down the TLS replica connection.
#[tokio::test]
async fn a_tls_follower_keeps_receiving_streamed_writes_after_its_resync() {
    let leader_dir = tempfile::tempdir().unwrap();
    let leader_engine = Arc::new(engine::Engine::new());
    let leader_aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &leader_dir.path().join("leader.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let leader_replication = Arc::new(rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&leader_engine),
        leader_dir.path().join("leader.snapshot"),
    ));

    let tls_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tls_addr = tls_listener.local_addr().unwrap();
    let server_tls_config =
        rocket_mem::tls::load_server_config(&fixture("test-cert.pem"), &fixture("test-key.pem"))
            .unwrap();
    tokio::spawn(rocket_mem::serve_tls(
        tls_listener,
        server_tls_config,
        Arc::clone(&leader_engine),
        Arc::clone(&leader_aof),
        Arc::clone(&leader_replication),
    ));

    let plain_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let plain_addr = plain_listener.local_addr().unwrap();
    tokio::spawn(rocket_mem::serve(
        plain_listener,
        Arc::clone(&leader_engine),
        Arc::clone(&leader_aof),
        Arc::clone(&leader_replication),
    ));

    let follower_dir = tempfile::tempdir().unwrap();
    let follower_engine = Arc::new(engine::Engine::new());
    let client_tls_config = rocket_mem::tls::load_client_config(&fixture("test-cert.pem")).unwrap();
    let follower_replication = Arc::new(
        rocket_mem::replication::ReplicationHandle::new(
            Arc::clone(&follower_engine),
            follower_dir.path().join("follower.snapshot"),
        )
        .with_replication_tls_client_config(client_tls_config),
    );
    follower_replication.start_replicating(tls_addr.to_string());

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !follower_replication.link_up() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the follower never linked up over TLS"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Written only after the link is up, so it can only reach the follower through the streamed
    // path on the split TLS socket -- never through the snapshot blob.
    let client = redis::Client::open(format!("redis://{plain_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = con.set("streamed-over-tls", "yes").await.unwrap();

    wait_for(&follower_engine, b"streamed-over-tls", b"yes").await;
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p rocket-mem --test replication a_tls_follower_keeps_receiving_streamed_writes_after_its_resync`

Expected: PASS. This one is a characterization test, not a red-then-green driver — Tasks 1 and 2 already landed the behavior it pins, and the point of writing it now is that nothing in the suite covered the TLS streaming path before. If it **fails**, the generic `split` does not behave on `TlsStream` the way it does on `TcpStream`, which is a real defect in Task 1's work: stop and fix `serve_replica`, do not weaken this test.

- [ ] **Step 3: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

Expected: all clean/green.

- [ ] **Step 4: Commit**

```bash
git add crates/server/tests/replication.rs
git commit -m "Cover streamed writes to a TLS follower"
```

---

## Next plan

[`05-replica-ack-tracking.md`](05-replica-ack-tracking.md) — restructure `ReplicaRegistry` around `ReplicaEntry`, parse the `REPLCONF ACK <offset>` frames this plan currently logs and drops, and report each replica's acked offset and lag in `INFO REPLICATION`.
