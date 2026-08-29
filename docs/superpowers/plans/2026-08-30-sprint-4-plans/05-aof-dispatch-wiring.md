# AOF Dispatch Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** every successful write command gets appended to the AOF — with `SPOP` rewritten to the member it actually removed, and every *relative* TTL rewritten to an absolute `PEXPIREAT` (the `EXPIRE` family, and `SET`'s `EX`/`PX` flags from `03-expire-family-and-set-ttl-dispatcher.md`) — without touching `dispatch`'s signature or any of its ~250 existing call sites.

**Architecture:** a new `dispatch_and_log` function in `dispatcher.rs` wraps `dispatch`: it calls `dispatch` first, and if the command is in `aof::WRITE_COMMANDS` and the reply isn't an error, appends the (possibly rewritten) command to the `AofWriter`. `connection.rs`'s real serving loop switches from `dispatch` to `dispatch_and_log`; nothing else changes.

**Tech Stack:** none new — `bytes::Bytes`, `protocol::Frame` (existing).

**Spec:** `../../specs/2026-08-30-sprint-4-spec.md` — the `SPOP`→`SREM`, `EXPIRE`-family→`PEXPIREAT`, and `SET ... EX/PX`→`SET` + `PEXPIREAT` rewrite rules, and why `dispatch` itself stays untouched, are authoritative.

**Depends on:** `03-expire-family-and-set-ttl-dispatcher.md` (the `EXPIRE` family must exist to rewrite), `04-aof-writer.md` (`AofWriter`, `WRITE_COMMANDS`).

## Global Constraints

- No existing call site of `dispatch(...)` changes — this plan only *adds* `dispatch_and_log` alongside it.
- A command that returned `Frame::Error(_)` is never logged — it didn't reach a mutation.

---

### Task 1: `dispatch_and_log` with the `SPOP`, `EXPIRE`-family, and `SET ... EX/PX` rewrites

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `aof::{AofWriter, WRITE_COMMANDS}` (from `04-aof-writer.md`).
- Produces: `pub fn dispatch_and_log(engine: &Engine, aof: &crate::aof::AofWriter, frame: Frame, protocol: &mut Protocol, client_id: u64) -> Frame`. `02-active-expiry-background-task.md`'s and every other plan's `dispatch`-calling tests are unaffected; `05`'s own tests below are the only callers of `dispatch_and_log` until `Task 2` wires it into `connection.rs`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
use crate::aof::{AofWriter, FsyncPolicy};

fn test_aof() -> (tempfile::TempDir, AofWriter) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
    (dir, writer)
}

fn read_aof(dir: &tempfile::TempDir) -> String {
    std::fs::read_to_string(dir.path().join("test.aof")).unwrap()
}

#[test]
fn dispatch_and_log_appends_a_write_command_verbatim() {
    let engine = Engine::new();
    let (dir, aof) = test_aof();
    let reply = dispatch_and_log(&engine, &aof, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(reply, Frame::Simple("OK".into()));
    aof.fsync().unwrap();
    assert_eq!(read_aof(&dir), "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
}

#[test]
fn dispatch_and_log_does_not_log_a_read_only_command() {
    let engine = Engine::new();
    let (dir, aof) = test_aof();
    dispatch_and_log(&engine, &aof, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    dispatch_and_log(&engine, &aof, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1);
    aof.fsync().unwrap();
    // only the one SET appears — GET never got appended
    assert_eq!(read_aof(&dir), "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
}

#[test]
fn dispatch_and_log_does_not_log_a_write_command_that_errored() {
    let engine = Engine::new();
    let (dir, aof) = test_aof();
    // wrong arg count -> Frame::Error, never reaches the engine
    dispatch_and_log(&engine, &aof, cmd(&[b"SET", b"onlykey"]), &mut Protocol::default(), 1);
    aof.fsync().unwrap();
    assert_eq!(read_aof(&dir), "");
}

#[test]
fn dispatch_and_log_rewrites_spop_to_srem_of_the_actually_popped_member() {
    let engine = Engine::new();
    let (dir, aof) = test_aof();
    dispatch_and_log(&engine, &aof, cmd(&[b"SADD", b"s", b"x"]), &mut Protocol::default(), 1);
    let reply = dispatch_and_log(&engine, &aof, cmd(&[b"SPOP", b"s"]), &mut Protocol::default(), 1);
    assert_eq!(reply, Frame::Bulk(Bytes::from_static(b"x"))); // the popped member
    aof.fsync().unwrap();
    let logged = read_aof(&dir);
    assert!(logged.ends_with("*3\r\n$4\r\nSREM\r\n$1\r\ns\r\n$1\r\nx\r\n"));
    assert!(!logged.contains("SPOP")); // the random command itself never hits the log
}

#[test]
fn dispatch_and_log_does_not_log_spop_on_a_missing_key() {
    let engine = Engine::new();
    let (dir, aof) = test_aof();
    dispatch_and_log(&engine, &aof, cmd(&[b"SPOP", b"missing"]), &mut Protocol::default(), 1);
    aof.fsync().unwrap();
    assert_eq!(read_aof(&dir), ""); // Frame::Null reply — nothing was popped, nothing to log
}

#[test]
fn dispatch_and_log_rewrites_expire_to_an_absolute_pexpireat() {
    let engine = Engine::new();
    let (dir, aof) = test_aof();
    dispatch_and_log(&engine, &aof, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    dispatch_and_log(&engine, &aof, cmd(&[b"EXPIRE", b"k", b"100"]), &mut Protocol::default(), 1);
    aof.fsync().unwrap();
    let logged = read_aof(&dir);
    assert!(logged.contains("PEXPIREAT"));
    assert!(!logged.contains("$6\r\nEXPIRE\r\n")); // the relative form never hits the log
}

#[test]
fn dispatch_and_log_does_not_log_expire_on_a_missing_key() {
    let engine = Engine::new();
    let (dir, aof) = test_aof();
    dispatch_and_log(&engine, &aof, cmd(&[b"EXPIRE", b"missing", b"100"]), &mut Protocol::default(), 1);
    aof.fsync().unwrap();
    assert_eq!(read_aof(&dir), ""); // Frame::Integer(0) reply — nothing changed
}

#[test]
fn dispatch_and_log_rewrites_set_with_ex_into_a_flagless_set_plus_pexpireat() {
    let engine = Engine::new();
    let (dir, aof) = test_aof();
    dispatch_and_log(&engine, &aof, cmd(&[b"SET", b"k", b"v", b"EX", b"100"]), &mut Protocol::default(), 1);
    aof.fsync().unwrap();
    let logged = read_aof(&dir);
    // the SET is logged with EX/100 stripped, followed by an absolute PEXPIREAT
    assert!(logged.starts_with("*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n"));
    assert!(logged.contains("PEXPIREAT"));
    assert!(!logged.contains("$2\r\nEX\r\n")); // the relative form never hits the log
}
```

- [ ] **Step 2: Add the `tempfile` dev-dependency reference**

`04-aof-writer.md`'s Task 1 already added `tempfile = "3"` to `crates/server/Cargo.toml`'s
`[dev-dependencies]` — no further manifest change needed here.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — `dispatch_and_log` is not defined yet

- [ ] **Step 4: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — add below the existing `pub fn dispatch`
/// Wraps `dispatch`, additionally appending successful write commands to `aof`. `dispatch`
/// itself is never modified — see ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md
/// for why AOF logging lives here instead of inside dispatch's own match arms.
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
    let original_frame = frame.clone();
    let reply = dispatch(engine, frame, protocol, client_id);
    if let Frame::Error(_) = reply {
        return reply;
    }

    let Frame::Array(items) = &original_frame else {
        return reply;
    };
    let Some(Frame::Bulk(name_bytes)) = items.first() else {
        return reply;
    };
    let name = String::from_utf8_lossy(name_bytes).to_ascii_uppercase();
    if !crate::aof::WRITE_COMMANDS.contains(&name.as_str()) {
        return reply;
    }

    // A Vec, not an Option: `SET k v EX n` logs as *two* frames (the flagless SET plus an
    // absolute PEXPIREAT), and several cases log none at all.
    let to_log: Vec<Frame> = match name.as_str() {
        "SPOP" => match (&reply, items.get(1)) {
            (Frame::Bulk(member), Some(key)) => vec![Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SREM")),
                key.clone(),
                Frame::Bulk(member.clone()),
            ])],
            _ => Vec::new(), // Frame::Null — nothing was popped
        },
        "EXPIRE" | "PEXPIRE" | "EXPIREAT" | "PEXPIREAT" => match &reply {
            Frame::Integer(1) => rewrite_expire_family_to_pexpireat(items)
                .map(|f| vec![f])
                .unwrap_or_default(),
            _ => Vec::new(), // Frame::Integer(0) — the key didn't exist, nothing changed
        },
        "SET" => match &reply {
            // Simple("OK") means the write applied, so any EX/PX on it needs the same
            // relative→absolute rewrite the EXPIRE family gets. A Null reply is an NX/XX
            // no-op: logging it verbatim is safe (replay re-resolves the condition the same
            // way and applies nothing, TTL included), so it needs no rewrite.
            Frame::Simple(_) => {
                rewrite_set_ttl_to_pexpireat(items).unwrap_or_else(|| vec![original_frame.clone()])
            }
            _ => vec![original_frame.clone()],
        },
        _ => vec![original_frame.clone()],
    };

    for frame_to_log in to_log {
        // a logging failure must not fail the client's reply
        let _ = aof.append(frame_to_log);
        // fsync timing for Always lives inside AofWriter::append itself; EverySecond's
        // periodic fsync loop lives in this plan's Task 2 (connection.rs); Never does
        // nothing here.
    }
    reply
}

/// Rewrites a logged EXPIRE/PEXPIRE/EXPIREAT/PEXPIREAT command's args into an absolute
/// `PEXPIREAT key <unix-ms>`, computed independently via SystemTime (not the Instant already
/// used inside `dispatch`'s own EXPIRE arm) — see
/// ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md's note on this small, accepted
/// duplication.
fn rewrite_expire_family_to_pexpireat(items: &[Frame]) -> Option<Frame> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let Frame::Bulk(name) = items.first()? else {
        return None;
    };
    let Frame::Bulk(key) = items.get(1)? else {
        return None;
    };
    let Frame::Bulk(arg) = items.get(2)? else {
        return None;
    };
    let n: i64 = std::str::from_utf8(arg).ok()?.parse().ok()?;
    let name_upper = String::from_utf8_lossy(name).to_ascii_uppercase();
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_millis() as i64;
    let target_unix_ms = match name_upper.as_str() {
        "EXPIRE" => now_ms + n.saturating_mul(1000),
        "PEXPIRE" => now_ms + n,
        "EXPIREAT" => n.saturating_mul(1000),
        "PEXPIREAT" => n,
        _ => return None,
    };
    Some(Frame::Array(vec![
        Frame::Bulk(Bytes::from_static(b"PEXPIREAT")),
        Frame::Bulk(key.clone()),
        Frame::Bulk(Bytes::from(target_unix_ms.to_string())),
    ]))
}

/// `SET key val EX n` / `PX n` (from 03-expire-family-and-set-ttl-dispatcher.md) carries a
/// *relative* TTL, so logging it verbatim restarts the countdown from replay time — the same
/// drift the EXPIRE family is rewritten to avoid, and the reason a static "everything else is
/// deterministic" rule isn't quite enough for SET. Splits the command into the flagless SET
/// (every other flag, e.g. NX/XX, preserved in place) plus an absolute `PEXPIREAT`.
/// Returns `None` when there was no EX/PX at all — nothing to rewrite, log it verbatim.
fn rewrite_set_ttl_to_pexpireat(items: &[Frame]) -> Option<Vec<Frame>> {
    use std::time::{SystemTime, UNIX_EPOCH};
    if items.len() < 3 {
        return None; // SET k v is the shortest valid form; anything shorter never applied
    }
    let Frame::Bulk(key) = items.get(1)? else {
        return None;
    };
    // items = [SET, key, value, flags...] — only index 3 onward is the flag region.
    let mut kept: Vec<Frame> = items[..3].to_vec();
    let mut ttl_ms: Option<i64> = None;
    let mut i = 3;
    while i < items.len() {
        let Frame::Bulk(raw) = &items[i] else {
            kept.push(items[i].clone());
            i += 1;
            continue;
        };
        let flag = String::from_utf8_lossy(raw).to_ascii_uppercase();
        if flag == "EX" || flag == "PX" {
            let Some(Frame::Bulk(v)) = items.get(i + 1) else {
                return None; // malformed; dispatch already rejected it, so log verbatim
            };
            let n: i64 = std::str::from_utf8(v).ok()?.parse().ok()?;
            ttl_ms = Some(if flag == "EX" { n.saturating_mul(1000) } else { n });
            i += 2; // drop both the flag and its value from the SET that gets logged
        } else {
            kept.push(items[i].clone());
            i += 1;
        }
    }
    let ttl_ms = ttl_ms?;
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_millis() as i64;
    Some(vec![
        Frame::Array(kept),
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"PEXPIREAT")),
            Frame::Bulk(key.clone()),
            Frame::Bulk(Bytes::from((now_ms + ttl_ms).to_string())),
        ]),
    ])
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 8 new ones

- [ ] **Step 6: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 7: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): add dispatch_and_log with SPOP/EXPIRE-family AOF rewrites`.

---

### Task 2: wire `dispatch_and_log` into `connection.rs` and `main.rs`

**Files:**
- Modify: `crates/server/src/connection.rs`
- Modify: `crates/server/src/main.rs`
- Modify: `crates/server/tests/integration.rs` (its `spawn_test_server` helper is a `serve(...)` call site too — see Step 5)

**Interfaces:**
- Consumes: `dispatch_and_log` (Task 1), `AofWriter::open`/`fsync` (from `04-aof-writer.md`), `handle_connection`'s existing pipelining batch logic (the `pending`/`feed`/`now_or_never`/`flush` pattern already in `connection.rs` — a prior perf fix, not something this plan should undo; see Step 3's note).
- Produces: `serve(listener, engine, aof)` — `AofWriter` threaded through as a new, required argument (this changes `serve`'s signature, unlike `dispatch`'s deliberately-unchanged one, because `serve` has only three call sites in the whole workspace: `main.rs`, `connection.rs`'s own tests, and `tests/integration.rs`'s `spawn_test_server` helper — no ~250-call-site ripple to avoid). A periodic `EverySecond`-fsync task is spawned alongside the accept loop and the active-expiry sweep loop from `02-active-expiry-background-task.md`.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/connection.rs — add to the existing tests module
#[tokio::test]
async fn serve_appends_write_commands_to_the_aof() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("test.aof");
    let aof = Arc::new(crate::aof::AofWriter::open(&aof_path, crate::aof::FsyncPolicy::Always).unwrap());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(Engine::new());
    tokio::spawn(serve(listener, engine, aof));

    let mut framed = Framed::new(
        TcpStream::connect(addr).await.unwrap(),
        RespCodec::default(),
    );
    framed
        .send(Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Bulk(Bytes::from_static(b"k")),
            Frame::Bulk(Bytes::from_static(b"v")),
        ]))
        .await
        .unwrap();
    assert_eq!(framed.next().await.unwrap().unwrap(), Frame::Simple("OK".into()));

    // give the (Always-policy, synchronous-fsync) append a moment to land on disk
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let contents = std::fs::read_to_string(&aof_path).unwrap();
    assert_eq!(contents, "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rocket-mem connection::tests::serve_appends_write_commands_to_the_aof`
Expected: FAIL — `serve` doesn't take an `aof` parameter yet, so this doesn't compile

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/connection.rs — replace the existing `pub async fn serve` and
// `async fn handle_connection` functions, and the imports at the top of the file.
//
// handle_connection's body below keeps the pipelining batch logic already in this file
// (peek the next frame with `now_or_never()`; `feed()` without flushing while more frames
// are already buffered from the same read; `flush()` only once nothing more is immediately
// ready) — a prior fix for a real regression where flushing after every single response
// turned client-side pipelining into a slowdown. Only the dispatch call itself changes,
// from `dispatch` to `dispatch_and_log`.
use crate::aof::AofWriter;
use crate::dispatcher;
use engine::Engine;
use futures_util::{FutureExt, SinkExt, StreamExt};
use protocol::codec::{Protocol, RespCodec};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::codec::Framed;

pub async fn serve(listener: TcpListener, engine: Arc<Engine>, aof: Arc<AofWriter>) {
    tokio::spawn(active_expire_loop(Arc::clone(&engine)));
    tokio::spawn(periodic_fsync_loop(Arc::clone(&aof)));

    let mut next_client_id: u64 = 1;
    loop {
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        let client_id = next_client_id;
        next_client_id += 1;
        let engine = Arc::clone(&engine);
        let aof = Arc::clone(&aof);
        tokio::spawn(handle_connection(socket, engine, aof, client_id));
    }
}

async fn handle_connection(
    socket: tokio::net::TcpStream,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    client_id: u64,
) {
    let mut framed = Framed::new(socket, RespCodec::default());
    let mut protocol = Protocol::default();
    // Carries a frame pulled ahead by the pipelining peek below, so it isn't re-read.
    let mut pending: Option<Option<std::io::Result<protocol::Frame>>> = None;
    loop {
        let next = match pending.take() {
            Some(n) => n,
            None => framed.next().await,
        };
        let frame = match next {
            Some(Ok(frame)) => frame,
            Some(Err(_)) | None => return, // malformed input or a dropped connection — end this task quietly
        };
        let response = dispatcher::dispatch_and_log(&engine, &aof, frame, &mut protocol, client_id);
        framed.codec_mut().protocol = protocol; // sync BEFORE sending this reply
        if framed.feed(response).await.is_err() {
            return; // client went away mid-response
        }
        // Peek whether the next request is already buffered from that same read
        // (i.e. genuinely pipelined) without blocking on the network. If so, keep
        // batching via feed(); only flush once nothing more is immediately ready.
        match framed.next().now_or_never() {
            Some(n) => pending = Some(n),
            None => {
                if framed.flush().await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Sweeps one shard per tick, rotating through all 16 — see
/// ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md's active-expiry decision.
async fn active_expire_loop(engine: Arc<Engine>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut shard_idx: usize = 0;
    loop {
        interval.tick().await;
        engine.active_expire_cycle(shard_idx);
        shard_idx = shard_idx.wrapping_add(1);
    }
}

/// `FsyncPolicy::EverySecond`'s periodic fsync — `Always` already fsyncs inline inside
/// `AofWriter::append`, and `Never` relies on the OS, so this loop firing harmlessly for
/// those two policies too (fsync is idempotent and cheap when there's nothing new to flush)
/// keeps this loop unconditional rather than needing to know which policy is active.
async fn periodic_fsync_loop(aof: Arc<AofWriter>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        interval.tick().await;
        let _ = aof.fsync();
    }
}
```

- [ ] **Step 4: Update every existing `serve(...)` call site in this file's own tests**

```rust
// crates/server/src/connection.rs — in the tests module, add near the top
fn test_aof() -> (tempfile::TempDir, Arc<crate::aof::AofWriter>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    let writer = crate::aof::AofWriter::open(&path, crate::aof::FsyncPolicy::Never).unwrap();
    (dir, Arc::new(writer))
}
```

Then update each pre-existing `tokio::spawn(serve(listener, engine));` call in this file's
tests (from Sprint 2's `serve_handles_a_full_set_get_round_trip_over_a_real_socket`,
`serve_handles_two_concurrent_connections_independently`,
`serve_closes_the_connection_cleanly_when_the_client_disconnects`, and
`02-active-expiry-background-task.md`'s `serve_actively_expires_a_key_even_without_any_read_touching_it`)
to:

```rust
let (_dir, aof) = test_aof();
tokio::spawn(serve(listener, engine, aof));
```

(binding the `TempDir` to `_dir` and keeping it alive for the duration of the test — a
`TempDir` deletes its directory on drop, and the server task still needs the file to exist
for as long as it might append to it).

- [ ] **Step 5: Update the `serve(...)` call site in `crates/server/tests/integration.rs`**

`connection.rs`'s tests are *not* the only other caller. `crates/server/tests/integration.rs`
has a `spawn_test_server()` helper with its own `tokio::spawn(rocket_mem::serve(listener,
engine));`, shared by all four of that file's tests — miss it and the whole integration test
target fails to compile. It needs the same treatment, plus returning the `TempDir` so the
caller keeps it alive:

```rust
// crates/server/tests/integration.rs — replace the existing spawn_test_server helper
async fn spawn_test_server() -> (tempfile::TempDir, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().unwrap();
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("test.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    tokio::spawn(rocket_mem::serve(listener, engine, aof));
    (dir, format!("redis://{addr}"))
}
```

Then update each of the four call sites from `let url = spawn_test_server().await;` to
`let (_dir, url) = spawn_test_server().await;` (the `_dir` binding keeps the temp directory
alive for the test's duration, exactly as in Step 4).

- [ ] **Step 6: Update `main.rs`**

```rust
// crates/server/src/main.rs
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::var("ROCKET_MEM_ADDR").unwrap_or_else(|_| "127.0.0.1:6379".to_string());
    let aof_path = std::env::var("ROCKET_MEM_AOF_PATH").unwrap_or_else(|_| "./appendonly.aof".to_string());

    let engine = Arc::new(engine::Engine::new());
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(std::path::Path::new(&aof_path), rocket_mem::aof::FsyncPolicy::EverySecond)
            .expect("failed to open AOF file"),
    );

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("Listening on {}", listener.local_addr()?);
    rocket_mem::serve(listener, engine, aof).await;
    Ok(())
}
```

- [ ] **Step 7: Run the connection and integration test targets to confirm no regressions**

Run: `cargo test -p rocket-mem connection::tests && cargo test -p rocket-mem --test integration`
Expected: PASS — the 3 pre-existing Sprint 2 connection tests, `02`'s active-expiry test, the
1 new AOF test, and all 4 pre-existing `tests/integration.rs` tests, all updated to pass an
`Arc<AofWriter>`

- [ ] **Step 8: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 9: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/connection.rs`, `crates/server/src/main.rs`, and
`crates/server/tests/integration.rs` — do not compose the commit message freeform. Suggested
subject:
`feat(server): wire dispatch_and_log and AOF fsync loop into serve()`.
