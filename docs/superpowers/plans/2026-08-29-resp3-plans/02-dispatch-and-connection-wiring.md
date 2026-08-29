# RESP3 Dispatch Signature & Connection Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** thread per-connection `Protocol` and `client_id` state from `connection.rs` through `dispatch()`, then implement `HELLO`'s full negotiation semantics.

**Architecture:** `crates/server/src/dispatcher.rs` and `crates/server/src/connection.rs` compile as one crate (`rocket_mem`), so Task 1 changes both files together in a single commit — there is no intermediate state where only one is updated that still compiles, unlike the crate-boundary staggering used in `01-frame-map-and-stateful-codec.md`. Task 2 then adds the actual `HELLO` command, touching only `dispatcher.rs`.

**Tech Stack:** no new dependencies.

**Spec:** `../../specs/2026-08-29-resp3-design.md` — `dispatch()`'s new signature, the per-connection `client_id` counter, and `HELLO`'s full semantics (report/switch/NOPROTO/syntax-error) are all authoritative.

**Depends on:** `01-frame-map-and-stateful-codec.md` must be complete (`Frame::Map` and `protocol::codec::{Protocol, RespCodec}` must already exist).

## Global Constraints

- `HELLO`'s reply is always `Frame::Map(...)` regardless of which protocol was or is being negotiated — protocol only changes how that `Map` is *encoded* on the wire (`01-frame-map-and-stateful-codec.md`'s `RespCodec::encode` already handles this). `dispatch()` never needs to branch on protocol to decide what *shape* of `Frame` to return, only whether to switch `*protocol`.
- `decode()` needs zero changes anywhere in this plan.

---

### Task 1: Thread `Protocol` and `client_id` through `dispatch()` and `connection.rs`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`
- Modify: `crates/server/src/connection.rs`

**Interfaces:**
- Consumes: `protocol::codec::{Protocol, RespCodec}` (`01-frame-map-and-stateful-codec.md`).
- Produces: `pub fn dispatch(engine: &Engine, frame: Frame, protocol: &mut Protocol, client_id: u64) -> Frame` (parameters temporarily named `_protocol`/`_client_id` and unused until Task 2 — this task adds no new command behavior, `HELLO` still falls through to the unknown-command error exactly as before). `serve()`'s public signature (`pub async fn serve(listener: TcpListener, engine: Arc<Engine>)`) is unchanged — only its internal implementation and `handle_connection`'s signature change.

- [ ] **Step 1: Update `dispatch()`'s signature and imports**

```rust
// crates/server/src/dispatcher.rs — top of file, replace the `use` block with:
use bytes::Bytes;
use engine::{commands, Engine, Value};
use protocol::codec::Protocol;
use protocol::Frame;
```

```rust
// crates/server/src/dispatcher.rs — replace the dispatch() signature line with:
pub fn dispatch(engine: &Engine, frame: Frame, _protocol: &mut Protocol, _client_id: u64) -> Frame {
```

Leave the function body's `match name.as_str() { ... }` block completely unchanged — every
existing arm ignores the two new (underscore-prefixed, temporarily unused) parameters.
Task 2 renames them and adds the `HELLO` arm that actually uses them.

- [ ] **Step 2: Migrate every existing `dispatch()` call site in this file's tests**

Every call of the form `dispatch(&engine, X)` becomes
`dispatch(&engine, X, &mut Protocol::default(), 1)` — a fixed literal suffix appended to
every call, no other changes. There are 45 call sites across the `tests` module (some
test functions call `dispatch` more than once). For example:

```rust
// before
assert_eq!(dispatch(&engine, cmd(&[b"GET", b"missing"])), Frame::Null);

// after
assert_eq!(
    dispatch(&engine, cmd(&[b"GET", b"missing"]), &mut Protocol::default(), 1),
    Frame::Null
);
```

Also add `Protocol` to the test module's imports:

```rust
// crates/server/src/dispatcher.rs — tests module, replace the use block with:
use super::*;
use bytes::Bytes;
use engine::Engine;
use protocol::codec::Protocol;
use protocol::Frame;
```

Do **not** touch the `hello_is_not_implemented_and_falls_through_to_unknown_command` test's
assertion yet — with no `HELLO` arm added in this task, `HELLO` still falls through to the
unknown-command error exactly as before, so that test stays correct here (only its call
site gains the two new arguments, same as every other test). Task 2 replaces this test.

- [ ] **Step 3: Wire a per-connection `client_id` and `Protocol` into `connection.rs`**

```rust
// crates/server/src/connection.rs — replace the entire file above the tests module with:
use crate::dispatcher;
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use protocol::codec::{Protocol, RespCodec};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::codec::Framed;

pub async fn serve(listener: TcpListener, engine: Arc<Engine>) {
    let mut next_client_id: u64 = 1;
    loop {
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        let client_id = next_client_id;
        next_client_id += 1;
        let engine = Arc::clone(&engine);
        tokio::spawn(handle_connection(socket, engine, client_id));
    }
}

async fn handle_connection(socket: tokio::net::TcpStream, engine: Arc<Engine>, client_id: u64) {
    let mut framed = Framed::new(socket, RespCodec::default());
    let mut protocol = Protocol::default();
    while let Some(result) = framed.next().await {
        let frame = match result {
            Ok(frame) => frame,
            Err(_) => return, // malformed input or a dropped connection — end this task quietly
        };
        let response = dispatcher::dispatch(&engine, frame, &mut protocol, client_id);
        framed.codec_mut().protocol = protocol; // sync BEFORE sending this reply
        if framed.send(response).await.is_err() {
            return; // client went away mid-response
        }
    }
}
```

No atomic needed for `next_client_id` — `serve()`'s accept loop is already single-threaded
and sequential; only that loop ever increments it, before handing a plain `u64` value off
to each spawned task.

- [ ] **Step 4: Migrate `connection.rs`'s test module to `RespCodec::default()`**

```rust
// crates/server/src/connection.rs — tests module, replace the use block with:
use super::*;
use bytes::Bytes;
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use protocol::{codec::RespCodec, Frame};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
```

Then replace each of the 4 bare `RespCodec` values in the 3 test functions
(`serve_handles_a_full_set_get_round_trip_over_a_real_socket`,
`serve_handles_two_concurrent_connections_independently` — 2 occurrences,
`serve_closes_the_connection_cleanly_when_the_client_disconnects`) with
`RespCodec::default()`. For example:

```rust
// before
let mut framed = Framed::new(stream, RespCodec);

// after
let mut framed = Framed::new(stream, RespCodec::default());
```

None of the tests' assertions change — `RespCodec::default()` produces the same RESP2
behavior these tests already exercise. `serve(listener, engine)`'s call sites in these
tests are unchanged (its public signature didn't change).

- [ ] **Step 5: Run the full workspace check**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean — this is the first point since `01-frame-map-and-stateful-codec.md`
started where the whole workspace compiles and every existing test (dispatcher, codec,
connection, and the `redis-rs`/malformed-input integration tests) passes unchanged.

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` and `crates/server/src/connection.rs` — do not compose
the commit message freeform. Suggested subject:
`feat(server): thread Protocol and client_id through dispatch and connection.rs`.

---

### Task 2: Implement `HELLO`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `_protocol: &mut Protocol`, `_client_id: u64` (Task 1 — renamed back to
  `protocol`/`client_id` in this task, now genuinely used).
- Produces: a `HELLO` match arm and a private `hello_reply(protocol: Protocol, client_id: u64) -> Frame` helper.

- [ ] **Step 1: Write the failing tests**

```rust
// add to crates/server/src/dispatcher.rs tests module, replacing
// hello_is_not_implemented_and_falls_through_to_unknown_command entirely
#[test]
fn hello_with_no_args_reports_current_protocol_without_switching() {
    let engine = Engine::new();
    let mut protocol = Protocol::Resp2;
    let reply = dispatch(&engine, cmd(&[b"HELLO"]), &mut protocol, 7);
    assert_eq!(protocol, Protocol::Resp2); // unchanged
    assert_eq!(
        reply,
        Frame::Map(vec![
            (
                Frame::Bulk(Bytes::from_static(b"server")),
                Frame::Bulk(Bytes::from_static(b"redis"))
            ),
            (
                Frame::Bulk(Bytes::from_static(b"version")),
                Frame::Bulk(Bytes::from_static(b"rocket-mem-0.1.0"))
            ),
            (Frame::Bulk(Bytes::from_static(b"proto")), Frame::Integer(2)),
            (Frame::Bulk(Bytes::from_static(b"id")), Frame::Integer(7)),
            (
                Frame::Bulk(Bytes::from_static(b"mode")),
                Frame::Bulk(Bytes::from_static(b"standalone"))
            ),
            (
                Frame::Bulk(Bytes::from_static(b"role")),
                Frame::Bulk(Bytes::from_static(b"master"))
            ),
            (Frame::Bulk(Bytes::from_static(b"modules")), Frame::Array(vec![])),
        ])
    );
}

#[test]
fn hello_2_switches_protocol_to_resp2() {
    let engine = Engine::new();
    let mut protocol = Protocol::Resp3;
    let reply = dispatch(&engine, cmd(&[b"HELLO", b"2"]), &mut protocol, 1);
    assert_eq!(protocol, Protocol::Resp2);
    let Frame::Map(pairs) = reply else {
        panic!("expected Map")
    };
    assert!(pairs.contains(&(
        Frame::Bulk(Bytes::from_static(b"proto")),
        Frame::Integer(2)
    )));
}

#[test]
fn hello_3_switches_protocol_to_resp3() {
    let engine = Engine::new();
    let mut protocol = Protocol::Resp2;
    let reply = dispatch(&engine, cmd(&[b"HELLO", b"3"]), &mut protocol, 42);
    assert_eq!(protocol, Protocol::Resp3);
    assert_eq!(
        reply,
        Frame::Map(vec![
            (
                Frame::Bulk(Bytes::from_static(b"server")),
                Frame::Bulk(Bytes::from_static(b"redis"))
            ),
            (
                Frame::Bulk(Bytes::from_static(b"version")),
                Frame::Bulk(Bytes::from_static(b"rocket-mem-0.1.0"))
            ),
            (Frame::Bulk(Bytes::from_static(b"proto")), Frame::Integer(3)),
            (Frame::Bulk(Bytes::from_static(b"id")), Frame::Integer(42)),
            (
                Frame::Bulk(Bytes::from_static(b"mode")),
                Frame::Bulk(Bytes::from_static(b"standalone"))
            ),
            (
                Frame::Bulk(Bytes::from_static(b"role")),
                Frame::Bulk(Bytes::from_static(b"master"))
            ),
            (Frame::Bulk(Bytes::from_static(b"modules")), Frame::Array(vec![])),
        ])
    );
}

#[test]
fn hello_with_unsupported_protover_returns_noproto_and_leaves_protocol_unchanged() {
    let engine = Engine::new();
    let mut protocol = Protocol::Resp2;
    let reply = dispatch(&engine, cmd(&[b"HELLO", b"4"]), &mut protocol, 1);
    assert_eq!(protocol, Protocol::Resp2); // unchanged
    assert_eq!(
        reply,
        Frame::Error("NOPROTO unsupported protocol version".into())
    );
}

#[test]
fn hello_with_extra_args_after_protover_is_a_syntax_error() {
    let engine = Engine::new();
    let mut protocol = Protocol::Resp2;
    let reply = dispatch(
        &engine,
        cmd(&[b"HELLO", b"3", b"AUTH", b"user", b"pass"]),
        &mut protocol,
        1,
    );
    assert_eq!(protocol, Protocol::Resp2); // unchanged — the switch never happened
    assert_eq!(reply, Frame::Error("ERR syntax error".into()));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — no `HELLO` arm exists yet, so all four new tests get the unknown-command
error instead

- [ ] **Step 3: Rename the parameters and add the `HELLO` arm**

```rust
// crates/server/src/dispatcher.rs — replace the dispatch() signature line with:
pub fn dispatch(engine: &Engine, frame: Frame, protocol: &mut Protocol, client_id: u64) -> Frame {
```

```rust
// crates/server/src/dispatcher.rs — add this arm to the match, above the `_ =>` catch-all
"HELLO" => match rest.first() {
    None => hello_reply(*protocol, client_id),
    Some(arg) => match arg.as_ref() {
        b"2" => {
            if rest.len() > 1 {
                return Frame::Error("ERR syntax error".into());
            }
            *protocol = Protocol::Resp2;
            hello_reply(*protocol, client_id)
        }
        b"3" => {
            if rest.len() > 1 {
                return Frame::Error("ERR syntax error".into());
            }
            *protocol = Protocol::Resp3;
            hello_reply(*protocol, client_id)
        }
        _ => Frame::Error("NOPROTO unsupported protocol version".into()),
    },
},
```

```rust
// crates/server/src/dispatcher.rs — add this function above dispatch()
fn hello_reply(protocol: Protocol, client_id: u64) -> Frame {
    Frame::Map(vec![
        (
            Frame::Bulk(Bytes::from_static(b"server")),
            Frame::Bulk(Bytes::from_static(b"redis")),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"version")),
            Frame::Bulk(Bytes::from_static(b"rocket-mem-0.1.0")),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"proto")),
            Frame::Integer(match protocol {
                Protocol::Resp2 => 2,
                Protocol::Resp3 => 3,
            }),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"id")),
            Frame::Integer(client_id as i64),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"mode")),
            Frame::Bulk(Bytes::from_static(b"standalone")),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"role")),
            Frame::Bulk(Bytes::from_static(b"master")),
        ),
        (Frame::Bulk(Bytes::from_static(b"modules")), Frame::Array(vec![])),
    ])
}
```

- [ ] **Step 4: Delete the now-stale test**

Remove `hello_is_not_implemented_and_falls_through_to_unknown_command` entirely — `HELLO`
is implemented now, so its assertion (`Frame::Error("ERR unknown command 'HELLO'")`) is no
longer true. The four new tests from Step 1 replace its coverage.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 4 new ones (net: 4 added, 1 removed from the
pre-Task-1 count)

- [ ] **Step 6: Run the full workspace check**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean.

- [ ] **Step 7: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): implement HELLO for RESP3 negotiation`.
