# Per-Shard AOF Ordering Locks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close out the per-shard AOF ordering work by adding the five correctness tests the spec requires and proving the throughput win it predicts — the locking mechanism itself is already built.

**Architecture:** `AofWriter` already holds `order: Vec<Mutex<()>>` (one guard per engine shard) and exposes `lock_shards(&[usize])` / `lock_all_shards()`, which sort and deduplicate to enforce ascending acquisition. Every call site is already wired. What is missing is the test suite that makes the three guarantees enforceable rather than incidental, plus the benchmark that confirms the predicted 3.10x.

**Tech Stack:** Rust 2021, `std::sync::Mutex`, `std::thread`, `tempfile`, `tokio` (integration tests only), `redis-benchmark` via `scripts/benchmark.sh`.

**Spec:** [`../specs/2026-09-08-per-shard-aof-ordering-spec.md`](../specs/2026-09-08-per-shard-aof-ordering-spec.md)

## Implementation status at plan time

Verified against the working tree on 2026-09-08. **The mechanism is done.** Do not re-implement it:

| Spec item | Status | Location |
|---|---|---|
| `order: Vec<Mutex<()>>` sized from shard count | Done | `crates/server/src/aof.rs:116`, built at `:217` from `engine::SHARD_COUNT` |
| `lock_shards` (sort + dedup + ascending) | Done | `crates/server/src/aof.rs:329` |
| `lock_all_shards` | Done | `crates/server/src/aof.rs:350` |
| `#[must_use]` on both accessors | Done | `crates/server/src/aof.rs:326`, `:349` |
| `Store::shard_index` / `Engine::shard_index` | Done | `crates/engine/src/store.rs:42`, `crates/engine/src/engine.rs:99` |
| `command_keys` reused for shard selection | Done | `crates/server/src/dispatcher.rs:1363` |
| `dispatch_and_log_inner` per-shard locking + all-shards fallback | Done | `crates/server/src/dispatcher.rs:3055-3064` |
| `handle_save` takes all guards | Done | `crates/server/src/dispatcher.rs:2667` |
| `handle_bgrewriteaof` takes all guards | Done | `crates/server/src/dispatcher.rs:2581` |
| `serve_replica` takes all guards | Done | `crates/server/src/connection.rs:305` |
| Follower apply loop takes all guards | Done | `crates/server/src/replication.rs:717` |
| Spec test 2 — disjoint shards proceed concurrently | Done | `crates/server/src/aof.rs:1062` `guards_for_different_shards_do_not_block_each_other` |
| Spec test 5 — deadlock under opposite orders | Done | `crates/server/src/aof.rs:1088` `overlapping_multi_key_acquisitions_do_not_deadlock` |
| Spec test 1 — per-key ordering survives, end to end | **Missing** | Task 2 |
| Spec test 3 — snapshot consistency under load | **Missing** | Task 3 |
| Spec test 4 — multi-key atomicity vs a snapshot walk | **Missing** | Task 4 |
| Risk mitigation — every write command yields a key | **Missing** | Task 1 |
| Expected-result measurement | **Missing** | Task 5 |

Note `crates/server/tests/replication.rs:62` `snapshot_plus_tail_recovery_reconstructs_identical_state_to_full_aof_replay` looks like spec test 3 but is **not** — it is a sequential recovery benchmark with no concurrency. It does not cover job 2.

## Global Constraints

- `cargo test --workspace`, `cargo fmt --all -- --check`, and `cargo clippy --workspace --all-targets -- -D warnings` gate every commit. Clippy is strict — no warnings at all, including dead code, and it lints test code too.
- The locking primitive stays `std::sync::Mutex`. Poison is recovered from, never propagated: the guard is held across arbitrary command dispatch, so a panicking handler must not become a permanent server-wide write outage.
- `#[must_use]` stays on `lock_shards` and `lock_all_shards`.
- Guards are **only ever** acquired through `lock_shards` / `lock_all_shards`. No test or call site may touch `order` directly — the ascending-index rule lives in exactly one place and must stay there.
- The engine's own per-shard `RwLock`s are untouched. These ordering guards sit above them.
- No wire-protocol change, no command-semantics change, no weakening of any durability or replication guarantee.
- Any re-measurement uses `scripts/benchmark.sh`, which passes `-r`. A single-key benchmark cannot show this change and must not be used to judge it.
- Comments use easy, short, full sentences ending in a punctuation mark. No emojis.

## File Structure

- `crates/server/src/dispatcher.rs` — Task 1's static invariant test, in the existing `mod tests`. This is where `key_spec` and `WRITE_COMMANDS`'s consumer both live, so the guard belongs beside them.
- `crates/server/src/aof.rs` — Tasks 2, 3 and 4's concurrency tests, in the existing `mod tests` alongside the four guard tests already there. They need `AofWriter` internals and the existing `test_aof`-style setup.
- `docs/superpowers/specs/2026-09-08-per-shard-aof-ordering-spec.md` — Task 5 updates the status line and records measured numbers.

No new files. No production-code changes are expected; if a test fails, that is a real defect and the fix goes in the file that owns the invariant.

---

### Task 1: Write-command key-coverage guard

The spec's third risk: `command_keys` and `extract_write_command_name` derive their answers independently (`key_spec` versus the `WRITE_COMMANDS` table), so nothing structurally forces them to agree. The all-shards fallback contains the damage, but silently degrading to a global guard is exactly the 3x regression this work exists to remove. This test turns the agreement into an enforced invariant.

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (add to `mod tests`)

**Interfaces:**
- Consumes: `crate::aof::WRITE_COMMANDS: &[&str]` (`crates/server/src/aof.rs:399`); `key_spec(name: &str) -> KeySpec` (`crates/server/src/dispatcher.rs:1338`); `KeySpec::None` (`:1324`).
- Produces: nothing. Pure regression guard.

`KeySpec` derives nothing, so compare with `matches!` rather than `==`.

- [ ] **Step 1: Write the test**

Add to `mod tests` in `crates/server/src/dispatcher.rs`, next to the other `command_keys` tests (around line 9198):

```rust
    /// Every write command must enumerate at least one key, or `dispatch_and_log_inner` silently
    /// falls back to locking all sixteen shards for it -- correct, but it throws away the entire
    /// point of per-shard ordering. `WRITE_COMMANDS` and `key_spec` derive their answers
    /// independently, so nothing but this test keeps them in step.
    #[test]
    fn every_write_command_enumerates_at_least_one_key() {
        for name in crate::aof::WRITE_COMMANDS {
            assert!(
                !matches!(key_spec(name), KeySpec::None),
                "{name} is in WRITE_COMMANDS but key_spec reports no keys, so every write of it \
                 would lock all shards. Either give it a KeySpec, or add it to this test's \
                 deliberate-all-shards list with a comment explaining why."
            );
        }
    }
```

- [ ] **Step 2: Run it**

Run: `cargo test -p rocket-mem --lib every_write_command_enumerates_at_least_one_key`

Expected: **PASS.** The invariant holds today — this is a regression guard, not a red-green cycle. Do not fake a failure.

- [ ] **Step 3: Prove the guard actually bites**

A guard that cannot fail is worthless. Temporarily add `"PING"` to the end of `WRITE_COMMANDS` in `crates/server/src/aof.rs:399`, then re-run the test.

Expected: FAIL with `PING is in WRITE_COMMANDS but key_spec reports no keys`.

**Then revert that edit** — `git checkout crates/server/src/aof.rs` is wrong here because that file has other uncommitted work, so remove the `"PING"` line by hand and confirm with `git diff crates/server/src/aof.rs` that only your intended changes remain.

- [ ] **Step 4: Re-run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p rocket-mem --lib every_write_command_enumerates_at_least_one_key
```

Expected: clean, and the test passes.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill to generate the message. Stage only:

```bash
git add crates/server/src/dispatcher.rs
```

---

### Task 2: Per-key ordering survives, end to end

Spec test 1, and spec job 1. `aof.rs:1021` already proves one shard's guard serializes its holders, but that tests the primitive in isolation. This proves the property the guarantee is actually about: after concurrent writers hammer one key, replaying the AOF reproduces the value that was committed, not some earlier one.

**Files:**
- Modify: `crates/server/src/aof.rs` (add to `mod tests`)

**Interfaces:**
- Consumes: `AofWriter::open(&Path, FsyncPolicy) -> io::Result<AofWriter>`; `AofWriter::fsync()`; `aof::recover(aof_path: &Path, snapshot_path: &Path) -> io::Result<engine::Engine>` (`crates/server/src/aof.rs:591`); `crate::dispatcher::{dispatch_and_log, Session}`; `crate::replication::ReplicationHandle::new(Arc<Engine>, PathBuf)` (`crates/server/src/replication.rs:207`); `engine::Engine::get(&[u8]) -> Option<Value>`.
- Produces: nothing.

All eight writers target one key, so they all contend for one shard — this is the case per-shard locking does *not* speed up, and must not break.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/server/src/aof.rs`, after `overlapping_multi_key_acquisitions_do_not_deadlock`:

```rust
    /// Job 1 of the ordering guard, end to end: the AOF's append order must match the order
    /// mutations committed in, so a replay reproduces the value that was actually committed.
    /// One key means one shard, so every writer here contends -- that is the point.
    #[test]
    fn concurrent_writes_to_one_key_replay_to_the_committed_value() {
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("ordering.aof");
        let aof = Arc::new(AofWriter::open(&aof_path, FsyncPolicy::Never).unwrap());
        let engine = Arc::new(engine::Engine::new());
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            dir.path().join("ordering.snapshot"),
        ));

        let mut writers = Vec::new();
        for w in 0..8 {
            let engine = Arc::clone(&engine);
            let aof = Arc::clone(&aof);
            let replication = Arc::clone(&replication);
            writers.push(std::thread::spawn(move || {
                for i in 0..250 {
                    let value = format!("w{w}-{i}");
                    let frame = protocol::Frame::Array(vec![
                        protocol::Frame::Bulk(bytes::Bytes::from_static(b"SET")),
                        protocol::Frame::Bulk(bytes::Bytes::from_static(b"hot")),
                        protocol::Frame::Bulk(bytes::Bytes::from(value)),
                    ]);
                    crate::dispatcher::dispatch_and_log(
                        &engine,
                        &aof,
                        &replication,
                        frame,
                        &crate::dispatcher::Session::new(),
                        1,
                    );
                }
            }));
        }
        for w in writers {
            w.join().unwrap();
        }
        aof.fsync().unwrap();

        // Replaying the log must land on whatever the engine actually holds. If an append ever
        // overtook the mutation it logged, the last line of the AOF would be some other writer's
        // value and these would differ.
        let replayed = recover(&aof_path, &dir.path().join("absent.snapshot")).unwrap();
        assert_eq!(
            replayed.get(b"hot"),
            engine.get(b"hot"),
            "AOF replay diverged from committed state"
        );
    }
```

- [ ] **Step 2: Run it**

Run: `cargo test -p rocket-mem --lib concurrent_writes_to_one_key_replay_to_the_committed_value`

Expected: **PASS**, because the mechanism is already correct. If it fails, you have found a real ordering defect — stop and investigate before touching anything else.

- [ ] **Step 3: Prove the test detects the defect it exists for**

Temporarily change `crates/server/src/dispatcher.rs:3055` so the guard is dropped immediately instead of held, by appending `;` semantics — concretely, replace the binding `let _order_guard = write_name.as_ref().map(|_| {` with `let _ = write_name.as_ref().map(|_| {`. That releases the guards at the end of the statement, which is exactly the bug `#[must_use]` guards against.

Run the test 5 times: `for i in 1 2 3 4 5; do cargo test -p rocket-mem --lib concurrent_writes_to_one_key_replay_to_the_committed_value; done`

Expected: at least one FAIL with `AOF replay diverged from committed state`. If all five pass, raise the writer count to 16 and the iteration count to 1000 until it fails — a test that cannot detect the defect is not worth committing.

**Then revert the dispatcher edit by hand** and confirm with `git diff crates/server/src/dispatcher.rs`.

- [ ] **Step 4: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p rocket-mem --lib
```

Expected: all clean.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill. Stage only `crates/server/src/aof.rs`.

---

### Task 3: Snapshot consistency under a concurrent write load

Spec test 3, and spec job 2 — the guarantee the spec flags as easiest to miss and which has no coverage today. `handle_save` holds all guards across "read the AOF offset, then walk the keyspace". If that cut is not point-in-time, recovery re-applies commands the snapshot already contains. Harmless for `SET`; wrong for `INCR`. **This test must use `INCR`,** because an idempotent command cannot detect the bug.

**Files:**
- Modify: `crates/server/src/aof.rs` (add to `mod tests`)

**Interfaces:**
- Consumes: everything Task 2 consumes, plus `crate::aof::read_generation`, `crate::aof::generation_path`, and `engine::Engine::load_snapshot(&[u8]) -> Result<u64, SnapshotError>` (`crates/engine/src/engine.rs:122`).
- Produces: nothing.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/server/src/aof.rs`:

```rust
    /// Job 2: `SAVE` must produce a point-in-time cut of (offset, keyspace). If the offset is read
    /// at one instant and the walk happens at another, recovery replays commands the snapshot
    /// already contains -- which double-counts every non-idempotent command. INCR is used
    /// deliberately: with SET this bug is invisible.
    #[test]
    fn a_save_racing_writes_produces_a_replayable_point_in_time_cut() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("cut.aof");
        let snapshot_path = dir.path().join("cut.snapshot");
        let aof = Arc::new(AofWriter::open(&aof_path, FsyncPolicy::Never).unwrap());
        let engine = Arc::new(engine::Engine::new());
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            snapshot_path.clone(),
        ));

        let stop = Arc::new(AtomicBool::new(false));
        let mut writers = Vec::new();
        for w in 0..4 {
            let engine = Arc::clone(&engine);
            let aof = Arc::clone(&aof);
            let replication = Arc::clone(&replication);
            let stop = Arc::clone(&stop);
            writers.push(std::thread::spawn(move || {
                let key = format!("counter{w}");
                let mut issued = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let frame = protocol::Frame::Array(vec![
                        protocol::Frame::Bulk(bytes::Bytes::from_static(b"INCR")),
                        protocol::Frame::Bulk(bytes::Bytes::from(key.clone())),
                    ]);
                    crate::dispatcher::dispatch_and_log(
                        &engine,
                        &aof,
                        &replication,
                        frame,
                        &crate::dispatcher::Session::new(),
                        1,
                    );
                    issued += 1;
                }
                issued
            }));
        }

        // Fire SAVEs while the counters are climbing, so at least one cut is taken mid-flight.
        for _ in 0..20 {
            crate::dispatcher::dispatch_and_log(
                &engine,
                &aof,
                &replication,
                protocol::Frame::Array(vec![protocol::Frame::Bulk(bytes::Bytes::from_static(
                    b"SAVE",
                ))]),
                &crate::dispatcher::Session::new(),
                1,
            );
        }

        stop.store(true, Ordering::Relaxed);
        for w in writers {
            w.join().unwrap();
        }
        aof.fsync().unwrap();

        // The AOF alone is the reference: every INCR, replayed once. Snapshot-plus-tail must land
        // on exactly the same counters. A non-atomic cut double-counts and these diverge.
        let from_aof_only = recover(&aof_path, &dir.path().join("absent.snapshot")).unwrap();
        let from_snapshot_and_tail = recover(&aof_path, &snapshot_path).unwrap();
        for w in 0..4 {
            let key = format!("counter{w}");
            assert_eq!(
                from_snapshot_and_tail.get(key.as_bytes()),
                from_aof_only.get(key.as_bytes()),
                "snapshot+tail diverged from full replay at {key}"
            );
        }
    }
```

- [ ] **Step 2: Run it**

Run: `cargo test -p rocket-mem --lib a_save_racing_writes_produces_a_replayable_point_in_time_cut`

Expected: **PASS.** A failure is a genuine job-2 defect — investigate rather than adjusting the test.

- [ ] **Step 3: Prove it detects a non-atomic cut**

In `crates/server/src/dispatcher.rs:2667`, temporarily move the offset read outside the guard by changing:

```rust
    let bytes = {
        let _order_guard = aof.lock_all_shards();
        let offset = match aof.current_offset() {
```

to read the offset *before* taking the guard:

```rust
    let bytes = {
        let offset = match aof.current_offset() {
```

...and move the `let _order_guard = aof.lock_all_shards();` line to just after the offset `match` block. Run the test 5 times.

Expected: at least one FAIL with `snapshot+tail diverged from full replay`. **Then revert by hand** and confirm with `git diff crates/server/src/dispatcher.rs`.

- [ ] **Step 4: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill. Stage only `crates/server/src/aof.rs`.

---

### Task 4: Multi-key atomicity against a snapshot walk

Spec test 4, and spec job 3 — today protected only by a comment. `Store::snapshot_entries` walks shard by shard. An `MSET` spanning two shards must never be observed half-applied by that walk.

**Files:**
- Modify: `crates/server/src/aof.rs` (add to `mod tests`)

**Interfaces:**
- Consumes: everything Task 3 consumes, plus `engine::Engine::shard_index(&[u8]) -> usize` (`crates/engine/src/engine.rs:99`).
- Produces: nothing.

The two keys must be picked so they genuinely land on different shards — hardcoding names and hoping is how this test silently stops testing anything.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/server/src/aof.rs`:

```rust
    /// Job 3: `Store::snapshot_entries` walks shard by shard, so a multi-key write spanning
    /// shards must be atomic with respect to that walk. Both halves of each MSET carry the same
    /// generation number, so a half-applied snapshot is directly observable.
    #[test]
    fn an_mset_spanning_shards_is_never_snapshotted_half_applied() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("mset.aof");
        let snapshot_path = dir.path().join("mset.snapshot");
        let aof = Arc::new(AofWriter::open(&aof_path, FsyncPolicy::Never).unwrap());
        let engine = Arc::new(engine::Engine::new());
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            snapshot_path.clone(),
        ));

        // Pick a pair that actually straddles two shards. Asserting it rather than assuming it
        // keeps this test meaningful if the hash or the shard count ever changes.
        let (left, right) = (0..1000)
            .map(|i| (format!("pair-a-{i}"), format!("pair-b-{i}")))
            .find(|(a, b)| engine.shard_index(a.as_bytes()) != engine.shard_index(b.as_bytes()))
            .expect("no key pair landed on different shards");

        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let engine = Arc::clone(&engine);
            let aof = Arc::clone(&aof);
            let replication = Arc::clone(&replication);
            let stop = Arc::clone(&stop);
            let (left, right) = (left.clone(), right.clone());
            std::thread::spawn(move || {
                let mut generation = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    generation += 1;
                    let value = bytes::Bytes::from(generation.to_string());
                    let frame = protocol::Frame::Array(vec![
                        protocol::Frame::Bulk(bytes::Bytes::from_static(b"MSET")),
                        protocol::Frame::Bulk(bytes::Bytes::from(left.clone())),
                        protocol::Frame::Bulk(value.clone()),
                        protocol::Frame::Bulk(bytes::Bytes::from(right.clone())),
                        protocol::Frame::Bulk(value),
                    ]);
                    crate::dispatcher::dispatch_and_log(
                        &engine,
                        &aof,
                        &replication,
                        frame,
                        &crate::dispatcher::Session::new(),
                        1,
                    );
                }
            })
        };

        for _ in 0..50 {
            crate::dispatcher::dispatch_and_log(
                &engine,
                &aof,
                &replication,
                protocol::Frame::Array(vec![protocol::Frame::Bulk(bytes::Bytes::from_static(
                    b"SAVE",
                ))]),
                &crate::dispatcher::Session::new(),
                1,
            );

            // Read back the snapshot SAVE just wrote and check the pair agrees. Loading it into a
            // fresh engine deliberately skips the AOF tail: the snapshot alone must be coherent.
            let gen = read_generation(&snapshot_path).unwrap();
            let written = generation_path(&snapshot_path, gen);
            if let Ok(raw) = std::fs::read(&written) {
                let restored = engine::Engine::new();
                restored.load_snapshot(&raw).unwrap();
                let (l, r) = (
                    restored.get(left.as_bytes()),
                    restored.get(right.as_bytes()),
                );
                // Both absent is fine -- the snapshot predates the first MSET.
                if l.is_some() || r.is_some() {
                    assert_eq!(l, r, "snapshot caught an MSET half-applied across shards");
                }
            }
        }

        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }
```

- [ ] **Step 2: Run it**

Run: `cargo test -p rocket-mem --lib an_mset_spanning_shards_is_never_snapshotted_half_applied`

Expected: **PASS.**

- [ ] **Step 3: Prove it detects a half-applied write**

Temporarily change `crates/server/src/dispatcher.rs:3061` so multi-key commands lock only their *first* key's shard rather than all of them — replace `aof.lock_shards(&shards)` with `aof.lock_shards(&shards[..1])`. That is precisely the "multi-key command is not atomic across shards" bug.

Run 5 times. Expected: at least one FAIL with `snapshot caught an MSET half-applied across shards`. If it never fails, raise the SAVE loop from 50 to 200.

**Then revert by hand** and confirm with `git diff crates/server/src/dispatcher.rs`.

- [ ] **Step 4: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill. Stage only `crates/server/src/aof.rs`.

---

### Task 5: Measure the result and close the spec

The spec predicts pipelined 3B `SET` moves from 2.78x behind `redis-server` to roughly 1.0–1.3x. That prediction is untested. Until it is measured, nobody knows whether the mechanism delivered.

**Files:**
- Modify: `docs/superpowers/specs/2026-09-08-per-shard-aof-ordering-spec.md` (status line and a measured-results section)

**Interfaces:**
- Consumes: `scripts/benchmark.sh`.
- Produces: recorded numbers for future comparison.

- [ ] **Step 1: Build release**

```bash
cargo build --release --workspace
```

- [ ] **Step 2: Run the benchmark**

```bash
./scripts/benchmark.sh
```

Use this script and nothing else. It passes `-r` (multi-key), which this change requires — a single-key run cannot show any improvement, because one key is one shard. It also matches durability settings between rocket-mem and the system `redis-server`; hand-rolled `redis-benchmark` invocations flatter Redis, because rocket-mem cannot disable its AOF.

Take the **median of four runs**, matching the spec's own methodology.

- [ ] **Step 3: Record the numbers in the spec**

Add a `## Measured result` section to `docs/superpowers/specs/2026-09-08-per-shard-aof-ordering-spec.md`, directly after `## Expected result`, with a table of the pipelined 3B `SET` and 1KB `SET` rows: throughput before, throughput now, and ratio against `redis-server`. State plainly whether the 1.0–1.3x prediction held. If it did not, say by how much and stop — do not tune anything in this task.

- [ ] **Step 4: Update the spec status**

Change line 4 of the spec from:

```markdown
**Status:** Proposed — not yet approved
```

to `Implemented` plus the date, and add one sentence noting the mechanism landed before this plan was written, with the tests following in tasks 1-4.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill. Stage only the spec file.

---

## Self-Review

**Spec coverage.** Every numbered guarantee and test in the spec maps to a task: job 1 → Task 2; job 2 → Task 3; job 3 → Task 4; spec tests 2 and 5 already exist and are cited in the status table; the `command_keys`/`extract_write_command_name` risk → Task 1; the expected-result claim → Task 5. The mechanism items (`Vec<Mutex<()>>`, `lock_shards`, `shard_index`, every call site) are all implemented already and are listed with file:line rather than given tasks — writing tasks for finished work would waste an executor's time.

**Placeholders.** None. Every test step carries complete, compilable code; every mutation step names the exact line and the exact edit; every revert step says how to confirm it.

**Type consistency.** `lock_shards(&[usize])`, `lock_all_shards()`, `Engine::shard_index(&[u8]) -> usize`, `aof::recover(&Path, &Path) -> io::Result<Engine>`, `Engine::load_snapshot(&[u8])`, and `ReplicationHandle::new(Arc<Engine>, PathBuf)` are used identically everywhere they appear, and each was read from the source rather than assumed.

**One deviation from strict TDD, stated deliberately.** Tasks 1-4 are expected to pass on first run, because the implementation already exists. A red-green cycle is impossible without inventing a failure. Each task therefore substitutes a **mutation step**: break the specific invariant the test guards, watch the test fail, revert. That is what red-green buys — evidence the test can detect the defect — and it is the honest way to get it here. Do not skip those steps; a test that has never failed has never been tested.
