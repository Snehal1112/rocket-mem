# Auth Gate & AUTH Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** enforce ACL permissions on every command through `dispatch_and_log_inner`, and give clients a way to authenticate via `AUTH`.

**Architecture:** a new `auth_gate` check at the very top of `dispatch_and_log_inner` (ahead of `cluster_redirect`, matching real Redis's own auth-before-everything ordering) — `None` (proceed) when `replication.acl.is_empty()`, or when the command is `AUTH`/`ACL` (always reachable regardless of auth state), or when the session's authenticated user is permitted; `Some(error)` otherwise. `handle_auth` is a new interception, following the exact `CLUSTER`/`SLOWLOG`/`INFO` precedent already in this file, that authenticates and calls `session.set_authenticated_user`.

**Tech Stack:** nothing new — builds on plan 03 (`AclUser::is_allowed`), plan 04 (`AclStore::authenticate`), plan 05 (`Session`).

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: ACL data model..." section's "Command surface" and "Auth error shapes" tables, and "Decision: `Session`..." section's "Auth gate placement" paragraph.

## Global Constraints

- **Zero behavior change with `replication.acl.is_empty()`.** This is the single most load-bearing test in this plan — every one of the ~600 pre-existing tests must keep passing with no ACL configured, which they will not touch at all differently after this plan lands.
- Error text matches real Redis's own shapes exactly (`NOAUTH Authentication required.`, `WRONGPASS invalid username-password pair or user is disabled.`, the two `NOPERM` variants, and `ERR Client sent AUTH, but no password is set.` for `AUTH` against an unconfigured ACL) — client libraries often pattern-match on these prefixes.
- `AUTH` and `ACL` command names are exempt from the gate itself (checked by name, before any auth-state check) — `ACL`'s own subcommands don't exist as real commands until plan 08, but the gate must not block them with `NOAUTH` in the meantime; an unauthenticated `ACL WHOAMI` sent before plan 08 lands simply reaches `dispatch`'s ordinary "unknown command" error, which is correct and requires no special-casing here.

---

### Task 1: `auth_gate` — the NOAUTH/NOPERM check

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `Session::authenticated_user` (plan 05), `AclStore::is_empty` (plan 04), `AclUser::is_allowed` (plan 03), the existing `pub(crate) fn command_name_upper(frame: &Frame) -> Option<CommandName>` and `fn command_keys(frame: &Frame) -> Vec<&Bytes>`.
- Produces: `fn auth_gate(replication: &ReplicationHandle, session: &Session, frame: &Frame) -> Option<Frame>`, called first thing inside `dispatch_and_log_inner`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
fn acl_user(rules: Vec<crate::acl::AclRule>) -> std::sync::Arc<crate::acl::AclUser> {
    std::sync::Arc::new(crate::acl::AclUser {
        username: "app".to_string(),
        password_hash: None,
        enabled: true,
        rules,
    })
}

#[test]
fn auth_gate_with_no_acl_users_configured_lets_everything_through() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"GET")), Frame::Bulk(Bytes::from_static(b"k"))]);
    assert!(auth_gate(&replication, &session, &frame).is_none());
}

#[test]
fn auth_gate_denies_an_unauthenticated_connection_once_acl_users_exist() {
    let replication = ReplicationHandle::default();
    replication.acl.set_user("app", &[Bytes::from_static(b"on")]).unwrap();
    let session = Session::new();
    let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"GET")), Frame::Bulk(Bytes::from_static(b"k"))]);
    let reply = auth_gate(&replication, &session, &frame).unwrap();
    assert_eq!(reply, Frame::Error("NOAUTH Authentication required.".into()));
}

#[test]
fn auth_gate_lets_auth_and_acl_commands_through_even_when_unauthenticated() {
    let replication = ReplicationHandle::default();
    replication.acl.set_user("app", &[Bytes::from_static(b"on")]).unwrap();
    let session = Session::new();
    let auth_frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"AUTH")), Frame::Bulk(Bytes::from_static(b"pw"))]);
    let acl_frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"ACL")), Frame::Bulk(Bytes::from_static(b"WHOAMI"))]);
    assert!(auth_gate(&replication, &session, &auth_frame).is_none());
    assert!(auth_gate(&replication, &session, &acl_frame).is_none());
}

#[test]
fn auth_gate_denies_a_command_the_authenticated_user_lacks_a_grant_for() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    session.set_authenticated_user(Some(acl_user(vec![
        crate::acl::AclRule::AllowCommand("GET".to_string()),
        crate::acl::AclRule::AllKeys,
    ])));
    let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"SET")), Frame::Bulk(Bytes::from_static(b"k")), Frame::Bulk(Bytes::from_static(b"v"))]);
    let reply = auth_gate(&replication, &session, &frame).unwrap();
    assert_eq!(reply, Frame::Error("NOPERM this user has no permissions to run this command".into()));
}

#[test]
fn auth_gate_denies_a_key_outside_the_authenticated_users_pattern() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    session.set_authenticated_user(Some(acl_user(vec![
        crate::acl::AclRule::AllCommands,
        crate::acl::AclRule::KeyPattern("app:*".to_string()),
    ])));
    let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"GET")), Frame::Bulk(Bytes::from_static(b"other:1"))]);
    let reply = auth_gate(&replication, &session, &frame).unwrap();
    assert_eq!(reply, Frame::Error("NOPERM no permissions to access a key".into()));
}

#[test]
fn auth_gate_permits_a_command_the_authenticated_user_is_granted() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    session.set_authenticated_user(Some(acl_user(vec![
        crate::acl::AclRule::AllCommands,
        crate::acl::AclRule::AllKeys,
    ])));
    let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"GET")), Frame::Bulk(Bytes::from_static(b"k"))]);
    assert!(auth_gate(&replication, &session, &frame).is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::auth_gate -- --nocapture`
Expected: FAIL to compile — `auth_gate` doesn't exist yet.

- [ ] **Step 3: Implement `auth_gate` and wire it in**

```rust
// crates/server/src/dispatcher.rs
/// `None` = proceed to `dispatch`. `Some(frame)` = reply with this instead, without touching the
/// engine, the AOF, the replica fan-out, or any lock -- same shape as `cluster_redirect` above.
/// Checked first, ahead of `cluster_redirect`, matching real Redis's own auth-before-everything
/// ordering: an unauthenticated client should not learn cluster topology or reach any other gate.
fn auth_gate(
    replication: &crate::replication::ReplicationHandle,
    session: &Session,
    frame: &Frame,
) -> Option<Frame> {
    if replication.acl.is_empty() {
        return None; // the fast path -- every existing deployment and test, unchanged
    }
    let name = command_name_upper(frame)?;
    let name = name.as_str();
    if name == "AUTH" || name == "ACL" {
        return None; // always reachable regardless of auth state
    }
    let Some(user) = session.authenticated_user() else {
        return Some(Frame::Error("NOAUTH Authentication required.".into()));
    };
    let keys = command_keys(frame);
    if !user.is_allowed(name, &keys) {
        let msg = if user.is_allowed(name, &[]) {
            "NOPERM no permissions to access a key"
        } else {
            "NOPERM this user has no permissions to run this command"
        };
        return Some(Frame::Error(msg.into()));
    }
    None
}
```

In `dispatch_and_log_inner`, add the call as the very first line of the function body, before the existing `cluster_redirect` check:

```rust
if let Some(reply) = auth_gate(replication, session, &frame) {
    return reply;
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::auth_gate -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green — confirms the empty-ACL fast path leaves every pre-existing test's behavior untouched.

Use the `1-git-commit` skill/command to commit `crates/server/src/dispatcher.rs`.

---

### Task 2: `AUTH` command

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `AclStore::authenticate`, `AclStore::is_empty` (plan 04), `Session::set_authenticated_user` (plan 05).
- Produces: `fn handle_auth(frame: &Frame, session: &Session, replication: &ReplicationHandle) -> Option<Frame>`, intercepted in `dispatch_and_log_inner` alongside `handle_replicaof`/`handle_cluster`/`handle_info`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
fn auth_frame(args: &[&[u8]]) -> Frame {
    let mut items = vec![Frame::Bulk(Bytes::from_static(b"AUTH"))];
    items.extend(args.iter().map(|a| Frame::Bulk(Bytes::copy_from_slice(a))));
    Frame::Array(items)
}

#[test]
fn auth_with_no_acl_configured_returns_the_no_password_set_error() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    let reply = handle_auth(&auth_frame(&[b"anything"]), &session, &replication).unwrap();
    assert_eq!(
        reply,
        Frame::Error("ERR Client sent AUTH, but no password is set.".into())
    );
}

#[test]
fn auth_single_arg_form_checks_against_the_default_user() {
    let replication = ReplicationHandle::default();
    replication.acl.set_user("default", &[Bytes::from_static(b"on"), Bytes::from_static(b">pw")]).unwrap();
    let session = Session::new();
    let reply = handle_auth(&auth_frame(&[b"pw"]), &session, &replication).unwrap();
    assert_eq!(reply, Frame::Simple("OK".into()));
    assert!(session.authenticated_user().is_some());
}

#[test]
fn auth_two_arg_form_checks_against_the_named_user() {
    let replication = ReplicationHandle::default();
    replication.acl.set_user("app", &[Bytes::from_static(b"on"), Bytes::from_static(b">pw")]).unwrap();
    let session = Session::new();
    let reply = handle_auth(&auth_frame(&[b"app", b"pw"]), &session, &replication).unwrap();
    assert_eq!(reply, Frame::Simple("OK".into()));
    assert_eq!(session.authenticated_user().unwrap().username, "app");
}

#[test]
fn auth_with_the_wrong_password_returns_wrongpass_and_does_not_authenticate() {
    let replication = ReplicationHandle::default();
    replication.acl.set_user("app", &[Bytes::from_static(b"on"), Bytes::from_static(b">pw")]).unwrap();
    let session = Session::new();
    let reply = handle_auth(&auth_frame(&[b"app", b"wrong"]), &session, &replication).unwrap();
    assert_eq!(
        reply,
        Frame::Error("WRONGPASS invalid username-password pair or user is disabled.".into())
    );
    assert!(session.authenticated_user().is_none());
}

#[test]
fn auth_with_too_many_arguments_is_a_syntax_error() {
    let replication = ReplicationHandle::default();
    replication.acl.set_user("app", &[Bytes::from_static(b"on")]).unwrap();
    let session = Session::new();
    let reply = handle_auth(&auth_frame(&[b"a", b"b", b"c"]), &session, &replication).unwrap();
    assert!(matches!(reply, Frame::Error(_)));
}

#[test]
fn a_non_auth_frame_is_not_intercepted() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"GET")), Frame::Bulk(Bytes::from_static(b"k"))]);
    assert!(handle_auth(&frame, &session, &replication).is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::auth_ -- --nocapture`
Expected: FAIL to compile — `handle_auth` doesn't exist yet.

- [ ] **Step 3: Implement `handle_auth` and wire it in**

```rust
// crates/server/src/dispatcher.rs
/// Returns `Some(reply)` if `frame` was `AUTH` -- handled entirely here, never reaching
/// `dispatch` -- or `None` for any other command. Same interception shape as `handle_replicaof`/
/// `handle_cluster` above.
fn handle_auth(
    frame: &Frame,
    session: &Session,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"AUTH") {
        return None;
    }
    let (username, password): (String, &Bytes) = match items.len() {
        2 => {
            let Frame::Bulk(pw) = &items[1] else {
                return Some(Frame::Error("ERR syntax error".into()));
            };
            ("default".to_string(), pw)
        }
        3 => {
            let (Frame::Bulk(user), Frame::Bulk(pw)) = (&items[1], &items[2]) else {
                return Some(Frame::Error("ERR syntax error".into()));
            };
            (String::from_utf8_lossy(user).into_owned(), pw)
        }
        _ => {
            return Some(Frame::Error(
                "ERR wrong number of arguments for 'auth' command".into(),
            ))
        }
    };
    if replication.acl.is_empty() {
        return Some(Frame::Error(
            "ERR Client sent AUTH, but no password is set.".into(),
        ));
    }
    let password = String::from_utf8_lossy(password);
    match replication.acl.authenticate(&username, &password) {
        Some(user) => {
            session.set_authenticated_user(Some(user));
            Some(Frame::Simple("OK".into()))
        }
        None => Some(Frame::Error(
            "WRONGPASS invalid username-password pair or user is disabled.".into(),
        )),
    }
}
```

In `dispatch_and_log_inner`, add the call among the existing interception chain (after the `-READONLY` check, alongside `handle_replicaof`/`handle_cluster`):

```rust
if let Some(reply) = handle_auth(&frame, session, replication) {
    return reply;
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::auth_ -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/src/dispatcher.rs`.

---

### Task 3: Real-socket RESP proof — denied before `AUTH`, allowed after, on one connection

**Files:**
- Modify: `crates/server/tests/integration.rs`

**Interfaces:**
- Consumes: nothing new — end-to-end proof over a real `TcpStream`, exercising `serve` (which threads through `connection.rs`'s `Session` wiring from plan 05).
- Produces: nothing new.

- [ ] **Step 1: Write the test**

```rust
// crates/server/tests/integration.rs
#[tokio::test]
async fn a_resp_connection_is_denied_before_auth_and_permitted_after_on_the_same_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = std::sync::Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().unwrap();
    let aof = std::sync::Arc::new(
        rocket_mem::aof::AofWriter::open(&dir.path().join("test.aof"), rocket_mem::aof::FsyncPolicy::Never)
            .unwrap(),
    );
    let replication = std::sync::Arc::new(rocket_mem::replication::ReplicationHandle::default());
    replication
        .acl
        .set_user(
            "app",
            &[
                bytes::Bytes::from_static(b"on"),
                bytes::Bytes::from_static(b">pw"),
                bytes::Bytes::from_static(b"allcommands"),
                bytes::Bytes::from_static(b"allkeys"),
            ],
        )
        .unwrap();
    tokio::spawn(rocket_mem::serve(listener, engine, aof, std::sync::Arc::clone(&replication)));

    use futures_util::{SinkExt, StreamExt};
    let mut framed = tokio_util::codec::Framed::new(
        tokio::net::TcpStream::connect(addr).await.unwrap(),
        protocol::codec::RespCodec::default(),
    );

    framed
        .send(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"GET")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"k")),
        ]))
        .await
        .unwrap();
    assert_eq!(
        framed.next().await.unwrap().unwrap(),
        protocol::Frame::Error("NOAUTH Authentication required.".into())
    );

    framed
        .send(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"AUTH")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"app")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"pw")),
        ]))
        .await
        .unwrap();
    assert_eq!(
        framed.next().await.unwrap().unwrap(),
        protocol::Frame::Simple("OK".into())
    );

    // Same connection, no reconnect -- proves auth state persisted via Session (plan 05).
    framed
        .send(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"GET")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"k")),
        ]))
        .await
        .unwrap();
    assert_eq!(framed.next().await.unwrap().unwrap(), protocol::Frame::Null);
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p rocket-mem --test integration a_resp_connection_is_denied_before_auth -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/tests/integration.rs`.
