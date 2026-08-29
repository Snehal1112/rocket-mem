# AOF Replay & Corrupt-Tail Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** on startup, replay every command an AOF file holds back into a fresh `Engine`; a truncated/corrupt final frame (the file mid-write when the process died) is recovered from — not panicked on — and the corrupt bytes are removed from the file on disk, not just skipped in memory.

**Architecture:** `aof::replay(path, engine)` decodes frames from the file with the same `RespCodec` the network path uses, calling the plain `dispatcher::dispatch` (never `dispatch_and_log` — replay must not re-append what it's replaying) against each one. If decoding stops before reaching the end of the file (a corrupt or incomplete final frame), the file is truncated on disk to the last successfully-decoded byte offset, so a *second* replay (or a fresh `AofWriter::open` in append mode right after) never has to re-skip the same garbage bytes — see this plan's Task 1 for why skip-in-memory alone isn't enough.

**Tech Stack:** `tokio_util::codec::Decoder` (existing dependency, already used by `RespCodec`'s network-side decoding).

**Spec:** `../../specs/2026-08-30-sprint-4-spec.md` — "replay calls `dispatch`, never `dispatch_and_log`" is authoritative.

**Depends on:** `04-aof-writer.md` for Task 1 (the RESP-encoded file format `AofWriter::append` produces is what this plan decodes) — **and `05-aof-dispatch-wiring.md` for Task 2**, whose `main.rs` rewrite below calls the 3-argument `serve(listener, engine, aof)` that `05` introduces. Task 1 alone only needs `04`; don't start Task 2 before `05` has landed.

## Global Constraints

- Replay must never panic on a truncated or corrupted final frame — the whole point of this plan is that a `kill -9` mid-write doesn't lose everything written before it.
- A corrupt/incomplete tail must be truncated from the **file on disk**, not merely skipped while decoding in memory — otherwise every future replay has to re-skip the same bytes, and worse, any *new* valid data appended after them (via `AofWriter::open`'s append mode) would sit *after* garbage bytes that a future replay might misinterpret as still being mid-frame, silently losing everything appended since.

---

### Task 1: `aof::replay` with truncate-on-corrupt-tail

**Files:**
- Modify: `crates/server/src/aof.rs`

**Interfaces:**
- Consumes: `protocol::codec::RespCodec` (existing), `dispatcher::dispatch` (existing, unmodified).
- Produces: `pub fn replay(path: &std::path::Path, engine: &engine::Engine) -> std::io::Result<()>`. `05-aof-dispatch-wiring.md`'s `AofWriter` is not consumed here — replay reads the file directly, independent of any open `AofWriter` handle. `Task 2` of this plan consumes `replay` from `main.rs`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/aof.rs — add to the existing tests module
use engine::{Engine, Value};

fn write_raw(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

#[test]
fn replay_on_a_missing_file_is_a_no_op_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does-not-exist.aof");
    let engine = Engine::new();
    replay(&path, &engine).unwrap();
    assert!(engine.keys().is_empty());
}

#[test]
fn replay_reconstructs_state_from_a_well_formed_aof() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    write_raw(
        &path,
        b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n",
    );
    let engine = Engine::new();
    replay(&path, &engine).unwrap();
    assert_eq!(
        engine.get(b"a"),
        Some(Value::String(bytes::Bytes::from_static(b"1")))
    );
    assert_eq!(
        engine.get(b"b"),
        Some(Value::String(bytes::Bytes::from_static(b"2")))
    );
}

#[test]
fn replay_recovers_every_valid_command_before_a_corrupt_tail_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");
    write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$3\r\ngar"); // truncated mid-bulk-body
    let engine = Engine::new();
    replay(&path, &engine).unwrap(); // must not panic
    assert_eq!(
        engine.get(b"a"),
        Some(Value::String(bytes::Bytes::from_static(b"1")))
    );
    assert_eq!(engine.get(b"b"), None); // the truncated command never applied
}

#[test]
fn replay_truncates_the_corrupt_tail_off_the_file_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    let valid = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
    write_raw(&path, valid);
    write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$3\r\ngar"); // truncated mid-bulk-body
    let engine = Engine::new();
    replay(&path, &engine).unwrap();

    let on_disk = std::fs::read(&path).unwrap();
    assert_eq!(on_disk, valid); // corrupt bytes physically removed, not just skipped in memory

    // proves future appends land cleanly right after the last valid frame, not after garbage
    let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
    writer
        .append(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"SET")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"c")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"3")),
        ]))
        .unwrap();
    writer.fsync().unwrap();
    let engine2 = Engine::new();
    replay(&path, &engine2).unwrap();
    assert_eq!(
        engine2.get(b"a"),
        Some(Value::String(bytes::Bytes::from_static(b"1")))
    );
    assert_eq!(
        engine2.get(b"c"),
        Some(Value::String(bytes::Bytes::from_static(b"3")))
    );
}

#[test]
fn replay_on_a_fully_well_formed_file_does_not_truncate_anything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    let valid = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
    write_raw(&path, valid);
    let engine = Engine::new();
    replay(&path, &engine).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), valid);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem aof::tests`
Expected: FAIL — `replay` is not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/aof.rs — add below the existing `impl AofWriter` block
/// Replays every command in the AOF at `path` against `engine`, via the plain (non-logging)
/// `dispatcher::dispatch` — never `dispatch_and_log`, which would re-append what's being
/// replayed. A missing file is a no-op (nothing to recover on first run). A corrupt or
/// incomplete final frame stops replay at the last fully-decoded frame and truncates the
/// file on disk to that exact byte offset — see this plan's Global Constraints for why an
/// in-memory-only skip isn't sufficient.
pub fn replay(path: &Path, engine: &engine::Engine) -> std::io::Result<()> {
    use tokio_util::codec::Decoder;

    let raw = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let mut buf = bytes::BytesMut::from(&raw[..]);
    let mut codec = protocol::codec::RespCodec::default();
    let mut valid_len = 0usize;
    loop {
        let before = buf.len();
        match codec.decode(&mut buf) {
            Ok(Some(frame)) => {
                valid_len += before - buf.len();
                let mut protocol = protocol::codec::Protocol::default();
                crate::dispatcher::dispatch(engine, frame, &mut protocol, 0);
            }
            Ok(None) | Err(_) => break, // incomplete or corrupt tail — stop here, keep what decoded
        }
    }

    if valid_len < raw.len() {
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_len(valid_len as u64)?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem aof::tests`
Expected: PASS, all tests including the 5 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/aof.rs` — do not compose the commit message freeform. Suggested subject:
`feat(server): add AOF replay with corrupt-tail truncation`.

---

### Task 2: replay on startup, before opening the AOF writer

**Files:**
- Modify: `crates/server/src/main.rs`

**Interfaces:**
- Consumes: `aof::replay` (Task 1), `serve(listener, engine, aof)` (from `05-aof-dispatch-wiring.md`'s Task 2).
- Produces: `main` now replays any existing AOF into the engine before opening the listener or the `AofWriter` for further appends.

- [ ] **Step 1: Write the implementation**

```rust
// crates/server/src/main.rs — replace the existing main() body
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::var("ROCKET_MEM_ADDR").unwrap_or_else(|_| "127.0.0.1:6379".to_string());
    let aof_path = std::env::var("ROCKET_MEM_AOF_PATH").unwrap_or_else(|_| "./appendonly.aof".to_string());
    let aof_path = std::path::Path::new(&aof_path);

    let engine = Arc::new(engine::Engine::new());
    rocket_mem::aof::replay(aof_path, &engine)?;
    println!("Replayed AOF from {}", aof_path.display());

    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(aof_path, rocket_mem::aof::FsyncPolicy::EverySecond)
            .expect("failed to open AOF file"),
    );

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("Listening on {}", listener.local_addr()?);
    rocket_mem::serve(listener, engine, aof).await;
    Ok(())
}
```

This supersedes `05-aof-dispatch-wiring.md`'s Task 2 Step 5 version of `main.rs` (which opened
the `AofWriter` without first replaying) — if executing these plans in order, this is the
version that ships; there's no need to apply both.

- [ ] **Step 2: Manually verify end-to-end**

Run:
```bash
cargo build --workspace
ROCKET_MEM_ADDR=127.0.0.1:16399 ROCKET_MEM_AOF_PATH=/tmp/manual-test.aof ./target/debug/rocket-mem &
redis-cli -p 16399 SET foo bar
redis-cli -p 16399 GET foo
kill %1
ROCKET_MEM_ADDR=127.0.0.1:16399 ROCKET_MEM_AOF_PATH=/tmp/manual-test.aof ./target/debug/rocket-mem &
redis-cli -p 16399 GET foo   # expect "bar" — survived the restart
kill %1
rm -f /tmp/manual-test.aof
```
Expected: the second `GET foo` returns `bar`, proving the write survived a stop/restart cycle
(this is a manual sanity check, not the automated proof — `08-kill-and-recover-tests.md`
covers the automated `kill -9` case)

- [ ] **Step 3: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 4: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/main.rs` — do not compose the commit message freeform. Suggested subject:
`feat(server): replay the AOF on startup before serving`.
