# RMP Integration Tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** prove, end-to-end against the real binary's code paths (real `Engine`, real `dispatch_and_log`, both listeners bound to the same shared state), the two claims the spec and the production plan both name explicitly: RESP and RMP see one consistent shared keyspace, and RMP correctly multiplexes concurrent requests on a single connection.

**Architecture:** one new integration test file, `crates/server/tests/rmp.rs`, mirroring `crates/server/tests/integration.rs`'s `spawn_test_server` pattern but binding *both* a RESP listener (`rocket_mem::serve`) and an RMP listener (`rocket_mem::rmp_connection::serve`) to one shared `Arc<Engine>`/`Arc<AofWriter>`/`Arc<ReplicationHandle>`. RESP-side assertions use the `redis` crate (already a dev-dependency); RMP-side assertions use the `rmp-client` crate from Plan 03.

**Tech Stack:** `redis` (existing dev-dependency), `rmp-client` (Plan 03, new dev-dependency), `protocol::rmp` (Plan 01, for the one raw-socket test that needs to control message framing directly), `tempfile` (existing dev-dependency).

**Spec:** [`../../specs/2026-08-31-sprint-7-spec.md`](../../specs/2026-08-31-sprint-7-spec.md) — "Testing strategy" is authoritative for this plan. Depends on `01-wire-format-codec.md`, `02-server-connection-handling-and-listener.md`, and `03-rust-client-library.md` all being complete.

## Global Constraints

- **These tests exercise the real dispatcher, not a mock.** No test in this plan stubs out `Engine`, `AofWriter`, or `ReplicationHandle` — that would prove nothing about the "reuse `dispatch_and_log` unchanged" design decision this sprint's whole risk profile rests on.
- **Every test binds both listeners to one shared `Engine`/`AofWriter`/`ReplicationHandle`**, exactly as `main.rs` does in production — a test that stood up two independent engines would not be testing the "shared keyspace" claim at all.

---

### Task 1: Test harness — `spawn_dual_protocol_server`

**Files:**
- Modify: `crates/server/Cargo.toml` (`[dev-dependencies]`, currently `redis = { version = "0.27", features = ["tokio-comp"] }` and `tempfile = "3"`)
- Create: `crates/server/tests/rmp.rs`

**Interfaces:**
- Consumes: `rocket_mem::serve`, `rocket_mem::rmp_connection::serve`, `rocket_mem::aof::AofWriter`, `rocket_mem::replication::ReplicationHandle`, `engine::Engine`, `rmp_client::RmpClient`.
- Produces: `async fn spawn_dual_protocol_server() -> (tempfile::TempDir, String, std::net::SocketAddr)` — a `redis://`-prefixed URL for the RESP side (matching `integration.rs`'s existing convention) and a plain `SocketAddr` for the RMP side — consumed by every test in this plan.

- [ ] **Step 1: Add `rmp-client` as a dev-dependency**

```toml
# crates/server/Cargo.toml
[dev-dependencies]
redis = { version = "0.27", features = ["tokio-comp"] }
rmp-client = { path = "../rmp-client" }
tempfile = "3"
```

- [ ] **Step 2: Write the harness**

```rust
// crates/server/tests/rmp.rs — top of file
use bytes::Bytes;
use protocol::rmp::{MsgType, RmpCodec, RmpMessage};
use protocol::Frame;
use redis::AsyncCommands;
use std::sync::Arc;
use tokio::net::TcpListener;

async fn spawn_dual_protocol_server() -> (tempfile::TempDir, String, std::net::SocketAddr) {
    let engine = Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().unwrap();
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("test.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::default());

    let resp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let resp_addr = resp_listener.local_addr().unwrap();
    let rmp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rmp_addr = rmp_listener.local_addr().unwrap();

    tokio::spawn(rocket_mem::serve(
        resp_listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));
    tokio::spawn(rocket_mem::rmp_connection::serve(
        rmp_listener,
        engine,
        aof,
        replication,
    ));

    (dir, format!("redis://{resp_addr}"), rmp_addr)
}

fn command(args: &[&[u8]]) -> Frame {
    Frame::Array(
        args.iter()
            .map(|a| Frame::Bulk(Bytes::copy_from_slice(a)))
            .collect(),
    )
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo test -p rocket-mem --test rmp --no-run`
Expected: compiles cleanly (no tests exist yet, so nothing runs).

- [ ] **Step 4: Commit**

```bash
git add crates/server/Cargo.toml crates/server/tests/rmp.rs
git commit -m "test(server): dual-protocol integration test harness"
```

---

### Task 2: Shared keyspace — RESP and RMP see the same writes

**Files:**
- Modify: `crates/server/tests/rmp.rs`

**Interfaces:**
- Consumes: `spawn_dual_protocol_server` (Task 1), `redis::Client`, `rmp_client::RmpClient`.
- Produces: nothing new — this task is pure test coverage.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/tests/rmp.rs — append
#[tokio::test]
async fn resp_write_is_visible_to_a_read_over_rmp() {
    let (_dir, resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let redis_client = redis::Client::open(resp_url).unwrap();
    let mut resp_con = redis_client.get_multiplexed_async_connection().await.unwrap();
    let _: () = resp_con.set("k", "v").await.unwrap();

    let rmp_client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();
    assert_eq!(rmp_client.get("k").await.unwrap(), Some(Bytes::from_static(b"v")));
}

#[tokio::test]
async fn rmp_write_is_visible_to_a_read_over_resp() {
    let (_dir, resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let rmp_client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();
    rmp_client.set("k", "v").await.unwrap();

    let redis_client = redis::Client::open(resp_url).unwrap();
    let mut resp_con = redis_client.get_multiplexed_async_connection().await.unwrap();
    let value: String = resp_con.get("k").await.unwrap();
    assert_eq!(value, "v");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --test rmp`
Expected: FAIL — `spawn_dual_protocol_server` exists (Task 1) but no server code exists yet unless Plans 01-03 are already merged. If Plans 01-03 are complete, these should already PASS; if so, treat this step as confirmation rather than a red step, and proceed.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --test rmp`
Expected: PASS (both directions).

- [ ] **Step 4: Commit**

```bash
git add crates/server/tests/rmp.rs
git commit -m "test(server): RESP and RMP share one keyspace"
```

---

### Task 3: Multiplexing — concurrent requests correlate correctly

**Files:**
- Modify: `crates/server/tests/rmp.rs`

**Interfaces:**
- Consumes: `spawn_dual_protocol_server` (Task 1), `rmp_client::RmpClient`.
- Produces: nothing new — pure test coverage.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/tests/rmp.rs — append
#[tokio::test]
async fn rmp_correctly_multiplexes_concurrent_requests_on_one_connection() {
    let (_dir, _resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();
    client.set("a", "1").await.unwrap();

    // Fired concurrently on the same connection, without either awaiting the other first.
    let (get_result, set_result) = tokio::join!(client.get("a"), client.set("b", "2"));
    assert_eq!(get_result.unwrap(), Some(Bytes::from_static(b"1")));
    set_result.unwrap();
    assert_eq!(client.get("b").await.unwrap(), Some(Bytes::from_static(b"2")));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --test rmp rmp_correctly_multiplexes`
Expected: PASS if Plans 01-03 are complete (this exercises already-built functionality); this step exists to confirm the assertion is meaningful, not to find a red bar.

- [ ] **Step 3: Run the test to verify it passes**

Run: `cargo test -p rocket-mem --test rmp rmp_correctly_multiplexes`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/server/tests/rmp.rs
git commit -m "test(server): RMP multiplexing proof against the real server"
```

---

### Task 4: Full parity — `INFO` and `CLUSTER` reach RMP too

**Files:**
- Modify: `crates/server/tests/rmp.rs`

**Interfaces:**
- Consumes: `spawn_dual_protocol_server` (Task 1), `rmp_client::RmpClient::call`.
- Produces: nothing new — pure test coverage, proving the spec's "reuse `dispatch_and_log` unchanged" decision actually delivers on commands outside the plain string/collection families.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/tests/rmp.rs — append
#[tokio::test]
async fn rmp_reaches_info_and_cluster_commands() {
    let (_dir, _resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();

    let info = client.call(vec![Bytes::from_static(b"INFO")]).await.unwrap();
    match info {
        Frame::Bulk(b) => assert!(String::from_utf8_lossy(&b).contains("# Server")),
        other => panic!("expected INFO to reply Bulk, got {other:?}"),
    }

    // 12182 is the known reference value for key_slot(b"foo") (Sprint 6 spec), independent of
    // whether cluster mode is configured -- CLUSTER KEYSLOT is a pure function of the key.
    let slot = client
        .call(vec![
            Bytes::from_static(b"CLUSTER"),
            Bytes::from_static(b"KEYSLOT"),
            Bytes::from_static(b"foo"),
        ])
        .await
        .unwrap();
    assert_eq!(slot, Frame::Integer(12182));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --test rmp rmp_reaches_info_and_cluster_commands`
Expected: PASS if Plans 01-02 are complete (this is a coverage proof, not new functionality); confirms the assertion is meaningful.

- [ ] **Step 3: Run the test to verify it passes**

Run: `cargo test -p rocket-mem --test rmp rmp_reaches_info_and_cluster_commands`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/server/tests/rmp.rs
git commit -m "test(server): RMP reaches INFO and CLUSTER via the shared dispatcher"
```

---

### Task 5: Disconnect mid-flight doesn't take the connection handling down

**Files:**
- Modify: `crates/server/tests/rmp.rs`

**Interfaces:**
- Consumes: `spawn_dual_protocol_server` (Task 1), `protocol::rmp::{RmpCodec, RmpMessage, MsgType}` (raw framing, to control exactly when the socket closes relative to sending), `command` helper (Task 1).
- Produces: nothing new — pure test coverage, mirroring `connection.rs`'s existing `serve_closes_the_connection_cleanly_when_the_client_disconnects`.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/tests/rmp.rs — append
use futures_util::SinkExt;
use tokio_util::codec::Framed;

#[tokio::test]
async fn the_server_survives_an_rmp_client_disconnecting_before_reading_its_reply() {
    let (_dir, _resp_url, rmp_addr) = spawn_dual_protocol_server().await;

    {
        let socket = tokio::net::TcpStream::connect(rmp_addr).await.unwrap();
        let mut framed = Framed::new(socket, RmpCodec::default());
        framed
            .send(RmpMessage {
                request_id: 1,
                msg_type: MsgType::Request,
                frame: command(&[b"PING"]),
            })
            .await
            .unwrap();
        // `framed` (and its socket) drops here, before the reply is ever read.
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // A second, independent connection must still work -- proves the dropped connection's
    // spawned tasks and writer loop didn't take the whole listener down.
    let client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();
    assert_eq!(
        client.call(vec![Bytes::from_static(b"PING")]).await.unwrap(),
        Frame::Simple("PONG".into())
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --test rmp the_server_survives_an_rmp_client_disconnecting`
Expected: PASS if Plan 02 is complete; confirms the assertion is meaningful.

- [ ] **Step 3: Run the test to verify it passes**

Run: `cargo test -p rocket-mem --test rmp the_server_survives_an_rmp_client_disconnecting`
Expected: PASS.

- [ ] **Step 4: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green — this is the sprint's full Definition of Done check.

```bash
git add crates/server/tests/rmp.rs
git commit -m "test(server): RMP survives a client disconnecting mid-flight"
```
