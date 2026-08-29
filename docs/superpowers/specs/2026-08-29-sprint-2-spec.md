# Sprint 2 — RESP Protocol, Networking & Client Compatibility: Spec & Design

**Goal:** real Redis clients (`redis-cli`, plus 2+ language libraries) can connect to rocket-mem over TCP and run the full Sprint 1 command set.

**Scope:** covers Sprint 2's 8 backlog items (see `../../rocket-mem-sprint-plan.md`, Sprint 2, and `../../rocket-mem-production-plan.md`, Weeks 3–4). This doc fixes the shared design decisions — the `Frame` type, the RESP2 wire format, the RESP3/`HELLO` decision, the dispatcher shape, and the workspace/crate changes — that every implementation plan below assumes as ground truth. Individual plans don't re-derive these; they reference this doc.

**Architecture recap:** per the master plan's Architecture Decision Record — layered, sharded, lock-based, task-per-connection. Sprint 1 built the bottom layer (storage engine). This sprint builds the middle and top layers: Command Dispatcher and Protocol Layer (RESP2 only — RESP3 is explicitly rejected this sprint, see below).

---

## Workspace changes

Two new workspace dependencies, added to the root `Cargo.toml`:

```toml
[workspace.dependencies]
# ...existing entries unchanged...
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "io-util", "time"] }
tokio-util = { version = "0.7", features = ["codec"] }
```

`redis` (the `redis-rs` client crate) is added as a **dev-dependency only**, on `crates/server`, for the integration test harness (item 06) — never a normal dependency, per the production plan's Week 4 note.

## `crates/server` becomes a lib+bin hybrid crate

Sprint 1 left `crates/server` as a bin-only crate (`src/main.rs`, `fn main() {}`). That doesn't survive this sprint: the command dispatcher and the TCP accept loop need to exist and be testable *before* `main()` wires them together, and a bin target has no "this could be used externally" excuse the way a lib target does — see "Known CI gotcha, pre-solved this time" below for why that distinction matters.

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
tokio.workspace = true

[dev-dependencies]
redis = "0.27"
```

```rust
// crates/server/src/main.rs
#[tokio::main]
async fn main() -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:6379").await?;
    println!("Listening on {}", listener.local_addr()?);
    let engine = std::sync::Arc::new(engine::Engine::new());
    rocket_mem::serve(listener, engine).await;
    Ok(())
}
```

`src/lib.rs` (crate name `rocket_mem`, package name stays `rocket-mem`) is built up across items 03–05 below: `pub mod dispatcher;` plus a `pub async fn serve(...)`.

## The `Frame` type (lives in `crates/protocol`)

```rust
// crates/protocol/src/frame.rs
use bytes::Bytes;

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Bytes),
    Null,
    Array(Vec<Frame>),
}
```

This is RESP2 only — no Map/Set/Double/Boolean/Verbatim/Big Number/Push types from RESP3. `Null` encodes as RESP2's null bulk string (`$-1\r\n`), not RESP3's `_\r\n`, since this sprint never negotiates RESP3 (see below).

## RESP2 wire format (encoding rules, authoritative)

| Frame variant | Wire encoding |
|---|---|
| `Simple(s)` | `+{s}\r\n` |
| `Error(s)` | `-{s}\r\n` |
| `Integer(n)` | `:{n}\r\n` |
| `Bulk(b)` | `${b.len()}\r\n{b}\r\n` |
| `Null` | `$-1\r\n` |
| `Array(items)` | `*{items.len()}\r\n` followed by each item's own encoding, concatenated |

Every line-terminated field ends `\r\n` — not bare `\n`. Decoding must treat a lone `\n` without a preceding `\r` as an incomplete frame (need more bytes), never as a valid terminator — real RESP always pairs them.

## RESP3 / `HELLO` decision — **reject, force RESP2**

Sprint 2's named risk in `../../rocket-mem-sprint-plan.md` says decide this explicitly, don't discover it mid-debug. **Decision: rocket-mem does not implement RESP3.** `HELLO` is treated as an unrecognized command and gets the same `Frame::Error("ERR unknown command 'HELLO'")` response as any other unknown command (see item 05). This is safe: `redis-py`, `ioredis`, and `go-redis` all send `HELLO 3` optimistically on connect and fall back to RESP2 on any error response — this is the documented client-negotiation behavior all three follow, not a rocket-mem-specific accommodation. Revisit RESP3 only if a client library is found that hard-fails instead of falling back (none of the three targeted in item 07 do).

## Command dispatcher shape (`crates/server/src/dispatcher.rs`)

```rust
pub fn dispatch(engine: &engine::Engine, frame: protocol::Frame) -> protocol::Frame {
    // 1. extract Vec<Bytes> args from an Array-of-Bulk frame
    // 2. uppercase args[0] for case-insensitive command matching
    // 3. match against the command table, call the matching engine::commands::* fn
    // 4. map Result<T, common::EngineError> -> Frame (Ok -> success frame, Err -> Frame::Error)
    // 5. unknown command -> Frame::Error("ERR unknown command '<name>'")
}
```

Case-insensitivity: `SET`, `set`, `Set` are equivalent — this matches real Redis and is a P0 correctness requirement, not a nicety.

**Error mapping:** `common::EngineError` already carries the exact wire-format error text via its `thiserror` `Display` impl (`"WRONGTYPE Operation against a key holding the wrong kind of value"`, `"value is not an integer or out of range"`). The dispatcher maps `Err(e)` to `Frame::Error(e.to_string())` directly — no separate error-message table to keep in sync.

## Integration test approach — in-process, not subprocess

The production plan's Week 4 example test implies spawning the server as a subprocess and discovering its port via stdout. **Decision: don't do that.** Because `crates/server` is now a lib (`rocket_mem`), integration tests in `crates/server/tests/` can call `rocket_mem::serve(listener, engine)` directly in-process, inside a `#[tokio::test]`, against a `TcpListener` bound to `127.0.0.1:0` (OS-assigned port, read back via `local_addr()`). This is faster, simpler, and avoids a whole class of subprocess-lifecycle/port-discovery flakiness for no loss of coverage — the actual bytes still go over a real TCP socket via `redis-rs`, which is what matters for protocol-level tests. See item 06.

## Known CI gotcha, pre-solved this time

Sprint 1's retro: `cargo clippy --workspace -- -D warnings` flags any function with no caller as `dead_code`, and this bit `engine::commands` hard because no dispatcher existed yet when those functions were written. **The fix carries forward as a rule for this sprint too:** every new module in `crates/protocol` and `crates/server`'s `src/lib.rs` must be declared `pub` at every level from the crate root down (`pub mod frame;`, `pub mod codec;`, `pub mod dispatcher;`, etc.), even before a downstream item wires it up. A `pub` item in a library crate's public API surface is never flagged as unused, because external code could call it — this is exactly why converting `crates/server` to a lib (above) matters: without a `[lib]` target, `dispatcher.rs`'s functions would be unreachable-until-`main()`-calls-them and clippy *would* flag them, with no `pub`-based escape hatch available for a bin target. Do not add `#[allow(dead_code)]` anywhere as a substitute for this — it hides real dead code too.

## Sequencing

Plans depend on each other in this order (all live in `../plans/2026-08-29-sprint-2-plans/`):
1. `01-resp-frame-and-parser.md` — `Frame` type + RESP2 decoder/encoder (no I/O yet, buffer-in/buffer-out)
2. `02-partial-read-framing.md` (depends on 1) — proves the decoder handles a command split across multiple reads
3. `03-command-dispatcher.md` (depends on 1, and Sprint 1's `engine`) — no networking yet, calls `dispatch()` directly with hand-built `Frame`s
4. `04-tcp-listener.md` (depends on 1, 3) — wires `Framed<TcpStream, RespCodec>` + `dispatch()` into a real accept loop
5. `05-stub-commands.md` (depends on 3) — `PING`/`ECHO`/`SELECT`/`COMMAND`/`INFO`, independent of 4, can run in parallel with it
6. `06-integration-test-harness.md` (depends on 4, 5) — `redis-rs` driving the in-process server
7. `07-manual-client-verification.md` (depends on 6) — checklist run against `redis-py`/`ioredis`/`go-redis`
8. `08-benchmark-smoke-test.md` (depends on 4) — independent of 5–7, can run any time after networking exists

## Definition of done for the sprint

Matches Sprint 2 in `../../rocket-mem-sprint-plan.md`:
- [ ] `redis-cli` runs every Sprint 1 command correctly over real TCP
- [ ] At least 2 non-Rust client libraries connect and run a basic workload
- [ ] Split/malformed-input integration tests pass in CI
- [ ] Phase 1 retro note added to the repo (per the master plan's Week 4 task)
- [ ] `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` clean, `cargo fmt --all -- --check` clean (carried forward from Sprint 1, not re-stated per item below)
