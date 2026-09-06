# AofWriter Rotation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `AofWriter::rotate_to`, so a rewrite can redirect appends to a brand-new, empty file without any new synchronization primitive — it reuses `lock_for_ordering()`, the same lock every write command already holds.

**Architecture:** a new `AofMsg::Rotate` variant handled by the existing writer thread (`AofWriter::open`'s spawned loop), acked like `Flush`. `AofWriter`'s `path` field becomes `Mutex<PathBuf>` (was a plain `PathBuf`) so `rotate_to` can repoint it after a successful rotation; `current_offset` reads through the same lock. A second, separate, never-mutated `base_path: PathBuf` field is added alongside it: `path()` reflects whichever generation is *currently* active (changes on every rotation), while `base_path()` always returns the original path `open` was called with, un-suffixed — plan 03's generation-path math needs that stable value, not the rotating one, to avoid double-suffixing on a second rewrite (`dump.aof.1.2` instead of `dump.aof.2`).

**Tech Stack:** `std::sync::mpsc`, `std::sync::Mutex` — no new dependency.

**Spec:** [`../../specs/2026-09-06-aof-compaction-design.md`](../../specs/2026-09-06-aof-compaction-design.md), "Decision: `AofWriter::rotate_to`".

## Global Constraints

- `rotate_to` must be callable while the caller already holds `lock_for_ordering()` — it must not itself try to take that lock (it doesn't need to; the writer thread's message queue already serializes it against concurrent appends, since every write command's `append` call is only ever issued while holding that same lock).
- This plan does not touch the dispatcher or the manifest/generation-path functions from plan 01 — `rotate_to` takes a plain `&Path` and knows nothing about generations.

---

### Task 1: `path()` accessor, `AofMsg::Rotate`, `rotate_to`

**Files:**
- Modify: `crates/server/src/aof.rs`

**Interfaces:**
- Consumes: nothing from other plans.
- Produces: `pub fn AofWriter::path(&self) -> PathBuf`, `pub fn AofWriter::base_path(&self) -> &Path`, `pub fn AofWriter::rotate_to(&self, new_path: &Path) -> std::io::Result<()>` — plan 03's `start_rewrite`/`handle_bgrewriteaof` call `base_path()` and `rotate_to()`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/aof.rs — inside `mod tests`
#[test]
fn path_reports_the_file_aof_writer_was_opened_with() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.aof");
    let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
    assert_eq!(writer.path(), path);
}

#[test]
fn rotate_to_freezes_the_old_file_and_directs_new_appends_to_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("old.aof");
    let new_path = dir.path().join("new.aof");
    let writer = AofWriter::open(&old_path, FsyncPolicy::Never).unwrap();

    writer.append(frame(&[b"SET", b"a", b"1"])).unwrap();
    writer.fsync().unwrap();

    writer.rotate_to(&new_path).unwrap();

    writer.append(frame(&[b"SET", b"b", b"2"])).unwrap();
    writer.fsync().unwrap();

    assert_eq!(
        std::fs::read(&old_path).unwrap(),
        b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n"
    ); // frozen at rotation, never touched again
    assert_eq!(
        std::fs::read(&new_path).unwrap(),
        b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n"
    ); // starts fresh at byte 0
}

#[test]
fn rotate_to_updates_path_and_current_offset() {
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("old.aof");
    let new_path = dir.path().join("new.aof");
    let writer = AofWriter::open(&old_path, FsyncPolicy::Never).unwrap();
    writer.append(frame(&[b"SET", b"a", b"1"])).unwrap();
    writer.fsync().unwrap();

    writer.rotate_to(&new_path).unwrap();

    assert_eq!(writer.path(), new_path);
    assert_eq!(writer.current_offset().unwrap(), 0); // the new file starts empty
}

#[test]
fn rotate_to_an_already_existing_file_appends_after_its_current_content() {
    // Mirrors `open_on_an_existing_file_appends_rather_than_truncating` — rotation must not
    // clobber a new-generation file a previous, interrupted rewrite already partially wrote.
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("old.aof");
    let new_path = dir.path().join("new.aof");
    write_raw(&new_path, b"*3\r\n$3\r\nSET\r\n$1\r\nx\r\n$1\r\n0\r\n");

    let writer = AofWriter::open(&old_path, FsyncPolicy::Never).unwrap();
    writer.rotate_to(&new_path).unwrap();
    writer.append(frame(&[b"SET", b"y", b"1"])).unwrap();
    writer.fsync().unwrap();

    assert_eq!(
        std::fs::read(&new_path).unwrap(),
        b"*3\r\n$3\r\nSET\r\n$1\r\nx\r\n$1\r\n0\r\n*3\r\n$3\r\nSET\r\n$1\r\ny\r\n$1\r\n1\r\n"
    );
}

#[test]
fn base_path_never_changes_across_a_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let original_path = dir.path().join("original.aof");
    let writer = AofWriter::open(&original_path, FsyncPolicy::Never).unwrap();

    writer.rotate_to(&dir.path().join("gen1.aof")).unwrap();
    writer.rotate_to(&dir.path().join("gen2.aof")).unwrap();

    assert_eq!(writer.base_path(), original_path); // unchanged despite two rotations
    assert_eq!(writer.path(), dir.path().join("gen2.aof")); // this one does change
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib aof::tests::path_reports -- --nocapture && cargo test -p rocket-mem --lib aof::tests::rotate_to -- --nocapture`
Expected: FAIL to compile — `path()`/`rotate_to` don't exist yet.

- [ ] **Step 3: Implement**

First, change the struct field and its one existing reader:

```rust
// crates/server/src/aof.rs — AofWriter struct definition
pub struct AofWriter {
    tx: mpsc::SyncSender<AofMsg>,
    policy: FsyncPolicy,
    order: Mutex<()>,
    /// The file currently being appended to. `Mutex`-wrapped (not a plain `PathBuf`) so
    /// `rotate_to` can repoint it after a successful rotation; read by `current_offset`.
    path: Mutex<PathBuf>,
    /// The original path `open` was given. Never changes, even across `rotate_to` calls --
    /// generation-path math (plan 03) must always start from this, never from `path`, which
    /// already reflects whatever generation is currently active.
    base_path: PathBuf,
}
```

In `AofWriter::open`, change the struct literal:

```rust
        Ok(Self {
            tx,
            policy,
            order: Mutex::new(()),
            path: Mutex::new(path.to_path_buf()),
            base_path: path.to_path_buf(),
        })
```

Update `current_offset` to read through the lock:

```rust
    pub fn current_offset(&self) -> std::io::Result<u64> {
        self.fsync()?;
        let path = self.path.lock().unwrap_or_else(|e| e.into_inner());
        Ok(std::fs::metadata(&*path)?.len())
    }
```

Add the new message variant:

```rust
enum AofMsg {
    Append(Vec<u8>),
    AppendAndFsync(Vec<u8>, mpsc::SyncSender<std::io::Result<()>>),
    Flush(mpsc::SyncSender<std::io::Result<()>>),
    /// Flushes and fsyncs the current file, opens `new_path` (create, append — so a rotation
    /// onto a file an interrupted previous rewrite already partially wrote appends after its
    /// content rather than clobbering it), and swaps the writer thread's target to it.
    Rotate(PathBuf, mpsc::SyncSender<std::io::Result<()>>),
}
```

Handle it in the writer thread's loop (`AofWriter::open`'s spawned closure), alongside the existing `Flush` arm:

```rust
                        AofMsg::Rotate(new_path, ack) => {
                            let result = writer.flush().and_then(|_| writer.get_ref().sync_data());
                            let result = result.and_then(|_| {
                                OpenOptions::new().create(true).append(true).open(&new_path)
                            });
                            let result = match result {
                                Ok(file) => {
                                    writer = BufWriter::new(file);
                                    Ok(())
                                }
                                Err(e) => Err(e),
                            };
                            let _ = ack.send(result);
                        }
```

Add the two new public methods, near `fsync`:

```rust
    /// The file currently being appended to.
    pub fn path(&self) -> PathBuf {
        self.path.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The original path `open` was given — stable across any number of `rotate_to` calls. See
    /// the `base_path` field's doc comment for why generation-path math must use this, not
    /// `path()`.
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Flushes and fsyncs the current file, then redirects subsequent appends to `new_path`
    /// (created fresh, or appended-to if it already exists). Must be called while the caller
    /// already holds `lock_for_ordering()` — see this plan's Global Constraints for why no new
    /// lock is needed here. Acked like `fsync()`, so the caller knows the rotation completed
    /// (and `path()`/`current_offset()` reflect the new file) before proceeding.
    pub fn rotate_to(&self, new_path: &Path) -> std::io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        self.send(AofMsg::Rotate(new_path.to_path_buf(), ack_tx))?;
        ack_rx.recv().map_err(writer_gone)??;
        *self.path.lock().unwrap_or_else(|e| e.into_inner()) = new_path.to_path_buf();
        Ok(())
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib aof:: -- --nocapture`
Expected: all PASS — including every pre-existing `aof.rs` test, since `path`'s new `Mutex` wrapping is internal and every existing caller (`current_offset`) still compiles and behaves identically for a writer that never rotates.

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --lib aof::`
Expected: all green.

Use the `1-git-commit` skill to commit `crates/server/src/aof.rs`.

---

## Next plan

This plan has no dependency on [`01-manifest-and-generation-paths.md`](./01-manifest-and-generation-paths.md) — it can be done before, after, or in parallel with it. Once both `01` and this plan are merged, continue with [`03-bgrewriteaof-core-sequence.md`](./03-bgrewriteaof-core-sequence.md), which needs `generation_path`/`read_generation` (from `01`) and `rotate_to`/`base_path` (from this plan) together.
