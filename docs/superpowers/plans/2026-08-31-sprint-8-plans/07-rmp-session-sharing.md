# RMP Session-Per-Connection Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** RMP connections share **one** `Session` across every request on that connection, instead of plan 05's placeholder fresh `Session::new()` per request — the fix that makes `AUTH` over RMP actually persist, which is the entire reason `Session` gained shared interior mutability in plan 05.

**Architecture:** `rmp_connection.rs::handle_connection` builds one `Arc<dispatcher::Session>` right after accepting the connection (alongside its existing `Arc<Engine>`/`Arc<AofWriter>`/`Arc<ReplicationHandle>`), and clones that `Arc` into every spawned per-request task instead of constructing a throwaway `Session::new()` inside each task.

**Tech Stack:** nothing new.

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: `Session` replaces the bare `Protocol` parameter" section, "RMP" ownership paragraph.

## Global Constraints

- This plan changes exactly one thing: *where* the `Session` is constructed (once per connection, not once per request). It does not change `dispatch_and_log`'s signature (plan 05 already did that) or add any new command (plan 06 already added `AUTH`).
- Every one of plan 07's own tests must exercise the *real* spawn-per-request concurrency model (`tokio::spawn`, not an inline call) — a test that happens to pass only because it never actually spawns a second concurrent task would not prove sharing across tasks, which is the entire point.

---

### Task 1: One `Arc<Session>` per RMP connection

**Files:**
- Modify: `crates/server/src/rmp_connection.rs`

**Interfaces:**
- Consumes: `dispatcher::Session` (plan 05).
- Produces: nothing new at the type level — this is a wiring change inside `handle_connection`.

- [ ] **Step 1: Change where `Session` is constructed**

In `crates/server/src/rmp_connection.rs`'s `handle_connection`, move the session out of the spawned task and up to connection scope. Change:

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
    let (mut sink, mut stream) = framed.split();
```

to:

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
    let (mut sink, mut stream) = framed.split();
    // ONE Session for this connection's whole lifetime, shared by every request spawned below --
    // this is what lets AUTH on one request be observed by a later, independently-spawned
    // request on the same connection. See
    // ../../docs/superpowers/plans/2026-08-31-sprint-8-plans/07-rmp-session-sharing.md.
    let session = Arc::new(dispatcher::Session::new());
```

Inside the read loop's spawned task, change:

```rust
        let engine = Arc::clone(&engine);
        let aof = Arc::clone(&aof);
        let replication = Arc::clone(&replication);
        let tx = tx.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let session = dispatcher::Session::new(); // still one-per-request here; plan 07 shares one per connection
            let reply = dispatcher::dispatch_and_log(
                &engine,
                &aof,
                &replication,
                request.frame,
                &session,
                client_id,
            );
```

to:

```rust
        let engine = Arc::clone(&engine);
        let aof = Arc::clone(&aof);
        let replication = Arc::clone(&replication);
        let tx = tx.clone();
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            let _permit = permit;
            let reply = dispatcher::dispatch_and_log(
                &engine,
                &aof,
                &replication,
                request.frame,
                &session,
                client_id,
            );
```

- [ ] **Step 2: Verify the crate compiles and existing RMP tests still pass**

Run: `cargo build -p rocket-mem && cargo test -p rocket-mem --lib rmp_connection::`
Expected: clean build, all pre-existing `rmp_connection::tests` PASS unchanged — none of them depend on session freshness, so this wiring change is invisible to them.

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/src/rmp_connection.rs`.

---

### Task 2: Prove `AUTH` persists across independently-spawned RMP requests on one connection

**Files:**
- Modify: `crates/server/src/rmp_connection.rs` (inside its own `mod tests`)

**Interfaces:**
- Consumes: nothing new — end-to-end proof over a real RMP socket.
- Produces: nothing new. This is the test that would have caught Sprint 7's original per-request `Protocol::default()` gap had `AUTH` existed then — see the spec's own framing of this test's purpose.

- [ ] **Step 1: Write the test**

```rust
// crates/server/src/rmp_connection.rs — inside `mod tests`
#[tokio::test]
async fn auth_on_one_rmp_request_is_visible_to_a_later_request_on_the_same_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = Arc::new(ReplicationHandle::default());
    replication
        .acl
        .set_user(
            "app",
            &[
                Bytes::from_static(b"on"),
                Bytes::from_static(b">pw"),
                Bytes::from_static(b"allcommands"),
                Bytes::from_static(b"allkeys"),
            ],
        )
        .unwrap();
    tokio::spawn(serve(listener, engine, aof, Arc::clone(&replication)));

    let mut con = connect(addr).await;

    // Denied before AUTH -- proves the gate is live on this connection at all.
    con.send(RmpMessage {
        request_id: 1,
        msg_type: MsgType::Request,
        frame: command(&[b"GET", b"k"]),
    })
    .await
    .unwrap();
    let reply = con.next().await.unwrap().unwrap();
    assert_eq!(reply.frame, Frame::Error("NOAUTH Authentication required.".into()));

    // AUTH is its own independently-spawned request (a fresh tokio::spawn inside
    // handle_connection, same as every other request) -- awaited here before sending the next
    // one, per the documented rule that a client needing B to observe A's effect must await A
    // first (Sprint 7 spec's multiplexing caveat). This is exactly the scenario where the old
    // per-request `Session::new()` would have silently discarded the authentication.
    con.send(RmpMessage {
        request_id: 2,
        msg_type: MsgType::Request,
        frame: command(&[b"AUTH", b"app", b"pw"]),
    })
    .await
    .unwrap();
    let reply = con.next().await.unwrap().unwrap();
    assert_eq!(reply.frame, Frame::Simple("OK".into()));

    // A third, independently-spawned request on the SAME connection -- must see request 2's
    // authentication.
    con.send(RmpMessage {
        request_id: 3,
        msg_type: MsgType::Request,
        frame: command(&[b"GET", b"k"]),
    })
    .await
    .unwrap();
    let reply = con.next().await.unwrap().unwrap();
    assert_eq!(reply.frame, Frame::Null, "authenticated GET of a missing key, not NOAUTH");
}

#[tokio::test]
async fn a_second_rmp_connection_does_not_share_the_first_connections_authentication() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = Arc::new(ReplicationHandle::default());
    replication
        .acl
        .set_user("app", &[Bytes::from_static(b"on"), Bytes::from_static(b">pw"), Bytes::from_static(b"allcommands"), Bytes::from_static(b"allkeys")])
        .unwrap();
    tokio::spawn(serve(listener, engine, aof, Arc::clone(&replication)));

    let mut a = connect(addr).await;
    a.send(RmpMessage {
        request_id: 1,
        msg_type: MsgType::Request,
        frame: command(&[b"AUTH", b"app", b"pw"]),
    })
    .await
    .unwrap();
    assert_eq!(a.next().await.unwrap().unwrap().frame, Frame::Simple("OK".into()));

    let mut b = connect(addr).await; // a second, independent connection
    b.send(RmpMessage {
        request_id: 1,
        msg_type: MsgType::Request,
        frame: command(&[b"GET", b"k"]),
    })
    .await
    .unwrap();
    assert_eq!(
        b.next().await.unwrap().unwrap().frame,
        Frame::Error("NOAUTH Authentication required.".into()),
        "each connection must have its own Session, not a globally shared one"
    );
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p rocket-mem --lib rmp_connection::tests::auth_on_one_rmp_request rmp_connection::tests::a_second_rmp_connection -- --nocapture`
Expected: both PASS given Task 1's wiring. If Task 1 had *not* moved `Session` construction to connection scope, the first test would fail at its final assertion (`NOAUTH` instead of `Null`) — this is worth confirming once by temporarily reverting Task 1's change and re-running, per this codebase's existing convention of proving a test is meaningful (see Sprint 7 follow-up hardening plan's Task 4, Step 6, for the same pattern), then restoring Task 1's fix.

- [ ] **Step 3: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/src/rmp_connection.rs`.
