# Verbose Logging Plan 11: ACL and Auth Events

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Log the ACL/auth lifecycle — `AUTH` success and failure, permission-denied, `HELLO`'s RESP3 protocol upgrade, and `ACL SETUSER`/`DELUSER` — at the levels the spec's catalogue calls for, without ever letting a password reach a log line.

**Architecture:** Every site this plan touches lives in `crates/server/src/dispatcher.rs`, all reachable from `dispatch_and_log_inner` before a command's own reply is built:

- `try_authenticate` (line 1139) is the single choke point both `handle_auth` (`AUTH`) and `apply_hello_extra_args` (`HELLO ... AUTH ...`) call to check a username/password pair — instrumenting it once covers both surfaces.
- `auth_gate` (line 2792) is the single choke point every command's permission check passes through, run inside `dispatch_and_log_inner` before the command itself dispatches.
- `handle_hello` (line 2003) is where `HELLO 2`/`HELLO 3` actually flips `session`'s negotiated protocol.
- `handle_acl`'s `acl_setuser`/`acl_deluser` (lines 2605, 2626) are where `ACL SETUSER`/`DELUSER` actually mutate the ACL store.

Per this plan's brief, the `cmd` span (opened in `dispatch_and_log`, wrapping the entire `dispatch_and_log_inner` call these four functions run inside) already carries `cmd`/`key`/`argc`, and the `conn` span (opened around `handle_connection`, wrapping the whole connection this dispatch call happens on) already carries `conn_id`/`peer`/`protocol`/`tls`. Every event below is emitted from code that runs nested inside both spans, so a subscriber attaches all of those fields automatically — this is why the spec catalogue's "permission denied with `user`/`cmd`/`key`" only needs a plain `user` field added at the call site: `cmd` and `key` are already there from the `cmd` span, and re-adding them would duplicate what the span already carries. The same reasoning covers "auth failure with `user` + `peer`" — `peer` comes from the enclosing `conn` span.

None of these four sites sit on the hot path a benchmark exercises: `auth_gate` returns immediately on its first line (`if !replication.acl.has_ever_been_configured() { return None; }`) for every deployment and test that hasn't configured any ACL user — which is exactly `scripts/benchmark.sh`'s setup — so none of this plan's code runs at all during a benchmark's `SET`/`GET` workload. The 2% throughput gate is not at risk from this plan.

---

## CRITICAL SECURITY POINT

**Every function this plan touches handles a password or a token that carries one.** `try_authenticate` receives a plaintext password directly. `acl_setuser` receives `ACL SETUSER`'s raw token list, which can contain a `>password` token (see `crates/server/src/acl.rs`'s `AclToken::Password`). **No log line added by this plan may include the password argument, the raw token list, or anything derived from either.** Log only the **username** (a `&str`/`String` the caller already has, never the credential itself).

This is exactly the boundary `crates/server/src/logging.rs`'s `is_sensitive(cmd, args)` (plan 03) draws for `AUTH`, `HELLO ... AUTH`, and `ACL SETUSER`/`GETUSER`: those commands' *raw argument lists* must never reach `redact_args`'s trace-level rendering or any other log call. The functions this plan instruments are a second, independent place the same secrets pass through — `is_sensitive`/`redact_args` guard the generic per-command `trace!` site plan 08 adds in `dispatch_and_log`; they do **not** automatically protect a bespoke `debug!`/`info!`/`warn!` call added inside `try_authenticate` or `acl_setuser` themselves. Each task below states explicitly which local variable is safe to log (`username`) and which must never appear in a log macro's argument list (`password`, `raw_tokens`).

**Tech Stack:** Rust 2021, `tracing 0.1`, `tracing-subscriber 0.3` (both already dependencies of `crates/server`).

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the "ACL" row of the Event catalogue ("auth success with `user` (info); auth failure with `user` + `peer`, never the secret (warn); permission denied with `user`/`cmd`/`key` (warn); `SETUSER`/`DELUSER` (info)") and the "Connection" row's "`HELLO`/RESP3 protocol upgrade (debug)".

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting.

**Additional constraints specific to this plan:**
- The CRITICAL SECURITY POINT above applies to every task without exception. If a step in this plan is ever unclear about whether a value is safe to log, the answer is no — log the username only.
- Every `tracing::` macro call uses a fully-qualified path (`tracing::info!`/`tracing::warn!`/`tracing::debug!`), matching plan 10's convention, since `dispatcher.rs` does not otherwise import these macros.
- Tests use `capture_logs_at`, the helper plan 10's Task 1 added to `crates/server/src/dispatcher.rs`'s inline `mod tests` — it is not redefined here.
- A failing test in this plan that would only pass by relaxing a "must never contain the password" assertion is a security defect, not a style nit — matching plan 03's own additional constraint. Stop and report rather than weaken such an assertion.

---

### Task 1: `AUTH` success and failure — `info`/`warn` in `try_authenticate`

**Files:**
- Modify: `crates/server/src/dispatcher.rs:1139-1155` (`try_authenticate`)

**Interfaces:**
- Consumes: `username: &str` (already a parameter — safe to log) and `password: &[u8]` (already a parameter — **never** to be logged).
- Produces: nothing new consumed elsewhere. `try_authenticate`'s signature and `Result` shape are unchanged, so `handle_auth` and `apply_hello_extra_args` need no edits themselves — both get the new events for free.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests`:

```rust
    #[test]
    fn try_authenticate_logs_success_at_info_naming_the_user() {
        let replication = ReplicationHandle::default();
        replication
            .acl
            .set_user("app", &[Bytes::from_static(b"on"), Bytes::from_static(b">hunter2")])
            .unwrap();
        let log = capture_logs_at(tracing::Level::INFO, || {
            let result = try_authenticate(&replication, "app", b"hunter2");
            assert!(result.is_ok());
        });
        assert!(log.contains("app"), "expected the username in the event, got: {log}");
        assert!(!log.contains("hunter2"), "the password must never be logged, got: {log}");
    }

    #[test]
    fn try_authenticate_logs_failure_at_warn_naming_the_user_never_the_password() {
        let replication = ReplicationHandle::default();
        replication
            .acl
            .set_user("app", &[Bytes::from_static(b"on"), Bytes::from_static(b">hunter2")])
            .unwrap();
        let log = capture_logs_at(tracing::Level::WARN, || {
            let result = try_authenticate(&replication, "app", b"wrong-password");
            assert!(result.is_err());
        });
        assert!(log.contains("app"), "expected the username in the event, got: {log}");
        assert!(
            !log.contains("wrong-password"),
            "the password must never be logged, got: {log}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem dispatcher::tests::try_authenticate_logs
```

Expected: both compile, both fail at their first assertion:

```
assertion failed: log.contains("app")
expected the username in the event, got:
```

(the captured `log` is empty in both cases — neither branch logs anything yet).

- [ ] **Step 3: Implement**

At `crates/server/src/dispatcher.rs:1139-1155`, change:

```rust
fn try_authenticate(
    replication: &crate::replication::ReplicationHandle,
    username: &str,
    password: &[u8],
) -> Result<std::sync::Arc<crate::acl::AclUser>, Frame> {
    if !replication.acl.has_ever_been_configured() {
        return Err(Frame::Error(
            "ERR Client sent AUTH, but no password is set.".into(),
        ));
    }
    match replication.acl.authenticate(username, password) {
        Some(user) => Ok(user),
        None => Err(Frame::Error(
            "WRONGPASS invalid username-password pair or user is disabled.".into(),
        )),
    }
}
```

to:

```rust
fn try_authenticate(
    replication: &crate::replication::ReplicationHandle,
    username: &str,
    password: &[u8],
) -> Result<std::sync::Arc<crate::acl::AclUser>, Frame> {
    if !replication.acl.has_ever_been_configured() {
        return Err(Frame::Error(
            "ERR Client sent AUTH, but no password is set.".into(),
        ));
    }
    match replication.acl.authenticate(username, password) {
        Some(user) => {
            // `peer` is already attached by the enclosing `conn` span (plan 05/06) -- see this
            // plan's Architecture section. Never log `password` here.
            tracing::info!(user = %username, "auth success");
            Ok(user)
        }
        None => {
            // Never log `password` here -- see this plan's CRITICAL SECURITY POINT.
            tracing::warn!(user = %username, "auth failure");
            Err(Frame::Error(
                "WRONGPASS invalid username-password pair or user is disabled.".into(),
            ))
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem dispatcher::tests::try_authenticate_logs
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: both PASS, fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(logging): log auth success/failure by username, never the password"
```

---

### Task 2: Permission denied — `warn` in `auth_gate`

**Files:**
- Modify: `crates/server/src/dispatcher.rs:2834-2842` (the `NOPERM` branch inside `auth_gate`)

**Interfaces:**
- Consumes: `user: Arc<AclUser>` (already in scope at this point in `auth_gate`, via the live `replication.acl.get_user(...)` lookup a few lines above) — its `.username` field is safe to log.
- Produces: nothing new consumed elsewhere. `auth_gate`'s signature and both possible `NOPERM` message texts are unchanged.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` (reuses the existing `acl_user(...)` helper already defined in this module, just above `auth_gate_with_no_acl_users_configured_lets_everything_through`):

```rust
    #[test]
    fn auth_gate_logs_permission_denied_at_warn_naming_the_user() {
        let replication = ReplicationHandle::default();
        replication
            .acl
            .set_user(
                "app",
                &[Bytes::from_static(b"on"), Bytes::from_static(b"+get"), Bytes::from_static(b"~*")],
            )
            .unwrap();
        let session = Session::new();
        session.set_authenticated_user(Some(acl_user(vec![
            crate::acl::AclRule::AllowCommand("GET".to_string()),
            crate::acl::AclRule::AllKeys,
        ])));
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Bulk(Bytes::from_static(b"k")),
            Frame::Bulk(Bytes::from_static(b"v")),
        ]);
        let log = capture_logs_at(tracing::Level::WARN, || {
            let reply = auth_gate(&replication, &session, &frame).unwrap();
            assert_eq!(
                reply,
                Frame::Error("NOPERM this user has no permissions to run this command".into())
            );
        });
        assert!(log.contains("app"), "expected the username in the event, got: {log}");
        assert!(log.contains("permission denied"), "got: {log}");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem dispatcher::tests::auth_gate_logs_permission_denied_at_warn_naming_the_user
```

Expected: compiles, fails at the assertion:

```
assertion failed: log.contains("app")
expected the username in the event, got:
```

- [ ] **Step 3: Implement**

At `crates/server/src/dispatcher.rs`, inside `auth_gate`, change:

```rust
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

to:

```rust
    let keys = command_keys(frame);
    if !user.is_allowed(name, &keys) {
        let msg = if user.is_allowed(name, &[]) {
            "NOPERM no permissions to access a key"
        } else {
            "NOPERM this user has no permissions to run this command"
        };
        // `cmd`/`key` are already attached by the enclosing `cmd` span (plan 07) -- see this
        // plan's Architecture section, so only `user` needs adding here.
        tracing::warn!(user = %user.username, "permission denied");
        return Some(Frame::Error(msg.into()));
    }
    None
}
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p rocket-mem dispatcher::tests::auth_gate_logs_permission_denied_at_warn_naming_the_user
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS, fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(logging): log NOPERM denials by username"
```

---

### Task 3: `HELLO`/RESP3 upgrade (`debug`), and `ACL SETUSER`/`DELUSER` (`info`)

**Files:**
- Modify: `crates/server/src/dispatcher.rs:2060` and `:2073` (the `Resp2`/`Resp3` arms inside `handle_hello`)
- Modify: `crates/server/src/dispatcher.rs:2605-2624` (`acl_setuser`)
- Modify: `crates/server/src/dispatcher.rs:2626-2639` (`acl_deluser`)

**Interfaces:**
- Consumes: `handle_hello`'s already-local `session` (for the negotiated `Protocol`, only used to pick the log's `resp_version` field, not re-logged as a span field); `acl_setuser`/`acl_deluser`'s already-local `username: String` (safe to log) and, in `acl_setuser`'s case, `raw_tokens: Vec<Bytes>` (**never** to be logged — see the CRITICAL SECURITY POINT: this is exactly the vector that can carry `ACL SETUSER`'s `>password` token).
- Produces: nothing new consumed elsewhere. None of the three functions' signatures or return values change.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests`:

```rust
    #[test]
    fn handle_hello_logs_a_debug_event_on_resp3_upgrade() {
        let replication = ReplicationHandle::default();
        let session = Session::new();
        let frame = cmd(&[b"HELLO", b"3"]);
        let log = capture_logs_at(tracing::Level::DEBUG, || {
            let reply = handle_hello(&frame, &session, 1, &replication);
            assert!(reply.is_some());
        });
        assert_eq!(session.protocol(), Protocol::Resp3);
        assert!(log.contains("HELLO protocol negotiated"), "got: {log}");
        assert!(log.contains('3'), "expected the negotiated version in the event, got: {log}");
    }

    #[test]
    fn acl_setuser_logs_an_info_event_naming_the_user_never_the_password_token() {
        let replication = ReplicationHandle::default();
        let frame = cmd(&[b"ACL", b"SETUSER", b"alice", b"on", b">hunter2"]);
        let log = capture_logs_at(tracing::Level::INFO, || {
            let reply = handle_acl(&frame, &Session::new(), &replication);
            assert_eq!(reply, Some(Frame::Simple("OK".into())));
        });
        assert!(log.contains("alice"), "expected the username in the event, got: {log}");
        assert!(
            !log.contains("hunter2"),
            "the ACL SETUSER password token must never be logged, got: {log}"
        );
    }

    #[test]
    fn acl_deluser_logs_an_info_event_naming_the_deleted_user() {
        let replication = ReplicationHandle::default();
        replication.acl.set_user("alice", &[Bytes::from_static(b"on")]).unwrap();
        let frame = cmd(&[b"ACL", b"DELUSER", b"alice"]);
        let log = capture_logs_at(tracing::Level::INFO, || {
            let reply = handle_acl(&frame, &Session::new(), &replication);
            assert_eq!(reply, Some(Frame::Integer(1)));
        });
        assert!(log.contains("alice"), "expected the deleted username in the event, got: {log}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem dispatcher::tests::handle_hello_logs_a_debug_event_on_resp3_upgrade
cargo test -p rocket-mem dispatcher::tests::acl_setuser_logs_an_info_event_naming_the_user_never_the_password_token
cargo test -p rocket-mem dispatcher::tests::acl_deluser_logs_an_info_event_naming_the_deleted_user
```

Expected: all three compile, all three fail at their first `log.contains(...)` assertion, e.g.:

```
assertion failed: log.contains("HELLO protocol negotiated")
got:
```

(the two ACL tests' `reply` assertions already pass before this task's implementation — only the log-content assertions are new and failing.)

- [ ] **Step 3: Implement**

In `handle_hello`, change:

```rust
                session.set_protocol(Protocol::Resp2);
                hello_reply(session.protocol(), client_id, role, mode)
            }
```

to:

```rust
                session.set_protocol(Protocol::Resp2);
                tracing::debug!(resp_version = %2, "HELLO protocol negotiated");
                hello_reply(session.protocol(), client_id, role, mode)
            }
```

and change:

```rust
                session.set_protocol(Protocol::Resp3);
                hello_reply(session.protocol(), client_id, role, mode)
            }
```

to:

```rust
                session.set_protocol(Protocol::Resp3);
                tracing::debug!(resp_version = %3, "HELLO protocol negotiated");
                hello_reply(session.protocol(), client_id, role, mode)
            }
```

In `acl_setuser`, change:

```rust
    let username = String::from_utf8_lossy(username).into_owned();
    match replication.acl.set_user(&username, &raw_tokens) {
        Ok(()) => Frame::Simple("OK".into()),
        Err(e) => Frame::Error(e.to_string()),
    }
}
```

to:

```rust
    let username = String::from_utf8_lossy(username).into_owned();
    match replication.acl.set_user(&username, &raw_tokens) {
        Ok(()) => {
            // `raw_tokens` can carry ACL SETUSER's `>password` token -- never log it. Only
            // `username` is safe here; see this plan's CRITICAL SECURITY POINT.
            tracing::info!(user = %username, "ACL SETUSER");
            Frame::Simple("OK".into())
        }
        Err(e) => Frame::Error(e.to_string()),
    }
}
```

In `acl_deluser`, change:

```rust
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

to:

```rust
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
        .filter(|b| {
            let username = String::from_utf8_lossy(b);
            let removed = replication.acl.del_user(&username);
            if removed {
                tracing::info!(user = %username, "ACL DELUSER");
            }
            removed
        })
        .count();
    Frame::Integer(deleted as i64)
}
```

(One `info!` per actually-deleted user, not per requested name — matching `del_user`'s own "was this name really present" semantics, and consistent with `DELUSER`'s reply already counting only real deletions.)

- [ ] **Step 4: Run the tests to verify they pass, then the full plan verification**

```bash
cargo test -p rocket-mem dispatcher::tests::handle_hello_logs_a_debug_event_on_resp3_upgrade
cargo test -p rocket-mem dispatcher::tests::acl_setuser_logs_an_info_event_naming_the_user_never_the_password_token
cargo test -p rocket-mem dispatcher::tests::acl_deluser_logs_an_info_event_naming_the_deleted_user
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three new tests PASS, every pre-existing test in the workspace still passes unchanged, fmt clean, clippy clean.

- [ ] **Step 5: Re-run the benchmark gate**

```bash
cd /home/numericlabs/data/rocket/rocket-mem
./scripts/benchmark.sh
```

Compare against the Mean column in `docs/benchmarks/2026-09-09-pre-logging-baseline.md`. Expected: within 2%, and trivially so — `scripts/benchmark.sh` never configures an ACL user, so `auth_gate`'s very first line (`if !replication.acl.has_ever_been_configured() { return None; }`) returns before any of this plan's code runs, and the benchmark's `SET`/`GET` workload never sends `AUTH`, `HELLO`, or `ACL`.

- [ ] **Step 6: Manual verification of the events themselves**

The tests above assert the events fire and never contain a secret; they do not assert the exact rendered line an operator would see. Confirm that by hand:

```bash
RUST_LOG=debug cargo run -p rocket-mem -- --port 7000 &
redis-cli -p 7000 HELLO 3
redis-cli -p 7000 ACL SETUSER alice on '>hunter2' '~*' '+get'
redis-cli -p 7000 -3 AUTH alice hunter2
redis-cli -p 7000 -3 AUTH alice wrong-password
redis-cli -p 7000 -3 -u redis://alice:hunter2@127.0.0.1:7000 SET k v   # alice has no +set grant
redis-cli -p 7000 ACL SETUSER alice off
redis-cli -p 7000 ACL DELUSER alice
```

Check the server's stderr for: a `HELLO protocol negotiated` debug line; an `ACL SETUSER` info line naming `alice` and nowhere containing `hunter2`; an `auth success` info line and an `auth failure` warn line, both naming `alice`, neither containing `hunter2`/`wrong-password`; a `permission denied` warn line naming `alice`; and an `ACL DELUSER` info line naming `alice`. Kill the server (`kill %1`) when done.

---

## Next plan

[`12-engine-shard-routing-trace.md`](12-engine-shard-routing-trace.md) — `trace`-level shard routing and byte-delta events in `engine/engine.rs`, `engine/shard.rs`, and `engine/store.rs`, the first plan to instrument the newly-dependency-added `engine` crate from plan 01.
