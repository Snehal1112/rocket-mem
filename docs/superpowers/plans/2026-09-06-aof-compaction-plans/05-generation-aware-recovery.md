# Generation-Aware Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `recover()` resolves the current generation before doing anything else, and the two crash scenarios the design spec's safety argument depends on are proven, not just asserted in prose.

**Architecture:** one three-line addition at the top of `crate::aof::recover` (`aof.rs:278-321`) — resolve the generation, shadow `aof_path`/`snapshot_path` with their generation-qualified equivalents, then run the function's existing body completely unchanged. The two crash-safety tests construct on-disk states directly (no real process kill), matching this file's own existing style (see `recover_with_a_snapshot_whose_offset_overshoots_the_aof_discards_it_and_replays_from_zero`).

**Tech Stack:** none new.

**Spec:** [`../../specs/2026-09-06-aof-compaction-design.md`](../../specs/2026-09-06-aof-compaction-design.md), "Decision: `recover()` changes" and "Testing strategy".

## Global Constraints

- Every existing `recover()` test must keep passing unmodified — they all operate at generation 0, where `generation_path(base, 0) == base`, so the function's behavior for them must be byte-for-byte identical to today.
- Depends only on plan 01 (`read_generation`/`generation_path`) — independent of plans 02–04.

---

### Task 1: Generation resolution in `recover()`

**Files:**
- Modify: `crates/server/src/aof.rs`

**Interfaces:**
- Consumes: `read_generation`, `generation_path` (plan 01, same file).
- Produces: no new public interface — `recover`'s existing signature (`pub fn recover(aof_path: &Path, snapshot_path: &Path) -> std::io::Result<engine::Engine>`) is unchanged.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/aof.rs — inside `mod tests`
#[test]
fn recover_reads_generation_1_once_the_manifest_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("test.aof");
    let snapshot_path = dir.path().join("test.snapshot");

    // Generation 0 exists but must be ignored once the manifest points past it.
    write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\nold\r\n$1\r\n1\r\n");

    // Generation 1 is complete and committed.
    let gen1_engine = Engine::new();
    gen1_engine.set(
        Bytes::from_static(b"new"),
        Value::String(Bytes::from_static(b"2")),
    );
    std::fs::write(
        generation_path(&snapshot_path, 1),
        gen1_engine.snapshot(0),
    )
    .unwrap();
    std::fs::write(generation_path(&aof_path, 1), b"").unwrap();
    write_generation_atomically(&snapshot_path, 1).unwrap();

    let engine = recover(&aof_path, &snapshot_path).unwrap();
    assert_eq!(
        engine.get(b"new"),
        Some(Value::String(Bytes::from_static(b"2")))
    ); // from generation 1
    assert_eq!(engine.get(b"old"), None); // generation 0 must be ignored once superseded
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --lib aof::tests::recover_reads_generation_1 -- --nocapture`
Expected: FAIL — today's `recover` has no manifest awareness, so it reads generation 0's files and returns `old` instead of `new`.

- [ ] **Step 3: Implement**

```rust
// crates/server/src/aof.rs — recover()'s new first three lines
pub fn recover(aof_path: &Path, snapshot_path: &Path) -> std::io::Result<engine::Engine> {
    let gen = read_generation(snapshot_path)?;
    let aof_path = &generation_path(aof_path, gen);
    let snapshot_path = &generation_path(snapshot_path, gen);

    let engine = engine::Engine::new();
    // ... the rest of the function's existing body is unchanged: it already only ever uses the
    // `aof_path`/`snapshot_path` names, which now refer to the resolved generation's files.
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib aof:: -- --nocapture`
Expected: all PASS, including every pre-existing `recover`/`replay` test — they all implicitly exercise generation 0, where `generation_path(base, 0) == base` makes the new first three lines a no-op.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill to commit `crates/server/src/aof.rs`.

---

### Task 2: Crash-safety — frozen before the manifest rename

**Files:**
- Modify: `crates/server/src/aof.rs`

**Interfaces:**
- Consumes: `generation_path` (plan 01), `recover` (Task 1).
- Produces: nothing new — a regression test locking in the design spec's central safety claim.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/aof.rs — inside `mod tests`
#[test]
fn recover_ignores_a_new_generation_whose_manifest_was_never_committed() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("test.aof");
    let snapshot_path = dir.path().join("test.snapshot");

    // Generation 0: committed history, exactly as if no rewrite had ever been attempted.
    write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");

    // Simulate a rewrite that got as far as writing generation 1's files but crashed before
    // the manifest commit -- this is the exact state `start_rewrite` + a snapshot write leave
    // on disk one line before `write_generation_atomically` runs.
    let gen1_engine = Engine::new();
    gen1_engine.set(
        Bytes::from_static(b"b"),
        Value::String(Bytes::from_static(b"orphaned")),
    );
    std::fs::write(
        generation_path(&snapshot_path, 1),
        gen1_engine.snapshot(0),
    )
    .unwrap();
    std::fs::write(generation_path(&aof_path, 1), b"").unwrap();
    // No manifest written -- this is the frozen crash point.

    let engine = recover(&aof_path, &snapshot_path).unwrap();
    assert_eq!(
        engine.get(b"a"),
        Some(Value::String(Bytes::from_static(b"1")))
    ); // from generation 0's AOF, completely untouched by the abandoned attempt
    assert_eq!(engine.get(b"b"), None); // generation 1 must be entirely ignored
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --lib aof::tests::recover_ignores_a_new_generation -- --nocapture`
Expected: PASS already if Task 1 is correctly implemented (`read_generation` returns 0 when no manifest exists, regardless of what generation-1 files happen to exist on disk). If it fails, Task 1's implementation is incomplete — fix Task 1, don't special-case this scenario here.

- [ ] **Step 3: No production code change**

This test exists to prove, not to drive, a behavior — `read_generation`'s "missing manifest ⇒ 0" default (plan 01, Task 1) is exactly what makes this safe, with no additional code.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib aof:: -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill to commit `crates/server/src/aof.rs`.

---

### Task 3: Crash-safety — frozen after the manifest rename, before cleanup

**Files:**
- Modify: `crates/server/src/aof.rs`

**Interfaces:**
- Consumes: `generation_path`, `write_generation_atomically` (plan 01), `recover` (Task 1).
- Produces: nothing new — a regression test locking in the other half of the design spec's safety claim.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/aof.rs — inside `mod tests`
#[test]
fn recover_uses_the_new_generation_once_committed_even_if_old_files_remain() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("test.aof");
    let snapshot_path = dir.path().join("test.snapshot");

    // Generation 0's files are still present on disk (cleanup hasn't run yet) but must be
    // ignored -- this is the exact state `handle_bgrewriteaof` leaves one line before its
    // best-effort `remove_file` calls run.
    write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\nold\r\n$1\r\n1\r\n");

    // Generation 1 is complete and committed.
    let gen1_engine = Engine::new();
    gen1_engine.set(
        Bytes::from_static(b"new"),
        Value::String(Bytes::from_static(b"2")),
    );
    std::fs::write(
        generation_path(&snapshot_path, 1),
        gen1_engine.snapshot(0),
    )
    .unwrap();
    std::fs::write(generation_path(&aof_path, 1), b"").unwrap();
    write_generation_atomically(&snapshot_path, 1).unwrap();

    let engine = recover(&aof_path, &snapshot_path).unwrap();
    assert_eq!(
        engine.get(b"new"),
        Some(Value::String(Bytes::from_static(b"2")))
    ); // from generation 1
    assert_eq!(engine.get(b"old"), None); // generation 0 ignored once the manifest points past it

    // The old files being merely unreferenced, not gone, must not change the outcome.
    assert!(aof_path.exists());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --lib aof::tests::recover_uses_the_new_generation -- --nocapture`
Expected: PASS already if Task 1 is correctly implemented — `read_generation` returns 1 once the manifest is committed, regardless of generation 0's files still being present. If it fails, Task 1's implementation is incomplete.

- [ ] **Step 3: No production code change**

Same rationale as Task 2 — this locks in a guarantee Task 1 already provides.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --lib aof::`
Expected: all green.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill to commit `crates/server/src/aof.rs`.

---

## Next plan

This plan only depends on `01`, so it can be done in parallel with `02`, `03`, and `04` rather than strictly after them. Once this plan **and** [`04-wire-bgrewriteaof-into-dispatcher.md`](./04-wire-bgrewriteaof-into-dispatcher.md) are both merged, continue with [`06-concurrency-and-end-to-end.md`](./06-concurrency-and-end-to-end.md), which needs the real `BGREWRITEAOF` command (from `04`) and generation-aware `recover()` (from this plan) together.
