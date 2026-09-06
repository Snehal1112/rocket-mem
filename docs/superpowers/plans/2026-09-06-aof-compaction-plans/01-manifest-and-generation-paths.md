# Manifest & Generation Paths Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the pure file-path/manifest logic a rewrite and a generation-aware `recover()` both need — no dependency on `AofWriter`, the dispatcher, or networking.

**Architecture:** three free functions added to `crates/server/src/aof.rs`: `generation_path` (pure path math), `read_generation` (manifest read, default 0), `write_generation_atomically` (manifest write, the eventual commit point of a rewrite). All `pub`, all unit-testable in isolation with a tempdir.

**Tech Stack:** `std::fs`/`std::io` only — no new dependency.

**Spec:** [`../../specs/2026-09-06-aof-compaction-design.md`](../../specs/2026-09-06-aof-compaction-design.md), "Decision: generations + a manifest, not in-place rewriting".

## Global Constraints

- No new configuration surface: the manifest path is always `<snapshot_path>.manifest`, generation-numbered files are always `<base>.<gen>` — never separately configured, per the spec's Scope line.
- Generation 0 must resolve to the bare, unmodified base path (`generation_path(base, 0) == base`) — this is what makes every pre-compaction deployment's files valid with zero migration.
- This plan does not touch `AofWriter`, `recover()`, or the dispatcher — that's plans 02, 05, and 03/04. These three functions have no dependency on any of them.

---

### Task 1: `generation_path` + `read_generation`

**Files:**
- Modify: `crates/server/src/aof.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn generation_path(base: &Path, gen: u64) -> PathBuf`, `pub fn read_generation(snapshot_path: &Path) -> std::io::Result<u64>` — plan 02's `start_rewrite` (via plan 03), plan 05's `recover()`, and this plan's own Task 2 all call these.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/aof.rs — inside `mod tests`
#[test]
fn generation_path_for_generation_zero_is_the_bare_path_unchanged() {
    let base = std::path::Path::new("/tmp/dump.snapshot");
    assert_eq!(generation_path(base, 0), base);
}

#[test]
fn generation_path_for_a_later_generation_appends_dot_gen() {
    let base = std::path::Path::new("/tmp/dump.snapshot");
    assert_eq!(
        generation_path(base, 1),
        std::path::PathBuf::from("/tmp/dump.snapshot.1")
    );
    assert_eq!(
        generation_path(base, 42),
        std::path::PathBuf::from("/tmp/dump.snapshot.42")
    );
}

#[test]
fn read_generation_with_no_manifest_on_disk_is_zero() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot_path = dir.path().join("dump.snapshot"); // never created
    assert_eq!(read_generation(&snapshot_path).unwrap(), 0);
}

#[test]
fn read_generation_reads_back_a_hand_written_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot_path = dir.path().join("dump.snapshot");
    std::fs::write(dir.path().join("dump.snapshot.manifest"), "7").unwrap();
    assert_eq!(read_generation(&snapshot_path).unwrap(), 7);
}

#[test]
fn read_generation_tolerates_a_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot_path = dir.path().join("dump.snapshot");
    std::fs::write(dir.path().join("dump.snapshot.manifest"), "3\n").unwrap();
    assert_eq!(read_generation(&snapshot_path).unwrap(), 3);
}

#[test]
fn read_generation_on_a_corrupt_manifest_is_an_error_not_a_silent_zero() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot_path = dir.path().join("dump.snapshot");
    std::fs::write(dir.path().join("dump.snapshot.manifest"), "not-a-number").unwrap();
    assert!(read_generation(&snapshot_path).is_err());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib aof::tests::generation_path -- --nocapture && cargo test -p rocket-mem --lib aof::tests::read_generation -- --nocapture`
Expected: FAIL to compile — `generation_path`/`read_generation` don't exist yet.

- [ ] **Step 3: Implement**

```rust
// crates/server/src/aof.rs — near `replay`/`recover`, above `recover`
/// The manifest path derived from `snapshot_path` — always `<snapshot_path>.manifest`, never
/// separately configured. See the design spec's "no new configuration surface" scope note.
fn manifest_path(snapshot_path: &Path) -> PathBuf {
    let mut os = snapshot_path.as_os_str().to_owned();
    os.push(".manifest");
    PathBuf::from(os)
}

/// `base` unchanged for generation 0 — so every pre-compaction deployment's files keep their
/// exact names forever, no migration needed — or `<base>.<gen>` for generation 1 and up.
pub fn generation_path(base: &Path, gen: u64) -> PathBuf {
    if gen == 0 {
        return base.to_path_buf();
    }
    let mut os = base.as_os_str().to_owned();
    os.push(format!(".{gen}"));
    PathBuf::from(os)
}

/// The current generation, read from `<snapshot_path>.manifest`. A missing manifest means
/// generation 0 — every deployment that has never run `BGREWRITEAOF` — so `snapshot_path`/
/// `aof_path` are used exactly as configured. See the design spec's "generations + a manifest"
/// decision.
pub fn read_generation(snapshot_path: &Path) -> std::io::Result<u64> {
    match std::fs::read_to_string(manifest_path(snapshot_path)) {
        Ok(contents) => contents
            .trim()
            .parse::<u64>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib aof::tests -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill to commit `crates/server/src/aof.rs`.

---

### Task 2: `write_generation_atomically`

**Files:**
- Modify: `crates/server/src/aof.rs`

**Interfaces:**
- Consumes: `manifest_path` (private, Task 1, same file).
- Produces: `pub fn write_generation_atomically(snapshot_path: &Path, gen: u64) -> std::io::Result<()>` — plan 03's `handle_bgrewriteaof` calls this as the rewrite's commit point.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/aof.rs — inside `mod tests`
#[test]
fn write_generation_atomically_is_read_back_by_read_generation() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot_path = dir.path().join("dump.snapshot");
    write_generation_atomically(&snapshot_path, 5).unwrap();
    assert_eq!(read_generation(&snapshot_path).unwrap(), 5);
}

#[test]
fn write_generation_atomically_overwrites_a_previous_generation() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot_path = dir.path().join("dump.snapshot");
    write_generation_atomically(&snapshot_path, 1).unwrap();
    write_generation_atomically(&snapshot_path, 2).unwrap();
    assert_eq!(read_generation(&snapshot_path).unwrap(), 2);
}

#[test]
fn write_generation_atomically_does_not_leave_a_tmp_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot_path = dir.path().join("dump.snapshot");
    write_generation_atomically(&snapshot_path, 1).unwrap();
    assert!(!dir.path().join("dump.snapshot.manifest.tmp").exists());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib aof::tests::write_generation_atomically -- --nocapture`
Expected: FAIL to compile — `write_generation_atomically` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
// crates/server/src/aof.rs — directly below `read_generation`
/// Atomically (tmp + fsync + rename — the same primitive `dispatcher::write_snapshot_atomically`
/// uses, duplicated here rather than shared so `aof.rs` doesn't gain a dependency on
/// `dispatcher.rs`, reversing this crate's existing one-way dependency direction) writes `gen`
/// as the new current generation. This rename is the single commit point of a rewrite: before
/// it, generation `gen - 1`'s files are authoritative; after it, generation `gen`'s are. See the
/// design spec's "Decision: `BGREWRITEAOF` command", step 3.
pub fn write_generation_atomically(snapshot_path: &Path, gen: u64) -> std::io::Result<()> {
    use std::io::Write;
    let path = manifest_path(snapshot_path);
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp_path = PathBuf::from(tmp_os);
    {
        let file = std::fs::File::create(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        write!(writer, "{gen}")?;
        writer.flush()?;
        writer.get_ref().sync_data()?;
    }
    std::fs::rename(&tmp_path, &path)?;
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib aof::tests -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --lib aof::`
Expected: all green.

Use the `1-git-commit` skill to commit `crates/server/src/aof.rs`.

---

## Next plan

This plan has no dependency on [`02-aof-writer-rotation.md`](./02-aof-writer-rotation.md) or [`05-generation-aware-recovery.md`](./05-generation-aware-recovery.md) — either can be started next, or in parallel with each other. `03-bgrewriteaof-core-sequence.md` needs both this plan and `02` merged first, so it can't start until both are done. If working through the plans in a single line rather than in parallel, do `02-aof-writer-rotation.md` next.
