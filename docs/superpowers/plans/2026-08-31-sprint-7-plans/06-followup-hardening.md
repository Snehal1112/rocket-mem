# Sprint 7 Follow-Up Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** close all five items deferred by rulings during Sprint 7's final whole-branch review and fix wave — RMP connections invisible to connection metrics, `RmpClient`'s `Drop` leaking its reader task, no backpressure on an RMP connection, no test proving the server genuinely delivers replies out of order (not just that the client can cope with it), and a stale `CLAUDE.md`.

**Architecture:** five independent, small changes. No shared design beyond two decisions fixed here as ground truth for Task 3 and Task 4 (see Global Constraints) — this plan is deliberately lightweight (no separate spec doc) since every item was already scoped during Sprint 7's final review; this plan is the design record for the two genuinely new decisions (the in-flight cap and `DEBUG SLEEP`).

**Tech Stack:** `tokio::sync::Semaphore` (already a transitive dependency via `tokio`'s `sync` feature, already enabled workspace-wide — no `Cargo.toml` change needed anywhere in this plan).

**Spec:** [`../../specs/2026-08-31-sprint-7-spec.md`](../../specs/2026-08-31-sprint-7-spec.md) for the original RMP design this plan hardens. No separate spec doc for this follow-up; the two new decisions below are this plan's own design record.

## Global Constraints

- **In-flight request cap per RMP connection: 256**, enforced by a `tokio::sync::Semaphore` the read loop acquires a permit from before spawning each request's handling task, held by that task until it finishes. The reply channel becomes a bounded `mpsc::channel(256)` (matching the cap) instead of unbounded. A client that pipelines past the cap simply has the read loop pause acquiring the next permit — which stops it from reading more off the socket, which applies ordinary TCP backpressure to the sender, mirroring how RESP already gets backpressure for free from its sequential loop.
- **`DEBUG SLEEP <seconds>` is a real, permanent command**, not a test-only hook — matching real Redis's own `DEBUG SLEEP`. It blocks the calling task (via `std::thread::sleep`) for the given (possibly fractional) number of seconds, returns `Simple("OK")`, takes no keys, and is added to `dispatch`'s match arms and `KNOWN_COMMANDS` exactly like every other command — meaning it becomes reachable over both RESP and RMP for free, the same way every other command in Sprint 7 did.
- **Every fix here still respects Sprint 7's core invariant: `dispatcher::dispatch_and_log` and `dispatch` are the only place command behavior lives.** `DEBUG SLEEP` is a new match arm in `dispatch`, not a special case anywhere else. The RMP connection-metrics fix and the backpressure fix both live entirely in `rmp_connection.rs`; neither touches `dispatch_and_log`.
- **`RmpClient` stays a "minimal" client** (per the original spec's own framing) — the `Drop` fix is scoped to stopping the leak, not adding a graceful-shutdown API surface.

---

### Task 1: RMP connections counted in connection metrics

**Files:**
- Modify: `crates/server/src/connection.rs` (the private `ClientGuard` struct and its `Drop` impl — make `pub(crate)` so `rmp_connection.rs` can reuse it instead of duplicating it)
- Modify: `crates/server/src/rmp_connection.rs` (`handle_connection`)

**Interfaces:**
- Consumes: `ReplicationHandle::{connection_opened, connection_closed, connected_clients, total_connections}` (all already exist, used by `connection.rs` today).
- Produces: nothing new — this task makes an existing type (`ClientGuard`) crate-visible so a second file can use it.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/rmp_connection.rs — inside `mod tests`
#[tokio::test]
async fn serve_tracks_connected_rmp_clients_and_drops_the_count_on_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = Arc::new(ReplicationHandle::default());
    tokio::spawn(serve(listener, engine, aof, Arc::clone(&replication)));

    let mut con = connect(addr).await;
    con.send(RmpMessage {
        request_id: 1,
        msg_type: MsgType::Request,
        frame: command(&[b"PING"]),
    })
    .await
    .unwrap();
    con.next().await.unwrap().unwrap();
    assert_eq!(replication.connected_clients(), 1);
    assert_eq!(replication.total_connections(), 1);

    drop(con);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(replication.connected_clients(), 0);
    assert_eq!(replication.total_connections(), 1); // the lifetime total never drops
}
```

(This mirrors `connection.rs`'s own `serve_tracks_connected_clients_and_drops_the_count_on_disconnect` test byte-for-byte in shape — reuse its `connect`/`command` helpers already in `rmp_connection.rs`'s test module if present, or the existing `spawn_test_server`-style setup already in this file's tests.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --lib rmp_connection::tests::serve_tracks_connected_rmp_clients -- --nocapture`
Expected: FAIL — `connected_clients()`/`total_connections()` stay `0` throughout, since nothing calls `connection_opened`/`connection_closed` yet.

- [ ] **Step 3: Make `ClientGuard` crate-visible**

In `crates/server/src/connection.rs`, change:

```rust
struct ClientGuard(Arc<ReplicationHandle>);
```

to:

```rust
pub(crate) struct ClientGuard(pub(crate) Arc<ReplicationHandle>);
```

(Leave its `Drop` impl and every existing call site in `connection.rs` unchanged — this is a pure visibility widening, not a behavior change.)

- [ ] **Step 4: Use it in `rmp_connection.rs`**

In `crates/server/src/rmp_connection.rs`, add the import and the two lines at the top of `handle_connection`, before the `Framed::new(...)` line:

```rust
use crate::connection::ClientGuard;
```

```rust
async fn handle_connection(
    socket: tokio::net::TcpStream,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) {
    replication.connection_opened();
    let _client_guard = ClientGuard(Arc::clone(&replication));
    let framed = Framed::new(socket, RmpCodec);
    // ... unchanged from here
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p rocket-mem --lib rmp_connection:: -- --nocapture`
Expected: all tests in this module PASS, including the new one.

- [ ] **Step 6: Update the README's known-limits note**

In `README.md`'s known-limits paragraph, find:

```markdown
RMP connections are not yet counted in `rocket_mem_connected_clients`/`rocket_mem_connections_total`
— those counters are only wired into RESP's connection lifecycle (`connection.rs`'s `ClientGuard`);
extending them to RMP is a small, contained follow-up (an equivalent guard in
`rmp_connection.rs`), not attempted this sprint to keep it scoped to the protocol itself.
```

Replace with:

```markdown
RMP connections are now counted in `rocket_mem_connected_clients`/`rocket_mem_connections_total`
alongside RESP's — both protocols share the same `ClientGuard` (`connection.rs`).
```

- [ ] **Step 7: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test -p rocket-mem`
Expected: all green.

```bash
git add crates/server/src/connection.rs crates/server/src/rmp_connection.rs README.md
git commit -m "feat(server): count RMP connections in connection metrics, like RESP"
```

---

### Task 2: `RmpClient` doesn't leak its reader task on drop

**Files:**
- Modify: `crates/rmp-client/src/lib.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `RmpClient` gains a private field and a `Drop` impl; its public API (`connect`/`call`/`get`/`set`/`del`) is unchanged.

- [ ] **Step 1: Write the failing test**

```rust
// crates/rmp-client/src/lib.rs — inside `mod tests`
#[tokio::test]
async fn dropping_the_client_closes_its_connection_promptly() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (peer_saw_eof_tx, peer_saw_eof_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(socket, RmpCodec);
        // The peer's read loop ends (None) once the client-side socket fully closes --
        // which requires both the writer half (dropped via write_tx) AND the reader
        // half (currently leaked forever without the Drop fix) to actually go away.
        while framed.next().await.is_some() {}
        let _ = peer_saw_eof_tx.send(());
    });

    let client = RmpClient::connect(addr).await.unwrap();
    drop(client);

    tokio::time::timeout(std::time::Duration::from_secs(2), peer_saw_eof_rx)
        .await
        .expect("peer never saw EOF -- the client's socket leaked past its own drop")
        .unwrap();
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `timeout 10 cargo test -p rmp-client dropping_the_client_closes_its_connection_promptly -- --nocapture`
Expected: FAIL with a timeout panic ("peer never saw EOF...") — the reader task's `SplitStream` half keeps the socket open indefinitely since nothing aborts it.

- [ ] **Step 3: Capture the reader task's handle and add `Drop`**

In `crates/rmp-client/src/lib.rs`, add a field to `RmpClient`:

```rust
pub struct RmpClient {
    write_tx: mpsc::UnboundedSender<RmpMessage>,
    shared: Arc<Shared>,
    reader_handle: tokio::task::JoinHandle<()>,
}
```

In `connect`, capture the reader task's spawn result instead of discarding it:

```rust
let reader_shared = Arc::clone(&shared);
let reader_handle = tokio::spawn(async move {
    while let Some(next) = stream.next().await {
        let Ok(msg) = next else { break };
        if msg.msg_type != MsgType::Response {
            continue;
        }
        if let Some(tx) = reader_shared.pending.lock().unwrap().remove(&msg.request_id) {
            let _ = tx.send(msg.frame);
        }
    }
    reader_shared.pending.lock().unwrap().close();
});

Ok(RmpClient { write_tx, shared, reader_handle })
```

(Adjust the exact body to match whatever the current `PendingReplies`-based implementation actually does — this step only adds capturing the `JoinHandle` and the new struct field, it does not change the reader task's own logic.)

Add the `Drop` impl:

```rust
impl Drop for RmpClient {
    fn drop(&mut self) {
        // Aborting (not gracefully joining) is correct here: nothing can still be
        // awaiting a reply through `self` once `self` is being dropped -- `call`
        // borrows `&self`, so any in-flight call future must have already
        // completed or is being dropped concurrently as part of this same drop.
        self.reader_handle.abort();
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `timeout 10 cargo test -p rmp-client dropping_the_client_closes_its_connection_promptly -- --nocapture`
Expected: PASS, well under the 2-second timeout.

- [ ] **Step 5: Run the full crate's tests to confirm nothing regressed**

Run: `cargo test -p rmp-client`
Expected: all tests pass, including the pre-existing ones from Sprint 7.

- [ ] **Step 6: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test -p rmp-client`
Expected: all green.

```bash
git add crates/rmp-client/src/lib.rs
git commit -m "fix(rmp-client): abort the reader task on drop, closing the socket promptly"
```

---

### Task 3: Backpressure on RMP connections

**Files:**
- Modify: `crates/server/src/rmp_connection.rs`

**Interfaces:**
- Consumes: `tokio::sync::Semaphore` (new import, no new dependency — already part of `tokio`'s enabled `sync` feature).
- Produces: `MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION: usize = 256` (crate-internal constant), consumed only within this file.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/rmp_connection.rs — inside `mod tests`
#[tokio::test]
async fn more_than_the_in_flight_cap_concurrent_requests_all_still_succeed() {
    let (_dir, addr, _engine) = spawn_test_server().await;
    let con = std::sync::Arc::new(tokio::sync::Mutex::new(connect(addr).await));

    // 2x the cap, all fired without waiting for any individual reply first -- proves
    // the semaphore-based cap throttles (pauses reading more requests) rather than
    // ever dropping, corrupting, or deadlocking a request once the cap is in play.
    let total = MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION * 2;
    for i in 0..total as u64 {
        let mut con = con.lock().await;
        con.send(RmpMessage {
            request_id: i,
            msg_type: MsgType::Request,
            frame: command(&[b"PING"]),
        })
        .await
        .unwrap();
    }

    let mut seen = std::collections::HashSet::new();
    for _ in 0..total {
        let mut con = con.lock().await;
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(reply.frame, Frame::Simple("PONG".into()));
        seen.insert(reply.request_id);
    }
    assert_eq!(seen.len(), total);
}
```

(Adjust the connect/send helper usage to match whatever pattern this file's existing tests already use for opening a raw `Framed<TcpStream, RmpCodec>` connection — this test only needs the ability to send many requests without reading replies in between.)

- [ ] **Step 2: Run the test to verify it fails (or times out)**

Run: `timeout 30 cargo test -p rocket-mem --lib rmp_connection::tests::more_than_the_in_flight_cap -- --nocapture`
Expected: with today's unbounded design this test should actually already pass (nothing is bounded yet) — this step is a baseline confirmation, not a red bar. The real test of the cap's *existence* is Step 4 below; this test's job is to guard that adding the cap doesn't break heavy concurrent usage.

- [ ] **Step 3: Add the semaphore and switch to a bounded channel**

In `crates/server/src/rmp_connection.rs`, add near the top:

```rust
use tokio::sync::Semaphore;

/// Caps how many requests on one RMP connection can be mid-dispatch at once. Once the
/// cap is hit, the read loop's next `semaphore.acquire_owned().await` blocks -- it stops
/// reading more requests off the socket, which applies ordinary TCP backpressure to
/// whatever sent them, mirroring how RESP's sequential loop already gets backpressure
/// for free. Without this, a client that pipelines aggressively and never reads its
/// replies could make the server spawn unbounded tasks and queue unbounded encoded
/// replies in memory.
const MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION: usize = 256;
```

In `handle_connection`, change the channel to bounded and add the semaphore:

```rust
let (tx, mut rx) = mpsc::channel::<RmpMessage>(MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION);
let semaphore = Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION));
```

Update the read loop to acquire a permit before spawning, and hold it inside the spawned task:

```rust
while let Some(next) = stream.next().await {
    let request = match next {
        Ok(msg) if msg.msg_type == MsgType::Request => msg,
        Ok(_) => break,
        Err(e) => {
            eprintln!("rmp decode error: {e}");
            break;
        }
    };
    let permit = match Arc::clone(&semaphore).acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => break, // semaphore closed -- only happens if it were explicitly closed, which nothing here does
    };
    let engine = Arc::clone(&engine);
    let aof = Arc::clone(&aof);
    let replication = Arc::clone(&replication);
    let tx = tx.clone();
    tokio::spawn(async move {
        let _permit = permit; // released (dropped) when this task ends, freeing a slot
        let mut protocol = Protocol::default();
        let reply = dispatcher::dispatch_and_log(
            &engine,
            &aof,
            &replication,
            request.frame,
            &mut protocol,
            client_id,
        );
        let _ = tx
            .send(RmpMessage {
                request_id: request.request_id,
                msg_type: MsgType::Response,
                frame: reply,
            })
            .await; // bounded channel: send is now async
    });
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `timeout 30 cargo test -p rocket-mem --lib rmp_connection:: -- --nocapture`
Expected: all tests PASS, including `more_than_the_in_flight_cap_concurrent_requests_all_still_succeed` — proving 512 concurrent requests against a 256-permit cap still all complete correctly (no deadlock, no dropped reply), just throttled.

- [ ] **Step 5: Run the full integration suite to confirm nothing regressed**

Run: `cargo test -p rocket-mem --test rmp`
Expected: all 5+ pre-existing tests in `crates/server/tests/rmp.rs` still pass unchanged — the cap (256) is far above what any of those tests' handful of concurrent requests would ever hit.

- [ ] **Step 6: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test -p rocket-mem`
Expected: all green.

```bash
git add crates/server/src/rmp_connection.rs
git commit -m "feat(server): bound in-flight RMP requests per connection to 256"
```

---

### Task 4: `DEBUG SLEEP` and a genuine out-of-order-delivery test

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (`dispatch`, `KNOWN_COMMANDS`, `key_spec`)
- Modify: `crates/server/tests/rmp.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: a `DEBUG SLEEP <seconds>` command reachable via both RESP and RMP (through the unmodified `dispatch`/`dispatch_and_log` every other command already goes through).

- [ ] **Step 1: Write the failing dispatcher-level test**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
#[test]
fn debug_sleep_blocks_for_approximately_the_requested_duration() {
    let engine = Engine::new();
    let mut protocol = Protocol::default();
    let started = std::time::Instant::now();
    let reply = dispatch(
        &engine,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"DEBUG")),
            Frame::Bulk(Bytes::from_static(b"SLEEP")),
            Frame::Bulk(Bytes::from_static(b"0.05")),
        ]),
        &mut protocol,
        1,
    );
    assert_eq!(reply, Frame::Simple("OK".into()));
    assert!(started.elapsed() >= std::time::Duration::from_millis(45)); // small slack under 50ms
}

#[test]
fn debug_sleep_rejects_a_non_numeric_argument() {
    let engine = Engine::new();
    let mut protocol = Protocol::default();
    let reply = dispatch(
        &engine,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"DEBUG")),
            Frame::Bulk(Bytes::from_static(b"SLEEP")),
            Frame::Bulk(Bytes::from_static(b"not-a-number")),
        ]),
        &mut protocol,
        1,
    );
    assert!(matches!(reply, Frame::Error(_)));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::debug_sleep -- --nocapture`
Expected: FAIL — `DEBUG` isn't a known command yet, so both hit the `_ => Frame::Error("ERR unknown command ...")` arm; the first test's `assert_eq!(reply, Frame::Simple("OK".into()))` fails.

- [ ] **Step 3: Add the `DEBUG` command**

In `crates/server/src/dispatcher.rs`'s `dispatch` function, add a new match arm (placed near `"MEMORY"`/`"OBJECT"`, before the `_ =>` fallthrough):

```rust
"DEBUG" => {
    require_args!(rest, 1, "debug");
    let subcommand = String::from_utf8_lossy(&rest[0]).to_ascii_uppercase();
    match subcommand.as_str() {
        "SLEEP" => {
            require_args!(rest, 2, "debug sleep");
            let secs: f64 = match std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(s) => s,
                None => return Frame::Error("ERR value is not a valid float".into()),
            };
            std::thread::sleep(std::time::Duration::from_secs_f64(secs.max(0.0)));
            Frame::Simple("OK".into())
        }
        _ => Frame::Error(format!("ERR unknown DEBUG subcommand '{subcommand}'")),
    }
}
```

Add `"DEBUG"` to `KNOWN_COMMANDS` — the list is sorted (a `binary_search` guard test enforces this), and `"DEBUG"` sorts immediately before `"DECR"`:

```rust
pub(crate) const KNOWN_COMMANDS: &[&str] = &[
    "APPEND",
    "CLUSTER",
    "COMMAND",
    "DEBUG",
    "DECR",
    // ... rest unchanged
```

Add `"DEBUG"` to `key_spec`'s `KeySpec::None` group:

```rust
"PING" | "ECHO" | "SELECT" | "COMMAND" | "INFO" | "HELLO" | "KEYS" | "SCAN"
| "RANDOMKEY" | "CLUSTER" | "SAVE" | "REPLICAOF" | "PSYNC" | "SLOWLOG" | "DEBUG" => {
    KeySpec::None
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::debug_sleep -- --nocapture`
Expected: both PASS. Also run `cargo test -p rocket-mem --lib dispatcher:: -- --nocapture` in full to confirm the `KNOWN_COMMANDS` sortedness guard test (if present) still passes with `DEBUG` inserted.

- [ ] **Step 5: Write the failing RMP genuine-reordering integration test**

```rust
// crates/server/tests/rmp.rs — append
#[tokio::test]
async fn rmp_genuinely_delivers_a_fast_reply_before_a_slower_concurrent_request_on_one_connection(
) {
    let (_dir, _resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();

    let order: std::sync::Arc<tokio::sync::Mutex<Vec<&str>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));

    let slow_order = std::sync::Arc::clone(&order);
    let slow = async {
        let reply = client
            .call(vec![
                Bytes::from_static(b"DEBUG"),
                Bytes::from_static(b"SLEEP"),
                Bytes::from_static(b"0.3"),
            ])
            .await;
        slow_order.lock().await.push("slow");
        reply
    };

    let fast_order = std::sync::Arc::clone(&order);
    let fast = async {
        let reply = client.call(vec![Bytes::from_static(b"PING")]).await;
        fast_order.lock().await.push("fast");
        reply
    };

    let (slow_result, fast_result) = tokio::join!(slow, fast);
    assert_eq!(slow_result.unwrap(), Frame::Simple("OK".into()));
    assert_eq!(fast_result.unwrap(), Frame::Simple("PONG".into()));

    // The real proof: PING (fast) completed before DEBUG SLEEP 0.3 (slow), even though
    // slow's request was fired first in program order -- this is genuine server-side
    // out-of-order delivery, not just "the client can cope with hypothetical reordering."
    assert_eq!(*order.lock().await, vec!["fast", "slow"]);
}
```

- [ ] **Step 6: Run the test to verify it fails (or passes) meaningfully**

Run: `timeout 10 cargo test -p rocket-mem --test rmp rmp_genuinely_delivers_a_fast_reply -- --nocapture`
Expected: PASS once Tasks 1-3 and this task's Steps 1-4 are already merged (this test exercises already-correct concurrency, so it isn't a red-bar step in the traditional TDD sense) — but it MUST fail if you temporarily revert to a sequential-await connection handler (i.e., it is a meaningful test, not a tautology). If you want to confirm this, temporarily change the read loop in `rmp_connection.rs` to `await` each spawned task before reading the next request, re-run this one test (expect it to now take ~300ms and FAIL the ordering assertion), then revert that temporary change.

- [ ] **Step 7: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

```bash
git add crates/server/src/dispatcher.rs crates/server/tests/rmp.rs
git commit -m "feat(server): add DEBUG SLEEP; test genuine RMP reply reordering"
```

---

### Task 5: `CLAUDE.md` accuracy pass

**Files:**
- Modify: `CLAUDE.md`

**Interfaces:**
- Consumes: `README.md`'s already-accurate Sprint 1-7 status (written and verified during Sprint 7's close-out), the actual current `crates/` layout.
- Produces: an accurate agent-facing project guide.

- [ ] **Step 1: Update "What this is"**

Find:

```markdown
Only Sprint 1 is built so far: a protocol-agnostic storage engine with no networking. There is no RESP parser, no dispatcher, and no TCP listener yet — that's Sprint 2.
```

Replace with:

```markdown
Sprints 1-7 are built: a protocol-agnostic storage engine, RESP2/RESP3 networking, the full command set (strings/hashes/lists/sets/sorted sets/keys), TTL expiry, AOF persistence, snapshotting, leader/follower replication, hash-slot clustering, Prometheus observability, and a second wire protocol of the project's own (RMP, alongside RESP). See `README.md`'s "Status" section for the sprint-by-sprint detail and known limits; only Sprint 8 (auth, ACLs, TLS, release) remains.
```

- [ ] **Step 2: Update "Workspace layout"**

Find:

```markdown
Four crates under `crates/`:

- **`common`** — shared `EngineError` enum (`WrongType`, `NotAnInteger`). Zero dependencies on other crates.
- **`engine`** — the storage engine. Everything implemented so far lives here.
- **`protocol`** — empty placeholder; RESP parser/encoder goes here in Sprint 2.
- **`server`** — empty placeholder binary, package name `rocket-mem` (folder name `server` follows responsibility naming, but `cargo run --bin rocket-mem` is what starts the server once networking exists).

Target end-state architecture (from the production plan) is three layers — Protocol → Command Dispatcher → Storage Engine, with the engine kept protocol-agnostic so RESP and a later custom protocol (Phase 4) can both sit on top without touching engine code. Right now only the bottom layer exists.
```

Replace with:

```markdown
Five crates under `crates/`:

- **`common`** — shared `EngineError` enum (`WrongType`, `NotAnInteger`, `NoSuchKey`). Zero dependencies on other crates.
- **`engine`** — the storage engine: `Value`, the 16-shard `Store`, and one free function per command under `commands/`.
- **`protocol`** — wire formats: RESP's `Frame`/`RespCodec` and RMP's envelope/value codec (`rmp` module), both handling split-read reassembly.
- **`server`** — the binary (package name `rocket-mem`): dual RESP/RMP accept loops, the shared command dispatcher every protocol calls, AOF, snapshotting, replication, cluster routing, Prometheus metrics, and the slow log.
- **`rmp-client`** — a minimal async Rust client for RMP.

This is the three-layer architecture (Protocol → Command Dispatcher → Storage Engine) the production plan targeted from the start, now fully built: the engine stayed protocol-agnostic throughout, which is exactly what let RMP (Sprint 7) sit on top of the same dispatcher RESP already used, without touching engine code.
```

- [ ] **Step 3: Fix the stale "Deferred scope" bullet**

Find:

```markdown
- **Deferred scope**: `SET`'s `EX`/`PX` flags are intentionally not implemented (only `NX`/`XX`) — there's no expiry reaper until Sprint 4, so time-based flags would be dead code until then.
```

Replace with:

```markdown
- **`SET`'s `EX`/`PX` flags**: implemented since Sprint 4 (the TTL/expiry sprint) — `SET k v EX n` sets an absolute expiry the same way a following `EXPIRE` would.
```

- [ ] **Step 4: Update the `engine.rs` facade description**

Find:

```markdown
- **`engine.rs`** — `Engine`, a thin public facade over `Store` (`get`/`set`/`del`/`exists`/`keys`). This is the single entry point Sprint 2's dispatcher will call.
```

Replace with:

```markdown
- **`engine.rs`** — `Engine`, a thin public facade over `Store` — the single entry point the command dispatcher calls. Grew well beyond `get`/`set`/`del`/`exists`/`keys` across later sprints (TTL, snapshotting, eviction, `scan`, `with_ref`/`with_mut`); read the file directly for the current method list rather than trusting a hardcoded one here.
```

- [ ] **Step 5: Proofread and commit**

Run: `grep -n "Sprint 1\|Sprint 2\|empty placeholder\|Four crates\|dead code until then" CLAUDE.md` and confirm no stale matches remain (a `Sprint 2` reference inside `docs/superpowers/...` path examples, like `docs/superpowers/plans/2026-08-29-sprint-2-plans/`, is fine to keep — those are real historical paths, not status claims).

```bash
git add CLAUDE.md
git commit -m "docs: bring CLAUDE.md's project status up to date through Sprint 7"
```

---

## Definition of done

- [ ] `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace` green after every task
- [ ] RMP connections appear in `rocket_mem_connected_clients`/`rocket_mem_connections_total`
- [ ] Dropping an `RmpClient` closes its socket promptly (proven by a peer-side EOF test)
- [ ] An RMP connection handling more requests than the in-flight cap still completes all of them correctly
- [ ] A test proves the server genuinely delivers a fast reply before a slower concurrent one on one RMP connection
- [ ] `CLAUDE.md` accurately describes the project through Sprint 7
