# Concurrency & End-to-End Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** prove the two properties the design spec promises but no single earlier plan tests end-to-end: a write landing in the narrow window between rotation and the manifest commit survives recovery, and a full `BGREWRITEAOF` round trip through the real dispatcher reconstructs correct state after old-generation cleanup.

**Architecture:** two tests only, no production code — everything they exercise (`start_rewrite`, `handle_bgrewriteaof`, `recover`, the `BGREWRITEAOF` command) already exists after plans 01–05. Both live in `crates/server/src/dispatcher.rs`'s test module, alongside `handle_bgrewriteaof`'s other tests.

**Tech Stack:** none new.

**Spec:** [`../../specs/2026-09-06-aof-compaction-design.md`](../../specs/2026-09-06-aof-compaction-design.md), "Testing strategy" — the "concurrent writes during rewrite" and "`BGREWRITEAOF` end-to-end" items.

## Global Constraints

- No production code changes in this plan. If either test fails, the bug is in plans 01–05, not something to patch here.
- Depends on plans 01–05 all being merged first.

---

### Task 1: A write between rotation and manifest commit survives recovery

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `start_rewrite` (plan 03), `write_snapshot_atomically` (existing), `crate::aof::write_generation_atomically` (plan 01), `crate::aof::recover` (plan 05).
- Produces: nothing new.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
#[test]
fn a_write_landing_between_rotation_and_manifest_commit_survives_recovery() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let aof_path = dir.path().join("test.aof");
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

    dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"SET", b"before", b"1"]),
        &Session::new(),
        1,
    );

    // Manually walk `handle_bgrewriteaof`'s steps, interleaving a write right after rotation
    // lands but before the new generation's snapshot/manifest are durable -- exactly the window
    // a genuinely concurrent writer could land in, since rotation is the only part protected by
    // `lock_for_ordering()`.
    let (next_gen, bytes) = start_rewrite(&aof, &replication).unwrap();

    dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"SET", b"during", b"2"]),
        &Session::new(),
        1,
    );

    let new_snapshot_path = crate::aof::generation_path(replication.snapshot_path(), next_gen);
    write_snapshot_atomically(&new_snapshot_path, &bytes).unwrap();
    crate::aof::write_generation_atomically(replication.snapshot_path(), next_gen).unwrap();
    aof.fsync().unwrap();

    let recovered = crate::aof::recover(&aof_path, &snapshot_path).unwrap();
    assert_eq!(
        recovered.get(b"before"),
        Some(Value::String(Bytes::from_static(b"1")))
    ); // captured in the snapshot
    assert_eq!(
        recovered.get(b"during"),
        Some(Value::String(Bytes::from_static(b"2")))
    ); // captured in generation 1's AOF tail, replayed after the snapshot
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::a_write_landing_between -- --nocapture`
Expected: PASS already if plans 01–05 are all correctly implemented. If it fails, don't patch this test — find which earlier plan's guarantee actually broke (most likely: `start_rewrite`'s rotation isn't landing writes in the right file, or `recover`'s offset-0 replay against the new generation's snapshot is wrong).

- [ ] **Step 3: No production code change**

This test locks in a property that emerges from composing plans 01–05 correctly; nothing new to implement.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher:: -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill to commit `crates/server/src/dispatcher.rs`.

---

### Task 2: `BGREWRITEAOF` end-to-end through the real dispatcher

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `dispatch_and_log` with the real `BGREWRITEAOF` command (plan 04), `crate::aof::recover` (plan 05).
- Produces: nothing new.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
#[test]
fn bgrewriteaof_end_to_end_preserves_writes_before_and_after_the_rewrite() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let aof_path = dir.path().join("test.aof");
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

    dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"SET", b"before", b"1"]),
        &Session::new(),
        1,
    );

    let reply = dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"BGREWRITEAOF"]),
        &Session::new(),
        1,
    );
    assert_eq!(reply, Frame::Simple("OK".into()));

    dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"SET", b"after", b"2"]),
        &Session::new(),
        1,
    );
    aof.fsync().unwrap();

    let recovered = crate::aof::recover(&aof_path, &snapshot_path).unwrap();
    assert_eq!(
        recovered.get(b"before"),
        Some(Value::String(Bytes::from_static(b"1")))
    );
    assert_eq!(
        recovered.get(b"after"),
        Some(Value::String(Bytes::from_static(b"2")))
    );

    // Generation 0's original files are cleaned up once generation 1 is committed.
    assert!(!aof_path.exists());
    assert!(!snapshot_path.exists());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::bgrewriteaof_end_to_end -- --nocapture`
Expected: PASS already if plans 01–05 are all correctly implemented — same rationale as Task 1.

- [ ] **Step 3: No production code change**

Nothing new to implement.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --lib dispatcher::`
Expected: all green.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill to commit `crates/server/src/dispatcher.rs`.

---

## Next plan

Continue with [`07-documentation.md`](./07-documentation.md) — the final plan for this feature. It only touches `README.md` and `docs/command-compatibility.md`, documenting the now-complete, now-tested `BGREWRITEAOF` command.
