# Stub Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** implement `PING`, `ECHO`, `SELECT`, `COMMAND`, and `INFO` in the dispatcher — the handshake/capability-probe commands real client libraries send before or alongside actual data commands, per `../rocket-mem-production-plan.md` Week 4.

**Architecture:** five new arms in `dispatcher::dispatch`'s `match`. None of these touch `Engine` — they're protocol-level bookkeeping, not storage operations.

**Tech Stack:** no new dependencies.

**Spec:** `00-sprint-2-spec.md` — the RESP3/`HELLO` decision (reject, don't implement) is directly relevant: `HELLO` deliberately falls through to the existing "unknown command" arm, it does not get its own case here.

**Depends on:** `03-command-dispatcher.md` must be complete. Independent of `04-tcp-listener.md` — these are pure `dispatch()` tests, no socket needed.

---

### Task 1: `PING` and `ECHO`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// add to crates/server/src/dispatcher.rs tests module
#[test]
fn ping_with_no_args_replies_pong() {
    let engine = Engine::new();
    assert_eq!(dispatch(&engine, cmd(&[b"PING"])), Frame::Simple("PONG".into()));
}

#[test]
fn ping_with_a_message_echoes_it_back_as_a_bulk_string() {
    let engine = Engine::new();
    assert_eq!(dispatch(&engine, cmd(&[b"PING", b"hello"])), Frame::Bulk(Bytes::from_static(b"hello")));
}

#[test]
fn echo_returns_its_argument() {
    let engine = Engine::new();
    assert_eq!(dispatch(&engine, cmd(&[b"ECHO", b"hi"])), Frame::Bulk(Bytes::from_static(b"hi")));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — `PING`/`ECHO` fall through to the "unknown command" arm today

- [ ] **Step 3: Add the match arms**

```rust
// crates/server/src/dispatcher.rs — add arms to the match in dispatch(), above the `_ =>` catch-all
"PING" => match rest.first() {
    Some(msg) => Frame::Bulk(msg.clone()),
    None => Frame::Simple("PONG".into()),
},
"ECHO" => Frame::Bulk(rest[0].clone()),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 3 new ones

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): add PING/ECHO"
```

---

### Task 2: `SELECT`, `COMMAND`, `INFO`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// add to crates/server/src/dispatcher.rs tests module
#[test]
fn select_always_replies_ok_single_db_only() {
    let engine = Engine::new();
    assert_eq!(dispatch(&engine, cmd(&[b"SELECT", b"0"])), Frame::Simple("OK".into()));
}

#[test]
fn command_replies_with_an_empty_array_rather_than_erroring() {
    let engine = Engine::new();
    assert_eq!(dispatch(&engine, cmd(&[b"COMMAND"])), Frame::Array(vec![]));
}

#[test]
fn info_replies_a_non_empty_bulk_string() {
    let engine = Engine::new();
    let Frame::Bulk(info) = dispatch(&engine, cmd(&[b"INFO"])) else { panic!("expected Bulk") };
    assert!(!info.is_empty());
}

#[test]
fn hello_is_not_implemented_and_falls_through_to_unknown_command() {
    // per 00-sprint-2-spec.md's RESP3 decision: HELLO gets the same
    // treatment as any other unrecognized command, on purpose
    let engine = Engine::new();
    assert_eq!(dispatch(&engine, cmd(&[b"HELLO", b"3"])), Frame::Error("ERR unknown command 'HELLO'".into()));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — `SELECT`/`COMMAND`/`INFO` don't exist yet (the `HELLO` test should already pass, since it's just exercising the existing catch-all — confirm that one passes without changes)

- [ ] **Step 3: Add the match arms**

```rust
// crates/server/src/dispatcher.rs — add arms to the match in dispatch()
"SELECT" => Frame::Simple("OK".into()), // single logical DB only, per 00-sprint-2-spec.md scope
"COMMAND" => Frame::Array(vec![]), // enough that clients probing capabilities don't choke
"INFO" => Frame::Bulk(Bytes::from_static(
    b"# Server\r\nredis_version:rocket-mem-0.1.0\r\n",
)),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 4 new ones

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): add SELECT/COMMAND/INFO stubs"
```
