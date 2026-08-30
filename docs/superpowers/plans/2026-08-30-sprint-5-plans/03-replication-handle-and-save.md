# ReplicationHandle & SAVE Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a `ReplicationHandle` carries leader/follower state through `dispatch_and_log` without touching plain `dispatch`'s signature, and `SAVE` writes an atomic, crash-safe snapshot to `ROCKET_MEM_SNAPSHOT_PATH`.

**Architecture:** a new `crates/server/src/replication.rs` module defines `ReplicaRegistry` (a skeleton this plan only needs to compile — `04-replica-registry-and-leader-fanout.md` gives it `register`/`broadcast`) and the full `ReplicationHandle` struct. `dispatch_and_log`, `serve`, and `handle_connection` each gain one new parameter. `SAVE` is intercepted inside `dispatch_and_log`, before it would otherwise delegate to `dispatch`, exactly like `04`'s later `PSYNC`/`REPLICAOF` interceptions will be.

**Tech Stack:** `tokio::sync`, `tokio::task::JoinHandle` (both already workspace dependencies via `tokio`).

**Spec:** `../../specs/2026-08-30-sprint-5-spec.md` — "one `ReplicationHandle` struct threads leader/follower replication state..." and "`SAVE` is a blocking P0 command..." are authoritative for this plan. Depends on `01-snapshot-serialization.md` (`Engine::snapshot`) and `02-hybrid-recovery-and-aof-offset.md` (`AofWriter::current_offset`).

## Global Constraints

- `ROCKET_MEM_SNAPSHOT_PATH` is read once in `main.rs`, default `./dump.snapshot` — the same variable `02-hybrid-recovery-and-aof-offset.md`'s `recover` call already reads; this plan reuses that same `main.rs` local rather than re-reading the env var.
- `SAVE` is never added to `crate::aof::WRITE_COMMANDS` — it has nothing for the AOF to log, and must remain reachable regardless of replica role.
- Any test that issues `SAVE` must construct its `ReplicationHandle` with `ReplicationHandle::new(..., a tempfile::tempdir() path)`, never `ReplicationHandle::default()` — the default's snapshot path is `./dump.snapshot` in the process's actual working directory, and a `SAVE` test using it would litter the repo root.

---

### Task 1: `replication.rs` — `ReplicaRegistry` skeleton and `ReplicationHandle`

**Files:**
- Create: `crates/server/src/replication.rs`
- Modify: `crates/server/src/lib.rs`

**Interfaces:**
- Consumes: `engine::Engine` (existing).
- Produces: `ReplicaRegistry` (skeleton — no `register`/`broadcast` yet, added in `04-replica-registry-and-leader-fanout.md`), `ReplicationHandle` with `pub registry: ReplicaRegistry`, `pub is_replica: AtomicBool`, `ReplicationHandle::new(engine: Arc<Engine>, snapshot_path: PathBuf) -> Self`, `ReplicationHandle::engine(&self) -> &Arc<Engine>`, `ReplicationHandle::snapshot_path(&self) -> &Path`, and `impl Default for ReplicationHandle`. All consumed by Task 2 below and by `04`/`05`.

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/src/replication.rs — new file, tests module at the bottom
#[cfg(test)]
mod tests {
    use super::*;
    use engine::Engine;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    #[test]
    fn new_starts_as_not_a_replica() {
        let h = ReplicationHandle::new(Arc::new(Engine::new()), "/tmp/does-not-matter".into());
        assert!(!h.is_replica.load(Ordering::Relaxed));
    }

    #[test]
    fn engine_and_snapshot_path_return_what_new_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.snapshot");
        let engine = Arc::new(Engine::new());
        engine.set(bytes::Bytes::from_static(b"k"), engine::Value::String(bytes::Bytes::from_static(b"v")));
        let h = ReplicationHandle::new(Arc::clone(&engine), path.clone());
        assert_eq!(h.snapshot_path(), path.as_path());
        assert_eq!(h.engine().get(b"k"), Some(engine::Value::String(bytes::Bytes::from_static(b"v"))));
    }

    #[test]
    fn default_is_idle_with_no_replicas_and_is_not_a_replica() {
        let h = ReplicationHandle::default();
        assert!(!h.is_replica.load(Ordering::Relaxed));
    }
}
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem replication::tests`
Expected: FAIL with "module `replication` doesn't exist" — the next step creates the file, and `lib.rs` gets updated after, matching Task 5's ordering pattern from `01-snapshot-serialization.md`. Temporarily add `pub mod replication;` to `crates/server/src/lib.rs` so this file compiles in isolation for this step; Step 3 below makes that permanent.

- [x] **Step 3: Implement `replication.rs`**

```rust
// crates/server/src/replication.rs
use engine::Engine;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// A skeleton for now — holds nothing yet. `04-replica-registry-and-leader-fanout.md` adds the
/// actual `Mutex<Vec<mpsc::UnboundedSender<Bytes>>>` field plus `register`/`broadcast`. It
/// exists this early only so `ReplicationHandle` below has something concrete to name.
#[derive(Default)]
pub struct ReplicaRegistry {}

/// Threads leader/follower replication state through `dispatch_and_log` without adding a
/// parameter to plain `dispatch` — see the sprint-5 spec's `ReplicationHandle` decision for
/// why `dispatch`'s ~250 call sites must stay untouched.
pub struct ReplicationHandle {
    /// Leader side: connected replicas to fan writes out to. Empty until
    /// `04-replica-registry-and-leader-fanout.md` gives `ReplicaRegistry` a `register` method
    /// for `PSYNC` handling to call.
    pub registry: ReplicaRegistry,
    /// Follower side: gates client-originated writes once this node is replicating from a
    /// leader. Read by `dispatch_and_log`'s `-READONLY` check, added in
    /// `05-replicaof-and-follower-apply-loop.md`. A plain field, not `Arc<AtomicBool>`: the
    /// whole handle is already behind one `Arc` wherever it's shared, so a second layer of
    /// sharing buys nothing.
    pub is_replica: AtomicBool,
    /// Follower side: the running `replication_client_loop`, if any — set and aborted by
    /// `REPLICAOF`/`REPLICAOF NO ONE` in `05-replicaof-and-follower-apply-loop.md`. Not used
    /// by this plan; present now so the struct's shape doesn't change again later.
    follower_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The engine this handle's leader-side `PSYNC` snapshots from and follower-side
    /// `replication_client_loop` applies into. An owned `Arc`, not a borrow: a spawned
    /// follower task is `'static` and cannot hold a borrow of `dispatch_and_log`'s own
    /// `engine: &Engine` parameter. Invariant, enforced only by convention (there is no way to
    /// assert it in the type system): this must be the *same* `Engine` `serve()` was handed.
    engine: Arc<Engine>,
    /// Where `SAVE` writes, from `ROCKET_MEM_SNAPSHOT_PATH`.
    snapshot_path: PathBuf,
}

impl ReplicationHandle {
    pub fn new(engine: Arc<Engine>, snapshot_path: PathBuf) -> Self {
        Self {
            registry: ReplicaRegistry::default(),
            is_replica: AtomicBool::new(false),
            follower_task: Mutex::new(None),
            engine,
            snapshot_path,
        }
    }

    /// For `SAVE` and (later) `PSYNC` handling, which need the shared `Engine` to snapshot
    /// from, and for the follower apply loop, which needs it to apply replicated frames into.
    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// For `SAVE`, which needs to know where to write.
    pub fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }
}

/// An idle handle: no replicas registered, not a replica, no follower task running, its own
/// throwaway `Engine`, and the `./dump.snapshot` default path relative to the process's
/// current directory. Exists only so `dispatch_and_log`'s and `serve`'s existing tests stay
/// one-liners. Any test that actually exercises `SAVE` or `REPLICAOF` must use `new` instead
/// with an explicit `tempfile::tempdir()` path — see this plan's Global Constraints.
impl Default for ReplicationHandle {
    fn default() -> Self {
        Self::new(Arc::new(Engine::new()), PathBuf::from("./dump.snapshot"))
    }
}
```

Then in `crates/server/src/lib.rs`, replace the temporary line from Step 2 with the permanent module declaration:

```rust
// crates/server/src/lib.rs
pub mod aof;
pub mod connection;
pub mod dispatcher;
pub mod replication;
pub use connection::serve;
```

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem replication::tests`
Expected: PASS

- [x] **Step 5: Commit**

```bash
git add crates/server/src/replication.rs crates/server/src/lib.rs
git commit -m "feat(server): add ReplicationHandle and a ReplicaRegistry skeleton"
```

---

### Task 2: thread `ReplicationHandle` through `serve`/`handle_connection`/`dispatch_and_log`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`
- Modify: `crates/server/src/connection.rs`
- Modify: `crates/server/src/main.rs`
- Modify: `crates/server/tests/integration.rs`

**Interfaces:**
- Consumes: `ReplicationHandle` (Task 1).
- Produces: `dispatch_and_log(engine: &Engine, aof: &AofWriter, replication: &ReplicationHandle, frame: Frame, protocol: &mut Protocol, client_id: u64) -> Frame`, `serve(listener: TcpListener, engine: Arc<Engine>, aof: Arc<AofWriter>, replication: Arc<ReplicationHandle>)` — both signature changes, consumed by every later plan in this sprint and by Task 3 below.

- [x] **Step 1: Update `dispatch_and_log`'s signature and every call site**

In `crates/server/src/dispatcher.rs`, add the new parameter (the function body is otherwise untouched by this step — Task 3 adds the `SAVE` check):

```rust
// crates/server/src/dispatcher.rs
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
```

Every one of the 19 existing `dispatch_and_log(...)` calls in `crates/server/src/dispatcher.rs`'s tests module needs a new `&ReplicationHandle::default()` argument inserted as the third positional argument (right after `&aof`, right before the frame). Find every call site with `grep -n "dispatch_and_log(" crates/server/src/dispatcher.rs` and update each — for example:

```rust
// before
let reply = dispatch_and_log(
    &engine,
    &aof,
    cmd(&[b"SET", b"k", b"v"]),
    &mut Protocol::default(),
    1,
);

// after
let reply = dispatch_and_log(
    &engine,
    &aof,
    &ReplicationHandle::default(),
    cmd(&[b"SET", b"k", b"v"]),
    &mut Protocol::default(),
    1,
);
```

Add `use crate::replication::ReplicationHandle;` to `dispatcher.rs`'s `#[cfg(test)] mod tests` imports (alongside its existing `use super::*;` and other test-only imports) so the unqualified name resolves.

- [x] **Step 2: Update `serve`/`handle_connection` and their call sites**

In `crates/server/src/connection.rs`:

```rust
// crates/server/src/connection.rs
use crate::replication::ReplicationHandle;
// (add to the existing `use` block at the top of the file)

pub async fn serve(
    listener: TcpListener,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
) {
    tokio::spawn(active_expire_loop(Arc::clone(&engine)));
    tokio::spawn(periodic_fsync_loop(Arc::clone(&aof)));

    let mut next_client_id: u64 = 1;
    loop {
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let client_id = next_client_id;
        next_client_id += 1;
        let engine = Arc::clone(&engine);
        let aof = Arc::clone(&aof);
        let replication = Arc::clone(&replication);
        tokio::spawn(handle_connection(socket, engine, aof, replication, client_id));
    }
}
```

```rust
// crates/server/src/connection.rs
async fn handle_connection(
    socket: tokio::net::TcpStream,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) {
    let mut framed = Framed::new(socket, RespCodec::default());
    let mut protocol = Protocol::default();
    let mut pending: Option<Option<std::io::Result<protocol::Frame>>> = None;
    loop {
        let next = match pending.take() {
            Some(n) => n,
            None => framed.next().await,
        };
        let frame = match next {
            Some(Ok(frame)) => frame,
            Some(Err(_)) | None => return,
        };
        let response =
            dispatcher::dispatch_and_log(&engine, &aof, &replication, frame, &mut protocol, client_id);
        framed.codec_mut().protocol = protocol;
        if framed.feed(response).await.is_err() {
            return;
        }
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
```

`connection.rs`'s 5 existing tests each call `tokio::spawn(serve(listener, engine, aof))` — add `Arc::new(crate::replication::ReplicationHandle::default())` as a fourth positional argument to each, for example:

```rust
// before
tokio::spawn(serve(listener, engine, aof));

// after
tokio::spawn(serve(listener, engine, aof, Arc::new(crate::replication::ReplicationHandle::default())));
```

- [x] **Step 3: Update `main.rs` and `tests/integration.rs`'s one `serve` call site**

```rust
// crates/server/src/main.rs — insert before the existing `let listener = ...` line
let replication = Arc::new(rocket_mem::replication::ReplicationHandle::new(
    Arc::clone(&engine),
    snapshot_path.to_path_buf(),
));
```

Then update the final `serve` call:

```rust
// crates/server/src/main.rs
rocket_mem::serve(listener, engine, aof, replication).await;
```

In `crates/server/tests/integration.rs`, `spawn_test_server`'s one `tokio::spawn(rocket_mem::serve(listener, engine, aof));` becomes:

```rust
// crates/server/tests/integration.rs
tokio::spawn(rocket_mem::serve(
    listener,
    engine,
    aof,
    Arc::new(rocket_mem::replication::ReplicationHandle::default()),
));
```

- [x] **Step 4: Run the full test suite to verify every call site compiles and passes**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS — this is a pure signature-threading change, so no test's assertions should need to change, only its call site

- [x] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs crates/server/src/connection.rs crates/server/src/main.rs crates/server/tests/integration.rs
git commit -m "refactor(server): thread ReplicationHandle through dispatch_and_log/serve"
```

---

### Task 3: `SAVE` interception

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `AofWriter::lock_for_ordering`/`current_offset` (existing/`02`), `ReplicationHandle::engine`/`snapshot_path` (Task 1), `Engine::snapshot` (`01`).
- Produces: `SAVE` as a working command, reachable over RESP; nothing new consumed by later tasks in this plan (Task 3 is this plan's last).

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn save_writes_a_snapshot_that_load_snapshot_can_read_back() {
    let engine = std::sync::Arc::new(Engine::new());
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    let (dir, aof) = test_aof();
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

    let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SAVE"]), &mut Protocol::default(), 1);
    assert_eq!(reply, Frame::Simple("OK".into()));

    let bytes = std::fs::read(&snapshot_path).unwrap();
    let loaded = Engine::new();
    loaded.load_snapshot(&bytes).unwrap();
    assert_eq!(loaded.get(b"k"), Some(Value::String(Bytes::from_static(b"v"))));
}

#[test]
fn save_does_not_leave_a_tmp_file_behind_on_success() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

    dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SAVE"]), &mut Protocol::default(), 1);

    let mut tmp = snapshot_path.clone().into_os_string();
    tmp.push(".tmp");
    assert!(!std::path::Path::new(&tmp).exists());
}

#[test]
fn save_is_not_appended_to_the_aof() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path);

    dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SAVE"]), &mut Protocol::default(), 1);
    aof.fsync().unwrap();
    assert_eq!(read_aof(&dir), ""); // SAVE has nothing to replay -- it must not appear in the AOF
}
```

Note `dispatch_and_log`'s first parameter is declared `engine: &Engine`; passing `&engine` where `engine: Arc<Engine>` works via deref coercion (`&Arc<Engine>` coerces to `&Engine` at a call site expecting the latter), so no `.as_ref()`/`&*engine` is needed.

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::save`
Expected: FAIL — `SAVE` currently falls through to `dispatch`'s unknown-command error path, so the reply won't be `Frame::Simple("OK")` and no snapshot file gets written

- [x] **Step 3: Implement the interception**

Add near the top of `dispatch_and_log`'s body, before the existing `let original_frame = frame.clone();` line:

```rust
// crates/server/src/dispatcher.rs
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
    if is_save_command(&frame) {
        return handle_save(aof, replication);
    }

    let original_frame = frame.clone();
    // ... rest of the existing body, completely unchanged
```

Then add these three new private functions near `extract_write_command_name` (they're all part of the same "recognize a command before `dispatch` sees it" family):

```rust
// crates/server/src/dispatcher.rs
fn is_save_command(frame: &Frame) -> bool {
    let Frame::Array(items) = frame else { return false };
    let Some(Frame::Bulk(name)) = items.first() else { return false };
    name.eq_ignore_ascii_case(b"SAVE")
}

/// Snapshots `replication.engine()` — in production this is always the same `Arc<Engine>` as
/// `dispatch_and_log`'s own `engine` parameter (`main.rs` constructs one `Engine`, shares it
/// into both `serve`'s `engine` argument and `ReplicationHandle::new`), so using the handle's
/// copy here matches the pattern `04-replica-registry-and-leader-fanout.md`'s `PSYNC` handling
/// already uses (`replication.engine().snapshot(0)`) instead of introducing a second,
/// redundant `&Engine` parameter that would always alias it anyway — and writes the result to
/// `replication.snapshot_path()`.
///
/// Holds `aof.lock_for_ordering()` across the offset read and the snapshot walk/encode (never
/// across the disk write) — see the sprint-5 spec's SAVE atomicity decision for why: without
/// this, a write landing between `current_offset()` and the snapshot walk would be captured in
/// both the snapshot and the AOF tail after the recorded offset, double-applying on a future
/// hybrid recovery for any non-idempotent command like `RPUSH`.
fn handle_save(
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> Frame {
    let bytes = {
        let _order_guard = aof.lock_for_ordering();
        let offset = match aof.current_offset() {
            Ok(o) => o,
            Err(e) => return Frame::Error(format!("ERR failed to read AOF offset: {e}")),
        };
        replication.engine().snapshot(offset)
    };

    match write_snapshot_atomically(replication.snapshot_path(), &bytes) {
        Ok(()) => Frame::Simple("OK".into()),
        Err(e) => Frame::Error(format!("ERR failed to write snapshot: {e}")),
    }
}

/// Writes `bytes` to `<path>.tmp`, `sync_data`s it, then atomically renames it over `path`.
/// Without this, a crash partway through a direct write to `path` leaves a truncated file at
/// exactly the location startup will try to load next boot — `aof::recover` treats an
/// unreadable snapshot as a safe fallback to full AOF replay, so this never corrupts recovery,
/// but silently losing every snapshot on a crash defeats the feature's point.
fn write_snapshot_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp_path = std::path::PathBuf::from(tmp_os);
    {
        let file = std::fs::File::create(&tmp_path)?;
        let mut writer = std::io::BufWriter::new(file);
        writer.write_all(bytes)?;
        writer.flush()?;
        writer.get_ref().sync_data()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}
```

Add `use crate::replication::ReplicationHandle;` to the non-test imports at the top of `dispatcher.rs` if `ReplicationHandle` isn't already reachable there as a fully-qualified path (Task 2 used `crate::replication::ReplicationHandle` inline in the signature, so this is optional — keep whichever style Task 2 already settled on for consistency within the file).

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests::save`
Expected: PASS

- [x] **Step 5: Run the full workspace verification**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean/green

- [x] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): add SAVE with atomic write-then-rename"
```
