# RMP Server Connection Handling & Listener Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** an RMP `TcpListener`, running alongside the existing RESP listener, that multiplexes many concurrent requests per connection onto the same `dispatcher::dispatch_and_log` every RESP command already goes through — so RMP gets full command parity (including `INFO`/`CLUSTER`/`SAVE`/`REPLICAOF`/`SLOWLOG`) with zero changes to that function.

**Architecture:** `crates/server/src/rmp_connection.rs` mirrors the shape of `connection.rs`'s `serve`/`handle_connection`, but splits each connection into a read half and a write half joined by an `mpsc` channel keyed by nothing but arrival order into the channel (correlation is carried in each `RmpMessage`'s own `request_id` field, not by channel position): the read loop decodes a Request, `tokio::spawn`s a task that calls `dispatch_and_log` and sends `(request_id, reply)` into the channel, then immediately goes back to decoding the next request without waiting. A single writer task owns the channel's receiver and the socket's write half, writing each reply as it arrives. `main.rs` binds a second `TcpListener` (`ROCKET_MEM_RMP_ADDR`) and runs `rmp_connection::serve` alongside the existing `rocket_mem::serve` call, sharing the same `Arc<Engine>`/`Arc<AofWriter>`/`Arc<ReplicationHandle>`.

**Tech Stack:** `tokio` (`net`, `sync::mpsc`), `tokio_util::codec::Framed`, `futures_util::{SinkExt, StreamExt}` — all already dependencies of the `server` crate. Depends on `protocol::rmp::{RmpCodec, RmpMessage, MsgType}` from Plan 01.

**Spec:** [`../../specs/2026-08-31-sprint-7-spec.md`](../../specs/2026-08-31-sprint-7-spec.md) — "Decision: connection & concurrency model" and "Decision: listener wiring" are authoritative for this plan. Depends on `01-wire-format-codec.md` being complete.

## Global Constraints

- **`dispatcher::dispatch_and_log` is never modified.** Every RMP request reaches it by constructing the exact `Frame::Array(Vec<Frame::Bulk>)` shape it already expects from RESP.
- **RMP has no protocol-negotiation state.** `dispatch_and_log`'s `&mut Protocol` parameter is satisfied with a fresh `Protocol::default()` local per call; nothing persists it across requests, since nothing on the RMP side ever reads it.
- **The read loop never blocks on a spawned task.** It decodes the next Request immediately after spawning the previous one's handling task — this is what makes multiple in-flight requests on one connection possible at all.
- **Every reply that started processing is written out, even if the client disconnects mid-flight**, by construction of the channel's sender-count-based shutdown (see Task 2, Step 4's design note) — not by any explicit "wait for pending replies" loop.
- **RMP listens unconditionally**, with no opt-out env var, matching how the metrics listener in `main.rs` is already unconditional.

---

### Task 1: `rmp_connection::serve` and `handle_connection`

**Files:**
- Create: `crates/server/src/rmp_connection.rs`
- Modify: `crates/server/src/lib.rs` (currently `pub mod aof; pub mod cluster; pub mod connection; pub mod dispatcher; pub mod metrics; pub mod replication; pub mod slowlog; pub use connection::serve;`)

**Interfaces:**
- Consumes: `protocol::rmp::{RmpCodec, RmpMessage, MsgType}` (Plan 01); `dispatcher::dispatch_and_log(engine: &Engine, aof: &AofWriter, replication: &ReplicationHandle, frame: Frame, protocol: &mut Protocol, client_id: u64) -> Frame` (existing, unchanged); `engine::Engine`; `crate::aof::AofWriter`; `crate::replication::ReplicationHandle`; `protocol::codec::Protocol`.
- Produces: `pub async fn serve(listener: TcpListener, engine: Arc<Engine>, aof: Arc<AofWriter>, replication: Arc<ReplicationHandle>)`, consumed by `main.rs` (Task 2) and by the integration tests in `04-integration-tests.md`.

- [ ] **Step 1: Declare the module**

```rust
// crates/server/src/lib.rs — add the new module, keeping the list alphabetical
pub mod aof;
pub mod cluster;
pub mod connection;
pub mod dispatcher;
pub mod metrics;
pub mod replication;
pub mod rmp_connection;
pub mod slowlog;
pub use connection::serve;
```

- [ ] **Step 2: Write the failing tests**

```rust
// crates/server/src/rmp_connection.rs — the whole file, for now just imports and tests
use crate::aof::AofWriter;
use crate::dispatcher;
use crate::replication::ReplicationHandle;
use bytes::Bytes;
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use protocol::codec::Protocol;
use protocol::rmp::{MsgType, RmpCodec, RmpMessage};
use protocol::Frame;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream;

    fn test_aof() -> (tempfile::TempDir, Arc<AofWriter>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, crate::aof::FsyncPolicy::Never).unwrap();
        (dir, Arc::new(writer))
    }

    async fn spawn_test_server() -> (tempfile::TempDir, std::net::SocketAddr, Arc<Engine>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (dir, aof) = test_aof();
        let replication = Arc::new(ReplicationHandle::default());
        tokio::spawn(serve(listener, Arc::clone(&engine), aof, replication));
        (dir, addr, engine)
    }

    async fn connect(addr: std::net::SocketAddr) -> Framed<TcpStream, RmpCodec> {
        Framed::new(TcpStream::connect(addr).await.unwrap(), RmpCodec::default())
    }

    fn command(args: &[&[u8]]) -> Frame {
        Frame::Array(
            args.iter()
                .map(|a| Frame::Bulk(Bytes::copy_from_slice(a)))
                .collect(),
        )
    }

    #[tokio::test]
    async fn set_then_get_round_trips_over_a_real_socket() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut con = connect(addr).await;

        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"foo", b"bar"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(reply.request_id, 1);
        assert_eq!(reply.frame, Frame::Simple("OK".into()));

        con.send(RmpMessage {
            request_id: 2,
            msg_type: MsgType::Request,
            frame: command(&[b"GET", b"foo"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(reply.request_id, 2);
        assert_eq!(reply.frame, Frame::Bulk(Bytes::from_static(b"bar")));
    }

    #[tokio::test]
    async fn a_write_over_rmp_updates_the_same_engine_a_second_rmp_connection_reads_from() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut a = connect(addr).await;
        let mut b = connect(addr).await;

        a.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"k", b"v"]),
        })
        .await
        .unwrap();
        assert_eq!(
            a.next().await.unwrap().unwrap().frame,
            Frame::Simple("OK".into())
        );

        b.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"GET", b"k"]),
        })
        .await
        .unwrap();
        assert_eq!(
            b.next().await.unwrap().unwrap().frame,
            Frame::Bulk(Bytes::from_static(b"v"))
        );
    }

    #[tokio::test]
    async fn two_requests_sent_without_waiting_both_get_correct_replies_regardless_of_order() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut con = connect(addr).await;

        con.send(RmpMessage {
            request_id: 10,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"a", b"1"]),
        })
        .await
        .unwrap();
        con.send(RmpMessage {
            request_id: 20,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"b", b"2"]),
        })
        .await
        .unwrap();

        let mut replies = std::collections::HashMap::new();
        for _ in 0..2 {
            let reply = con.next().await.unwrap().unwrap();
            replies.insert(reply.request_id, reply.frame);
        }
        assert_eq!(replies[&10], Frame::Simple("OK".into()));
        assert_eq!(replies[&20], Frame::Simple("OK".into()));
    }

    #[tokio::test]
    async fn an_unknown_command_gets_the_same_error_shape_resp_would() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut con = connect(addr).await;

        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"NOTACOMMAND"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert!(matches!(reply.frame, Frame::Error(_)));
    }

    #[tokio::test]
    async fn the_server_survives_a_client_disconnecting_immediately() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let stream = TcpStream::connect(addr).await.unwrap();
        drop(stream);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // a second, independent connection must still work
        let mut con = connect(addr).await;
        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"PING"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(reply.frame, Frame::Simple("PONG".into()));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail (or don't compile)**

Run: `cargo test -p rocket-mem --lib rmp_connection:: -- --nocapture`
Expected: compile error — `serve` doesn't exist yet.

- [ ] **Step 4: Implement `serve` and `handle_connection`**

```rust
// crates/server/src/rmp_connection.rs — above the tests module
pub async fn serve(
    listener: TcpListener,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
) {
    let mut next_client_id: u64 = 1;
    loop {
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        let client_id = next_client_id;
        next_client_id += 1;
        tokio::spawn(handle_connection(
            socket,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            client_id,
        ));
    }
}

async fn handle_connection(
    socket: tokio::net::TcpStream,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) {
    let framed = Framed::new(socket, RmpCodec::default());
    let (mut sink, mut stream) = framed.split();

    // Every spawned request-handling task below gets its own clone of `tx`; this loop's own
    // clone is dropped when the read loop ends. The writer task's `rx.recv()` only returns
    // `None` once every clone has dropped -- i.e. once every in-flight task has also finished
    // and sent (or failed to send) its reply -- so a client disconnecting mid-flight still gets
    // every reply that was already in progress written out before the connection fully closes.
    let (tx, mut rx) = mpsc::unbounded_channel::<RmpMessage>();

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(msg).await.is_err() {
                break; // the client went away; nothing left to do but stop writing
            }
        }
    });

    while let Some(next) = stream.next().await {
        let request = match next {
            Ok(msg) if msg.msg_type == MsgType::Request => msg,
            _ => break, // decode error, dropped connection, or a stray Response from the client
        };
        let engine = Arc::clone(&engine);
        let aof = Arc::clone(&aof);
        let replication = Arc::clone(&replication);
        let tx = tx.clone();
        // Spawned, not awaited inline: the read loop must go straight back to decoding the next
        // request without waiting for this one's reply -- that's what makes multiple in-flight
        // requests on one connection possible at all.
        tokio::spawn(async move {
            let mut protocol = Protocol::default(); // RMP has no negotiation state to persist
            let reply = dispatcher::dispatch_and_log(
                &engine,
                &aof,
                &replication,
                request.frame,
                &mut protocol,
                client_id,
            );
            let _ = tx.send(RmpMessage {
                request_id: request.request_id,
                msg_type: MsgType::Response,
                frame: reply,
            });
        });
    }
    drop(tx);
    let _ = writer.await;
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib rmp_connection:: -- --nocapture`
Expected: all tests in Step 2 PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/rmp_connection.rs crates/server/src/lib.rs
git commit -m "feat(server): RMP connection handling, multiplexed via dispatch_and_log"
```

---

### Task 2: Wire the RMP listener into `main.rs`

**Files:**
- Modify: `crates/server/src/main.rs:98-103` (the final block, currently binding the RESP listener and calling `rocket_mem::serve(listener, engine, aof, replication).await;`)

**Interfaces:**
- Consumes: `rocket_mem::rmp_connection::serve` (Task 1).
- Produces: a running RMP listener whenever the binary starts, consumed by the integration tests in `04-integration-tests.md` when they spawn the real binary — though most of those tests spawn `serve`/`rmp_connection::serve` directly rather than the binary, matching how `crates/server/tests/integration.rs` already does it.

- [ ] **Step 1: Add the env var and bind the second listener before the final blocking call**

In `crates/server/src/main.rs`, replace the final three lines (`let listener = ...; println!(...); rocket_mem::serve(listener, engine, aof, replication).await; Ok(())`) with:

```rust
    let rmp_addr =
        std::env::var("ROCKET_MEM_RMP_ADDR").unwrap_or_else(|_| "127.0.0.1:6380".to_string());
    let rmp_listener = tokio::net::TcpListener::bind(&rmp_addr).await?;
    println!("RMP listening on {}", rmp_listener.local_addr()?);
    tokio::spawn(rocket_mem::rmp_connection::serve(
        rmp_listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("Listening on {}", listener.local_addr()?);
    rocket_mem::serve(listener, engine, aof, replication).await;
    Ok(())
}
```

(This must land *before* the pre-existing `let listener = ...` block, since the existing `rocket_mem::serve(listener, engine, aof, replication)` call consumes `engine`/`aof`/`replication` by value and blocks forever — the RMP listener needs its own `Arc::clone`s taken first.)

- [ ] **Step 2: Build and manually verify both listeners bind**

Run: `cargo build -p rocket-mem && ROCKET_MEM_AOF_PATH=/tmp/rmp-plan-manual-check.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/rmp-plan-manual-check.snapshot cargo run -p rocket-mem`
Expected output includes both `Listening on 127.0.0.1:6379` and `RMP listening on 127.0.0.1:6380`. Stop the process (Ctrl-C) once confirmed; delete the two `/tmp` files it created.

- [ ] **Step 3: Commit**

```bash
git add crates/server/src/main.rs
git commit -m "feat(server): bind the RMP listener alongside RESP in main.rs"
```
