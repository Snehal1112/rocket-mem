# Hybrid Recovery & AOF Offset Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** startup loads a snapshot (if one exists and its embedded AOF offset still fits) plus only the AOF bytes written after it, instead of always replaying the whole AOF from byte 0 — this is what makes the sprint's recovery-time benchmark have something to measure.

**Architecture:** `AofWriter` gains a durable `current_offset()` (fsync, then stat its own file). `aof::replay` gains a `start_at: u64` parameter. A new `aof::recover` function owns the branching (no snapshot / matching snapshot / unreadable snapshot / offset-overshoot) as a single testable unit, so `main.rs` stays a thin caller — matching this project's existing pattern of keeping `main.rs` untested and pushing every decision into `server`'s library crate.

**Tech Stack:** none new — `std::fs`, `std::io`, the existing `AofWriter`/`replay`.

**Spec:** `../../specs/2026-08-30-sprint-5-spec.md` — "the snapshot file embeds its own AOF offset..." is authoritative for this plan. Depends on `01-snapshot-serialization.md`'s `Engine::snapshot`/`load_snapshot`.

## Global Constraints

- No AOF compaction/rewrite this sprint — the AOF keeps growing regardless of snapshots taken; byte 0 onward is always its complete history, which is exactly what makes the offset-overshoot fallback (full replay from byte 0) always correct.
- `ROCKET_MEM_SNAPSHOT_PATH` is a new env var, default `./dump.snapshot`, read once at startup — same pattern as the existing `ROCKET_MEM_ADDR`/`ROCKET_MEM_AOF_PATH`.

---

### Task 1: `AofWriter::current_offset`

**Files:**
- Modify: `crates/server/src/aof.rs`

**Interfaces:**
- Consumes: `AofWriter::fsync` (existing).
- Produces: `AofWriter::current_offset(&self) -> std::io::Result<u64>`, `pub`, used by `03-replication-handle-and-save.md`'s `SAVE` interception.

- [x] **Step 1: Write the failing test**

```rust
// crates/server/src/aof.rs — add to the existing tests module
#[test]
fn current_offset_matches_the_file_length_after_appends_land() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
    writer.append(frame(&[b"SET", b"a", b"1"])).unwrap();
    let offset = writer.current_offset().unwrap();
    assert_eq!(offset, std::fs::metadata(&path).unwrap().len());
    assert!(offset > 0);
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem aof::tests::current_offset`
Expected: FAIL with "no method named `current_offset` found for struct `AofWriter`"

- [x] **Step 3: Add the stored `path` field and `current_offset`**

Add `use std::path::PathBuf;` to `aof.rs`'s imports (replacing/joining the existing `use std::path::Path;`), then add a `path` field to `AofWriter` and populate it in `open`:

```rust
// crates/server/src/aof.rs
pub struct AofWriter {
    /// Bounded at `AOF_QUEUE_CAPACITY`; see that constant for why.
    tx: mpsc::SyncSender<AofMsg>,
    policy: FsyncPolicy,
    order: Mutex<()>,
    /// The file `open` was given. Read back by `current_offset` after an `fsync`, so it must
    /// be the same path the writer thread is appending to — never mutated after `open`.
    path: PathBuf,
}
```

In `open`, add `path: path.to_path_buf(),` to the `Self { ... }` construction (alongside the existing `tx`, `policy`, `order` fields).

Then add the new method, next to the existing `fsync`:

```rust
// crates/server/src/aof.rs
/// Flushes and fsyncs (via the existing `Flush` message the writer thread already handles),
/// then returns the file's length in bytes. The returned offset is guaranteed durable: every
/// byte before it is confirmed on disk. Calling this while holding
/// `AofWriter::lock_for_ordering()` cannot deadlock: the writer thread only ever drains its
/// channel and touches the file, never acquiring `order` or calling back into the dispatcher —
/// the worst case is a bounded wait for whatever's already queued ahead of the `Flush`.
pub fn current_offset(&self) -> std::io::Result<u64> {
    self.fsync()?;
    Ok(std::fs::metadata(&self.path)?.len())
}
```

- [x] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem aof::tests::current_offset`
Expected: PASS

- [x] **Step 5: Commit**

```bash
git add crates/server/src/aof.rs
git commit -m "feat(server): add AofWriter::current_offset"
```

---

### Task 2: `aof::replay` gains `start_at`

**Files:**
- Modify: `crates/server/src/aof.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `replay(path: &Path, engine: &engine::Engine, start_at: u64) -> std::io::Result<()>` (signature change — every existing call site updated in this task), used by Task 3 below.

- [x] **Step 1: Update the existing tests' call sites and write the new failing test**

`replay`'s 5 existing tests in `crates/server/src/aof.rs` (`replay_on_a_missing_file_is_a_no_op_not_an_error`, `replay_reconstructs_state_from_a_well_formed_aof`, `replay_recovers_every_valid_command_before_a_corrupt_tail_without_panicking`, `replay_truncates_the_corrupt_tail_off_the_file_on_disk`, `replay_on_a_fully_well_formed_file_does_not_truncate_anything`) each call `replay(&path, &engine)` — change every one of those five call sites to `replay(&path, &engine, 0)`. In `replay_truncates_the_corrupt_tail_off_the_file_on_disk`, there's a second `replay` call (`replay(&path, &engine2)`) after appending a new frame — update that one too, to `replay(&path, &engine2, 0)`.

Then add the new test proving `start_at` is honored:

```rust
// crates/server/src/aof.rs — add to the existing tests module
#[test]
fn replay_with_a_nonzero_start_at_skips_commands_before_that_offset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    let first = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
    write_raw(&path, first);
    write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n");

    let engine = Engine::new();
    replay(&path, &engine, first.len() as u64).unwrap();

    assert_eq!(engine.get(b"a"), None); // before start_at -- skipped
    assert_eq!(engine.get(b"b"), Some(Value::String(bytes::Bytes::from_static(b"2"))));
}

#[test]
fn replay_with_a_start_at_past_the_end_of_the_file_replays_nothing_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");

    let engine = Engine::new();
    replay(&path, &engine, 999_999).unwrap(); // must not panic on an out-of-range slice
    assert_eq!(engine.get(b"a"), None);
}

#[test]
fn replay_with_a_nonzero_start_at_still_truncates_a_corrupt_tail_from_the_true_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    let first = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
    write_raw(&path, first);
    let second = b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n";
    write_raw(&path, second);
    write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\nc\r\n$3\r\ngar"); // truncated mid-bulk-body

    let engine = Engine::new();
    replay(&path, &engine, first.len() as u64).unwrap();

    let on_disk = std::fs::read(&path).unwrap();
    let mut expected = first.to_vec();
    expected.extend_from_slice(second);
    assert_eq!(on_disk, expected); // corrupt tail removed; the skipped-over prefix stays intact
}
```

- [x] **Step 2: Run the tests to verify the new ones fail**

Run: `cargo test -p rocket-mem aof::tests::replay`
Expected: the three new tests FAIL (wrong number of arguments to `replay`); the five pre-existing ones also FAIL to compile until their call sites are updated per Step 1 above — make sure Step 1's edits are applied to all five before running this

- [x] **Step 3: Implement `start_at`**

```rust
// crates/server/src/aof.rs
/// Replays every command in the AOF at `path` against `engine`, via the plain (non-logging)
/// `dispatcher::dispatch` — never `dispatch_and_log`, which would re-append what's being
/// replayed. A missing file is a no-op (nothing to recover on first run). `start_at` is
/// clamped to the file's actual length rather than trusted blindly, so a caller passing a
/// stale or wrong offset degrades to "replay nothing" instead of panicking on an
/// out-of-range slice; `aof::recover` (below) is what decides *whether* a mismatched offset
/// should reach this function at all. A corrupt or incomplete final frame stops replay at the
/// last fully-decoded frame and truncates the file on disk to that exact byte offset.
pub fn replay(path: &Path, engine: &engine::Engine, start_at: u64) -> std::io::Result<()> {
    use tokio_util::codec::Decoder;

    let raw = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let start = (start_at as usize).min(raw.len());
    let mut buf = bytes::BytesMut::from(&raw[start..]);
    let mut codec = protocol::codec::RespCodec::default();
    let mut valid_len = start;
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

The only change from the existing body: `start_at`'s clamped value seeds both the slice `buf` is built from and `valid_len`'s initial value (was `0`, now `start`) — the truncation math downstream is otherwise untouched, so it still measures corruption relative to the whole file, not relative to `start_at`.

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem aof::tests`
Expected: PASS, all of `aof.rs`'s tests including the 5 pre-existing `replay_*` ones and the 3 new ones

- [x] **Step 5: Commit**

```bash
git add crates/server/src/aof.rs
git commit -m "feat(server): add start_at to aof::replay for hybrid recovery"
```

---

### Task 3: `aof::recover` and the `main.rs` rewrite

**Files:**
- Modify: `crates/server/src/aof.rs`
- Modify: `crates/server/src/main.rs`

**Interfaces:**
- Consumes: `Engine::snapshot`/`load_snapshot` (`01-snapshot-serialization.md`), `AofWriter::current_offset` (Task 1), `replay` with `start_at` (Task 2).
- Produces: `recover(aof_path: &Path, snapshot_path: &Path) -> std::io::Result<engine::Engine>`, `pub`, called only from `main.rs`.

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/src/aof.rs — add to the existing tests module
#[test]
fn recover_with_neither_file_present_returns_an_empty_engine() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("missing.aof");
    let snapshot_path = dir.path().join("missing.snapshot");
    let engine = recover(&aof_path, &snapshot_path).unwrap();
    assert!(engine.keys().is_empty());
}

#[test]
fn recover_with_only_an_aof_replays_it_in_full() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("test.aof");
    write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");
    let snapshot_path = dir.path().join("missing.snapshot");

    let engine = recover(&aof_path, &snapshot_path).unwrap();
    assert_eq!(engine.get(b"a"), Some(Value::String(bytes::Bytes::from_static(b"1"))));
}

#[test]
fn recover_with_a_matching_snapshot_and_offset_loads_the_snapshot_then_only_the_aof_tail() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("test.aof");
    let before_snapshot = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
    write_raw(&aof_path, before_snapshot);

    // Build the "already snapshotted" engine, snapshot it at the AOF's current length, then
    // append one more command after that point -- the AOF tail recover() must still pick up.
    let snapshotted_engine = Engine::new();
    replay(&aof_path, &snapshotted_engine, 0).unwrap();
    let snapshot_bytes = snapshotted_engine.snapshot(before_snapshot.len() as u64);
    let snapshot_path = dir.path().join("test.snapshot");
    std::fs::write(&snapshot_path, snapshot_bytes).unwrap();

    write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n");

    let engine = recover(&aof_path, &snapshot_path).unwrap();
    assert_eq!(engine.get(b"a"), Some(Value::String(bytes::Bytes::from_static(b"1")))); // from the snapshot
    assert_eq!(engine.get(b"b"), Some(Value::String(bytes::Bytes::from_static(b"2")))); // from the AOF tail
}

#[test]
fn recover_with_an_unreadable_snapshot_falls_back_to_a_full_aof_replay() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("test.aof");
    write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");
    let snapshot_path = dir.path().join("test.snapshot");
    std::fs::write(&snapshot_path, b"not a real snapshot").unwrap(); // fewer than 8 header bytes... actually more, so it'll fail bincode decode

    let engine = recover(&aof_path, &snapshot_path).unwrap();
    assert_eq!(engine.get(b"a"), Some(Value::String(bytes::Bytes::from_static(b"1"))));
}

#[test]
fn recover_with_a_snapshot_whose_offset_overshoots_the_aof_discards_it_and_replays_from_zero() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("test.aof");
    write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");

    // A snapshot claiming an AOF offset far larger than the AOF's real (small) size --
    // as if the AOF were deleted and recreated smaller after the snapshot was taken.
    let stale_engine = Engine::new();
    stale_engine.set(bytes::Bytes::from_static(b"stale"), Value::String(bytes::Bytes::from_static(b"old")));
    let snapshot_bytes = stale_engine.snapshot(999_999);
    let snapshot_path = dir.path().join("test.snapshot");
    std::fs::write(&snapshot_path, snapshot_bytes).unwrap();

    let engine = recover(&aof_path, &snapshot_path).unwrap();
    assert_eq!(engine.get(b"stale"), None); // the mismatched snapshot's data must not survive
    assert_eq!(engine.get(b"a"), Some(Value::String(bytes::Bytes::from_static(b"1")))); // full AOF replay instead
}
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem aof::tests::recover`
Expected: FAIL with "cannot find function `recover` in this scope"

- [x] **Step 3: Implement `recover`**

```rust
// crates/server/src/aof.rs
/// Orchestrates startup recovery: loads `snapshot_path` if it exists and decodes cleanly,
/// checks whether its embedded AOF offset still fits within `aof_path`'s actual length, and
/// either replays just the AOF tail after that offset (the fast path) or falls back to a full
/// replay from byte 0 on a completely fresh `Engine` (the safe path, taken when there's no
/// snapshot, the snapshot is unreadable, or its offset no longer corresponds to this AOF).
/// See `../../docs/superpowers/specs/2026-08-30-sprint-5-spec.md` for why the "no compaction"
/// constraint is what makes "byte 0 onward is always the complete history" always true, and
/// therefore why the fallback is always correct rather than merely convenient.
pub fn recover(aof_path: &Path, snapshot_path: &Path) -> std::io::Result<engine::Engine> {
    let engine = engine::Engine::new();
    let start_at = match std::fs::read(snapshot_path) {
        Ok(bytes) => match engine.load_snapshot(&bytes) {
            Ok(offset) => {
                let aof_len = std::fs::metadata(aof_path).map(|m| m.len()).unwrap_or(0);
                if offset > aof_len {
                    eprintln!(
                        "snapshot at {} names an AOF offset ({offset}) past the AOF's actual \
                         length ({aof_len}) -- discarding the snapshot and replaying the full \
                         AOF from byte 0 instead",
                        snapshot_path.display()
                    );
                    let fresh = engine::Engine::new();
                    replay(aof_path, &fresh, 0)?;
                    return Ok(fresh);
                }
                offset
            }
            Err(e) => {
                eprintln!(
                    "snapshot at {} is unreadable ({e}); falling back to full AOF replay",
                    snapshot_path.display()
                );
                0
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
    replay(aof_path, &engine, start_at)?;
    Ok(engine)
}
```

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem aof::tests`
Expected: PASS, all of `aof.rs`'s tests

- [x] **Step 5: Rewrite `main.rs` to call `recover`**

```rust
// crates/server/src/main.rs
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::var("ROCKET_MEM_ADDR").unwrap_or_else(|_| "127.0.0.1:6379".to_string());
    let aof_path =
        std::env::var("ROCKET_MEM_AOF_PATH").unwrap_or_else(|_| "./appendonly.aof".to_string());
    let aof_path = std::path::Path::new(&aof_path);
    let snapshot_path = std::env::var("ROCKET_MEM_SNAPSHOT_PATH")
        .unwrap_or_else(|_| "./dump.snapshot".to_string());
    let snapshot_path = std::path::Path::new(&snapshot_path);

    let engine = Arc::new(rocket_mem::aof::recover(aof_path, snapshot_path)?);
    println!(
        "Recovered state from {} and {}",
        snapshot_path.display(),
        aof_path.display()
    );

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

This drops the old two-line `replay(...)` + `println!("Replayed AOF from ...")` pair entirely — `recover` now owns both the loading and the log line's information (its own `eprintln!`s cover the fallback cases; this one `println!` covers the success path). `03-replication-handle-and-save.md` touches `main.rs` again to additionally construct a `ReplicationHandle` from this same `engine`/`snapshot_path`.

- [x] **Step 6: Run the full workspace verification**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean/green

- [x] **Step 7: Commit**

```bash
git add crates/server/src/aof.rs crates/server/src/main.rs
git commit -m "feat(server): add aof::recover for snapshot+AOF-tail hybrid startup"
```
