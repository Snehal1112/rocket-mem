# AOF Blocking-I/O Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** stop `AofWriter::fsync`, `append_encoded`, and `rotate_to` from blocking whichever Tokio worker OS thread happens to call them, so a slow disk fsync (or a full write queue) no longer starves unrelated tasks on the same runtime.

**Architecture:** a single private helper, `run_blocking`, wraps the existing blocking `mpsc` `recv()`/`send()` calls inside those three methods. It runs the blocking closure via `tokio::task::block_in_place` when called from inside a Tokio runtime (freeing the worker thread for other tasks while it blocks), and falls back to calling the closure directly when there's no runtime context at all — which is every one of `aof.rs`'s ~48 existing synchronous unit tests, none of which run inside a Tokio runtime, so their behavior is unchanged by this fix.

**Tech Stack:** Rust, Tokio (`rt-multi-thread`, `sync`, `macros`, `time` features — all already enabled workspace-wide), `std::sync::mpsc`.

**Spec:** `docs/superpowers/specs/2026-09-07-redis-parity-perf-design.md` (Phase 0)

## Global Constraints

- No change to RESP/RMP wire behavior, command semantics, or durability guarantees — a write must be at least as durable after this fix as before.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must both pass clean before any commit (this project's CI gate, `.github/workflows/ci.yml`).
- `cargo test --workspace` (773 tests as of this plan) must pass after every step that changes non-test code.
- Comments in changed code: short, full sentences, ending in punctuation (this project's comment convention — see any existing doc comment in `aof.rs` for the house style).
- Every existing test in `crates/server/src/aof.rs` is a plain `#[test]` (not `#[tokio::test]`) and must keep passing unmodified — this plan must not need to touch any of them.

---

### Task 1: Add `run_blocking` and wire it into `AofWriter`'s three blocking methods

**Files:**
- Modify: `crates/server/src/aof.rs:189-193` (`fsync`), `:166-176` (`append_encoded`), `:229-235` (`rotate_to`)
- Test: `crates/server/src/aof.rs` (inside the existing `#[cfg(test)] mod tests` block starting at line 507)

**Interfaces:**
- Produces: `fn run_blocking<F, R>(f: F) -> R where F: FnOnce() -> R` — a private, module-level free function in `aof.rs`. Not `pub`; only `fsync`, `append_encoded`, `rotate_to`, and this task's own test call it (the test via `super::*`).
- Consumes: nothing new — uses `tokio::runtime::Handle::try_current()` and `tokio::task::block_in_place`, both already available (`tokio` is a direct, non-dev dependency of the `server` crate with the `rt-multi-thread` feature already enabled workspace-wide).

- [ ] **Step 1: Write the failing test proving the starvation bug exists**

Add this to `crates/server/src/aof.rs`'s existing `mod tests` block (anywhere among the other tests — e.g. right after the `use` statements at line 513):

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn run_blocking_does_not_starve_other_tasks_while_it_blocks() {
        // A single-worker-thread runtime, deliberately: with only one worker thread, any
        // blocking call that doesn't free it makes every other task provably unable to run
        // until the blocking call returns -- no timing luck needed to observe the difference.
        let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();

        let other_task = tokio::spawn(async move {
            start_rx.await.ok();
            done_tx.send(()).ok();
        });

        // Tell other_task it may proceed, then immediately enter the blocking region. We do
        // not yield ourselves between these two lines, so other_task can only actually run
        // (a) inside run_blocking, if it frees the worker thread, or (b) never, if it doesn't.
        start_tx.send(()).ok();

        run_blocking(|| std::thread::sleep(std::time::Duration::from_millis(150)));

        // A non-blocking check, still without this task having yielded once since start_tx.send
        // above: if other_task's `done` message is already here, it ran *during* the blocking
        // call, proving run_blocking freed the worker thread for it.
        assert!(
            done_rx.try_recv().is_ok(),
            "other_task did not complete while run_blocking was blocking this worker thread; \
             run_blocking starved the runtime instead of freeing it via block_in_place"
        );
        other_task.await.unwrap();
    }
```

- [ ] **Step 2: Run the test to verify it fails to compile**

Run: `cargo test -p server --lib aof::tests::run_blocking_does_not_starve_other_tasks_while_it_blocks`
Expected: compile error, `cannot find function `run_blocking` in this scope` — `run_blocking` doesn't exist yet.

- [ ] **Step 3: Implement `run_blocking`**

Add this above `impl AofWriter` in `crates/server/src/aof.rs` (near the other free functions like `encode_frame`, above the `pub struct AofWriter` block starting at line 49):

```rust
/// Runs `f`, freeing the current worker thread for other tasks while it blocks, if called from
/// inside a Tokio runtime. `AofWriter`'s ack-channel `recv()` and bounded-channel `send()` calls
/// block the calling OS thread for real I/O (a disk fsync, or backpressure from a full writer
/// queue) -- called directly from an async task, that freezes the whole worker thread, starving
/// every other task queued on it for the wait's duration. `tokio::task::block_in_place` is the
/// fix: it hands the runtime a replacement worker so other tasks keep running.
///
/// `block_in_place` requires an actual multi-threaded Tokio runtime context and panics without
/// one. Every existing synchronous unit test in this file calls `fsync`/`append_encoded`/
/// `rotate_to` with no runtime present at all, so `Handle::try_current()` gates the call: no
/// runtime means `f` runs directly, exactly as before this fix.
fn run_blocking<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(f)
    } else {
        f()
    }
}
```

- [ ] **Step 4: Run the test to verify it now passes**

Run: `cargo test -p server --lib aof::tests::run_blocking_does_not_starve_other_tasks_while_it_blocks`
Expected: PASS (1 passed).

- [ ] **Step 5: Wire `run_blocking` into `fsync`, `append_encoded`, and `rotate_to`**

Replace `fsync` (currently at `crates/server/src/aof.rs:189-193`):

```rust
    pub fn fsync(&self) -> std::io::Result<()> {
        run_blocking(|| {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            self.send(AofMsg::Flush(ack_tx))?;
            ack_rx.recv().map_err(writer_gone)?
        })
    }
```

Replace `append_encoded` (currently at `crates/server/src/aof.rs:166-176`) — note the `else` branch's plain `send` is wrapped too, since its own doc comment already documents it can block on a full queue:

```rust
    pub fn append_encoded(&self, bytes: Vec<u8>) -> std::io::Result<()> {
        run_blocking(|| {
            if self.policy == FsyncPolicy::Always {
                let (ack_tx, ack_rx) = mpsc::sync_channel(1);
                self.send(AofMsg::AppendAndFsync(bytes, ack_tx))?;
                // Two failure modes, flattened into one: the writer thread vanished (recv error),
                // or it ran and the write itself failed (the inner result).
                ack_rx.recv().map_err(writer_gone)?
            } else {
                self.send(AofMsg::Append(bytes))
            }
        })
    }
```

Replace `rotate_to` (currently at `crates/server/src/aof.rs:229-235`) — same blocking-recv shape as `fsync`, on the `BGREWRITEAOF` path:

```rust
    pub fn rotate_to(&self, new_path: &Path) -> std::io::Result<()> {
        run_blocking(|| {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            self.send(AofMsg::Rotate(new_path.to_path_buf(), ack_tx))?;
            ack_rx.recv().map_err(writer_gone)??;
            *self.path.lock().unwrap_or_else(|e| e.into_inner()) = new_path.to_path_buf();
            Ok(())
        })
    }
```

Leave every other method (`current_offset`, `path`, `base_path`, `policy`, `lock_for_ordering`, `lock_for_rewrite`, `send`, `open`, `open_at_generation`, `open_with_base`) untouched — `current_offset` already benefits automatically since it calls `self.fsync()?`.

- [ ] **Step 6: Run every existing test in `aof.rs` to confirm zero regressions**

Run: `cargo test -p server --lib aof::`
Expected: all ~48 pre-existing tests plus the new one pass (49+ passed, 0 failed). None of the pre-existing tests run inside a Tokio runtime, so `run_blocking`'s fallback path (`f()` called directly) is exactly what they exercised before this change — their behavior must be byte-for-byte identical.

- [ ] **Step 7: Run the full workspace test suite, clippy, and fmt**

Run: `cargo test --workspace`
Expected: all tests pass (773+ — the 772 pre-existing plus this task's new one; exact count may differ slightly if other work has landed since this plan was written).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean, zero warnings.

Run: `cargo fmt --all -- --check`
Expected: clean, no diff. If it reports a diff, run `cargo fmt --all` and re-check.

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/aof.rs
git commit -m "$(cat <<'EOF'
fix: stop AofWriter from blocking Tokio worker threads

fsync(), append_encoded(), and rotate_to() called blocking
std::sync::mpsc recv()/send() directly from async tasks
(periodic_fsync_loop, dispatch_and_log's write path), freezing
whichever worker thread ran them for the real fsync/backpressure
duration and starving every other task queued on it. A slow disk
fsync could stall unrelated GET/SET requests for hundreds of ms.

run_blocking() now routes these calls through
tokio::task::block_in_place when a Tokio runtime is present, so the
runtime hands off a replacement worker instead of freezing the
calling one. Outside a runtime (every existing synchronous unit
test in this file) it falls back to a direct call, unchanged.

See docs/superpowers/specs/2026-09-07-redis-parity-perf-design.md
(Phase 0) for the root-cause trace.
EOF
)"
```

---

## Self-Review Notes

- **Spec coverage:** implements the spec's "Decision: fix the stall bug first" section in full — both named blocking call sites (`fsync`'s `ack_rx.recv()`, `append_encoded`'s `self.send()`) are fixed, plus `rotate_to` (same pattern, same helper, not named in the spec's diagnosis but sharing the identical root cause on the `BGREWRITEAOF` path — free to fix alongside at no extra risk).
- **Placeholder scan:** none — every step has real, complete code.
- **Type consistency:** `run_blocking<F, R>(f: F) -> R where F: FnOnce() -> R` is defined once in Step 3 and used identically (a closure returning `std::io::Result<()>` or `std::io::Result<std::io::Result<()>>` flattened via `?` inside the closure, matching each method's existing return type) in all three call sites in Step 5.
- **Test correctness:** the `worker_threads = 1` + paired-oneshot design was chosen specifically so the test fails deterministically pre-fix and passes deterministically post-fix, without relying on real disk I/O timing (which would be slow, flaky, and hard to control) — see Task 1's step-1 comment block for why each line is ordered the way it is.
