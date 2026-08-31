# ACL Admin Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ACL SETUSER`/`DELUSER`/`WHOAMI`/`LIST`/`GETUSER`, reachable over both RESP and RMP for free via `dispatch_and_log`, matching the exact interception pattern `CLUSTER`/`SLOWLOG` already use.

**Architecture:** one new `handle_acl` interception in `dispatch_and_log_inner`, dispatching to per-subcommand helper functions — no changes to `dispatch`, `auth_gate`, or `handle_auth` (plan 06 already exempts `ACL` from the gate by name).

**Tech Stack:** nothing new — builds on plan 03/04's `AclStore`/`AclUser`/`AclRule`, plan 05's `Session`.

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: ACL data model..." section's command-surface table.

## Global Constraints

- `AUTH` was added to `dispatch_and_log_inner`'s interception chain in plan 06 but not yet to `KNOWN_COMMANDS`/`key_spec` (both list entries that are keyless interceptions, like `CLUSTER`/`SLOWLOG` already are). This plan adds **both** `AUTH` and `ACL` to those two lists in one coherent edit (Task 1), rather than splitting the addition across two plans.
- `ACL SETUSER`'s reply on success is `+OK`; on a malformed token it is the same `AclError::SyntaxError` message `acl.rs` already produces (Task 1 of plan 03), surfaced verbatim — no new error-text invention here.

---

### Task 1: `handle_acl` dispatcher + `SETUSER`/`DELUSER`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `AclStore::set_user`/`del_user` (plan 04), `AclError` (plan 03).
- Produces: `fn handle_acl(frame: &Frame, session: &Session, replication: &ReplicationHandle) -> Option<Frame>`, intercepted in `dispatch_and_log_inner`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
fn acl_cmd(args: &[&[u8]]) -> Frame {
    let mut items = vec![Frame::Bulk(Bytes::from_static(b"ACL"))];
    items.extend(args.iter().map(|a| Frame::Bulk(Bytes::copy_from_slice(a))));
    Frame::Array(items)
}

#[test]
fn acl_setuser_creates_a_user_and_returns_ok() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    let reply = handle_acl(&acl_cmd(&[b"SETUSER", b"app", b"on", b">pw", b"allcommands", b"allkeys"]), &session, &replication).unwrap();
    assert_eq!(reply, Frame::Simple("OK".into()));
    assert!(replication.acl.get_user("app").unwrap().enabled);
}

#[test]
fn acl_setuser_with_a_malformed_token_returns_the_syntax_error() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    let reply = handle_acl(&acl_cmd(&[b"SETUSER", b"app", b"garbage"]), &session, &replication).unwrap();
    assert!(matches!(reply, Frame::Error(_)));
}

#[test]
fn acl_deluser_removes_users_and_counts_them() {
    let replication = ReplicationHandle::default();
    replication.acl.set_user("a", &[Bytes::from_static(b"on")]).unwrap();
    replication.acl.set_user("b", &[Bytes::from_static(b"on")]).unwrap();
    let session = Session::new();
    let reply = handle_acl(&acl_cmd(&[b"DELUSER", b"a", b"b", b"nonexistent"]), &session, &replication).unwrap();
    assert_eq!(reply, Frame::Integer(2));
}

#[test]
fn an_unknown_acl_subcommand_is_an_error() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    let reply = handle_acl(&acl_cmd(&[b"BOGUS"]), &session, &replication).unwrap();
    assert!(matches!(reply, Frame::Error(_)));
}

#[test]
fn a_non_acl_frame_is_not_intercepted() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"GET")), Frame::Bulk(Bytes::from_static(b"k"))]);
    assert!(handle_acl(&frame, &session, &replication).is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::acl_ -- --nocapture`
Expected: FAIL to compile — `handle_acl` doesn't exist yet.

- [ ] **Step 3: Implement `handle_acl`, `SETUSER`, `DELUSER`**

```rust
// crates/server/src/dispatcher.rs
/// Returns `Some(reply)` if `frame` was `ACL` -- handled entirely here, never reaching
/// `dispatch` -- or `None` for any other command. Same interception shape as `handle_cluster`/
/// `handle_slowlog` above.
fn handle_acl(
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
    if !name.eq_ignore_ascii_case(b"ACL") {
        return None;
    }
    let Some(Frame::Bulk(sub_bytes)) = items.get(1) else {
        return Some(Frame::Error(
            "ERR wrong number of arguments for 'acl' command".into(),
        ));
    };
    let sub = String::from_utf8_lossy(sub_bytes).to_ascii_uppercase();
    Some(match sub.as_str() {
        "SETUSER" => acl_setuser(items, replication),
        "DELUSER" => acl_deluser(items, replication),
        _ => Frame::Error(format!("ERR unknown ACL subcommand '{sub}'")),
    })
}

fn acl_setuser(items: &[Frame], replication: &crate::replication::ReplicationHandle) -> Frame {
    let Some(Frame::Bulk(username)) = items.get(2) else {
        return Frame::Error("ERR wrong number of arguments for 'acl|setuser' command".into());
    };
    let raw_tokens: Vec<Bytes> = items[3..]
        .iter()
        .filter_map(|f| match f {
            Frame::Bulk(b) => Some(b.clone()),
            _ => None,
        })
        .collect();
    if raw_tokens.len() != items[3..].len() {
        return Frame::Error("ERR syntax error".into());
    }
    let username = String::from_utf8_lossy(username).into_owned();
    match replication.acl.set_user(&username, &raw_tokens) {
        Ok(()) => Frame::Simple("OK".into()),
        Err(e) => Frame::Error(e.to_string()),
    }
}

fn acl_deluser(items: &[Frame], replication: &crate::replication::ReplicationHandle) -> Frame {
    if items.len() < 3 {
        return Frame::Error("ERR wrong number of arguments for 'acl|deluser' command".into());
    }
    let deleted = items[2..]
        .iter()
        .filter_map(|f| match f {
            Frame::Bulk(b) => Some(b),
            _ => None,
        })
        .filter(|b| replication.acl.del_user(&String::from_utf8_lossy(b)))
        .count();
    Frame::Integer(deleted as i64)
}
```

In `dispatch_and_log_inner`, add the call among the existing interception chain (alongside `handle_auth`/`handle_cluster`):

```rust
if let Some(reply) = handle_acl(&frame, session, replication) {
    return reply;
}
```

Add `"ACL"` and `"AUTH"` to `KNOWN_COMMANDS` (the sorted list — `"ACL"` sorts first alphabetically, before `"APPEND"`; `"AUTH"` sorts right after `"APPEND"`, before `"CLUSTER"`) and to `key_spec`'s `KeySpec::None` group (alongside `"CLUSTER"`/`"SLOWLOG"`):

```rust
pub(crate) const KNOWN_COMMANDS: &[&str] = &[
    "ACL",
    "APPEND",
    "AUTH",
    "CLUSTER",
    // ... rest unchanged
```

```rust
"PING" | "ECHO" | "SELECT" | "COMMAND" | "INFO" | "HELLO" | "KEYS" | "SCAN"
| "RANDOMKEY" | "CLUSTER" | "SAVE" | "REPLICAOF" | "PSYNC" | "SLOWLOG" | "ACL" | "AUTH" => {
    KeySpec::None
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::acl_ dispatcher::tests -- --nocapture 2>&1 | tail -30`
Expected: all PASS, including the pre-existing `known_commands_is_sorted_so_binary_search_works`-style guard test (confirms the two new insertions kept the list sorted).

- [ ] **Step 5: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/src/dispatcher.rs`.

---

### Task 2: `ACL WHOAMI` + `ACL LIST`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `Session::authenticated_user` (plan 05), `AclStore::list` (plan 04).
- Produces: `fn rule_token(rule: &AclRule) -> String` (the single source of truth for rendering one rule, reused by Task 3's `GETUSER`), `fn render_user_line(user: &AclUser) -> String`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
#[test]
fn acl_whoami_reports_default_when_unauthenticated() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    let reply = handle_acl(&acl_cmd(&[b"WHOAMI"]), &session, &replication).unwrap();
    assert_eq!(reply, Frame::Bulk(Bytes::from_static(b"default")));
}

#[test]
fn acl_whoami_reports_the_authenticated_username() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    session.set_authenticated_user(Some(std::sync::Arc::new(crate::acl::AclUser {
        username: "app".to_string(),
        password_hash: None,
        enabled: true,
        rules: vec![],
    })));
    let reply = handle_acl(&acl_cmd(&[b"WHOAMI"]), &session, &replication).unwrap();
    assert_eq!(reply, Frame::Bulk(Bytes::from_static(b"app")));
}

#[test]
fn acl_list_renders_one_line_per_user_real_redis_shaped() {
    let replication = ReplicationHandle::default();
    replication
        .acl
        .set_user("app", &[Bytes::from_static(b"on"), Bytes::from_static(b">pw"), Bytes::from_static(b"~app:*"), Bytes::from_static(b"+get")])
        .unwrap();
    let session = Session::new();
    let reply = handle_acl(&acl_cmd(&[b"LIST"]), &session, &replication).unwrap();
    let Frame::Array(lines) = reply else { panic!("expected an array") };
    assert_eq!(lines.len(), 1);
    let Frame::Bulk(line) = &lines[0] else { panic!("expected a bulk string") };
    let line = String::from_utf8_lossy(line);
    assert!(line.starts_with("user app on "), "got: {line}");
    assert!(line.contains("~app:*"));
    assert!(line.contains("+get"));
}

#[test]
fn rule_token_renders_every_variant() {
    assert_eq!(rule_token(&crate::acl::AclRule::AllCommands), "+@all");
    assert_eq!(rule_token(&crate::acl::AclRule::NoCommands), "-@all");
    assert_eq!(rule_token(&crate::acl::AclRule::AllowCommand("GET".to_string())), "+get");
    assert_eq!(rule_token(&crate::acl::AclRule::DenyCommand("SET".to_string())), "-set");
    assert_eq!(rule_token(&crate::acl::AclRule::AllKeys), "~*");
    assert_eq!(rule_token(&crate::acl::AclRule::KeyPattern("app:*".to_string())), "~app:*");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::acl_whoami dispatcher::tests::acl_list dispatcher::tests::rule_token -- --nocapture`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

```rust
// crates/server/src/dispatcher.rs
/// Renders one `AclRule` in `ACL SETUSER`'s own token vocabulary -- the single source of truth
/// both `render_user_line` and `GETUSER` (Task 3) build their output from.
fn rule_token(rule: &crate::acl::AclRule) -> String {
    use crate::acl::AclRule;
    match rule {
        AclRule::AllCommands => "+@all".to_string(),
        AclRule::NoCommands => "-@all".to_string(),
        AclRule::AllowCommand(c) => format!("+{}", c.to_lowercase()),
        AclRule::DenyCommand(c) => format!("-{}", c.to_lowercase()),
        AclRule::AllKeys => "~*".to_string(),
        AclRule::KeyPattern(p) => format!("~{p}"),
    }
}

fn render_user_line(user: &crate::acl::AclUser) -> String {
    let mut parts = vec!["user".to_string(), user.username.clone()];
    parts.push(if user.enabled { "on".to_string() } else { "off".to_string() });
    parts.push(match &user.password_hash {
        None => "nopass".to_string(),
        Some(h) => format!("#{h}"),
    });
    parts.extend(user.rules.iter().map(rule_token));
    parts.join(" ")
}

fn acl_whoami(session: &Session) -> Frame {
    match session.authenticated_user() {
        Some(u) => Frame::Bulk(Bytes::from(u.username.clone())),
        None => Frame::Bulk(Bytes::from_static(b"default")),
    }
}

fn acl_list(replication: &crate::replication::ReplicationHandle) -> Frame {
    Frame::Array(
        replication
            .acl
            .list()
            .into_iter()
            .map(|u| Frame::Bulk(Bytes::from(render_user_line(&u))))
            .collect(),
    )
}
```

Add the two new subcommand arms to `handle_acl`'s `match sub.as_str()`:

```rust
        "WHOAMI" => acl_whoami(session),
        "LIST" => acl_list(replication),
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::acl_ dispatcher::tests::rule_token -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/src/dispatcher.rs`.

---

### Task 3: `ACL GETUSER`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `rule_token` (Task 2), `AclStore::get_user` (plan 04).
- Produces: nothing new — the last `ACL` subcommand this sprint implements.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
#[test]
fn acl_getuser_returns_null_for_an_unknown_user() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    let reply = handle_acl(&acl_cmd(&[b"GETUSER", b"nobody"]), &session, &replication).unwrap();
    assert_eq!(reply, Frame::Null);
}

#[test]
fn acl_getuser_returns_a_structured_map_for_a_known_user() {
    let replication = ReplicationHandle::default();
    replication
        .acl
        .set_user("app", &[Bytes::from_static(b"on"), Bytes::from_static(b">pw"), Bytes::from_static(b"~app:*"), Bytes::from_static(b"+get")])
        .unwrap();
    let session = Session::new();
    let reply = handle_acl(&acl_cmd(&[b"GETUSER", b"app"]), &session, &replication).unwrap();
    let Frame::Map(fields) = reply else { panic!("expected a map") };
    let field = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| matches!(k, Frame::Bulk(b) if b.as_ref() == key.as_bytes()))
            .map(|(_, v)| v.clone())
    };
    assert_eq!(field("flags"), Some(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"on"))])));
    assert!(matches!(field("passwords"), Some(Frame::Array(v)) if v.len() == 1));
    assert_eq!(field("commands"), Some(Frame::Bulk(Bytes::from_static(b"+get"))));
    assert_eq!(field("keys"), Some(Frame::Bulk(Bytes::from_static(b"~app:*"))));
}

#[test]
fn acl_getuser_with_wrong_argument_count_is_an_error() {
    let replication = ReplicationHandle::default();
    let session = Session::new();
    let reply = handle_acl(&acl_cmd(&[b"GETUSER"]), &session, &replication).unwrap();
    assert!(matches!(reply, Frame::Error(_)));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::acl_getuser -- --nocapture`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `acl_getuser`**

```rust
// crates/server/src/dispatcher.rs
fn acl_getuser(items: &[Frame], replication: &crate::replication::ReplicationHandle) -> Frame {
    let Some(Frame::Bulk(username)) = items.get(2) else {
        return Frame::Error("ERR wrong number of arguments for 'acl|getuser' command".into());
    };
    let username = String::from_utf8_lossy(username);
    let Some(user) = replication.acl.get_user(&username) else {
        return Frame::Null;
    };
    use crate::acl::AclRule;
    let commands: Vec<String> = user
        .rules
        .iter()
        .filter(|r| matches!(r, AclRule::AllCommands | AclRule::NoCommands | AclRule::AllowCommand(_) | AclRule::DenyCommand(_)))
        .map(rule_token)
        .collect();
    let keys: Vec<String> = user
        .rules
        .iter()
        .filter(|r| matches!(r, AclRule::AllKeys | AclRule::KeyPattern(_)))
        .map(rule_token)
        .collect();
    Frame::Map(vec![
        (
            Frame::Bulk(Bytes::from_static(b"flags")),
            Frame::Array(vec![Frame::Bulk(Bytes::from(if user.enabled { "on" } else { "off" }))]),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"passwords")),
            Frame::Array(match &user.password_hash {
                Some(h) => vec![Frame::Bulk(Bytes::from(h.clone()))],
                None => vec![],
            }),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"commands")),
            Frame::Bulk(Bytes::from(commands.join(" "))),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"keys")),
            Frame::Bulk(Bytes::from(keys.join(" "))),
        ),
    ])
}
```

Add the arm to `handle_acl`'s `match sub.as_str()`: `"GETUSER" => acl_getuser(items, replication),`

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::acl_ -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/src/dispatcher.rs`.
