# BGREWRITEAOF Core Sequence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `handle_bgrewriteaof`, built in three TDD passes matching the spec's three-step protocol exactly — rotate under the lock, write the new generation's snapshot, commit the manifest, then clean up the old generation.

**Architecture:** two new functions in `crates/server/src/dispatcher.rs`, next to `handle_save`/`write_snapshot_atomically`: a private `start_rewrite` (the lock-protected half) and `handle_bgrewriteaof` (the full command handler) — both with the exact same `(aof: &AofWriter, replication: &ReplicationHandle)` signature shape `handle_save` already uses, since `AofWriter::base_path()` (plan 02) supplies the stable path this plan needs without threading a new parameter through `dispatch_and_log`. Not yet reachable from a real client — that's plan 04. This plan tests both functions by calling them directly, exactly like `handle_save` already is (see `save_writes_a_snapshot_that_load_snapshot_can_read_back`).

**Tech Stack:** reuses `crate::aof::{generation_path, read_generation, write_generation_atomically}` (plan 01) and `AofWriter::rotate_to`/`base_path` (plan 02). No new dependency.

**Spec:** [`../../specs/2026-09-06-aof-compaction-design.md`](../../specs/2026-09-06-aof-compaction-design.md), "Decision: `BGREWRITEAOF` command".

## Global Constraints

- **Always call `aof.base_path()`, never `aof.path()`, when computing a generation path.** `path()` reflects whichever generation is *currently* active and changes on every rotation; `generation_path(&aof.path(), 2)` after one prior rotation would double-suffix to `dump.aof.1.2` instead of `dump.aof.2`. `base_path()` is the one stable value, by construction, for the lifetime of the `AofWriter`.
- `start_rewrite` and `handle_bgrewriteaof` are private (`fn`, not `pub`) — plan 04 wires the command name to `handle_bgrewriteaof` from within the same module (`dispatch_and_log_inner`).
- Depends on plan 01 (`generation_path`/`read_generation`/`write_generation_atomically`) and plan 02 (`AofWriter::rotate_to`/`base_path`) being merged first.
- Does not touch `KNOWN_COMMANDS`, `key_spec`, `WRITE_COMMANDS`, or `dispatch_and_log_inner`'s interception chain — that's plan 04. `handle_bgrewriteaof` is unreachable from a real client until then; this plan proves its logic directly.

---

### Task 1: `start_rewrite` — the lock-protected half

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `crate::aof::read_generation`, `crate::aof::generation_path` (plan 01); `AofWriter::rotate_to`, `AofWriter::base_path`, `AofWriter::lock_for_ordering` (plan 02, existing).
- Produces: `fn start_rewrite(aof: &AofWriter, replication: &ReplicationHandle) -> std::io::Result<(u64, Vec<u8>)>` — Task 2 of this plan calls it.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
#[test]
fn start_rewrite_rotates_the_aof_and_returns_the_next_generation_with_a_snapshot() {
    let engine = std::sync::Arc::new(Engine::new());
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    let (dir, aof) = test_aof();
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path);

    let (next_gen, bytes) = start_rewrite(&aof, &replication).unwrap();

    assert_eq!(next_gen, 1); // no manifest yet -- current generation 0, next is 1
    let loaded = Engine::new();
    let embedded_offset = loaded.load_snapshot(&bytes).unwrap();
    assert_eq!(embedded_offset, 0); // pairs with a brand-new, currently-empty generation-1 AOF
    assert_eq!(
        loaded.get(b"k"),
        Some(Value::String(Bytes::from_static(b"v")))
    );
    assert_eq!(
        aof.path(),
        crate::aof::generation_path(&dir.path().join("test.aof"), 1)
    ); // rotated onto the generation-1 file

    // Prove the second-rewrite case doesn't double-suffix: rotating again must compute the
    // next path from `base_path()`, never from `path()`'s already-rotated value.
    let (next_gen2, _bytes2) = start_rewrite(&aof, &replication).unwrap();
    assert_eq!(next_gen2, 2);
    assert_eq!(
        aof.path(),
        crate::aof::generation_path(&dir.path().join("test.aof"), 2)
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::start_rewrite -- --nocapture`
Expected: FAIL to compile — `start_rewrite` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
// crates/server/src/dispatcher.rs — directly above `handle_save`
/// The lock-protected first half of a rewrite: reads the current generation, snapshots the
/// engine at offset 0 (this snapshot will pair with a brand-new, currently-empty next-generation
/// AOF file), and rotates `aof` onto that new file — all under `lock_for_ordering()`, the same
/// lock every write command already holds around "mutate, then log", so no concurrent append can
/// land between the snapshot and the rotation. Returns the new generation number and the
/// snapshot bytes still to be written to disk, done by the caller outside this lock. See the
/// design spec's "Decision: `BGREWRITEAOF` command", step 1.
fn start_rewrite(
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> std::io::Result<(u64, Vec<u8>)> {
    let _order_guard = aof.lock_for_ordering();
    let current_gen = crate::aof::read_generation(replication.snapshot_path())?;
    let next_gen = current_gen + 1;
    let bytes = replication.engine().snapshot(0);
    aof.rotate_to(&crate::aof::generation_path(aof.base_path(), next_gen))?;
    Ok((next_gen, bytes))
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::start_rewrite -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill to commit `crates/server/src/dispatcher.rs`.

---

### Task 2: `handle_bgrewriteaof` — snapshot write + manifest commit

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `start_rewrite` (Task 1); `write_snapshot_atomically` (existing, same file); `crate::aof::write_generation_atomically` (plan 01).
- Produces: `fn handle_bgrewriteaof(aof: &AofWriter, replication: &ReplicationHandle) -> Frame` — plan 04's dispatcher interception calls this; plan 06's integration tests call it via a real `BGREWRITEAOF` command.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
#[test]
fn handle_bgrewriteaof_commits_a_readable_generation_1() {
    let engine = std::sync::Arc::new(Engine::new());
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    let (dir, aof) = test_aof();
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

    let reply = handle_bgrewriteaof(&aof, &replication);

    assert_eq!(reply, Frame::Simple("OK".into()));
    assert_eq!(crate::aof::read_generation(&snapshot_path).unwrap(), 1);
    let bytes = std::fs::read(crate::aof::generation_path(&snapshot_path, 1)).unwrap();
    let loaded = Engine::new();
    loaded.load_snapshot(&bytes).unwrap();
    assert_eq!(
        loaded.get(b"k"),
        Some(Value::String(Bytes::from_static(b"v")))
    );
}

#[test]
fn handle_bgrewriteaof_does_not_leave_a_tmp_snapshot_file_behind() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

    handle_bgrewriteaof(&aof, &replication);

    let gen1_snapshot = crate::aof::generation_path(&snapshot_path, 1);
    let mut tmp = gen1_snapshot.into_os_string();
    tmp.push(".tmp");
    assert!(!std::path::Path::new(&tmp).exists());
}

#[test]
fn a_second_bgrewriteaof_advances_to_generation_2() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

    handle_bgrewriteaof(&aof, &replication);
    let reply = handle_bgrewriteaof(&aof, &replication);

    assert_eq!(reply, Frame::Simple("OK".into()));
    assert_eq!(crate::aof::read_generation(&snapshot_path).unwrap(), 2);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::handle_bgrewriteaof -- --nocapture && cargo test -p rocket-mem --lib dispatcher::tests::a_second_bgrewriteaof -- --nocapture`
Expected: FAIL to compile — `handle_bgrewriteaof` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
// crates/server/src/dispatcher.rs — directly below `start_rewrite`
/// `BGREWRITEAOF`: discards AOF bytes a snapshot has already made obsolete, without the
/// crash-unsafety a naive in-place truncation has. See the design spec's "Decision:
/// `BGREWRITEAOF` command" for the full protocol and why each step is ordered this way.
fn handle_bgrewriteaof(
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> Frame {
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
    if let Err(e) = crate::aof::write_generation_atomically(replication.snapshot_path(), next_gen)
    {
        return Frame::Error(format!("ERR failed to commit AOF rewrite: {e}"));
    }

    Frame::Simple("OK".into())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher:: -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill to commit `crates/server/src/dispatcher.rs`.

---

### Task 3: old-generation cleanup

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `crate::aof::generation_path` (plan 01); `AofWriter::base_path` (plan 02).
- Produces: no new public interface — extends `handle_bgrewriteaof`'s existing behavior.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
#[test]
fn handle_bgrewriteaof_deletes_the_old_generations_files_after_committing() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let aof_path = dir.path().join("test.aof");
    let snapshot_path = dir.path().join("test.snapshot");
    write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n"); // generation 0's AOF has content
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

    handle_bgrewriteaof(&aof, &replication);

    assert!(!aof_path.exists()); // generation 0's AOF, now superseded, is gone
    assert!(!snapshot_path.exists()); // generation 0's snapshot never existed, but must not error
    assert!(crate::aof::generation_path(&aof_path, 1).exists()); // rotated-to file remains
    assert!(crate::aof::generation_path(&snapshot_path, 1).exists()); // committed snapshot remains
}

#[test]
fn handle_bgrewriteaof_cleanup_deletes_the_immediately_prior_generation_only() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let aof_path = dir.path().join("test.aof");
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

    handle_bgrewriteaof(&aof, &replication); // -> generation 1
    handle_bgrewriteaof(&aof, &replication); // -> generation 2, deletes generation 1

    assert!(!crate::aof::generation_path(&aof_path, 1).exists());
    assert!(!crate::aof::generation_path(&snapshot_path, 1).exists());
    assert!(crate::aof::generation_path(&aof_path, 2).exists());
    assert!(crate::aof::generation_path(&snapshot_path, 2).exists());
}
```

If `dispatcher.rs`'s test module doesn't already have a `write_raw` helper, add one identical to `aof.rs`'s (used only by these two tests): `fn write_raw(path: &std::path::Path, bytes: &[u8]) { use std::io::Write; std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap().write_all(bytes).unwrap(); }`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::handle_bgrewriteaof_deletes -- --nocapture && cargo test -p rocket-mem --lib dispatcher::tests::handle_bgrewriteaof_cleanup -- --nocapture`
Expected: FAIL — the old generation's files still exist (no cleanup implemented yet).

- [ ] **Step 3: Implement**

```rust
// crates/server/src/dispatcher.rs — inside `handle_bgrewriteaof`, replacing its final line
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher:: -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --lib dispatcher::`
Expected: all green.

Use the `1-git-commit` skill to commit `crates/server/src/dispatcher.rs`.

---

## Next plan

Continue with [`04-wire-bgrewriteaof-into-dispatcher.md`](./04-wire-bgrewriteaof-into-dispatcher.md), which makes `handle_bgrewriteaof` (built in this plan) reachable from a real `BGREWRITEAOF` command and ACL-gated. If `05-generation-aware-recovery.md` hasn't been done yet, it can be picked up in parallel with `04` — it only depends on `01`, not on this plan.
