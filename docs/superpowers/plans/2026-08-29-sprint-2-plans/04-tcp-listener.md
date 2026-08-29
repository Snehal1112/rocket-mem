# TCP Listener Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a `pub async fn serve(listener: TcpListener, engine: Arc<Engine>)` that accepts connections, spawns one Tokio task per connection, and runs each connection's decode → dispatch → encode loop — the task-per-connection model the master plan's Architecture Decision Record commits to.

**Architecture:** each connection wraps its socket in `Framed<TcpStream, RespCodec>` (from `tokio_util::codec`), which is both a `Stream<Item = Result<Frame, io::Error>>` and a `Sink<Frame>`. The per-connection task loops: read one frame, call `dispatcher::dispatch`, write the response frame, repeat until the client disconnects or a decode error occurs.

**Tech Stack:** `tokio::net::{TcpListener, TcpStream}`, `tokio::spawn`, `tokio_util::codec::Framed`, `futures-util::{StreamExt, SinkExt}` (new dependency — needed to call `.next()`/`.send()` on a `Framed`).

**Spec:** `../../specs/2026-08-29-sprint-2-spec.md` — the lib+bin crate split and the in-process integration-test approach are authoritative.

**Depends on:** `01-resp-frame-and-parser.md` and `03-command-dispatcher.md` must both be complete.

---

### Task 1: Add `tokio` and `futures-util` dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/server/Cargo.toml`

`tokio-util` was already added to `[workspace.dependencies]` by `01-resp-frame-and-parser.md`'s Task 2 — don't redeclare it here, just add the two new ones.

- [ ] **Step 1: Add the workspace dependencies**

```toml
# Cargo.toml — add to [workspace.dependencies] (tokio-util already present from 01-resp-frame-and-parser.md)
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "io-util", "time"] }
futures-util = "0.3"
```

- [ ] **Step 2: Add them to `crates/server`**

```toml
# crates/server/Cargo.toml
[dependencies]
engine = { path = "../engine" }
protocol = { path = "../protocol" }
common = { path = "../common" }
tokio.workspace = true
tokio-util.workspace = true
futures-util.workspace = true
```

- [ ] **Step 3: Verify the workspace still builds**

Run: `cargo build --workspace`
Expected: PASS — no code changes yet, just new dependencies resolving

- [ ] **Step 4: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`Cargo.toml`, `crates/server/Cargo.toml`, and `Cargo.lock` — do not compose the commit
message freeform. Suggested subject: `chore(server): add tokio, tokio-util, futures-util dependencies`.

---

### Task 2: `serve` — accept loop + per-connection dispatch loop

**Files:**
- Create: `crates/server/src/connection.rs`
- Modify: `crates/server/src/lib.rs`

**Interfaces:**
- Consumes: `dispatcher::dispatch` (Task 2 of `03-command-dispatcher.md`), `protocol::codec::RespCodec` (Task 2/3 of `01-resp-frame-and-parser.md`).
- Produces: `pub async fn serve(listener: tokio::net::TcpListener, engine: std::sync::Arc<engine::Engine>)` — `06-integration-test-harness.md` and `main.rs` both call this directly.

- [ ] **Step 1: Write the failing test**

This is an async, real-socket test — write it against `serve` directly rather than against a lower-level piece, since the whole point of this task is proving the accept loop and per-connection loop work together.

```rust
// crates/server/src/connection.rs
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use engine::Engine;
    use protocol::{codec::RespCodec, Frame};
    use std::sync::Arc;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::codec::Framed;
    use futures_util::{SinkExt, StreamExt};

    #[tokio::test]
    async fn serve_handles_a_full_set_get_round_trip_over_a_real_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        tokio::spawn(serve(listener, engine));

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec);

        framed.send(Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Bulk(Bytes::from_static(b"foo")),
            Frame::Bulk(Bytes::from_static(b"bar")),
        ])).await.unwrap();
        assert_eq!(framed.next().await.unwrap().unwrap(), Frame::Simple("OK".into()));

        framed.send(Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"GET")),
            Frame::Bulk(Bytes::from_static(b"foo")),
        ])).await.unwrap();
        assert_eq!(framed.next().await.unwrap().unwrap(), Frame::Bulk(Bytes::from_static(b"bar")));
    }

    #[tokio::test]
    async fn serve_handles_two_concurrent_connections_independently() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        tokio::spawn(serve(listener, engine));

        let mut a = Framed::new(TcpStream::connect(addr).await.unwrap(), RespCodec);
        let mut b = Framed::new(TcpStream::connect(addr).await.unwrap(), RespCodec);

        a.send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"SET")), Frame::Bulk(Bytes::from_static(b"k")), Frame::Bulk(Bytes::from_static(b"a"))])).await.unwrap();
        assert_eq!(a.next().await.unwrap().unwrap(), Frame::Simple("OK".into()));

        // same key, both connections share the one Engine — b sees a's write
        b.send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"GET")), Frame::Bulk(Bytes::from_static(b"k"))])).await.unwrap();
        assert_eq!(b.next().await.unwrap().unwrap(), Frame::Bulk(Bytes::from_static(b"a")));
    }

    #[tokio::test]
    async fn serve_closes_the_connection_cleanly_when_the_client_disconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        tokio::spawn(serve(listener, engine));

        let stream = TcpStream::connect(addr).await.unwrap();
        drop(stream); // disconnect immediately, before sending anything

        // give the server task a moment to observe the disconnect and return,
        // rather than panicking or looping forever
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // a second, independent connection must still work — proves the
        // dropped connection's task didn't take the whole server down with it
        let mut framed = Framed::new(TcpStream::connect(addr).await.unwrap(), RespCodec);
        framed.send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))])).await.unwrap();
        // PING isn't wired until 05-stub-commands.md — expect the current "unknown command" error, not a hang or crash
        assert!(matches!(framed.next().await.unwrap().unwrap(), Frame::Error(_)));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem connection::tests`
Expected: FAIL — `serve` not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/connection.rs (above the test module)
use crate::dispatcher;
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use protocol::codec::RespCodec;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::codec::Framed;

pub async fn serve(listener: TcpListener, engine: Arc<Engine>) {
    loop {
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        let engine = Arc::clone(&engine);
        tokio::spawn(handle_connection(socket, engine));
    }
}

async fn handle_connection(socket: tokio::net::TcpStream, engine: Arc<Engine>) {
    let mut framed = Framed::new(socket, RespCodec);
    while let Some(result) = framed.next().await {
        let frame = match result {
            Ok(frame) => frame,
            Err(_) => return, // malformed input or a dropped connection — end this task quietly
        };
        let response = dispatcher::dispatch(&engine, frame);
        if framed.send(response).await.is_err() {
            return; // client went away mid-response
        }
    }
}
```

```rust
// crates/server/src/lib.rs
pub mod connection;
pub mod dispatcher;
pub use connection::serve;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem connection::tests`
Expected: PASS, 3/3

- [ ] **Step 5: Wire `main.rs` to actually start the server**

```rust
// crates/server/src/main.rs
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:6379").await?;
    println!("Listening on {}", listener.local_addr()?);
    let engine = Arc::new(engine::Engine::new());
    rocket_mem::serve(listener, engine).await;
    Ok(())
}
```

- [ ] **Step 6: Manually verify against `redis-cli`**

Run: `cargo run --bin rocket-mem` in one terminal, then in another: `redis-cli -p 6379 set foo bar` then `redis-cli -p 6379 get foo`
Expected: `OK` then `"bar"` — this is the sprint's first real end-to-end proof, don't skip it even though the automated tests above already pass

- [ ] **Step 7: Run the full workspace check**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all clean

- [ ] **Step 8: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/connection.rs`, `crates/server/src/lib.rs`, and `crates/server/src/main.rs`
— do not compose the commit message freeform. Suggested subject:
`feat(server): add TCP accept loop, task-per-connection, wire up main()`.
