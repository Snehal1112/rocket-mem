# Verbose Logging Plan 10: Dispatch Error Events

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `dispatcher.rs`'s three error-reply shapes — unknown command, engine error (WRONGTYPE et al.), and wrong-arity — a `debug!` event each, so an operator running `RUST_LOG=debug` can see why a command failed without reproducing it against a client.

**Architecture:** Every one of these three error shapes already funnels through a small number of choke points, which is exactly what plan 01's spec called out as the reason `dispatch_and_log`/`engine_error_to_frame`-style functions are worth instrumenting instead of every call site individually:

- **Unknown command** has *two* distinct production sites in `dispatch`, not one, because there are two distinct ways a command can be "unknown": a name that cannot possibly be a real command at all (too long or non-ASCII — `upper_name` returns `None`, handled at the top of `dispatch`), and a name that is validly shaped but simply isn't in the big `match` (handled by that `match`'s own catch-all arm at the bottom of `dispatch`). Both get the same `debug!` event under Task 1, since both represent the same catalogue entry.
- **Engine errors** (WRONGTYPE, NotAnInteger, NoSuchKey) all pass through `engine_error_to_frame`, the single function that converts `common::EngineError` into a `Frame::Error` — Task 2 instruments that one function and every command that returns an engine error is covered for free.
- **Arity errors** are NOT all funneled through one function. 76 call sites go through the `require_args!` macro (a true single choke point once instrumented — a macro's expansion is the call site, but there is exactly one place its *text* lives). A further 18 production sites build their own `"ERR wrong number of arguments for '...'"` message inline (odd-length `HSET`/`MSET`/`MSETNX` pair checks, and per-subcommand arity checks in `AUTH`, `REPLICAOF`, `CLUSTER`, `CLIENT`, `CONFIG`, `SLOWLOG`, and `ACL`'s subcommands). Task 3 instruments the macro and explicitly lists the 18 sites left uncovered, per this plan's instructions — sweeping all 18 individually is a separate, larger piece of work than fits in a 3-task plan.

None of these three event sites sit on the hot path a benchmark exercises: `require_args!`'s `debug!` call is inside the `if` branch that only runs when arity already failed; `engine_error_to_frame`'s `debug!` call only runs on the `Err` arm callers already only reach on a genuine engine error; and both unknown-command sites only run for a command name `redis-benchmark`'s `SET`/`GET` workload never sends. A successful `SET`/`GET` never executes any of the three new `debug!` calls, so the 2% throughput gate is not at risk from this plan.

**Tech Stack:** Rust 2021, `tracing 0.1` (already a dependency of `crates/server`), `tracing-subscriber 0.3` with the `fmt` feature (already a dependency of `crates/server`, used here only in tests to capture what a subscriber would print).

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the "Dispatch" row of the Event catalogue: "unknown command, WRONGTYPE, and arity errors (debug)".

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting.

**Additional constraints specific to this plan:**
- Every `tracing::` macro call in this plan uses a **fully-qualified path** (`tracing::debug!(...)`), never a bare `debug!(...)`, since `dispatcher.rs` does not otherwise import `tracing`'s macros and this plan does not add a blanket `use tracing::*` — this keeps each edit self-contained and avoids assuming what an earlier plan's `use` block already added.
- `crates/server/src/dispatcher.rs`'s inline `#[cfg(test)] mod tests` (starting at its `#[cfg(test)]` line) is the only place these plans' tests live — every function touched here (`dispatch`, `engine_error_to_frame`, the `require_args!` macro) is private or `pub(crate)`, so an integration test under `crates/server/tests/` cannot reach it. This matches the file's own existing convention (see its ~600 existing inline tests).
- Where a test needs to observe a log line rather than a return value, it uses the small capture helper Task 1 adds (`capture_logs_at`) — built entirely from `tracing`/`tracing-subscriber`, both already ordinary (non-dev) dependencies of `crates/server`, so this is not "inventing a tracing-mock dependency."

---

### Task 1: Unknown command — `debug!` at both production sites, plus the shared test capture helper

**Files:**
- Modify: `crates/server/src/dispatcher.rs:193-200` (the `upper_name` failure cold path inside `dispatch`)
- Modify: `crates/server/src/dispatcher.rs:1129` (the big `match`'s catch-all arm, also inside `dispatch`)
- Modify: `crates/server/src/dispatcher.rs` inline `mod tests` — add the `capture_logs_at` helper

**Interfaces:**
- Consumes: nothing new. Reads `args[0]` (already in scope at line 193) and `name` (already in scope at line 1129) — no new parameters.
- Produces: `capture_logs_at(level: tracing::Level, f: impl FnOnce()) -> String`, a test-only helper. Every later task in plan 10 and plan 11 calls this directly (it lives in the same `mod tests` those tasks also edit) rather than redefining it.

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/dispatcher.rs`'s existing `mod tests` block (after the existing `fn cmd(...)` helper at line ~3407 is a natural spot, so the new helper sits next to the other shared test fixture):

```rust
    /// Runs `f` with a `tracing` subscriber installed (scoped to the current thread only, via
    /// `tracing::subscriber::with_default`) that writes formatted log lines into an in-memory
    /// buffer, and returns everything it wrote as a `String`. Built from `tracing`/
    /// `tracing-subscriber` alone -- both already ordinary dependencies of `crates/server` --
    /// so plan 10's tests can assert on log *content*, not just on the unchanged `Frame` reply,
    /// without pulling in a new mocking dependency.
    #[derive(Clone)]
    struct VecWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for VecWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for VecWriter {
        type Writer = VecWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn capture_logs_at<F: FnOnce()>(level: tracing::Level, f: F) -> String {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = VecWriter(buf.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_max_level(level)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let bytes = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        String::from_utf8(bytes).expect("tracing-subscriber's fmt output is always valid UTF-8")
    }

    #[test]
    fn unknown_command_from_the_catchall_match_arm_logs_a_debug_event() {
        let engine = Engine::new();
        let log = capture_logs_at(tracing::Level::DEBUG, || {
            let reply = dispatch(&engine, cmd(&[b"NOPE"]), &mut Protocol::default(), 1);
            assert_eq!(reply, Frame::Error("ERR unknown command 'NOPE'".into()));
        });
        assert!(log.contains("unknown command"), "expected an unknown-command event, got: {log}");
        assert!(log.contains("NOPE"), "expected the command name in the event, got: {log}");
    }

    #[test]
    fn unknown_command_from_the_malformed_name_cold_path_logs_a_debug_event() {
        let engine = Engine::new();
        // Longer than MAX_COMMAND_NAME_LEN (32) so `upper_name` returns `None` and `dispatch`
        // takes the cold path at line 193, not the catch-all match arm at line 1129.
        let too_long = b"A".repeat(MAX_COMMAND_NAME_LEN + 1);
        let log = capture_logs_at(tracing::Level::DEBUG, || {
            let reply = dispatch(&engine, cmd(&[&too_long]), &mut Protocol::default(), 1);
            assert!(matches!(reply, Frame::Error(_)));
        });
        assert!(log.contains("unknown command"), "expected an unknown-command event, got: {log}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem dispatcher::tests::unknown_command
```

Expected: both tests **compile** (the helper only uses existing dependencies) but **fail at the assertion**, e.g.:

```
assertion failed: log.contains("unknown command")
expected an unknown-command event, got:
```

(the captured `log` is empty in both cases, because neither site logs anything yet).

- [ ] **Step 3: Implement the two `debug!` sites**

At `crates/server/src/dispatcher.rs:193-200`, change:

```rust
    let Some(name) = upper_name(&args[0]) else {
        // Cold path only: a name too long or non-ASCII to be any command we know. The error text
        // is unchanged from before this optimization -- it echoes the client's own bytes.
        return Frame::Error(format!(
            "ERR unknown command '{}'",
            String::from_utf8_lossy(&args[0])
        ));
    };
```

to:

```rust
    let Some(name) = upper_name(&args[0]) else {
        // Cold path only: a name too long or non-ASCII to be any command we know. The error text
        // is unchanged from before this optimization -- it echoes the client's own bytes.
        let raw = String::from_utf8_lossy(&args[0]);
        tracing::debug!(cmd = %raw, "unknown command");
        return Frame::Error(format!("ERR unknown command '{raw}'"));
    };
```

At `crates/server/src/dispatcher.rs:1129`, change:

```rust
        _ => Frame::Error(format!("ERR unknown command '{}'", name.as_str())),
```

to:

```rust
        _ => {
            tracing::debug!(cmd = %name.as_str(), "unknown command");
            Frame::Error(format!("ERR unknown command '{}'", name.as_str()))
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem dispatcher::tests::unknown_command
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: both tests PASS, fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(logging): add debug events for both unknown-command paths"
```

---

### Task 2: Engine errors — `debug!` inside `engine_error_to_frame`

**Files:**
- Modify: `crates/server/src/dispatcher.rs:151-153` (`engine_error_to_frame`)

**Interfaces:**
- Consumes: `common::EngineError` (already the function's only parameter; it derives `Debug`, so `?e` is free to use).
- Produces: nothing new consumed elsewhere — `engine_error_to_frame`'s signature and return value are unchanged, so every one of its ~40 existing call sites across `dispatch`'s command arms needs no edit.

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    #[test]
    fn engine_error_to_frame_logs_a_debug_event_naming_the_error_variant() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"k"), Value::List(Default::default()));
        let log = capture_logs_at(tracing::Level::DEBUG, || {
            // GET against a List-typed key is the simplest reliable way to reach
            // engine_error_to_frame with a real WrongType error.
            let reply = dispatch(&engine, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1);
            assert_eq!(
                reply,
                Frame::Error(
                    "WRONGTYPE Operation against a key holding the wrong kind of value".into()
                )
            );
        });
        assert!(log.contains("WrongType"), "expected the error variant name, got: {log}");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem dispatcher::tests::engine_error_to_frame_logs_a_debug_event_naming_the_error_variant
```

Expected: compiles, fails at the assertion:

```
assertion failed: log.contains("WrongType")
expected the error variant name, got:
```

- [ ] **Step 3: Implement**

At `crates/server/src/dispatcher.rs:151-153`, change:

```rust
fn engine_error_to_frame(e: common::EngineError) -> Frame {
    Frame::Error(e.to_string())
}
```

to:

```rust
fn engine_error_to_frame(e: common::EngineError) -> Frame {
    tracing::debug!(error = ?e, "engine error");
    Frame::Error(e.to_string())
}
```

`?e` renders `EngineError`'s `Debug` output (`WrongType`, `NotAnInteger`, or `NoSuchKey`), distinct from `e.to_string()`'s `Display` output (the full Redis-style message text) already used for the reply — the log line is the compact variant name, not a repeat of the wire-visible message.

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p rocket-mem dispatcher::tests::engine_error_to_frame_logs_a_debug_event_naming_the_error_variant
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS, fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(logging): add debug event to engine_error_to_frame"
```

---

### Task 3: Arity errors — `debug!` in the `require_args!` macro, and the sites this leaves uncovered

**Files:**
- Modify: `crates/server/src/dispatcher.rs:174-183` (the `require_args!` macro definition)

**Interfaces:**
- Consumes: the macro's existing three arguments (`$rest`, `$n`, `$name`) — no new arguments, so all 76 existing call sites need no changes themselves.
- Produces: nothing new consumed elsewhere.

**Sites this task does NOT cover, and why:** `require_args!` is the single shared choke point for the *minimum* argument count check, but 18 production sites build their own `"ERR wrong number of arguments for '...'"` `Frame::Error` inline, for shapes `require_args!` cannot express (an odd/even pair-count check, or a subcommand-specific exact/max count):

| Site | Line | Shape |
|---|---|---|
| `HSET` field/value pairing | 342 | odd `pairs.len()` after the macro's own minimum-count check already passed |
| `MSET` field/value pairing | 770 | same shape as `HSET` |
| `MSETNX` field/value pairing | 794 | same shape as `HSET` |
| `AUTH` (`handle_auth`) | 1189 | exact count (2 or 3 items), not a minimum |
| `REPLICAOF` | 1540, 1545 | two separate exact-count checks (`NO ONE` vs `host port`) |
| `CLUSTER` | 1757 | exact count for the bare form |
| `CLUSTER KEYSLOT` | 1767 | exact count for the subcommand form |
| `CLIENT` | 2154 | exact count for the bare form |
| `CLIENT SETNAME` | 2164 | exact count for the subcommand form |
| `CONFIG` | 2346 | exact count for the bare form |
| `CONFIG GET` | 2361 | exact count for the subcommand form |
| `CONFIG SET` | 2379 | exact count for the subcommand form |
| `SLOWLOG` | 2414 | exact count for the bare form |
| `ACL` | 2488 | exact count for the bare form |
| `ACL GETUSER` | 2540 | exact count for the subcommand form |
| `ACL SETUSER` | 2607 | exact count for the subcommand form |
| `ACL DELUSER` | 2628 | minimum count, but expressed as `items.len() < 3` against `Frame` items, not `require_args!`'s `rest`/`Bytes` shape |

Instrumenting these 18 individually is a larger, separate piece of work than fits in this plan's 3-task limit — each is a different call shape (some check `Frame` items directly rather than the `rest: &[Bytes]` slice `require_args!` expects), so there is no second shared helper to point one `debug!` call at. A follow-up plan can sweep them if the operational value of arity-error visibility for these specific subcommands turns out to matter in practice; the 76 sites this task does cover are every ordinary command's minimum-arity check, which is the overwhelming majority of arity failures a client can trigger.

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    #[test]
    fn require_args_arity_failure_logs_a_debug_event_with_command_and_counts() {
        let engine = Engine::new();
        let log = capture_logs_at(tracing::Level::DEBUG, || {
            let reply = dispatch(&engine, cmd(&[b"GET"]), &mut Protocol::default(), 1);
            assert_eq!(
                reply,
                Frame::Error("ERR wrong number of arguments for 'get' command".into())
            );
        });
        assert!(log.contains("wrong number of arguments"), "got: {log}");
        assert!(log.contains("get"), "expected the command name in the event, got: {log}");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem dispatcher::tests::require_args_arity_failure_logs_a_debug_event_with_command_and_counts
```

Expected: compiles, fails at the assertion:

```
assertion failed: log.contains("wrong number of arguments")
got:
```

- [ ] **Step 3: Implement**

At `crates/server/src/dispatcher.rs:174-183`, change:

```rust
macro_rules! require_args {
    ($rest:expr, $n:expr, $name:expr) => {
        if $rest.len() < $n {
            return Frame::Error(format!(
                "ERR wrong number of arguments for '{}' command",
                $name
            ));
        }
    };
}
```

to:

```rust
macro_rules! require_args {
    ($rest:expr, $n:expr, $name:expr) => {
        if $rest.len() < $n {
            tracing::debug!(
                cmd = %$name,
                got = %$rest.len(),
                want = %$n,
                "wrong number of arguments"
            );
            return Frame::Error(format!(
                "ERR wrong number of arguments for '{}' command",
                $name
            ));
        }
    };
}
```

- [ ] **Step 4: Run the test to verify it passes, then the full plan verification**

```bash
cargo test -p rocket-mem dispatcher::tests::require_args_arity_failure_logs_a_debug_event_with_command_and_counts
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: the new test PASSES, every pre-existing test in the workspace still passes unchanged, fmt clean, clippy clean.

- [ ] **Step 5: Re-run the benchmark gate**

```bash
cd /home/numericlabs/data/rocket/rocket-mem
./scripts/benchmark.sh
```

Compare the `SET`/`GET` requests/sec against the Mean column in `docs/benchmarks/2026-09-09-pre-logging-baseline.md` (plan 01, Task 1). Expected: within 2%, and trivially so — all three `debug!` call sites this plan adds sit strictly inside branches a successful `SET`/`GET` never takes (an already-failed arity check, an already-failed engine call, or an already-unrecognized command name), so the successful-command path this benchmark exercises executes none of this plan's new code.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(logging): add debug event to the require_args! arity-check macro"
```

---

## Next plan

[`11-acl-events.md`](11-acl-events.md) — auth success/failure, permission-denied, HELLO/RESP3 upgrade, and `ACL SETUSER`/`DELUSER` events, all in `dispatcher.rs`'s auth and ACL handling.
