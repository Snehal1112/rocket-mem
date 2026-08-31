# Session Type & dispatch_and_log Signature Swap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** replace `dispatch_and_log`'s `protocol: &mut Protocol` parameter with `session: &Session` — a small struct carrying both RESP's existing protocol-negotiation state and a new shared, per-*connection* authenticated-user cell, which plan 06 will check and plan 07 will make RMP share correctly for the first time.

**Architecture:** `Session` uses interior mutability (`Cell<Protocol>` — `Protocol` is `Copy` — plus `Mutex<Option<Arc<AclUser>>>`) specifically so it can be passed as `&Session` (shared reference) rather than `&mut Protocol` (exclusive reference), which is what lets one RMP connection's several concurrently-spawned request tasks all hold a clone of the same `Arc<Session>` later (plan 07). This plan only changes `dispatch_and_log`'s signature and RESP's wiring; `dispatch` itself (called by AOF replay and the follower apply loop) is untouched — its `_protocol: &mut Protocol` parameter is already unused, and neither of those callers needs auth state.

**Tech Stack:** nothing new.

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: `Session` replaces the bare `Protocol` parameter" section.

## Global Constraints

- `dispatch`'s signature does **not** change in this plan (or ever, in this sprint) — only `dispatch_and_log`'s does. This keeps AOF replay and the follower apply loop, both of which call `dispatch` directly and never need auth state, completely untouched.
- Every call site in the crate that passes a `Protocol` to `dispatch_and_log` must be updated in the same commit that changes the signature, or the crate does not compile — Rust offers no partial-migration path here. This plan's Task 2 is scoped around that reality: it is one large, purely mechanical edit (two fixed substitution patterns, shown below, applied via the compiler's own error list as a checklist), not ~80 independently-designed changes.
- RMP's connection handling (`rmp_connection.rs`) gets only the *minimal* fix needed to keep compiling in this plan — still one throwaway `Session::new()` per request, identical in effect to today's per-request `Protocol::default()`. Upgrading it to one shared `Arc<Session>` per *connection* is plan 07's job, deliberately kept separate so this plan's diff stays reviewable as "change the type" without also being "change RMP's behavior."

---

### Task 1: `Session` struct + unit tests

**Files:**
- Create or modify: `crates/server/src/dispatcher.rs` (add near the top, alongside the existing `Protocol` import — `Session` is small enough not to need its own file, and it is dispatcher-owned state)

**Interfaces:**
- Consumes: `protocol::codec::Protocol` (existing), `crate::acl::AclUser` (plan 03).
- Produces: `pub struct Session { .. }` with `new`, `protocol`, `set_protocol`, `authenticated_user`, `set_authenticated_user`. Task 2 here, and plans 06/07, are the consumers.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
#[test]
fn a_new_session_is_unauthenticated_with_default_protocol() {
    let session = Session::new();
    assert_eq!(session.protocol(), Protocol::default());
    assert!(session.authenticated_user().is_none());
}

#[test]
fn set_protocol_and_set_authenticated_user_are_visible_through_get() {
    let session = Session::new();
    session.set_protocol(Protocol::Resp3);
    assert_eq!(session.protocol(), Protocol::Resp3);

    let user = std::sync::Arc::new(crate::acl::AclUser {
        username: "app".to_string(),
        password_hash: None,
        enabled: true,
        rules: vec![],
    });
    session.set_authenticated_user(Some(std::sync::Arc::clone(&user)));
    assert_eq!(
        session.authenticated_user().map(|u| u.username.clone()),
        Some("app".to_string())
    );
}

#[test]
fn a_mutation_through_one_arc_clone_is_visible_through_another() {
    // The property plan 07 depends on: several tasks holding independent Arc<Session> clones
    // (one per spawned RMP request) must all see the same underlying state.
    let session = std::sync::Arc::new(Session::new());
    let clone_a = std::sync::Arc::clone(&session);
    let clone_b = std::sync::Arc::clone(&session);

    let user = std::sync::Arc::new(crate::acl::AclUser {
        username: "app".to_string(),
        password_hash: None,
        enabled: true,
        rules: vec![],
    });
    clone_a.set_authenticated_user(Some(user));
    assert!(clone_b.authenticated_user().is_some());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::a_new_session dispatcher::tests::set_protocol dispatcher::tests::a_mutation_through -- --nocapture`
Expected: FAIL to compile — `Session` doesn't exist yet.

- [ ] **Step 3: Implement `Session`**

```rust
// crates/server/src/dispatcher.rs — near the top, after the existing `use` block
/// Per-connection state `dispatch_and_log` reads/writes, replacing the bare `protocol: &mut
/// Protocol` parameter it used through Sprint 7. Interior mutability (`Cell`/`Mutex`) is
/// deliberate: it lets this be passed as `&Session` (a shared reference) rather than `&mut
/// Protocol` (exclusive), which is what makes it possible for several concurrently-spawned RMP
/// request tasks to hold independent `Arc<Session>` clones of the *same* connection's state
/// (plan 07) -- an exclusive reference could never be handed to more than one task at a time.
///
/// RESP's connection loop owns one `Session` across its lifetime and passes `&session` each
/// iteration (Task 3 here). RMP's connection handler will own one `Arc<Session>` per accepted
/// connection and clone it into every spawned per-request task (plan 07) -- until then, this
/// plan's Task 2 gives RMP a fresh, throwaway `Session::new()` per request, identical in effect
/// to today's per-request `Protocol::default()`.
pub struct Session {
    protocol: std::cell::Cell<protocol::codec::Protocol>,
    authenticated_user: std::sync::Mutex<Option<std::sync::Arc<crate::acl::AclUser>>>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            protocol: std::cell::Cell::new(protocol::codec::Protocol::default()),
            authenticated_user: std::sync::Mutex::new(None),
        }
    }

    pub fn protocol(&self) -> protocol::codec::Protocol {
        self.protocol.get()
    }

    pub fn set_protocol(&self, p: protocol::codec::Protocol) {
        self.protocol.set(p);
    }

    pub fn authenticated_user(&self) -> Option<std::sync::Arc<crate::acl::AclUser>> {
        self.authenticated_user
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_authenticated_user(&self, user: Option<std::sync::Arc<crate::acl::AclUser>>) {
        *self.authenticated_user.lock().unwrap_or_else(|e| e.into_inner()) = user;
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::a_new_session dispatcher::tests::set_protocol dispatcher::tests::a_mutation_through -- --nocapture`
Expected: PASS. (The crate as a whole still compiles at this point — `Session` is a new, unused-by-`dispatch_and_log`-yet type, so nothing else breaks.)

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/src/dispatcher.rs`.

---

### Task 2: Swap `dispatch_and_log`'s `Protocol` parameter for `Session`, fix every call site

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (signature, `handle_hello`, and ~80 test call sites)
- Modify: `crates/server/src/connection.rs` (1 call site — real per-connection wiring)
- Modify: `crates/server/src/rmp_connection.rs` (1 call site — minimal compile fix only, see Global Constraints)
- Modify: `crates/server/src/replication.rs` (1 call site, inside its own test module)

**Interfaces:**
- Consumes: `Session` (Task 1).
- Produces: `pub fn dispatch_and_log(engine: &Engine, aof: &AofWriter, replication: &ReplicationHandle, frame: Frame, session: &Session, client_id: u64) -> Frame` — the new signature every later plan (06, 07, 08) builds on.

- [ ] **Step 1: Change the signature and `handle_hello`**

In `crates/server/src/dispatcher.rs`, change:

```rust
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
```

to:

```rust
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    session: &Session,
    client_id: u64,
) -> Frame {
```

Propagate the renamed parameter into `dispatch_and_log`'s own body (it forwards to `dispatch_and_log_inner`, which also currently takes `protocol: &mut Protocol` — rename that parameter and its type the same way, all the way through).

In `handle_hello` (currently `fn handle_hello(frame: &Frame, protocol: &mut Protocol, client_id: u64, replication: &ReplicationHandle) -> Option<Frame>`), change its `protocol` parameter to `session: &Session`, and replace every `*protocol = Protocol::Resp2;` / `*protocol = Protocol::Resp3;` with `session.set_protocol(Protocol::Resp2);` / `session.set_protocol(Protocol::Resp3);`, and every read of `*protocol` (in `hello_reply(*protocol, ...)`) with `session.protocol()`. Update `handle_hello`'s call site inside `dispatch_and_log_inner` from `handle_hello(&frame, protocol, client_id, replication)` to `handle_hello(&frame, session, client_id, replication)`.

- [ ] **Step 2: Fix `connection.rs`'s call site (real per-connection wiring)**

In `crates/server/src/connection.rs`'s `handle_connection`, change:

```rust
let mut protocol = Protocol::default();
```

to:

```rust
let session = dispatcher::Session::new();
```

and change the `dispatch_and_log` call from `&mut protocol` to `&session`. Change the post-dispatch sync line from:

```rust
framed.codec_mut().protocol = protocol; // sync BEFORE sending this reply
```

to:

```rust
framed.codec_mut().protocol = session.protocol(); // sync BEFORE sending this reply
```

- [ ] **Step 3: Fix `rmp_connection.rs`'s call site (minimal, behavior-preserving)**

In `crates/server/src/rmp_connection.rs`'s spawned per-request task, change:

```rust
let mut protocol = Protocol::default(); // RMP has no negotiation state to persist
let reply = dispatcher::dispatch_and_log(
    &engine,
    &aof,
    &replication,
    request.frame,
    &mut protocol,
    client_id,
);
```

to:

```rust
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

The `use protocol::codec::Protocol;` import at the top of this file becomes unused after this change — remove it (or leave `Protocol` referenced elsewhere in the file if it still is; check with `cargo clippy` in Step 5).

- [ ] **Step 4: Fix every same-crate test call site**

`dispatcher.rs`'s own `mod tests` (roughly 80 call sites) and `replication.rs`'s `mod tests` (1 call site) all pass `&mut Protocol::default()` as the 5th positional argument to `dispatch_and_log`. Run `cargo build -p rocket-mem --tests 2>&1 | grep "dispatcher.rs\|replication.rs"` to get the compiler's own list of every remaining mismatch, and fix each with one of these two exact patterns:

**Pattern A — the overwhelmingly common case**, a call site that builds its `Protocol` inline and discards it:

```rust
// before
dispatch_and_log(&engine, &aof, &replication, frame, &mut Protocol::default(), client_id)
// after
dispatch_and_log(&engine, &aof, &replication, frame, &Session::new(), client_id)
```

**Pattern B — a test that threads one `protocol` variable across multiple sequential calls** (roughly 9 sites, all exercising `HELLO` negotiation persisting across calls — e.g. `hello_2_then_a_null_reply_encodes_as_resp2s_null_bulk_string`-style tests). Change:

```rust
// before
let mut protocol = Protocol::default();
/* ... */ dispatch_and_log(&engine, &aof, &replication, frame1, &mut protocol, 1);
/* ... */ dispatch_and_log(&engine, &aof, &replication, frame2, &mut protocol, 1);
// after
let session = Session::new();
/* ... */ dispatch_and_log(&engine, &aof, &replication, frame1, &session, 1);
/* ... */ dispatch_and_log(&engine, &aof, &replication, frame2, &session, 1);
```

(`Session`'s interior mutability means the second call sees the first call's `HELLO`-driven protocol switch automatically — no `&mut` needed, which is itself the point of Task 1's design.)

Iterate `cargo build -p rocket-mem --tests` until it is clean; every remaining error will be one of these two patterns.

- [ ] **Step 5: Run the full workspace suite**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green — every one of the ~600 pre-existing tests must pass unchanged, since this task is a pure mechanical type substitution with no intended behavior change anywhere (RMP's per-request `Session::new()` is exactly as inert as its old per-request `Protocol::default()` was).

- [ ] **Step 6: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/src/dispatcher.rs`, `crates/server/src/connection.rs`, `crates/server/src/rmp_connection.rs`, `crates/server/src/replication.rs`.

---

### Task 3: Prove `Session` persists across sequential requests on one RESP connection

**Files:**
- Modify: `crates/server/tests/integration.rs` (or add alongside `connection.rs`'s own `mod tests` — either is fine; `tests/integration.rs` is chosen here since it already exercises real-socket RESP round trips)

**Interfaces:**
- Consumes: nothing new — this is a behavioral proof, not new production code.
- Produces: nothing new — confirms Task 2's refactor preserved real per-connection state correctly, using RESP3 protocol negotiation (already observable in wire encoding) as the proxy, since `ACL`/`AUTH` don't exist until plan 06.

- [ ] **Step 1: Write the test**

```rust
// crates/server/tests/integration.rs
#[tokio::test]
async fn session_state_persists_across_sequential_requests_on_one_resp_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = std::sync::Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().unwrap();
    let aof = std::sync::Arc::new(
        rocket_mem::aof::AofWriter::open(&dir.path().join("test.aof"), rocket_mem::aof::FsyncPolicy::Never)
            .unwrap(),
    );
    tokio::spawn(rocket_mem::serve(
        listener,
        engine,
        aof,
        std::sync::Arc::new(rocket_mem::replication::ReplicationHandle::default()),
    ));

    use futures_util::{SinkExt, StreamExt};
    let mut framed = tokio_util::codec::Framed::new(
        tokio::net::TcpStream::connect(addr).await.unwrap(),
        protocol::codec::RespCodec::default(),
    );

    // Negotiate RESP3 -- this mutates the connection's Session, not just this one reply.
    framed
        .send(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"HELLO")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"3")),
        ]))
        .await
        .unwrap();
    framed.next().await.unwrap().unwrap(); // the HELLO reply itself, not under test here

    // A second, independent command on the SAME connection. If Session's protocol field didn't
    // persist (e.g. a regression back to a fresh Protocol::default() per request), this GET's
    // Null reply would encode as RESP2's `$-1\r\n` instead of RESP3's `_\r\n` -- the client-side
    // codec here is also stateful (`RespCodec::protocol` synced from the server's own replies),
    // so decoding this successfully via `Frame::Null` at all is only possible if the server's
    // Session actually remembered RESP3 across the two requests.
    framed
        .send(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"GET")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"missing-key")),
        ]))
        .await
        .unwrap();
    assert_eq!(framed.next().await.unwrap().unwrap(), protocol::Frame::Null);
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p rocket-mem --test integration session_state_persists -- --nocapture`
Expected: PASS. (This is confirmation, not a red-bar step in the traditional sense — Task 2's refactor should already make this true; if it fails, the bug is in Task 2's `connection.rs` wiring, not new code here.)

- [ ] **Step 3: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/tests/integration.rs`.
