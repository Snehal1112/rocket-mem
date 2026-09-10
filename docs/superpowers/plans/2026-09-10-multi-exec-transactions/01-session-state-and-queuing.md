# MULTI/EXEC Transactions — Plan 01: Session State and Queuing

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `MULTI` and `DISCARD`, plus the per-connection queuing state that lets any other
command be captured instead of run while a transaction is open — with a hot-path cost of one
relaxed atomic load for every connection that never uses transactions at all.

**Architecture:** A new `TransactionState` enum lives behind a `Mutex` on `Session`
(`crates/server/src/dispatcher.rs`), guarded by a separate `AtomicBool` fast-path flag so the
common case (no transaction ever opened) never touches the mutex. A new
`intercept_for_transaction` function runs inside `dispatch_and_log_inner`, immediately after the
existing `auth_gate` call and before every other gate — it owns `MULTI`/`DISCARD` and, while
queuing, captures every other command as `+QUEUED` instead of letting it reach the engine.
`EXEC` itself is stubbed to reply `ERR EXEC without MULTI` in this plan; Plan 02 gives it a real
body.

**Tech Stack:** Rust 2021, `tracing`, `std::sync::{Mutex, atomic::AtomicBool}`.

**Spec:** [`../../specs/2026-09-10-multi-exec-transactions-spec.md`](../../specs/2026-09-10-multi-exec-transactions-spec.md)

## Global Constraints

Every task in **every plan in this series** (01–04) must satisfy all of these. Later plans link
back here rather than restating them.

- **Working directory.** Run every command from the **root of the checkout you are editing**. If
  you are working in a git worktree, that is the worktree root — never a hardcoded absolute path
  to another checkout.
- **Three gates, all green, before every commit** (exactly what CI runs):
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
  `clippy` is strict: no warnings at all, dead code included, and it lints test code too.
- **Baseline test count is relative, not a fixed number, for this series.** At spec-writing time
  (2026-09-10) `cargo test --workspace` on `main` **failed to compile** —
  `crates/server/src/dispatcher.rs:1913` calls `cluster_nodes_text(cluster)` with one argument
  against a two-argument signature, from another, uncommitted, in-progress session's edit
  (`PeerHealth`/cluster-liveness work, commits `2b356b9`/`e45b00e`). **Task 1, Step 1 of this
  plan is to confirm `cargo test --workspace` builds and passes before touching anything else.**
  If it still doesn't compile, stop and report — do not fix unrelated in-progress work as a side
  effect of this series. Once it passes, record the exact count it reports as *this series'*
  baseline, and confirm the count printed at the end of every later step in every plan in this
  series is exactly `baseline + (tests added so far)`. Every task below states how many tests
  *it* adds; none may remove or weaken one.
- **Scope.** `crates/server/src/dispatcher.rs` only for this plan. No `engine`, `protocol`,
  `common`, `rmp-client`, `aof.rs`, or `replication.rs` change — those come in later plans.
- **Comment style.** Short, full sentences ending in a punctuation mark. No emoji.
- **Logging redaction.** Never log a queued command's arguments or a value — command *names*
  and *counts* only, per `CLAUDE.md`'s redaction policy. This project's existing convention:
  `debug!` for boundary/lifecycle events, `trace!` for anything approaching per-command noise.
- **No test may load the repo-root `rocket-mem.toml`.** It is a live deployment's
  credential-bearing config.
- **Log-capture assertions live in `crates/server/tests/`, never in a `#[cfg(test)] mod tests`
  inside `src/`.** `tracing` caches per-callsite `Interest` process-globally, and a callsite
  first reached with no subscriber installed can be cached as never-enabled for the whole
  process — hit twice already in this project, fixed by commit `4e646d2`. Plans 02 and 03 in
  this series add tests that assert on the transaction-lifecycle `debug!`/`trace!` events; those
  assertions must live in `crates/server/tests/`, not inside `dispatcher.rs`'s own `mod tests`.
- **Any snippet that reads from a spawned process needs a deadline.** A blocking `read_line` (or
  `Command::output()`) against a server that never exits blocks forever instead of failing — the
  red case must *fail*, not hang. Relevant to Plan 03's kill-and-recover test.
- **Any assertion on log output must state the exact rendered form.** `tracing_subscriber::fmt`
  renders a `%`-sigil (Display) field unquoted (`queued_count=3`) and a bare `&str` passed
  through `?` (Debug) *quoted* (`command="SET"`). Use `%` for plain counts/names and assert
  unquoted.

---

### Task 1: `TransactionState` and `Session` fields

**Files:**
- Modify: `crates/server/src/dispatcher.rs` — `Session` struct and `impl Session` (top of file,
  lines 24–83 as read at spec time)
- Test: `crates/server/src/dispatcher.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```rust
  pub(crate) enum TransactionState {
      Idle,
      Queuing { commands: Vec<Frame>, dirty: bool },
  }
  ```
  and, on `Session`: `fn tx_state_for_test(&self) -> TransactionState` (test-only accessor,
  `#[cfg(test)]`, returns a clone since `TransactionState` derives `Clone`), plus the private
  fields `in_transaction: std::sync::atomic::AtomicBool` and
  `tx: std::sync::Mutex<TransactionState>`. Task 2 in this plan is the only other reader/writer
  of these fields; Plan 02's `EXEC` handler is the next consumer after that.

- [ ] **Step 1: Confirm the workspace builds before touching anything**

```bash
cargo test --workspace 2>&1 | tail -20
```

If this does not compile, stop here and report it — see the Global Constraints note above. Do
not proceed to Step 2 until this passes. Record the exact `test result: ok. N passed` totals
across all crates (sum them) as this series' baseline count, call it `BASELINE`.

- [ ] **Step 2: Write the failing tests**

Add to the `mod tests` block at the bottom of `crates/server/src/dispatcher.rs` (find it via
`grep -n "mod tests" crates/server/src/dispatcher.rs` and add near the existing `Session`-focused
tests, e.g. next to any `session_` prefixed test):

```rust
    #[test]
    fn new_session_starts_idle_with_no_queued_commands() {
        let session = Session::new();
        assert_eq!(session.tx_state_for_test(), TransactionState::Idle);
        assert!(!session.in_transaction.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn transaction_state_derives_clone_and_partial_eq_for_test_assertions() {
        // Guards the derive itself: if `Frame` (which `TransactionState::Queuing` embeds via
        // `Vec<Frame>`) ever loses `PartialEq`, this is the test that breaks first and points
        // at the real cause instead of a confusing failure somewhere in Task 2's tests.
        let a = TransactionState::Queuing {
            commands: vec![Frame::Simple("PING".into())],
            dirty: false,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --lib dispatcher::tests::new_session_starts_idle
cargo test -p rocket-mem --lib dispatcher::tests::transaction_state_derives_clone
```

Expected: **compile error** — `error[E0433]: failed to resolve: use of undeclared type
'TransactionState'` and `error[E0599]: no method named 'tx_state_for_test' found`.

- [ ] **Step 4: Add `TransactionState` and the new `Session` fields**

Add directly above the `pub struct Session` definition:

```rust
/// A connection's `MULTI`/`EXEC`/`DISCARD` state. `Idle` is the state every connection starts
/// and ends in; `Queuing` holds every command captured since the matching `MULTI`, plus whether
/// any of them was rejected at queue time (which turns `EXEC` into `EXECABORT` — see
/// `docs/superpowers/specs/2026-09-10-multi-exec-transactions-spec.md`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TransactionState {
    Idle,
    Queuing { commands: Vec<Frame>, dirty: bool },
}
```

In `Session`, add two fields after `peer_addr`:

```rust
    /// Fast path for the overwhelmingly common case (no transaction ever opened): checked with
    /// a relaxed load before `tx`'s mutex is ever touched, so an ordinary connection that never
    /// sends `MULTI` pays one atomic load per command and nothing else. Set `true` by `MULTI`,
    /// `false` by `DISCARD` and by `EXEC` once it finishes. See the spec's "Performance" section.
    in_transaction: std::sync::atomic::AtomicBool,
    /// The queue itself. A separate lock from `protocol`/`authenticated_user`/`name` above:
    /// nothing about a transaction's queue needs to be visible to, or block, an unrelated read
    /// of the connection's name or auth state.
    tx: std::sync::Mutex<TransactionState>,
```

In `Session::new()`, add both fields to the literal:

```rust
            in_transaction: std::sync::atomic::AtomicBool::new(false),
            tx: std::sync::Mutex::new(TransactionState::Idle),
```

`with_peer_addr` uses `..Self::new()`, so it needs no change.

Add the test-only accessor directly below `set_authenticated_user`:

```rust
    #[cfg(test)]
    pub(crate) fn tx_state_for_test(&self) -> TransactionState {
        self.tx.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --lib dispatcher::tests::new_session_starts_idle
cargo test -p rocket-mem --lib dispatcher::tests::transaction_state_derives_clone
```

Expected: PASS, both.

- [ ] **Step 6: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, total count `BASELINE + 2`.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(transactions): add TransactionState and Session queuing fields

AtomicBool fast path plus a Mutex<TransactionState> queue, so an
ordinary connection that never sends MULTI pays one relaxed atomic
load per command. Nothing reads or writes these yet."
```

---

### Task 2: `MULTI`, `DISCARD`, and queuing interception

**Files:**
- Modify: `crates/server/src/dispatcher.rs` — `dispatch_and_log_inner` (interception point,
  immediately after the existing `auth_gate` call), new `intercept_for_transaction` function
- Test: `crates/server/src/dispatcher.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `TransactionState`, `Session::in_transaction`/`tx` from Task 1; the existing
  `KNOWN_COMMANDS: &[&str]` sorted table and `upper_name`/`CommandName` (both already in this
  file, lines 1335 and 131 as read at spec time).
- Produces: `fn intercept_for_transaction(frame: &Frame, session: &Session) -> Option<Frame>` —
  `Some(reply)` when the frame was fully handled here (either it *was* `MULTI`/`DISCARD`, or a
  transaction is open and the frame was queued or rejected as unknown); `None` means "not
  intercepted, run normally." Plan 02's `EXEC` handling calls the same `Session` fields this
  function does, and slots into the same `match` arm.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests`, using this file's existing fixture pattern — confirmed at
`dispatch_and_log_still_behaves_identically_after_the_wrapper_split` (around line 5305):
`Engine::new()`, `let (_dir, aof) = test_aof();`, `ReplicationHandle::default()`, and the `cmd(&[...])
-> Frame` helper that builds a `Frame::Array` of `Frame::Bulk` from byte-slice literals. Every
test below starts with the same three setup lines:

```rust
    #[test]
    fn multi_replies_ok_and_opens_a_transaction() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"MULTI"]),
            &session,
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
        assert_eq!(
            session.tx_state_for_test(),
            TransactionState::Queuing { commands: vec![], dirty: false }
        );
    }

    #[test]
    fn nested_multi_errors_without_touching_the_existing_queue() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"v"]), &session, 1);
        let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        assert_eq!(
            reply,
            Frame::Error("ERR MULTI calls can not be nested".into())
        );
        assert_eq!(
            session.tx_state_for_test(),
            TransactionState::Queuing {
                commands: vec![cmd(&[b"SET", b"k", b"v"])],
                dirty: false,
            },
            "the queue from before the nested MULTI must survive untouched"
        );
    }

    #[test]
    fn a_known_command_while_queuing_is_captured_as_queued_and_not_run() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &session,
            1,
        );
        assert_eq!(reply, Frame::Simple("QUEUED".into()));
        assert_eq!(engine.get(b"k"), None, "queuing must not touch the engine");
        assert_eq!(
            session.tx_state_for_test(),
            TransactionState::Queuing {
                commands: vec![cmd(&[b"SET", b"k", b"v"])],
                dirty: false,
            }
        );
    }

    #[test]
    fn an_unknown_command_while_queuing_marks_the_transaction_dirty() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"NOPE"]), &session, 1);
        assert_eq!(reply, Frame::Error("ERR unknown command 'NOPE'".into()));
        assert_eq!(
            session.tx_state_for_test(),
            TransactionState::Queuing { commands: vec![], dirty: true }
        );
    }

    #[test]
    fn discard_without_multi_errors() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"DISCARD"]), &session, 1);
        assert_eq!(reply, Frame::Error("ERR DISCARD without MULTI".into()));
    }

    #[test]
    fn discard_clears_the_queue_and_replies_ok() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k", b"v"]), &session, 1);
        let reply = dispatch_and_log(&engine, &aof, &replication, cmd(&[b"DISCARD"]), &session, 1);
        assert_eq!(reply, Frame::Simple("OK".into()));
        assert_eq!(session.tx_state_for_test(), TransactionState::Idle);
        assert!(!session.in_transaction.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn a_connection_that_never_opens_a_transaction_never_locks_the_tx_mutex() {
        // Not a timing test -- a correctness one. If intercept_for_transaction ever locked
        // `tx` before checking `in_transaction`, this would still pass; it exists to document
        // the invariant Plan 01's Performance section depends on, for the next reader.
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &session,
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
        assert_eq!(session.tx_state_for_test(), TransactionState::Idle);
    }
```

`cmd` is this file's existing helper (confirmed at `dispatch_and_log_still_behaves_identically_after_the_wrapper_split`) — do not add a second one that does the same thing.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --lib dispatcher::tests -- multi_ nested_multi_ a_known_command_while_queuing a_connection_that_never an_unknown_command_while_queuing discard_
```

Expected: FAIL — `MULTI`/`DISCARD` currently fall through to `dispatch`'s unknown-command arm, so
every reply assertion fails (e.g. `MULTI` replies `ERR unknown command 'MULTI'`, not `+OK`).

- [ ] **Step 3: Write `intercept_for_transaction` and wire it in**

Add this function near `auth_gate` (search `fn auth_gate` to find its neighborhood):

```rust
/// Intercepts `MULTI`, `DISCARD`, and — while a transaction is open — every other command,
/// capturing it into the queue instead of letting it run. Returns `None` only when this frame
/// should proceed through the normal gated dispatch path: there is no open transaction, and
/// this frame is not itself `MULTI`/`DISCARD`. `EXEC` is matched here too but is a stub in this
/// plan; Plan 02 gives it a real body in the same arm.
///
/// Runs *after* `auth_gate` and *before* every other gate in `dispatch_and_log_inner` — an
/// unauthenticated client must not be able to open or feed a transaction, but nothing else
/// (cluster redirect, READONLY, fencing) is checked until `EXEC` actually runs each queued
/// command, per the spec's "Gate timing" section.
fn intercept_for_transaction(frame: &Frame, session: &Session) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name_bytes)) = items.first() else {
        return None;
    };
    let name = upper_name(name_bytes)?;
    match name.as_str() {
        "MULTI" => Some(handle_multi(session)),
        "DISCARD" => Some(handle_discard(session)),
        "EXEC" => Some(Frame::Error("ERR EXEC without MULTI".into())), // Plan 02 replaces this arm
        _ => {
            // Fast path: an ordinary connection that never opens a transaction never locks
            // `tx`. See the spec's "Performance" section.
            if !session.in_transaction.load(std::sync::atomic::Ordering::Relaxed) {
                return None;
            }
            let mut state = session.tx.lock().unwrap_or_else(|e| e.into_inner());
            let TransactionState::Queuing { commands, dirty } = &mut *state else {
                // `in_transaction` said true but the state is Idle: EXEC/DISCARD just reset it
                // and this call raced in ahead of the flag update on the same connection, which
                // cannot happen since both live behind the one call sequence of a single
                // connection's serial command loop. Kept as a safe fallthrough, not a panic.
                return None;
            };
            if KNOWN_COMMANDS.binary_search(&name.as_str()).is_err() {
                *dirty = true;
                tracing::debug!(command = %name.as_str(), "transaction marked dirty");
                return Some(Frame::Error(format!(
                    "ERR unknown command '{}'",
                    name.as_str()
                )));
            }
            commands.push(frame.clone());
            tracing::trace!(command = %name.as_str(), "command queued");
            Some(Frame::Simple("QUEUED".into()))
        }
    }
}

/// `MULTI`: opens a transaction, or errors without disturbing an already-open one.
fn handle_multi(session: &Session) -> Frame {
    let mut state = session.tx.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(*state, TransactionState::Queuing { .. }) {
        return Frame::Error("ERR MULTI calls can not be nested".into());
    }
    *state = TransactionState::Queuing {
        commands: Vec::new(),
        dirty: false,
    };
    session
        .in_transaction
        .store(true, std::sync::atomic::Ordering::Relaxed);
    tracing::debug!("transaction started");
    Frame::Simple("OK".into())
}

/// `DISCARD`: closes an open transaction without running anything it queued.
fn handle_discard(session: &Session) -> Frame {
    let mut state = session.tx.lock().unwrap_or_else(|e| e.into_inner());
    let TransactionState::Queuing { commands, .. } = &*state else {
        return Frame::Error("ERR DISCARD without MULTI".into());
    };
    tracing::debug!(queued_count = commands.len(), "transaction discarded");
    *state = TransactionState::Idle;
    session
        .in_transaction
        .store(false, std::sync::atomic::Ordering::Relaxed);
    Frame::Simple("OK".into())
}
```

`KNOWN_COMMANDS` does not include `MULTI`, `EXEC`, or `DISCARD` themselves (they are intercepted
before `KNOWN_COMMANDS` is ever consulted) — that is fine and deliberate; they never reach the
`_` arm above because `intercept_for_transaction`'s `match` catches them first, at the top.

Now wire it into `dispatch_and_log_inner`, immediately after the existing `auth_gate` call
(around where `cluster_redirect` is called today):

```rust
    if let Some(reply) = auth_gate(replication, session, &frame) {
        return reply;
    }

    if let Some(reply) = intercept_for_transaction(&frame, session) {
        return reply;
    }

    if let Some(redirect) = cluster_redirect(&frame, replication) {
```

(the `cluster_redirect` line and everything after it is unchanged — this only adds the new block
between `auth_gate` and it).

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --lib dispatcher::tests -- multi_ nested_multi_ a_known_command_while_queuing a_connection_that_never an_unknown_command_while_queuing discard_
```

Expected: PASS, all seven.

- [ ] **Step 5: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, total count `BASELINE + 2 + 7 = BASELINE + 9`.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(transactions): add MULTI, DISCARD, and command queuing

intercept_for_transaction runs right after auth_gate: MULTI opens a
queue, DISCARD closes it, and any other command is captured as
+QUEUED while one is open. An unknown command while queuing marks the
transaction dirty (EXECABORT territory, wired in Plan 02). EXEC is
still a stub -- 'ERR EXEC without MULTI' unconditionally."
```

---

## Next plan

[`02-exec-batch-execution-and-isolation.md`](02-exec-batch-execution-and-isolation.md) — give
`EXEC` a real body: run the queued batch under one `aof.lock_shards` guard spanning every touched
shard (writers-only isolation), with per-command gate re-checks and `EXECABORT`/per-command-error
semantics.
