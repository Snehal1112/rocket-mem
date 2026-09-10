# MULTI/EXEC Transactions — Plan 02: EXEC Batch Execution and Isolation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `EXEC` a real body: run the queued batch as one writers-only-isolated unit, reusing
every existing per-command gate (auth/cluster/READONLY/fencing) and log-rewrite path unchanged,
just called once per queued frame inside one batch-wide `aof.lock_shards` guard instead of once
per top-level dispatch.

**Architecture:** `dispatch_and_log_inner`'s body from `cluster_redirect` onward (unchanged
logic) becomes a new function, `dispatch_and_log_gated`, taking one new `take_own_guard: bool`
parameter that controls whether it acquires its own per-command `aof.lock_shards` guard. Ordinary
top-level dispatch calls it with `true` (today's behavior, byte for byte). `EXEC`'s handler
computes the union of every queued command's touched shards, acquires `aof.lock_shards` for that
whole set *once*, then calls `dispatch_and_log_gated(..., take_own_guard: false)` once per queued
frame inside that single guard scope — which is what makes every gate (all still inside
`dispatch_and_log_gated`, unchanged) re-run per queued command "for free," per the spec's "Gate
timing" section.

**Tech Stack:** Rust 2021, `tracing`, `std::collections::HashSet`.

**Spec:** [`../../specs/2026-09-10-multi-exec-transactions-spec.md`](../../specs/2026-09-10-multi-exec-transactions-spec.md)

**Global Constraints:** See
[`01-session-state-and-queuing.md`](01-session-state-and-queuing.md)'s "Global Constraints"
section — every rule there applies here too, without restatement. In particular: use the
`BASELINE` count established there, and this plan's `Task 1, Step 1` re-confirms the build is
still green before continuing (another concurrent session may have landed work on `main` since
Plan 01 finished).

---

### Task 1: `dispatch_and_log_gated` and a working `EXEC`

**Files:**
- Modify: `crates/server/src/dispatcher.rs` — `dispatch_and_log_inner` (shrinks to the auth +
  transaction-interception header), new `dispatch_and_log_gated` (today's post-interception
  body, plus the `take_own_guard` parameter), `intercept_for_transaction` (widened signature,
  real `EXEC` arm), new `handle_exec`
- Test: `crates/server/src/dispatcher.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `TransactionState`, `Session::{in_transaction, tx}`, `intercept_for_transaction`,
  `handle_multi`, `handle_discard` from Plan 01; `command_keys(&Frame) -> Vec<&Bytes>` and
  `engine.shard_index(&[u8]) -> usize` (both pre-existing, used identically to how
  `dispatch_and_log_inner`'s current single-command `_order_guard` already uses them);
  `aof.lock_shards(&[usize])`/`aof.lock_all_shards()` (pre-existing, in `crates/server/src/aof.rs`).
- Produces:
  `fn dispatch_and_log_gated(engine: &Engine, aof: &crate::aof::AofWriter, replication:
  &crate::replication::ReplicationHandle, frame: Frame, session: &Session, client_id: u64,
  take_own_guard: bool) -> Frame` — Task 2's isolation test and Plan 03's AOF-framing work both
  call this directly (Plan 03 to insert the `MULTI`/`EXEC` AOF markers around the same loop).
  `fn handle_exec(engine: &Engine, aof: &crate::aof::AofWriter, replication:
  &crate::replication::ReplicationHandle, session: &Session, client_id: u64) -> Frame`.

- [ ] **Step 1: Confirm the workspace still builds**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: PASS, total `BASELINE + 9` (Plan 01's count). If it does not compile, stop and report —
do not fix unrelated in-progress work as a side effect of this plan.

- [ ] **Step 2: Write the failing tests**

Add to `mod tests`, using the same three-line fixture pattern as Plan 01
(`Engine::new()` / `test_aof()` / `ReplicationHandle::default()`):

```rust
    #[test]
    fn exec_without_multi_errors() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"EXEC"]), &session, 1);
        assert_eq!(reply, Frame::Error("ERR EXEC without MULTI".into()));
    }

    #[test]
    fn exec_with_no_queued_commands_returns_an_empty_array() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"EXEC"]), &session, 1);
        assert_eq!(reply, Frame::Array(vec![]));
        assert_eq!(session.tx_state_for_test(), TransactionState::Idle);
    }

    #[test]
    fn a_dirty_transaction_execaborts_and_runs_nothing() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"v"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"NOPE"]), &session, 1); // marks dirty
        let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"EXEC"]), &session, 1);
        assert_eq!(
            reply,
            Frame::Error("EXECABORT Transaction discarded because of previous errors".into())
        );
        assert_eq!(engine.get(b"k"), None, "a dirty transaction must run nothing, not even the good commands");
        assert_eq!(session.tx_state_for_test(), TransactionState::Idle);
    }

    #[test]
    fn exec_runs_every_queued_command_in_order_and_returns_their_replies() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"v1"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"v2"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"GET", b"k"]), &session, 1);
        let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"EXEC"]), &session, 1);
        assert_eq!(
            reply,
            Frame::Array(vec![
                Frame::Simple("OK".into()),
                Frame::Simple("OK".into()),
                Frame::Bulk(Bytes::from_static(b"v2")),
            ])
        );
        assert_eq!(session.tx_state_for_test(), TransactionState::Idle);
        assert!(!session.in_transaction.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn a_runtime_error_inside_a_batch_does_not_abort_the_rest_of_it() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"LPUSH", b"list", b"a"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        // GET on a List key is a real, valid queue-time-known command with correct arity --
        // it only fails at *runtime*, which is exactly the case EXECABORT must not cover.
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"GET", b"list"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"v"]), &session, 1);
        let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"EXEC"]), &session, 1);
        let Frame::Array(replies) = reply else {
            panic!("EXEC must reply with an array");
        };
        assert_eq!(replies.len(), 2);
        assert!(matches!(&replies[0], Frame::Error(msg) if msg.contains("WRONGTYPE")));
        assert_eq!(replies[1], Frame::Simple("OK".into()));
        assert_eq!(
            engine.get(b"k"),
            Some(Value::String(Bytes::from_static(b"v"))),
            "the second command must still have applied despite the first one's runtime error"
        );
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --lib dispatcher::tests -- exec_without_multi exec_with_no_queued a_dirty_transaction exec_runs_every_queued a_runtime_error_inside_a_batch
```

Expected: FAIL — `EXEC` still replies `ERR EXEC without MULTI` unconditionally (Plan 01's stub),
so every test except `exec_without_multi_errors` fails.

- [ ] **Step 4: Extract `dispatch_and_log_gated`**

In `crates/server/src/dispatcher.rs`, find `fn dispatch_and_log_inner` (added to by Plan 01 right
after `auth_gate`). Everything in its body **from the `cluster_redirect` check through the final
`reply` line before the closing brace** — i.e. cluster redirect, the `READONLY` check, the
min-replicas fencing check, `handle_acl`, `is_save_command`/`is_bgrewriteaof_command`,
`handle_replicaof`, `handle_cluster`, `handle_info`, `handle_hello`, `handle_slowlog`,
`handle_config`, `handle_client`, the `write_name`/`original_frame`/`_order_guard` block, the
`dispatch(...)` call, the `to_log` computation, the AOF append loop, the broadcast loop, and the
trailing `aof_failed` check — moves **verbatim, unchanged**, into a new function:

```rust
fn dispatch_and_log_gated(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    session: &Session,
    client_id: u64,
    take_own_guard: bool,
) -> Frame {
    // <-- everything described above, moved here unchanged, EXCEPT the one line below -->
}
```

The **one** line that changes in the moved body is the `_order_guard` assignment. Today it reads:

```rust
    let _order_guard = write_name.as_ref().map(|_| {
        let shards: Vec<usize> = command_keys(&frame)
            .iter()
            .map(|k| engine.shard_index(k))
            .collect();
        if shards.is_empty() {
            aof.lock_all_shards()
        } else {
            aof.lock_shards(&shards)
        }
    });
```

Change it to only take a guard when `take_own_guard` is `true` — a batch call already holds one
for the whole transaction:

```rust
    let _order_guard = if take_own_guard {
        write_name.as_ref().map(|_| {
            let shards: Vec<usize> = command_keys(&frame)
                .iter()
                .map(|k| engine.shard_index(k))
                .collect();
            if shards.is_empty() {
                aof.lock_all_shards()
            } else {
                aof.lock_shards(&shards)
            }
        })
    } else {
        None
    };
```

`dispatch_and_log_inner` now reads, in full:

```rust
fn dispatch_and_log_inner(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    session: &Session,
    client_id: u64,
) -> Frame {
    if let Some(reply) = auth_gate(replication, session, &frame) {
        return reply;
    }
    if let Some(reply) =
        intercept_for_transaction(&frame, session, engine, aof, replication, client_id)
    {
        return reply;
    }
    dispatch_and_log_gated(engine, aof, replication, frame, session, client_id, true)
}
```

- [ ] **Step 5: Widen `intercept_for_transaction` and implement `handle_exec`**

Change `intercept_for_transaction`'s signature (added in Plan 01) to:

```rust
fn intercept_for_transaction(
    frame: &Frame,
    session: &Session,
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    client_id: u64,
) -> Option<Frame> {
```

Replace its `"EXEC" => ...` arm (Plan 01's stub) with:

```rust
        "EXEC" => Some(handle_exec(engine, aof, replication, session, client_id)),
```

Every other arm, and the whole body below the `match name.as_str() {` line, is unchanged from
Plan 01.

Add `handle_exec` next to `handle_multi`/`handle_discard`:

```rust
/// `EXEC`: runs every queued command as one writers-only-isolated unit (spec "Decision"
/// section), or replies `EXECABORT` without running anything if any queued command was rejected
/// at queue time.
///
/// The batch-wide guard is `aof.lock_shards` widened to the union of every queued command's
/// touched shards -- the same primitive `dispatch_and_log_gated`'s own per-command `_order_guard`
/// uses, just held across every queued command's execution instead of one. An empty union (every
/// queued command was keyless, or the queue itself was empty) falls back to
/// `aof.lock_all_shards()`, matching the single-command precedent for "could not enumerate any
/// keys" exactly.
fn handle_exec(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    session: &Session,
    client_id: u64,
) -> Frame {
    let queued = {
        let mut state = session.tx.lock().unwrap_or_else(|e| e.into_inner());
        let TransactionState::Queuing { commands, dirty } = &*state else {
            return Frame::Error("ERR EXEC without MULTI".into());
        };
        if *dirty {
            tracing::debug!(
                queued_count = commands.len(),
                "transaction execaborted"
            );
            *state = TransactionState::Idle;
            session
                .in_transaction
                .store(false, std::sync::atomic::Ordering::Relaxed);
            return Frame::Error(
                "EXECABORT Transaction discarded because of previous errors".into(),
            );
        }
        let queued = commands.clone();
        *state = TransactionState::Idle;
        queued
    };
    session
        .in_transaction
        .store(false, std::sync::atomic::Ordering::Relaxed);

    let started = std::time::Instant::now();
    let shard_set: std::collections::HashSet<usize> = queued
        .iter()
        .flat_map(command_keys)
        .map(|k| engine.shard_index(k))
        .collect();
    let shards: Vec<usize> = shard_set.into_iter().collect();
    let _batch_guard = if shards.is_empty() {
        aof.lock_all_shards()
    } else {
        aof.lock_shards(&shards)
    };

    let mut replies = Vec::with_capacity(queued.len());
    for queued_frame in queued.iter() {
        replies.push(dispatch_and_log_gated(
            engine,
            aof,
            replication,
            queued_frame.clone(),
            session,
            client_id,
            false,
        ));
    }
    drop(_batch_guard);

    tracing::debug!(
        queued_count = replies.len(),
        shard_count = shards.len(),
        elapsed_us = started.elapsed().as_micros(),
        "transaction executed"
    );
    Frame::Array(replies)
}
```

`command_keys` takes `&Frame`; `queued.iter().flat_map(command_keys)` works because `command_keys`
already has the signature `fn command_keys(frame: &Frame) -> Vec<&Bytes>`, matching
`Iterator::flat_map`'s expected `FnMut(&Frame) -> Vec<&Bytes>` here directly — no closure needed.

- [ ] **Step 6: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --lib dispatcher::tests -- exec_without_multi exec_with_no_queued a_dirty_transaction exec_runs_every_queued a_runtime_error_inside_a_batch
```

Expected: PASS, all five.

- [ ] **Step 7: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, total `BASELINE + 9 + 5 = BASELINE + 14`.

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(transactions): give EXEC a real body

dispatch_and_log_inner's post-auth body is now dispatch_and_log_gated,
taking a take_own_guard flag. EXEC computes the union of every queued
command's touched shards, takes one aof.lock_shards for the whole
batch, then calls dispatch_and_log_gated once per queued frame with
take_own_guard=false -- every existing gate re-runs per command for
free. A dirty transaction EXECABORTs and runs nothing; a runtime error
inside a clean batch does not abort the rest of it."
```

---

### Task 2: Writers-only isolation and per-command gate re-checks

**Files:**
- Test: `crates/server/src/dispatcher.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `handle_exec`, `dispatch_and_log_gated` from Task 1. No production code changes in
  this task — it exists to prove the isolation guarantee Task 1 already implements, and the
  "gates re-run per queued command" claim, which needs no new code beyond Task 1's refactor.

- [ ] **Step 1: Write the failing test for writers-only isolation**

This uses the pre-existing `DEBUG SLEEP <secs>` command (`dispatcher.rs`, `"DEBUG" => ... "SLEEP"
=>`, capped at 10s) to hold the batch guard open for a controlled duration, and an `mpsc` channel
plus a short fixed margin — not a `Barrier` — because the guard's *duration* is already
controlled by the sleep; the margin only needs to outlast the handful of instructions between
"about to call EXEC" and "guard acquired," not a whole rendezvous:

```rust
    #[test]
    fn a_write_to_the_same_key_blocks_until_exec_releases_its_batch_guard_but_a_read_does_not() {
        let engine = std::sync::Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let aof = std::sync::Arc::new(aof);
        let replication = std::sync::Arc::new(ReplicationHandle::default());

        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"first"]), &Session::new(), 1);

        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let engine2 = std::sync::Arc::clone(&engine);
        let aof2 = std::sync::Arc::clone(&aof);
        let replication2 = std::sync::Arc::clone(&replication);
        let exec_thread = std::thread::spawn(move || {
            let session = Session::new();
            dispatch_and_log(&engine2, &aof2, &replication2, cmd(&[b"MULTI"]), &session, 2);
            dispatch_and_log(
                &engine2, &aof2, &replication2,
                cmd(&[b"SET", b"k", b"from-exec"]),
                &session, 2,
            );
            dispatch_and_log(
                &engine2, &aof2, &replication2,
                cmd(&[b"DEBUG", b"SLEEP", b"0.4"]),
                &session, 2,
            );
            started_tx.send(()).expect("receiver still waiting");
            dispatch_and_log(&engine2, &aof2, &replication2, cmd(&[b"EXEC"]), &session, 2)
        });

        started_rx.recv().expect("exec thread panicked before signaling");
        // Margin for "signal sent" to become "guard actually held" -- a handful of instructions,
        // not a rendezvous; generous relative to that gap without eating into the 400ms sleep
        // the blocking assertion below depends on.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let read_started = std::time::Instant::now();
        let read_reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"GET", b"k"]), &Session::new(), 3);
        assert!(
            matches!(read_reply, Frame::Bulk(_)),
            "a concurrent read must not be blocked by the transaction's batch guard"
        );
        assert!(
            read_started.elapsed() < std::time::Duration::from_millis(200),
            "a concurrent read must not wait on the transaction's guard, took {:?}",
            read_started.elapsed()
        );

        let write_started = std::time::Instant::now();
        let write_reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"from-outside"]), &Session::new(), 4);
        assert_eq!(write_reply, Frame::Simple("OK".into()));
        assert!(
            write_started.elapsed() >= std::time::Duration::from_millis(200),
            "a concurrent write to the same key must block until EXEC releases its batch guard, \
             only waited {:?}",
            write_started.elapsed()
        );

        let exec_reply = exec_thread.join().expect("exec thread panicked");
        assert_eq!(
            exec_reply,
            Frame::Array(vec![Frame::Simple("OK".into()), Frame::Simple("OK".into())])
        );
    }
```

- [ ] **Step 2: Write the failing test for per-command gate re-checks**

```rust
    #[test]
    fn a_queued_write_against_a_read_only_replica_errors_without_aborting_the_rest_of_the_batch() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        // Queue while still a normal (non-replica) connection -- queuing itself never touches
        // the READONLY gate, per the spec's "Gate timing" section.
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"v"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"GET", b"k"]), &session, 1);

        // Only now does this connection's node become a read-only replica -- simulating a
        // REPLICAOF issued between this connection's MULTI and its EXEC.
        replication
            .is_replica
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"EXEC"]), &session, 1);
        let Frame::Array(replies) = reply else {
            panic!("EXEC must reply with an array");
        };
        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0],
            Frame::Error("READONLY You can't write against a read only replica.".into())
        );
        // The GET is unaffected: READONLY only gates writes, and one queued command's gate
        // rejection must not abort the rest of the batch (same rule as a runtime error).
        assert!(matches!(replies[1], Frame::Null | Frame::Bulk(_)));
    }
```

If `replication.is_replica` is not a directly settable public field on `ReplicationHandle` from
this test module, check how the existing `dispatch_and_log_does_not_broadcast_a_read_only_command`-style
tests (search `is_replica` in this file) construct a replica-mode `ReplicationHandle` and use that
exact construction instead.

- [ ] **Step 3: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --lib dispatcher::tests -- a_write_to_the_same_key_blocks a_queued_write_against_a_read_only_replica
```

Expected: the isolation test **fails on the timing assertion** (the write returns almost
immediately, well under 200ms) if Task 1's guard were somehow not held for the full batch — but
since Task 1 already implements the real guard, this should already be green. If it is not, that
is a real signal Task 1's guard scoping is wrong; stop and re-check Step 4/5 of Task 1 before
continuing. The gate-recheck test should already pass too, for the same reason — both tests exist
to *prove*, not to drive, behavior Task 1 already built.

- [ ] **Step 4: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, total `BASELINE + 14 + 2 = BASELINE + 16`. If either new test is flaky under
repeated runs (`cargo test -p rocket-mem --lib dispatcher::tests -- a_write_to_the_same_key_blocks
--test-threads=1` run 10 times), widen the sleep/margin constants rather than loosening the
assertion threshold.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "test(transactions): prove writers-only isolation and gate re-checks

A concurrent write to a key EXEC's batch touches blocks until the
batch guard releases; a concurrent read does not. A queued write that
would fail READONLY at EXEC time errors as that command's own array
entry without aborting the rest of the batch."
```

---

### Task 3: Transaction lifecycle logging, verified against real subscriber output

**Files:**
- Modify: `crates/server/tests/logging.rs` — reuses its existing `capture_during` helper
  (generic over an arbitrary closure; no new harness needed)
- Test: same file

**Interfaces:**
- Consumes: `capture_during(level: &str, f: impl FnOnce() -> T) -> (T, String)`, already defined
  in this file; `rocket_mem::dispatcher::{dispatch_and_log, Session}`, already used by this
  file's other tests.
- Produces: nothing new for later tasks — this is verification-only.

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/tests/logging.rs`, near its other `capture_during`-based tests:

```rust
#[test]
fn multi_and_discard_log_at_debug_with_counts_only_never_command_arguments() {
    let (_, output) = capture_during("debug", || {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine::Engine::new();
        let aof = rocket_mem::aof::AofWriter::open(
            &dir.path().join("tx-logging.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .expect("open aof");
        let replication = rocket_mem::replication::ReplicationHandle::default();
        let session = rocket_mem::dispatcher::Session::new();

        rocket_mem::dispatcher::dispatch_and_log(
            &engine, &aof, &replication,
            protocol::Frame::Array(vec![protocol::Frame::Bulk(bytes::Bytes::from_static(b"MULTI"))]),
            &session, 1,
        );
        rocket_mem::dispatcher::dispatch_and_log(
            &engine, &aof, &replication,
            protocol::Frame::Array(vec![
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"SET")),
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"super-secret-key")),
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"super-secret-value")),
            ]),
            &session, 1,
        );
        rocket_mem::dispatcher::dispatch_and_log(
            &engine, &aof, &replication,
            protocol::Frame::Array(vec![protocol::Frame::Bulk(bytes::Bytes::from_static(b"DISCARD"))]),
            &session, 1,
        );
    });

    assert!(output.contains("transaction started"), "got: {output}");
    assert!(
        output.contains("transaction discarded") && output.contains("queued_count=1"),
        "got: {output}"
    );
    assert!(
        !output.contains("super-secret-key") && !output.contains("super-secret-value"),
        "a queued command's key/value must never reach the log at debug, got: {output}"
    );
}

#[test]
fn exec_logs_queued_count_shard_count_and_elapsed_at_debug() {
    let (_, output) = capture_during("debug", || {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine::Engine::new();
        let aof = rocket_mem::aof::AofWriter::open(
            &dir.path().join("tx-exec-logging.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .expect("open aof");
        let replication = rocket_mem::replication::ReplicationHandle::default();
        let session = rocket_mem::dispatcher::Session::new();

        for frame in [
            vec!["MULTI"],
            vec!["SET", "k", "v"],
            vec!["EXEC"],
        ] {
            let frame = protocol::Frame::Array(
                frame
                    .into_iter()
                    .map(|s| protocol::Frame::Bulk(bytes::Bytes::from(s.to_string())))
                    .collect(),
            );
            rocket_mem::dispatcher::dispatch_and_log(&engine, &aof, &replication, frame, &session, 1);
        }
    });

    assert!(output.contains("transaction executed"), "got: {output}");
    assert!(output.contains("queued_count=1"), "got: {output}");
    assert!(output.contains("shard_count=1"), "got: {output}");
    assert!(output.contains("elapsed_us="), "got: {output}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --test logging -- multi_and_discard_log_at_debug exec_logs_queued_count
```

Expected: FAIL — Plan 01/02's `debug!` calls already emit these events with these exact field
names (`intercept_for_transaction`'s dirty branch aside, which this test does not exercise), so
if this fails it means either the field names drifted from what Task 1/2 actually implemented, or
`capture_during`'s `EnvFilter::new("debug")` needs the crate-scoped form
(`"rocket_mem=debug"`) — check how this file's other passing tests construct their filter string
and match it exactly.

- [ ] **Step 3: Fix any field-name or filter mismatch found in Step 2**

There is no new production code to write here if Plan 01/02's `debug!` calls already match — this
step exists only to reconcile the two if Step 2 surfaces a real mismatch. If it does, fix the
`tracing::debug!` call sites in `crates/server/src/dispatcher.rs` (not this test) to match the
spec's field names (`queued_count`, `shard_count`, `elapsed_us`) exactly, since those are the
names Plan 04's benchmark note (next plan after this series continues) and any future operator
tooling will grep for.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --test logging -- multi_and_discard_log_at_debug exec_logs_queued_count
```

Expected: PASS, both.

- [ ] **Step 5: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, total `BASELINE + 16 + 2 = BASELINE + 18`.

- [ ] **Step 6: Commit**

```bash
git add crates/server/tests/logging.rs
git commit -m "test(transactions): verify transaction logging against real subscriber output

Proves MULTI/DISCARD/EXEC's debug! events render with the documented
field names and never a queued command's key or value, using this
file's existing capture_during helper -- no new harness."
```

---

## Next plan

[`03-aof-and-replication-framing.md`](03-aof-and-replication-framing.md) — wrap a transaction's
AOF/replication output in `MULTI`/`EXEC` markers under the same batch guard, and teach both
`aof::replay_with_stats` and `replication::sync_once` to apply a replayed transaction as one
unit.
