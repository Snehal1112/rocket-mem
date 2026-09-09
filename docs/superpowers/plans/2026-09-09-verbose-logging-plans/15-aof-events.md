# Verbose Logging Plan 15: AOF Events

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Instrument `crates/server/src/aof.rs` per the spec's AOF event catalogue row: append `offset`/`bytes` at `trace`, fsync at `debug`, `BGREWRITEAOF` rewrite start/finish with generation + size at `info`, and a startup recovery replay summary (commands, bytes, duration) at `info`.

**Architecture:** Two of these four events sit on the write hot path (append, fsync) and two are one-off milestones (rewrite, recovery). The hot-path pair gets its own task and its own benchmark gate; the milestone pair shares a task because both are low-frequency, low-risk additions to already-tested control flow. The append/fsync trace and debug events are emitted from inside `AofWriter`'s dedicated writer thread, not from `append_encoded`/`fsync` on the calling side — the writer thread already processes every message strictly sequentially, so a plain (non-atomic) `u64` offset counter local to that thread's closure tracks the running file position with zero cross-thread synchronization cost and without adding the new atomic counter the Global Constraints forbid. The calling side of `append_encoded`/`fsync` is untouched, which is what keeps this change off the path the benchmark gate actually measures on the client-request side.

**Tech Stack:** Rust 2021, `tracing 0.1`, `crates/server/src/aof.rs`, `crates/server/src/dispatcher.rs`, `scripts/benchmark.sh`.

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the Event catalogue's AOF row.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting. The load-bearing ones here:

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` must all pass, with every pre-existing test passing **unchanged**.
- Throughput at the default `info` level must stay within **2%** of the baseline in `docs/benchmarks/2026-09-09-pre-logging-baseline.md`.
- No `format!` outside a log macro's argument list.
- All log fields use the `%` (Display) or `?` (Debug) sigil so formatting is lazy. (Plain integers such as `u64` offsets and byte counts implement `tracing::Value` natively with no formatting step at all, so they carry no sigil — the sigil rule exists to stop an eager `Display`/`Debug` render, and there is nothing to render for a number regardless of whether the level is enabled.)
- `Bytes` is never logged via `Debug` on the hot path — it renders byte-by-byte.
- No new atomic counters on the hot path.
- Redaction policy lives only in `crates/server`. Nothing in this plan logs value contents — only offsets, byte counts, generations, and durations — so no redaction is needed here.

---

### Task 1: Append trace + fsync debug on the AOF writer thread

**Files:**
- Modify: `crates/server/src/aof.rs` (`AofWriter::open_with_base`, lines 159–226 — the writer-thread spawn and its `AofMsg` match arms)

**Interfaces:**
- Consumes: `tracing::trace!`/`tracing::debug!` (already in scope via the crate's existing `tracing::error!` calls at line 173).
- Produces: a `trace`-level `"aof append"` event with `offset`/`bytes` fields on every successful write, and a `debug`-level `"aof fsync"` event with an `offset` field on every successful fsync/flush. No public signature changes — `append_encoded`, `append`, `fsync`, `rotate_to` are all untouched, so nothing outside this file needs to change.

**NOTE — hot path:** `AofMsg::Append` and `AofMsg::AppendAndFsync` are hit by every single write command that reaches the AOF (`dispatch_and_log_inner` calls `append_encoded` once per logged frame — `crates/server/src/dispatcher.rs:3233`). The fields below must stay lazy (no `format!`, no pre-formatted `String`) so a disabled level costs only the relaxed atomic load `tracing`'s filter already does.

This task's core logic — the running `offset` — cannot be observed independently of the log line it feeds (it is a variable local to the writer-thread closure, not exposed through any public method; `current_offset()` computes its own answer via a fresh `fsync` + `stat` and does not read this counter). Per the Global Constraints' testing exception, this is a case where a log line cannot be unit-asserted without heavy capture infrastructure — verification is the full pre-existing `aof.rs` test suite staying green (nothing about `offset` changes any observable return value) plus a documented manual check in Step 5.

- [ ] **Step 1: Add the running offset and the trace/debug log calls**

In `crates/server/src/aof.rs`, `open_with_base` currently opens the file and immediately wraps it in a `BufWriter` (lines 159–161):

```rust
    fn open_with_base(path: &Path, base_path: &Path, policy: FsyncPolicy) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut writer = BufWriter::new(file);
        let (tx, rx) = mpsc::sync_channel::<AofMsg>(AOF_QUEUE_CAPACITY);
```

Capture the file's existing length *before* it moves into the `BufWriter`, so a writer opened at a non-zero generation (or resuming a partially-written rotation target) starts its trace log at the true on-disk offset rather than at 0:

```rust
    fn open_with_base(path: &Path, base_path: &Path, policy: FsyncPolicy) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        // The writer thread processes `AofMsg`s strictly one at a time, so this plain `u64` —
        // not an atomic — is the whole cost of tracking the running offset for the trace log
        // below. No cross-thread synchronization, and `append_encoded`/`fsync` on the calling
        // side never touch it.
        let mut offset = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut writer = BufWriter::new(file);
        let (tx, rx) = mpsc::sync_channel::<AofMsg>(AOF_QUEUE_CAPACITY);
```

Now update the four `AofMsg` match arms (currently lines 171–212):

```rust
                        // Fire-and-forget: the caller already returned, so stderr is the only
                        // place an error can go.
                        AofMsg::Append(bytes) => {
                            let len = bytes.len() as u64;
                            match writer.write_all(&bytes) {
                                Ok(()) => {
                                    tracing::trace!(offset, bytes = len, "aof append");
                                    offset += len;
                                }
                                Err(e) => tracing::error!(error = %e, "aof append failed"),
                            }
                        }
                        // The acked variants hand the real I/O result back to the waiting
                        // caller instead of printing it, so a full disk surfaces where the
                        // write was requested. A failed send just means the caller gave up
                        // waiting; dropping the result is the only sensible response.
                        AofMsg::AppendAndFsync(bytes, ack) => {
                            let len = bytes.len() as u64;
                            let result = writer
                                .write_all(&bytes)
                                .and_then(|_| writer.flush())
                                .and_then(|_| writer.get_ref().sync_data());
                            if result.is_ok() {
                                tracing::trace!(offset, bytes = len, "aof append");
                                offset += len;
                                tracing::debug!(offset, "aof fsync");
                            }
                            let _ = ack.send(result);
                        }
                        AofMsg::Flush(ack) => {
                            let result = writer.flush().and_then(|_| writer.get_ref().sync_data());
                            if result.is_ok() {
                                tracing::debug!(offset, "aof fsync");
                            }
                            let _ = ack.send(result);
                        }
                        AofMsg::CheckIntact(path, ack) => {
                            let result = writer.get_ref().metadata().map(|fd_meta| {
                                std::fs::metadata(&path)
                                    .map(|path_meta| same_file(&fd_meta, &path_meta))
                                    .unwrap_or(false)
                            });
                            let _ = ack.send(result);
                        }
                        AofMsg::Rotate(new_path, ack) => {
                            let result = writer.flush().and_then(|_| writer.get_ref().sync_data());
                            let result = result.and_then(|_| {
                                OpenOptions::new().create(true).append(true).open(&new_path)
                            });
                            let result = match result {
                                Ok(file) => {
                                    // Not always 0: `rotate_to`'s own doc comment notes the new
                                    // path may already have content from an interrupted previous
                                    // rewrite, in which case appends resume after it, not at 0.
                                    offset = file.metadata().map(|m| m.len()).unwrap_or(0);
                                    writer = BufWriter::new(file);
                                    Ok(())
                                }
                                Err(e) => Err(e),
                            };
                            let _ = ack.send(result);
                        }
```

`CheckIntact` carries no write and no fsync, so it gets no new log line — `check_aof_intact` in `connection.rs` already logs the one thing worth logging there (an `error!` on a failed or negative intactness check).

- [ ] **Step 2: Run the full AOF test suite**

```bash
cargo test -p rocket-mem aof::
```

Expected: every existing test in `aof.rs` still passes — in particular `append_is_cumulative_across_multiple_calls`, `rotate_to_updates_path_and_current_offset`, and `rotate_to_an_already_existing_file_appends_after_its_current_content`, none of which read the new `offset` variable and so cannot regress from it, but all of which exercise the exact code paths now carrying the new log calls.

- [ ] **Step 3: Full workspace check**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, all green, with no test file touched by this step.

- [ ] **Step 4: Manual verification**

Start the server at `trace` and again at `debug`, and confirm the new lines appear and look sane (an increasing `offset`, plausible `bytes` per write):

```bash
RUST_LOG=trace cargo run -p rocket-mem -- --port 7777 &
redis-cli -p 7777 set foo bar
redis-cli -p 7777 set foo barbarbar
# expect two "aof append" trace lines with increasing offset and bytes matching each SET's
# encoded RESP frame size, and (under the default EverySecond fsync policy) a "aof fsync"
# debug line roughly once a second from the periodic fsync loop
kill %1
```

Then confirm the default `info` level shows neither line:

```bash
cargo run -p rocket-mem -- --port 7777 &
redis-cli -p 7777 set foo bar
# expect no "aof append" or "aof fsync" line in the server's stderr output
kill %1
```

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/aof.rs
git commit -m "feat(logging): trace AOF append offset/bytes, debug fsync"
```

---

### Task 2: AOF milestone events — rewrite start/finish and recovery replay summary

**Files:**
- Modify: `crates/server/src/aof.rs` (new `replay_with_stats`, `replay` becomes a thin wrapper over it; `recover`, lines 600–648)
- Modify: `crates/server/src/dispatcher.rs` (`handle_bgrewriteaof`, lines 2671–2707)
- Test: `crates/server/src/aof.rs` (same inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub struct ReplayStats { pub commands: u64, pub bytes: u64, pub elapsed: std::time::Duration }` and `pub fn replay_with_stats(path: &Path, engine: &engine::Engine, start_at: u64) -> std::io::Result<ReplayStats>`. `replay`'s existing signature and behavior are unchanged (it becomes `replay_with_stats(..).map(|_| ())`), so every one of the eleven existing call sites of `replay` in this file's test module keeps compiling and passing with no edits. `recover` calls `replay_with_stats` directly and logs the summary; nothing outside `aof.rs` calls `replay_with_stats` in this plan.

This task deliberately does *not* change `replay`'s signature, even though the spec's recovery-summary line needs a command count `replay` does not currently track. Changing `replay` itself would force editing all eleven of its existing call sites in this file's test module — exactly the "a test needs editing to accommodate a log line" smell the Global Constraints call out as a sign the change altered behavior. Splitting the counting logic into a new `replay_with_stats` and making `replay` a wrapper over it keeps every existing test compiling and passing completely unchanged.

- [ ] **Step 1: Write the failing test for `replay_with_stats`**

Add to the existing `mod tests` block in `crates/server/src/aof.rs`, near the other `replay_*` tests (after `replay_reconstructs_state_from_a_well_formed_aof`, around line 758):

```rust
    #[test]
    fn replay_with_stats_counts_every_command_and_byte_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let raw: &[u8] = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n";
        write_raw(&path, raw);
        let engine = Engine::new();
        let stats = replay_with_stats(&path, &engine, 0).unwrap();
        assert_eq!(stats.commands, 2);
        assert_eq!(stats.bytes, raw.len() as u64);
    }

    #[test]
    fn replay_with_stats_excludes_a_corrupt_tail_from_both_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let valid = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
        write_raw(&path, valid);
        write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$3\r\ngar"); // truncated mid-bulk-body
        let engine = Engine::new();
        let stats = replay_with_stats(&path, &engine, 0).unwrap();
        assert_eq!(stats.commands, 1);
        assert_eq!(stats.bytes, valid.len() as u64);
    }

    #[test]
    fn replay_with_stats_on_a_missing_file_reports_zero_commands_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.aof");
        let engine = Engine::new();
        let stats = replay_with_stats(&path, &engine, 0).unwrap();
        assert_eq!(stats.commands, 0);
        assert_eq!(stats.bytes, 0);
    }

    #[test]
    fn replay_still_reports_no_stats_and_behaves_exactly_as_before() {
        // `replay` is now a thin wrapper over `replay_with_stats`; this test pins its public
        // signature and behavior so a future change to `replay_with_stats` cannot silently
        // change what `replay`'s many existing callers observe.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");
        let engine = Engine::new();
        let result: std::io::Result<()> = replay(&path, &engine, 0);
        assert!(result.is_ok());
        assert_eq!(
            engine.get(b"a"),
            Some(Value::String(bytes::Bytes::from_static(b"1")))
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem aof::tests::replay_with_stats
```

Expected: FAIL with a compile error — `replay_with_stats` and `ReplayStats` do not exist yet (`error[E0425]: cannot find function `replay_with_stats` in this scope`).

- [ ] **Step 3: Implement `replay_with_stats` and reduce `replay` to a wrapper**

Replace the existing `replay` function (lines 449–487) with:

```rust
/// Summary of one `replay_with_stats` call: how many commands were replayed, how many bytes of
/// the file that consumed, and how long it took. `recover` logs this as the operator-facing line
/// that answers "why did startup take this long" — see the design spec's AOF event catalogue row
/// for the recovery replay summary.
pub struct ReplayStats {
    pub commands: u64,
    pub bytes: u64,
    pub elapsed: std::time::Duration,
}

/// Replays every command in the AOF at `path` against `engine`, via the plain (non-logging)
/// `dispatcher::dispatch` — never `dispatch_and_log`, which would re-append what's being
/// replayed. A missing file is a no-op (nothing to recover on first run). `start_at` is
/// clamped to the file's actual length rather than trusted blindly, so a caller passing a
/// stale or wrong offset degrades to "replay nothing" instead of panicking on an
/// out-of-range slice; `aof::recover` (below) is what decides *whether* a mismatched offset
/// should reach this function at all. A corrupt or incomplete final frame stops replay at the
/// last fully-decoded frame and truncates the file on disk to that exact byte offset.
///
/// Kept as a thin wrapper over `replay_with_stats` so its own signature and behavior never
/// change — see that function for the counting logic `recover`'s log line needs.
pub fn replay(path: &Path, engine: &engine::Engine, start_at: u64) -> std::io::Result<()> {
    replay_with_stats(path, engine, start_at).map(|_| ())
}

/// Does the same work as `replay`, additionally returning how many commands and bytes were
/// replayed and how long it took. Split out from `replay` rather than changing `replay` itself,
/// so `replay`'s eleven existing test call sites in this module need no changes at all.
pub fn replay_with_stats(
    path: &Path,
    engine: &engine::Engine,
    start_at: u64,
) -> std::io::Result<ReplayStats> {
    use tokio_util::codec::Decoder;

    let started = std::time::Instant::now();

    let raw = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReplayStats {
                commands: 0,
                bytes: 0,
                elapsed: started.elapsed(),
            });
        }
        Err(e) => return Err(e),
    };

    let start = (start_at as usize).min(raw.len());
    let mut buf = bytes::BytesMut::from(&raw[start..]);
    let mut codec = protocol::codec::RespCodec::default();
    let mut valid_len = start;
    let mut commands: u64 = 0;
    loop {
        let before = buf.len();
        match codec.decode(&mut buf) {
            Ok(Some(frame)) => {
                valid_len += before - buf.len();
                commands += 1;
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
    Ok(ReplayStats {
        commands,
        bytes: (valid_len - start) as u64,
        elapsed: started.elapsed(),
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem aof::tests::replay
```

Expected: all `replay_*` and `replay_with_stats_*` tests PASS, including every pre-existing `replay_*` test unchanged.

- [ ] **Step 5: Wire the recovery summary log into `recover`, and the rewrite start/finish logs into `handle_bgrewriteaof`**

In `crates/server/src/aof.rs`, `recover` (lines 600–648) currently ends with:

```rust
    replay(aof_path, &engine, start_at)?;
    Ok(engine)
```

and its earlier fallback branch (around line 628) calls `replay(aof_path, &fresh, 0)?;`. Change both call sites to `replay_with_stats` and log the summary:

```rust
                    Some(len) if offset > len => {
                        tracing::warn!(
                            snapshot_path = %snapshot_path.display(),
                            offset,
                            aof_len = len,
                            "snapshot offset past end of AOF; discarding snapshot and replaying full AOF"
                        );
                        let fresh = engine::Engine::new();
                        let stats = replay_with_stats(aof_path, &fresh, 0)?;
                        tracing::info!(
                            commands = stats.commands,
                            bytes = stats.bytes,
                            elapsed_us = stats.elapsed.as_micros() as u64,
                            "aof recovery replay complete"
                        );
                        return Ok(fresh);
                    }
```

and, at the end of `recover`:

```rust
    let stats = replay_with_stats(aof_path, &engine, start_at)?;
    tracing::info!(
        commands = stats.commands,
        bytes = stats.bytes,
        elapsed_us = stats.elapsed.as_micros() as u64,
        "aof recovery replay complete"
    );
    Ok(engine)
```

Now in `crates/server/src/dispatcher.rs`, `handle_bgrewriteaof` (lines 2671–2707) currently reads:

```rust
fn handle_bgrewriteaof(
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> Frame {
    // Held across every step below, not just the rotation: the generation read and the manifest
    // commit are what two concurrent rewrites collide on, and `start_rewrite`'s own
    // `lock_for_ordering()` is released long before the commit. See `AofWriter::lock_for_rewrite`.
    let _rewrite_guard = aof.lock_for_rewrite();

    let (next_gen, bytes) = match start_rewrite(aof, replication) {
        Ok(r) => r,
        Err(e) => return Frame::Error(format!("ERR failed to start AOF rewrite: {e}")),
    };

    let new_snapshot_path = crate::aof::generation_path(replication.snapshot_path(), next_gen);
    if let Err(e) = write_snapshot_atomically(&new_snapshot_path, &bytes) {
        return Frame::Error(format!("ERR failed to write rewritten snapshot: {e}"));
    }

    // The commit point: before this rename, generation `next_gen - 1` is still authoritative;
    // after it, `next_gen` is. See the design spec's crash-safety argument.
    if let Err(e) = crate::aof::write_generation_atomically(replication.snapshot_path(), next_gen) {
        return Frame::Error(format!("ERR failed to commit AOF rewrite: {e}"));
    }

    // Best-effort: an old generation's files are simply unreferenced once the manifest commit
    // above lands. A failure or a crash here is harmless — never a correctness problem, only
    // delayed disk reclamation. See the design spec's "Decision: `BGREWRITEAOF` command", step 4.
    let old_gen = next_gen - 1;
    let _ = std::fs::remove_file(crate::aof::generation_path(aof.base_path(), old_gen));
    let _ = std::fs::remove_file(crate::aof::generation_path(
        replication.snapshot_path(),
        old_gen,
    ));

    Frame::Simple("OK".into())
}
```

Change it to:

```rust
fn handle_bgrewriteaof(
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> Frame {
    // Held across every step below, not just the rotation: the generation read and the manifest
    // commit are what two concurrent rewrites collide on, and `start_rewrite`'s own
    // `lock_for_ordering()` is released long before the commit. See `AofWriter::lock_for_rewrite`.
    let _rewrite_guard = aof.lock_for_rewrite();
    let started = std::time::Instant::now();

    let (next_gen, bytes) = match start_rewrite(aof, replication) {
        Ok(r) => r,
        Err(e) => return Frame::Error(format!("ERR failed to start AOF rewrite: {e}")),
    };
    tracing::info!(generation = next_gen, "aof rewrite starting");

    let new_snapshot_path = crate::aof::generation_path(replication.snapshot_path(), next_gen);
    if let Err(e) = write_snapshot_atomically(&new_snapshot_path, &bytes) {
        return Frame::Error(format!("ERR failed to write rewritten snapshot: {e}"));
    }

    // The commit point: before this rename, generation `next_gen - 1` is still authoritative;
    // after it, `next_gen` is. See the design spec's crash-safety argument.
    if let Err(e) = crate::aof::write_generation_atomically(replication.snapshot_path(), next_gen) {
        return Frame::Error(format!("ERR failed to commit AOF rewrite: {e}"));
    }

    // Best-effort: an old generation's files are simply unreferenced once the manifest commit
    // above lands. A failure or a crash here is harmless — never a correctness problem, only
    // delayed disk reclamation. See the design spec's "Decision: `BGREWRITEAOF` command", step 4.
    let old_gen = next_gen - 1;
    let _ = std::fs::remove_file(crate::aof::generation_path(aof.base_path(), old_gen));
    let _ = std::fs::remove_file(crate::aof::generation_path(
        replication.snapshot_path(),
        old_gen,
    ));

    tracing::info!(
        generation = next_gen,
        bytes = bytes.len(),
        elapsed_us = started.elapsed().as_micros() as u64,
        "aof rewrite finished"
    );
    Frame::Simple("OK".into())
}
```

`bytes.len()` here is the size of the fresh generation's snapshot — the number that tells an operator how much the rewrite actually compacted the keyspace down to, which is what "resulting file size" means for a rewrite (the freshly-rotated AOF itself is empty immediately after rotation by construction).

The `handle_bgrewriteaof` change has no independently testable new logic beyond what `Step 1`'s replay tests already cover for the recovery half — it only adds two log calls and an `Instant` around already-tested control flow (`handle_bgrewriteaof_commits_a_readable_generation_1` and its neighbors, `dispatcher.rs:8689` onward, already exercise every branch this touches). Per the Global Constraints' testing exception, verification for this half is the existing `handle_bgrewriteaof_*` suite staying green plus the manual check in Step 7.

- [ ] **Step 6: Run the tests to verify everything still passes**

```bash
cargo test -p rocket-mem aof::
cargo test -p rocket-mem dispatcher::tests::handle_bgrewriteaof
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: every new `replay_with_stats_*` test passes, every pre-existing `replay_*` and `handle_bgrewriteaof_*` test still passes unchanged, fmt clean, clippy clean, full workspace suite green — `crates/server/tests/kill_and_recover.rs` included, since it drives a real `recover()` call end to end.

- [ ] **Step 7: Manual verification**

```bash
RUST_LOG=info cargo run -p rocket-mem -- --port 7777 --aof-path /tmp/rm.aof --snapshot-path /tmp/rm.snapshot &
redis-cli -p 7777 set a 1
redis-cli -p 7777 set b 2
redis-cli -p 7777 bgrewriteaof
# expect "aof rewrite starting" then "aof rewrite finished" with generation=1 and a bytes field
kill %1
RUST_LOG=info cargo run -p rocket-mem -- --port 7777 --aof-path /tmp/rm.aof --snapshot-path /tmp/rm.snapshot &
# expect one "aof recovery replay complete" line with commands/bytes/elapsed_us
kill %1
rm -f /tmp/rm.aof* /tmp/rm.snapshot*
```

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/aof.rs crates/server/src/dispatcher.rs
git commit -m "feat(logging): info-level AOF rewrite and recovery replay summary events"
```

---

### Task 3: Benchmark gate

**Files:**
- Modify: none (verification-only task).
- Reference: `docs/benchmarks/2026-09-09-pre-logging-baseline.md` (created in plan 01, Task 1).

**Interfaces:**
- Consumes: the `scripts/benchmark.sh` harness and the baseline document's recorded means.
- Produces: nothing new consumed downstream; this is the acceptance gate for this plan. Task 1 put new code directly on the AOF writer thread's hot path (every write command's append and, under `FsyncPolicy::Always`, every write's fsync), so this plan does not get to skip the gate the way a pure milestone-event plan might.

- [ ] **Step 1: Run the benchmark three times at the default `info` level**

```bash
cd /home/numericlabs/data/rocket/rocket-mem
for i in 1 2 3; do
  echo "=== run $i ==="
  ./scripts/benchmark.sh
done 2>&1 | tee /tmp/rocket-mem-plan15-benchmark.txt
```

- [ ] **Step 2: Compare against the baseline**

Compute the mean `SET`/`GET` requests/sec from Step 1's three runs and compare against the `Mean` column in `docs/benchmarks/2026-09-09-pre-logging-baseline.md`.

**Acceptance: within 2% of baseline.** If either workload regresses more than 2%, this task is not done — profile which of Task 1's changes is responsible (the leading suspects are the extra branch/field-construction in the hot `AofMsg::Append`/`AofMsg::AppendAndFsync` arms) before proceeding to any later plan.

- [ ] **Step 3: Record the result and commit**

Append a short section to `docs/benchmarks/2026-09-09-pre-logging-baseline.md` recording this plan's measured means and the verdict:

```markdown

## Plan 15 (AOF events) gate result

**Date:** <today>
**Commit:** <output of `git rev-parse --short HEAD`>

| Workload | Run 1 | Run 2 | Run 3 | Mean | vs. baseline |
|---|---|---|---|---|---|
| SET | | | | | |
| GET | | | | | |

Verdict: <within 2% / regressed — details>
```

```bash
git add docs/benchmarks/2026-09-09-pre-logging-baseline.md
git commit -m "docs: record plan 15 AOF-events benchmark gate result"
```

---

## Next plan

[`16-snapshot-events.md`](16-snapshot-events.md) — instruments `SAVE` and the startup snapshot-load path with `info`-level save/load events carrying path, bytes, and duration.
