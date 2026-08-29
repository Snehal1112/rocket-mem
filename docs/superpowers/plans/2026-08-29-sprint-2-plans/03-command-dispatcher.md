# Command Dispatcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a `dispatch(&Engine, Frame) -> Frame` function that parses a RESP command array, validates it, calls the matching Sprint 1 `engine::commands::*` function, and serializes the result back to a `Frame` — with no networking involved yet, called directly with hand-built `Frame`s in tests.

**Architecture:** lives in `crates/server`, which becomes a lib+bin hybrid this sprint (see `../../specs/2026-08-29-sprint-2-spec.md`). `dispatch` is the single entry point Sprint 2's TCP listener (item 04) will call per received frame — same relationship `Engine` had to Sprint 1's commands.

**Tech Stack:** no new dependencies — `bytes`, `protocol::Frame`, `engine::{Engine, commands::*}`, `common::EngineError`.

**Spec:** `../../specs/2026-08-29-sprint-2-spec.md` — the dispatcher shape, case-insensitivity rule, and `EngineError` → `Frame::Error` mapping convention are authoritative.

**Depends on:** `01-resp-frame-and-parser.md` must be complete. Independent of `02-partial-read-framing.md`.

---

### Task 1: Convert `crates/server` to a lib+bin crate

**Files:**
- Modify: `crates/server/Cargo.toml`
- Create: `crates/server/src/lib.rs`
- Modify: `crates/server/src/main.rs`

- [ ] **Step 1: Update the manifest**

```toml
# crates/server/Cargo.toml
[package]
name = "rocket-mem"
edition.workspace = true
version.workspace = true

[lib]
name = "rocket_mem"
path = "src/lib.rs"

[[bin]]
name = "rocket-mem"
path = "src/main.rs"

[dependencies]
engine = { path = "../engine" }
protocol = { path = "../protocol" }
common = { path = "../common" }
```

- [ ] **Step 2: Create the (currently empty) lib root**

```rust
// crates/server/src/lib.rs
pub mod dispatcher;
```

- [ ] **Step 3: Point `main.rs` at the lib (no behavior change yet)**

```rust
// crates/server/src/main.rs
fn main() {
    // Networking wired up in 04-tcp-listener.md.
}
```

- [ ] **Step 4: Verify the workspace still builds**

Run: `cargo build --workspace`
Expected: FAIL — `dispatcher` module referenced in `lib.rs` doesn't exist yet; this is expected, Task 2 creates it

- [ ] **Step 5: Commit is deferred to the end of Task 2** — an empty `pub mod dispatcher;` with no file behind it doesn't compile, so there's no green state to commit here. Continue directly to Task 2.

---

### Task 2: `dispatch` — command lookup, case-insensitivity, arg extraction

**Files:**
- Create: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `protocol::Frame` (from `01-resp-frame-and-parser.md`), `engine::Engine`, `engine::commands::string::{set_nx, set_xx, get, append, strlen, incr_by}`.
- Produces: `pub fn dispatch(engine: &engine::Engine, frame: protocol::Frame) -> protocol::Frame` — item 04's TCP listener calls this once per decoded frame.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use engine::Engine;
    use protocol::Frame;

    fn cmd(parts: &[&[u8]]) -> Frame {
        Frame::Array(parts.iter().map(|p| Frame::Bulk(Bytes::copy_from_slice(p))).collect())
    }

    #[test]
    fn dispatch_is_case_insensitive_on_command_name() {
        let engine = Engine::new();
        assert_eq!(dispatch(&engine, cmd(&[b"set", b"k", b"v"])), Frame::Simple("OK".into()));
        assert_eq!(dispatch(&engine, cmd(&[b"SeT", b"k2", b"v2"])), Frame::Simple("OK".into()));
    }

    #[test]
    fn dispatch_set_then_get_round_trips() {
        let engine = Engine::new();
        dispatch(&engine, cmd(&[b"SET", b"foo", b"bar"]));
        assert_eq!(dispatch(&engine, cmd(&[b"GET", b"foo"])), Frame::Bulk(Bytes::from_static(b"bar")));
    }

    #[test]
    fn dispatch_get_on_missing_key_returns_null() {
        let engine = Engine::new();
        assert_eq!(dispatch(&engine, cmd(&[b"GET", b"missing"])), Frame::Null);
    }

    #[test]
    fn dispatch_wrongtype_is_mapped_to_a_resp_error_frame() {
        let engine = Engine::new();
        dispatch(&engine, cmd(&[b"SET", b"k", b"v"]));
        // HSET on a string key: WRONGTYPE
        let reply = dispatch(&engine, cmd(&[b"HSET", b"k", b"f", b"v"]));
        assert_eq!(reply, Frame::Error("WRONGTYPE Operation against a key holding the wrong kind of value".into()));
    }

    #[test]
    fn dispatch_unknown_command_returns_resp_error() {
        let engine = Engine::new();
        assert_eq!(dispatch(&engine, cmd(&[b"NOPE"])), Frame::Error("ERR unknown command 'NOPE'".into()));
    }

    #[test]
    fn dispatch_on_non_array_frame_returns_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, Frame::Simple("not a command".into())),
            Frame::Error("ERR invalid request, expected array of bulk strings".into())
        );
    }

    #[test]
    fn dispatch_on_empty_array_returns_resp_error() {
        let engine = Engine::new();
        assert_eq!(dispatch(&engine, Frame::Array(vec![])), Frame::Error("ERR empty command".into()));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — `dispatch` and `frame_to_args` not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs (above the test module)
use bytes::Bytes;
use engine::{commands, Engine, Value};
use protocol::Frame;

/// Extracts the `Vec<Bytes>` command name+args from an `Array` of `Bulk` frames —
/// the only shape a real RESP client ever sends a command as.
fn frame_to_args(frame: Frame) -> Result<Vec<Bytes>, Frame> {
    let Frame::Array(items) = frame else {
        return Err(Frame::Error(
            "ERR invalid request, expected array of bulk strings".into(),
        ));
    };
    items
        .into_iter()
        .map(|item| match item {
            Frame::Bulk(b) => Ok(b),
            _ => Err(Frame::Error(
                "ERR invalid request, expected array of bulk strings".into(),
            )),
        })
        .collect()
}

fn engine_error_to_frame(e: common::EngineError) -> Frame {
    Frame::Error(e.to_string())
}

pub fn dispatch(engine: &Engine, frame: Frame) -> Frame {
    let args = match frame_to_args(frame) {
        Ok(a) => a,
        Err(e) => return e,
    };
    if args.is_empty() {
        return Frame::Error("ERR empty command".into());
    }
    let name = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
    let rest = &args[1..];

    match name.as_str() {
        "GET" => match commands::string::get(engine, &rest[0]) {
            Ok(Some(b)) => Frame::Bulk(b),
            Ok(None) => Frame::Null,
            Err(e) => engine_error_to_frame(e),
        },
        "SET" => {
            engine.set(rest[0].clone(), Value::String(rest[1].clone()));
            Frame::Simple("OK".into())
        }
        "APPEND" => match commands::string::append(engine, rest[0].clone(), &rest[1]) {
            Ok(len) => Frame::Integer(len as i64),
            Err(e) => engine_error_to_frame(e),
        },
        "STRLEN" => match commands::string::strlen(engine, &rest[0]) {
            Ok(len) => Frame::Integer(len as i64),
            Err(e) => engine_error_to_frame(e),
        },
        "INCR" => match commands::string::incr_by(engine, rest[0].clone(), 1) {
            Ok(n) => Frame::Integer(n),
            Err(e) => engine_error_to_frame(e),
        },
        "DECR" => match commands::string::incr_by(engine, rest[0].clone(), -1) {
            Ok(n) => Frame::Integer(n),
            Err(e) => engine_error_to_frame(e),
        },
        "HSET" => match commands::hash::hset(engine, rest[0].clone(), rest[1].clone(), rest[2].clone()) {
            Ok(()) => Frame::Integer(1),
            Err(e) => engine_error_to_frame(e),
        },
        "HGET" => match commands::hash::hget(engine, &rest[0], &rest[1]) {
            Ok(Some(b)) => Frame::Bulk(b),
            Ok(None) => Frame::Null,
            Err(e) => engine_error_to_frame(e),
        },
        _ => Frame::Error(format!("ERR unknown command '{name}'")),
    }
}
```

Note: this deliberately implements only enough commands (`GET`/`SET`/`APPEND`/`STRLEN`/`INCR`/`DECR`/`HSET`/`HGET`) to prove the dispatch pattern end-to-end and pass this task's tests — it is **not** the full Sprint 1 command surface. Wiring every remaining `engine::commands::*` function (the rest of hash, all of list, all of set, `set_nx`/`set_xx` flags) through this same `match` arm pattern is `06-integration-test-harness.md`'s Task 1, once the shape is proven here. Don't treat this list as complete when reviewing other tasks.

**Known gap, deliberately deferred, not silently missed:** every `rest[N]` index above assumes the caller sent enough arguments — a bare `HSET` with no key/field/value would panic on `rest[2]` instead of returning a clean RESP error, because nothing here checks `rest.len()` before indexing. This is exactly the kind of gap Sprint 1's own WRONGTYPE sweep (`06-wrongtype-error-handling-test-matrix.md`) was built to catch systematically rather than fix ad hoc per command — `06-integration-test-harness.md`'s Task 1 must add an arg-count check per command (`if rest.len() < N { return Frame::Error("ERR wrong number of arguments".into()); }`) *before* wiring the remaining commands through, not after. Do not ship the full command table without it — a client sending a malformed command must get a RESP error back, not have its connection task panic out from under it.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, 7/7

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings`
Expected: clean — `engine::commands` functions used here now have a real (non-test) caller for the first time since Sprint 1, on top of already being `pub mod`

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/Cargo.toml`, `crates/server/src/lib.rs`, `crates/server/src/main.rs`, and
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): add command dispatcher, convert server crate to lib+bin`.
