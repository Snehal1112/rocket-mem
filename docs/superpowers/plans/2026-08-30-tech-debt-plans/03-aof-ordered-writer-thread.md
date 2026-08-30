# AOF Ordered Writer Thread Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two known AOF gaps together with one mechanism: (1) append order isn't guaranteed to match engine-mutation order under concurrent connections, and (2) `AofWriter`'s file I/O runs synchronously on whatever tokio worker thread happens to be running the connection task.

**Architecture:** `AofWriter` moves its `BufWriter<File>` onto a dedicated OS thread, communicated with over an `std::sync::mpsc` channel — `append()`/`fsync()` become message-sends instead of direct syscalls. Separately, `dispatcher::dispatch_and_log` gains a small ordering `Mutex` (owned by `AofWriter`) held across "mutate the engine, then compute and send what to log" for write commands only, so send order to the channel always matches mutation-commit order. Reads take no lock and stay fully concurrent; only write-command dispatch+logging is serialized relative to other write commands.

**Tech Stack:** Rust, `std::sync::mpsc` and `std::thread` (already-available standard library — no new crate dependencies).

**Spec:** `../../specs/2026-08-30-tech-debt-cleanup-spec.md` (Item 2)

## Global Constraints

- No new dependencies in `crates/server/Cargo.toml` — `std::sync::mpsc`/`std::thread` cover everything needed.
- `AofWriter::append(&self, frame: Frame) -> std::io::Result<()>`, `AofWriter::fsync(&self) -> std::io::Result<()>`, and `AofWriter::policy(&self) -> FsyncPolicy` keep their exact current signatures — every existing caller (`dispatcher.rs`, `connection.rs`, `aof.rs`'s own tests) must compile unchanged.
- Under `FsyncPolicy::Always`, `append()` must still block until the write is durable before returning — this is an explicit, accepted exception to "no blocking I/O on the calling thread," documented in the spec.
- `cargo fmt --all -- --check` and `cargo clippy --workspace -- -D warnings` must both pass before any commit in this plan.

---

### Task 1: `AofWriter` moves its file onto a dedicated writer thread

**Files:**
- Modify: `crates/server/src/aof.rs:1-60` (the `FsyncPolicy` enum stays; `AofWriter` struct and `impl` block are replaced)
- Test: `crates/server/src/aof.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `AofWriter::open`, `append`, `fsync`, `policy` — unchanged signatures (see Global Constraints). New method: `pub fn lock_for_ordering(&self) -> std::sync::MutexGuard<'_, ()>`.
- Consumed by: Task 2 (`dispatcher.rs`), and unchanged by every existing caller in `connection.rs`.

- [ ] **Step 1: Confirm the existing behavioral tests as the baseline**

`crates/server/src/aof.rs` already has `append_writes_the_frame_in_resp_wire_format`, `append_is_cumulative_across_multiple_calls`, `open_on_an_existing_file_appends_rather_than_truncating`, `append_with_always_policy_fsyncs_after_every_write`, and the two `replay_*` tests that call `AofWriter::open`/`append`/`fsync`. These specify the exact external behavior this task must preserve — no new test is needed to pin that behavior down, since it's already pinned. Run them now, before changing anything, to confirm the starting point is green:

Run: `cargo test -p rocket-mem aof::tests`
Expected: PASS (baseline, current implementation).

- [ ] **Step 2: Write the new failing test for the ordering primitive**

Add to the `#[cfg(test)] mod tests` block in `crates/server/src/aof.rs`, after `write_commands_excludes_known_read_only_commands`:

```rust
    #[test]
    fn lock_for_ordering_serializes_concurrent_holders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = std::sync::Arc::new(AofWriter::open(&path, FsyncPolicy::Never).unwrap());
        let log: std::sync::Arc<Mutex<Vec<(usize, bool)>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for id in 0..4 {
            let writer = std::sync::Arc::clone(&writer);
            let log = std::sync::Arc::clone(&log);
            handles.push(std::thread::spawn(move || {
                for _ in 0..50 {
                    let _guard = writer.lock_for_ordering();
                    log.lock().unwrap().push((id, true)); // entered the critical section
                    std::thread::yield_now();
                    log.lock().unwrap().push((id, false)); // about to leave it
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Every "entered" for a given thread must be immediately followed by that same
        // thread's "leaving" -- if lock_for_ordering() didn't provide mutual exclusion,
        // another thread's "entered" could land between them.
        let log = log.lock().unwrap();
        let mut i = 0;
        while i < log.len() {
            let (id, entering) = log[i];
            assert!(entering, "expected an entry at position {i}");
            assert_eq!(
                log[i + 1],
                (id, false),
                "thread {id}'s critical section was interrupted by another holder"
            );
            i += 2;
        }
    }
```

- [ ] **Step 3: Run the test to verify it fails to compile**

Run: `cargo test -p rocket-mem aof::tests::lock_for_ordering_serializes_concurrent_holders`
Expected: compile FAILURE — `AofWriter` has no `lock_for_ordering` method yet.

- [ ] **Step 4: Replace `AofWriter`'s internals**

Replace lines 1-60 of `crates/server/src/aof.rs` (everything from the top of the file through the end of the current `AofWriter` `impl` block, i.e. up to but not including the `/// Commands whose successful execution...` doc comment above `WRITE_COMMANDS`) with:

```rust
use protocol::Frame;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{mpsc, Mutex};
use std::thread;
use tokio_util::codec::Encoder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsyncPolicy {
    Always,
    EverySecond,
    Never,
}

/// Sent to the dedicated writer thread spawned by `AofWriter::open`. `Append` is
/// fire-and-forget (`EverySecond`/`Never`: `append()` must not block on I/O at all).
/// `AppendAndFsync` and `Flush` both carry an ack channel so their caller can block until
/// the writer thread confirms the data is durable -- used for `FsyncPolicy::Always` and for
/// the explicit `fsync()` method, respectively.
enum AofMsg {
    Append(Vec<u8>),
    AppendAndFsync(Vec<u8>, mpsc::SyncSender<()>),
    Flush(mpsc::SyncSender<()>),
}

pub struct AofWriter {
    tx: mpsc::Sender<AofMsg>,
    policy: FsyncPolicy,
    /// Held by `dispatcher::dispatch_and_log` across "mutate the engine, then log it" for
    /// write commands, so concurrent writers' appends always land in the AOF in the same
    /// relative order their mutations committed in. See
    /// ../../docs/superpowers/specs/2026-08-30-tech-debt-cleanup-spec.md Item 2.
    order: Mutex<()>,
}

impl AofWriter {
    pub fn open(path: &Path, policy: FsyncPolicy) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut writer = BufWriter::new(file);
        let (tx, rx) = mpsc::channel::<AofMsg>();

        thread::Builder::new()
            .name("aof-writer".into())
            .spawn(move || {
                for msg in rx {
                    match msg {
                        AofMsg::Append(bytes) => {
                            if let Err(e) = writer.write_all(&bytes) {
                                eprintln!("aof append failed: {e}");
                            }
                        }
                        AofMsg::AppendAndFsync(bytes, ack) => {
                            if let Err(e) = writer
                                .write_all(&bytes)
                                .and_then(|_| writer.flush())
                                .and_then(|_| writer.get_ref().sync_data())
                            {
                                eprintln!("aof append failed: {e}");
                            }
                            let _ = ack.send(());
                        }
                        AofMsg::Flush(ack) => {
                            if let Err(e) =
                                writer.flush().and_then(|_| writer.get_ref().sync_data())
                            {
                                eprintln!("aof fsync failed: {e}");
                            }
                            let _ = ack.send(());
                        }
                    }
                }
            })
            .expect("failed to spawn aof writer thread");

        Ok(Self {
            tx,
            policy,
            order: Mutex::new(()),
        })
    }

    /// Encodes `frame` in RESP wire format and sends it to the dedicated writer thread.
    /// Under `FsyncPolicy::Always` this blocks until the write is fsynced -- matching the
    /// durability contract the caller relies on (the client's reply must not precede
    /// durability). Under `EverySecond`/`Never` it returns as soon as the message is
    /// enqueued, with no blocking I/O on the calling thread.
    pub fn append(&self, frame: Frame) -> std::io::Result<()> {
        let mut buf = bytes::BytesMut::new();
        protocol::codec::RespCodec::default().encode(frame, &mut buf)?;
        let bytes = buf.to_vec();
        if self.policy == FsyncPolicy::Always {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            self.send(AofMsg::AppendAndFsync(bytes, ack_tx))?;
            ack_rx.recv().map_err(writer_gone)
        } else {
            self.send(AofMsg::Append(bytes))
        }
    }

    /// Flushes the buffer and fsyncs the underlying file, blocking until the writer thread
    /// confirms it's done. Called directly by tests, and on a timer by
    /// `FsyncPolicy::EverySecond`'s periodic loop in `connection.rs`.
    pub fn fsync(&self) -> std::io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        self.send(AofMsg::Flush(ack_tx))?;
        ack_rx.recv().map_err(writer_gone)
    }

    /// The fsync policy this writer was opened with. Never changes after `open`.
    pub fn policy(&self) -> FsyncPolicy {
        self.policy
    }

    /// Acquired by `dispatcher::dispatch_and_log` around "mutate, then log" for write
    /// commands -- see the `order` field's doc comment above.
    pub fn lock_for_ordering(&self) -> std::sync::MutexGuard<'_, ()> {
        self.order.lock().unwrap()
    }

    fn send(&self, msg: AofMsg) -> std::io::Result<()> {
        self.tx.send(msg).map_err(|_| writer_gone_err())
    }
}

fn writer_gone<E>(_: E) -> std::io::Error {
    writer_gone_err()
}

fn writer_gone_err() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "aof writer thread is gone")
}
```

- [ ] **Step 5: Run the AOF test module**

Run: `cargo test -p rocket-mem aof::tests`
Expected: PASS — every test from Step 1's baseline plus the new `lock_for_ordering_serializes_concurrent_holders` from Step 2.

- [ ] **Step 6: Run the full workspace test suite and lints**

Run: `cargo test --workspace && cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings`
Expected: all green. (`connection.rs`'s AOF-related tests exercise `AofWriter` only through `append`/`fsync`/`policy`, all unchanged signatures, so they need no edits.)

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/aof.rs
git commit -m "feat(server): move AofWriter's file I/O onto a dedicated writer thread"
```

---

### Task 2: `dispatch_and_log` holds the ordering lock across mutate-and-log for write commands

**Files:**
- Modify: `crates/server/src/dispatcher.rs:930-996` (`dispatch_and_log`)

**Interfaces:**
- Consumes: `AofWriter::lock_for_ordering(&self) -> std::sync::MutexGuard<'_, ()>` from Task 1.
- Produces: no change to `dispatch_and_log`'s signature or its RESP-visible behavior — every existing test of it must keep passing unmodified.

- [ ] **Step 1: Confirm the existing tests as the baseline**

`dispatch_and_log_appends_a_write_command_verbatim`, `dispatch_and_log_does_not_log_a_read_only_command`, `dispatch_and_log_does_not_log_a_write_command_that_errored`, and `dispatch_and_log_rewrites_spop_to_srem_of_the_actually_popped_member` (around line 3291-3360) already pin down the exact behavior this task must preserve.

Run: `cargo test -p rocket-mem dispatcher::tests::dispatch_and_log`
Expected: PASS (baseline).

- [ ] **Step 2: Replace `dispatch_and_log`'s command-name extraction**

In `crates/server/src/dispatcher.rs`, replace the function's opening (from `pub fn dispatch_and_log(` through the `if !crate::aof::WRITE_COMMANDS.contains(&name.as_str()) { return reply; }` line) with:

```rust
/// Returns the uppercased command name from `frame` if it's one of `aof::WRITE_COMMANDS`,
/// else `None`. Computed before `dispatch` runs, so `dispatch_and_log` knows whether to hold
/// the AOF ordering lock without first having to inspect `reply`.
fn extract_write_command_name(frame: &Frame) -> Option<String> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name_bytes)) = items.first() else {
        return None;
    };
    let name = String::from_utf8_lossy(name_bytes).to_ascii_uppercase();
    crate::aof::WRITE_COMMANDS
        .contains(&name.as_str())
        .then_some(name)
}

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
    let write_name = extract_write_command_name(&original_frame);

    // Held across "mutate the engine, then log it" for write commands only, so two
    // concurrent connections' AOF appends always land in the order their mutations
    // committed in. Reads take no lock and stay fully concurrent. See
    // ../../docs/superpowers/specs/2026-08-30-tech-debt-cleanup-spec.md Item 2.
    let _order_guard = write_name.as_ref().map(|_| aof.lock_for_ordering());

    let reply = dispatch(engine, frame, protocol, client_id);
    if let Frame::Error(_) = reply {
        return reply;
    }
    let Some(name) = write_name else {
        return reply;
    };
    let Frame::Array(items) = &original_frame else {
        return reply;
    };
```

Everything after that (the `// A Vec, not an Option: ...` comment through the end of the function, computing `to_log` and looping `aof.append(...)`) is unchanged — it already refers to `name`, `reply`, and `items`, all of which are still in scope with the same types under the new structure.

- [ ] **Step 3: Run the dispatch_and_log tests**

Run: `cargo test -p rocket-mem dispatcher::tests::dispatch_and_log`
Expected: PASS, unchanged from baseline.

- [ ] **Step 4: Run the full workspace test suite and lints**

Run: `cargo test --workspace && cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "fix(server): serialize write-command dispatch+AOF logging so append order matches mutation order"
```
