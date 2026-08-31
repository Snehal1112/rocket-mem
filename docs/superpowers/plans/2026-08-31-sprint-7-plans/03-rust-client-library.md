# RMP Rust Client Library Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a minimal async Rust client (`crates/rmp-client`) that proves RMP's design end-to-end — connect, issue concurrent requests without waiting for each one, and get correctly-correlated replies back by `request_id` regardless of the order the server answers in.

**Architecture:** `RmpClient::connect` opens the TCP connection and splits it into a background writer task (drains an internal `mpsc` channel onto the socket) and a background reader task (demultiplexes incoming `RmpMessage::Response`s by `request_id` into a `HashMap<u64, oneshot::Sender<Frame>>`). `call(args) -> Frame` is the one primitive everything else is built on: it allocates the next `request_id` from an `AtomicU64`, registers a `oneshot` receiver for it, sends the Request, and awaits the reply. `get`/`set`/`del` are thin wrappers over `call` that also validate the reply shape.

**Tech Stack:** `tokio` (`net`, `sync::{mpsc, oneshot}`), `tokio_util::codec::Framed`, `futures_util::{SinkExt, StreamExt}`, `bytes`. Depends on `protocol::rmp::{RmpCodec, RmpMessage, MsgType}` and `protocol::Frame` from Plan 01.

**Spec:** [`../../specs/2026-08-31-sprint-7-spec.md`](../../specs/2026-08-31-sprint-7-spec.md) — "Decision: client library scope" is authoritative for this plan. Depends on `01-wire-format-codec.md` being complete; independent of `02-server-connection-handling-and-listener.md` (this plan's own tests use a small scripted fake peer, not the real server — proving the *real* server end-to-end is `04-integration-tests.md`'s job).

## Global Constraints

- **`call` is the escape hatch every other method is built on**, not a smaller command surface than the server supports — because the server side already gives full command parity (Plan 02), `call` alone can drive every command RESP can.
- **This plan's own tests never spin up the real `rocket-mem` server.** They script a bare `Framed<TcpStream, RmpCodec>` peer directly in the test file (reading the request the client sent, sending back a canned or deliberately-reordered response) so `RmpClient`'s own request-construction and reply-correlation logic is proven in isolation from the dispatcher. The full-system proof against the real engine/dispatcher lives in `04-integration-tests.md`.
- **A dropped connection fails every reply still waiting**, rather than hanging those callers forever: when the reader task's read loop ends, it drains the pending map and drops every remaining `oneshot::Sender`, which fails the matching `oneshot::Receiver` with a `RecvError` that `call` maps to `RmpError::ConnectionClosed`.

---

### Task 1: Crate scaffold and workspace registration

**Files:**
- Create: `crates/rmp-client/Cargo.toml`
- Create: `crates/rmp-client/src/lib.rs`
- Modify: `Cargo.toml` (workspace root, `members = ["crates/common", "crates/engine", "crates/protocol", "crates/server"]`)

**Interfaces:**
- Consumes: nothing yet (scaffold only).
- Produces: a compiling, empty `rmp-client` crate that later tasks fill in.

- [ ] **Step 1: Register the new workspace member**

```toml
# Cargo.toml (workspace root)
[workspace]
resolver = "2"
members = ["crates/common", "crates/engine", "crates/protocol", "crates/rmp-client", "crates/server"]
```

- [ ] **Step 2: Create the crate manifest**

```toml
# crates/rmp-client/Cargo.toml
[package]
name = "rmp-client"
edition.workspace = true
version.workspace = true

[dependencies]
protocol = { path = "../protocol" }
bytes.workspace = true
tokio.workspace = true
tokio-util.workspace = true
futures-util.workspace = true
```

- [ ] **Step 3: Create an empty lib and verify the workspace builds**

```rust
// crates/rmp-client/src/lib.rs
```

Run: `cargo build --workspace`
Expected: succeeds (an empty crate is a valid crate).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/rmp-client/Cargo.toml crates/rmp-client/src/lib.rs
git commit -m "chore(rmp-client): scaffold the crate and register it in the workspace"
```

---

### Task 2: `RmpError`

**Files:**
- Modify: `crates/rmp-client/src/lib.rs`

**Interfaces:**
- Consumes: `protocol::Frame`.
- Produces: `pub enum RmpError { Io(std::io::Error), ConnectionClosed, ServerError(String), UnexpectedReply(Frame) }`, implementing `std::fmt::Display` and `std::error::Error`, consumed by every method in Task 3.

- [ ] **Step 1: Write the failing test**

```rust
// crates/rmp-client/src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_error_displays_its_message() {
        let err = RmpError::ServerError("WRONGTYPE bad".to_string());
        assert_eq!(err.to_string(), "WRONGTYPE bad");
    }

    #[test]
    fn connection_closed_has_a_stable_message() {
        assert_eq!(RmpError::ConnectionClosed.to_string(), "rmp connection closed");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rmp-client`
Expected: compile error — `RmpError` doesn't exist yet.

- [ ] **Step 3: Implement `RmpError`**

```rust
// crates/rmp-client/src/lib.rs — above the tests module
use protocol::Frame;

#[derive(Debug)]
pub enum RmpError {
    Io(std::io::Error),
    ConnectionClosed,
    ServerError(String),
    UnexpectedReply(Frame),
}

impl std::fmt::Display for RmpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RmpError::Io(e) => write!(f, "rmp io error: {e}"),
            RmpError::ConnectionClosed => write!(f, "rmp connection closed"),
            RmpError::ServerError(msg) => write!(f, "{msg}"),
            RmpError::UnexpectedReply(frame) => write!(f, "unexpected rmp reply: {frame:?}"),
        }
    }
}

impl std::error::Error for RmpError {}

impl From<std::io::Error> for RmpError {
    fn from(e: std::io::Error) -> Self {
        RmpError::Io(e)
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rmp-client`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rmp-client/src/lib.rs
git commit -m "feat(rmp-client): RmpError"
```

---

### Task 3: `RmpClient::connect` and `call`

**Files:**
- Modify: `crates/rmp-client/src/lib.rs`

**Interfaces:**
- Consumes: `protocol::rmp::{RmpCodec, RmpMessage, MsgType}`, `protocol::Frame`, `RmpError` (Task 2).
- Produces: `pub struct RmpClient` with `pub async fn connect(addr: impl tokio::net::ToSocketAddrs) -> Result<Self, RmpError>` and `pub async fn call(&self, args: Vec<Bytes>) -> Result<Frame, RmpError>`, consumed by Task 4 and by `04-integration-tests.md`.

- [ ] **Step 1: Write the failing tests (a scripted fake peer, not a real server)**

```rust
// crates/rmp-client/src/lib.rs — inside `mod tests`
use futures_util::{SinkExt, StreamExt};
use protocol::rmp::{MsgType, RmpCodec, RmpMessage};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

#[tokio::test]
async fn call_sends_the_command_as_an_array_of_bulk_strings_and_returns_the_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(socket, RmpCodec::default());
        let request = framed.next().await.unwrap().unwrap();
        assert_eq!(request.msg_type, MsgType::Request);
        assert_eq!(
            request.frame,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"GET")),
                Frame::Bulk(Bytes::from_static(b"foo")),
            ])
        );
        framed
            .send(RmpMessage {
                request_id: request.request_id,
                msg_type: MsgType::Response,
                frame: Frame::Bulk(Bytes::from_static(b"bar")),
            })
            .await
            .unwrap();
    });

    let client = RmpClient::connect(addr).await.unwrap();
    let reply = client
        .call(vec![Bytes::from_static(b"GET"), Bytes::from_static(b"foo")])
        .await
        .unwrap();
    assert_eq!(reply, Frame::Bulk(Bytes::from_static(b"bar")));
}

#[tokio::test]
async fn call_correlates_replies_by_request_id_even_when_the_server_answers_out_of_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(socket, RmpCodec::default());
        let r1 = framed.next().await.unwrap().unwrap();
        let r2 = framed.next().await.unwrap().unwrap();
        // Deliberately answer the second request first -- proves the client doesn't assume
        // reply order matches request order.
        framed
            .send(RmpMessage {
                request_id: r2.request_id,
                msg_type: MsgType::Response,
                frame: Frame::Simple("OK".into()),
            })
            .await
            .unwrap();
        framed
            .send(RmpMessage {
                request_id: r1.request_id,
                msg_type: MsgType::Response,
                frame: Frame::Integer(42),
            })
            .await
            .unwrap();
    });

    let client = RmpClient::connect(addr).await.unwrap();
    let (r1, r2) = tokio::join!(
        client.call(vec![Bytes::from_static(b"GET"), Bytes::from_static(b"a")]),
        client.call(vec![
            Bytes::from_static(b"SET"),
            Bytes::from_static(b"b"),
            Bytes::from_static(b"1")
        ]),
    );
    assert_eq!(r1.unwrap(), Frame::Integer(42));
    assert_eq!(r2.unwrap(), Frame::Simple("OK".into()));
}

#[tokio::test]
async fn call_fails_with_connection_closed_once_the_peer_disconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        drop(socket); // disconnect immediately, without ever replying
    });

    let client = RmpClient::connect(addr).await.unwrap();
    let result = client.call(vec![Bytes::from_static(b"PING")]).await;
    assert!(matches!(result, Err(RmpError::ConnectionClosed)));
}
```

- [ ] **Step 2: Run the tests to verify they fail (or don't compile)**

Run: `cargo test -p rmp-client`
Expected: compile error — `RmpClient` doesn't exist yet.

- [ ] **Step 3: Implement `RmpClient::connect` and `call`**

```rust
// crates/rmp-client/src/lib.rs — above the tests module
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

struct Shared {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Frame>>>,
}

pub struct RmpClient {
    write_tx: mpsc::UnboundedSender<RmpMessage>,
    shared: Arc<Shared>,
}

impl RmpClient {
    pub async fn connect(addr: impl tokio::net::ToSocketAddrs) -> Result<Self, RmpError> {
        let socket = TcpStream::connect(addr).await?;
        let framed = Framed::new(socket, RmpCodec::default());
        let (mut sink, mut stream) = framed.split();

        let shared = Arc::new(Shared {
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
        });

        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<RmpMessage>();
        tokio::spawn(async move {
            while let Some(msg) = write_rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
        });

        let reader_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            while let Some(next) = stream.next().await {
                let Ok(msg) = next else { break };
                if msg.msg_type != MsgType::Response {
                    continue; // a stray Request from the server would be a protocol violation
                }
                if let Some(tx) = reader_shared.pending.lock().unwrap().remove(&msg.request_id) {
                    let _ = tx.send(msg.frame);
                }
            }
            // The connection ended: fail every reply still waiting instead of hanging forever.
            // Dropping each Sender fails its matching Receiver with a RecvError, which `call`
            // below maps to RmpError::ConnectionClosed.
            reader_shared.pending.lock().unwrap().clear();
        });

        Ok(RmpClient { write_tx, shared })
    }

    pub async fn call(&self, args: Vec<Bytes>) -> Result<Frame, RmpError> {
        let request_id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(request_id, tx);
        let command = Frame::Array(args.into_iter().map(Frame::Bulk).collect());
        self.write_tx
            .send(RmpMessage {
                request_id,
                msg_type: MsgType::Request,
                frame: command,
            })
            .map_err(|_| RmpError::ConnectionClosed)?;
        rx.await.map_err(|_| RmpError::ConnectionClosed)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rmp-client`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rmp-client/src/lib.rs
git commit -m "feat(rmp-client): RmpClient::connect and call"
```

---

### Task 4: `get`/`set`/`del` convenience wrappers

**Files:**
- Modify: `crates/rmp-client/src/lib.rs`

**Interfaces:**
- Consumes: `RmpClient::call` (Task 3).
- Produces: `pub async fn get(&self, key: impl Into<Bytes>) -> Result<Option<Bytes>, RmpError>`, `pub async fn set(&self, key: impl Into<Bytes>, value: impl Into<Bytes>) -> Result<(), RmpError>`, `pub async fn del(&self, key: impl Into<Bytes>) -> Result<bool, RmpError>`, consumed by `04-integration-tests.md`. (`impl Into<Bytes>` accepts `Bytes`, `Vec<u8>`, `String`, or a `&'static str` literal, per the `bytes` crate's own `From` impls.)

- [ ] **Step 1: Write the failing tests**

```rust
// crates/rmp-client/src/lib.rs — inside `mod tests`
#[tokio::test]
async fn get_returns_none_for_a_null_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(socket, RmpCodec::default());
        let request = framed.next().await.unwrap().unwrap();
        framed
            .send(RmpMessage {
                request_id: request.request_id,
                msg_type: MsgType::Response,
                frame: Frame::Null,
            })
            .await
            .unwrap();
    });

    let client = RmpClient::connect(addr).await.unwrap();
    assert_eq!(client.get("missing").await.unwrap(), None);
}

#[tokio::test]
async fn set_succeeds_on_a_simple_ok_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(socket, RmpCodec::default());
        let request = framed.next().await.unwrap().unwrap();
        assert_eq!(
            request.frame,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"k")),
                Frame::Bulk(Bytes::from_static(b"v")),
            ])
        );
        framed
            .send(RmpMessage {
                request_id: request.request_id,
                msg_type: MsgType::Response,
                frame: Frame::Simple("OK".into()),
            })
            .await
            .unwrap();
    });

    let client = RmpClient::connect(addr).await.unwrap();
    client.set("k", "v").await.unwrap();
}

#[tokio::test]
async fn del_returns_true_when_a_key_was_removed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(socket, RmpCodec::default());
        let request = framed.next().await.unwrap().unwrap();
        framed
            .send(RmpMessage {
                request_id: request.request_id,
                msg_type: MsgType::Response,
                frame: Frame::Integer(1),
            })
            .await
            .unwrap();
    });

    let client = RmpClient::connect(addr).await.unwrap();
    assert!(client.del("k").await.unwrap());
}

#[tokio::test]
async fn a_server_error_reply_becomes_rmp_error_server_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(socket, RmpCodec::default());
        let request = framed.next().await.unwrap().unwrap();
        framed
            .send(RmpMessage {
                request_id: request.request_id,
                msg_type: MsgType::Response,
                frame: Frame::Error("WRONGTYPE bad".into()),
            })
            .await
            .unwrap();
    });

    let client = RmpClient::connect(addr).await.unwrap();
    let err = client.get("k").await.unwrap_err();
    assert!(matches!(err, RmpError::ServerError(msg) if msg == "WRONGTYPE bad"));
}
```

- [ ] **Step 2: Run the tests to verify they fail (or don't compile)**

Run: `cargo test -p rmp-client`
Expected: compile error — `get`/`set`/`del` don't exist yet.

- [ ] **Step 3: Implement the wrappers**

```rust
// crates/rmp-client/src/lib.rs — inside `impl RmpClient`, below `call`
pub async fn get(&self, key: impl Into<Bytes>) -> Result<Option<Bytes>, RmpError> {
    match self.call(vec![Bytes::from_static(b"GET"), key.into()]).await? {
        Frame::Bulk(b) => Ok(Some(b)),
        Frame::Null => Ok(None),
        Frame::Error(e) => Err(RmpError::ServerError(e)),
        other => Err(RmpError::UnexpectedReply(other)),
    }
}

pub async fn set(&self, key: impl Into<Bytes>, value: impl Into<Bytes>) -> Result<(), RmpError> {
    match self
        .call(vec![Bytes::from_static(b"SET"), key.into(), value.into()])
        .await?
    {
        Frame::Simple(_) => Ok(()),
        Frame::Error(e) => Err(RmpError::ServerError(e)),
        other => Err(RmpError::UnexpectedReply(other)),
    }
}

pub async fn del(&self, key: impl Into<Bytes>) -> Result<bool, RmpError> {
    match self.call(vec![Bytes::from_static(b"DEL"), key.into()]).await? {
        Frame::Integer(n) => Ok(n > 0),
        Frame::Error(e) => Err(RmpError::ServerError(e)),
        other => Err(RmpError::UnexpectedReply(other)),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rmp-client`
Expected: all tests PASS.

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy -p rmp-client -- -D warnings && cargo test -p rmp-client`
Expected: all green.

```bash
git add crates/rmp-client/src/lib.rs
git commit -m "feat(rmp-client): get/set/del convenience wrappers over call"
```
