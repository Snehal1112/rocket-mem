# Verbose Logging Plan 09: The Level-Separation Integration Test

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove, in CI, that the per-command lines added by plans 07 and 08 emit **nothing** at the production default of `info`, emit at `debug`, and render arguments only at `trace` — so a level regression can never silently turn a production node into a firehose.

**Architecture:** The spec names this the failure mode with the worst consequences in the whole series: a one-character change from `debug_span!` to `info_span!`, or `debug!` to `info!`, is invisible in review, passes every existing test, and turns every production node into a per-request log emitter — with `trace` writing plaintext user data to disk. Nothing else in the series catches it. `cargo clippy` will not, and the benchmark gate only runs when a plan remembers to run it.

The test captures real subscriber output rather than inspecting the code: a `MakeWriter` that appends into an `Arc<Mutex<Vec<u8>>>`, a `tracing_subscriber::fmt` subscriber writing into it, and `tracing::subscriber::with_default` scoping that subscriber to one closure. No `tracing-mock` dependency is added — the crate already depends on `tracing-subscriber` (with the `env-filter` feature), which is everything this needs, and a mock would assert on callsite metadata rather than on the bytes an operator actually sees.

**The gotcha, stated once because it decides the whole file's shape:** `tracing::subscriber::with_default` installs the subscriber **for the current thread only**, and `cargo test` runs test functions on parallel threads. Every dispatch call whose output the test asserts on must therefore happen *inside* the `with_default` closure, on that same thread. These are plain `#[test]` functions, not `#[tokio::test]`, precisely so no work can escape onto a runtime worker thread — `dispatch_and_log` is synchronous, so there is no reason to introduce one. The upside of per-thread scoping is that these tests need no serialization and cannot see each other's output.

**Tech Stack:** Rust 2021, `tracing 0.1`, `tracing-subscriber 0.3` (`fmt` + `env-filter`), `tempfile` (dev-dependency).

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see Testing, item 3 ("Level separation").

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting.

The ones that decide this plan: `cargo clippy --workspace --all-targets -- -D warnings` **lints test code too**, so the harness must be warning-free; and every pre-existing test must keep passing unchanged.

This plan adds no code to any hot path and therefore carries no benchmark gate.

---

### Task 1: The capture harness and the `info` / `debug` separation

**Files:**
- Create: `crates/server/tests/logging.rs` (the crate's integration tests already live here: `integration.rs`, `metrics.rs`, `replication.rs`, `cluster.rs`, `rmp.rs`, `tls.rs`, `kill_and_recover.rs`)

**Interfaces:**
- Consumes: `rocket_mem::dispatcher::dispatch_and_log` and `Session::new` (both `pub`); `rocket_mem::aof::{AofWriter, FsyncPolicy}`; `rocket_mem::replication::ReplicationHandle::default`; `engine::Engine::new`; `protocol::Frame`. The `debug!` line's message `"command dispatched"` and its `elapsed_us` field, from [plan 07](07-command-span.md), Task 2.
- Produces: `fn capture_at(level: &str) -> String` inside `tests/logging.rs`. Tasks 2 and 3 call it.

- [ ] **Step 1: Write the failing tests**

Create `crates/server/tests/logging.rs` containing the imports and the two tests, but **not** the harness — so it fails on a missing symbol rather than on behavior:

```rust
//! Level separation for the per-command dispatch logging.
//!
//! The spec's worst failure mode: a one-character change from `debug_span!` to `info_span!`, or
//! `debug!` to `info!`, in `dispatcher.rs` turns every production node into a per-request log
//! emitter -- and at `trace`, into one writing plaintext user data to disk. Review does not
//! reliably catch it and no other test does, so this file asserts on the bytes a subscriber
//! actually produces at each level.
//!
//! See ../../../docs/superpowers/specs/2026-09-09-verbose-logging-design.md.

#[test]
fn info_emits_no_per_command_lines() {
    let output = capture_at("info");
    assert!(
        !output.contains("command dispatched"),
        "the per-command debug line escaped to the production default level; output was:\n{output}"
    );
    assert!(
        !output.contains("elapsed_us"),
        "the per-command debug line's fields escaped to `info`; output was:\n{output}"
    );
    assert!(
        !output.contains("level-key"),
        "a key reached the log at `info`; output was:\n{output}"
    );
}

#[test]
fn debug_emits_the_per_command_line() {
    let output = capture_at("debug");
    assert!(
        output.contains("command dispatched"),
        "the per-command debug line is missing at `debug`; output was:\n{output}"
    );
    assert!(
        output.contains("elapsed_us"),
        "the per-command line lost its elapsed_us field; output was:\n{output}"
    );
    assert!(
        output.contains("reply=ok"),
        "the per-command line lost its reply field; output was:\n{output}"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --test logging
```

Expected: FAIL to compile with `error[E0425]: cannot find function 'capture_at' in this scope`.

- [ ] **Step 3: Implement the harness**

Add above the tests in `crates/server/tests/logging.rs`:

```rust
use bytes::Bytes;
use protocol::Frame;
use std::io;
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::EnvFilter;

/// A `MakeWriter` that appends everything the subscriber writes into a shared buffer, so a test
/// can assert on the exact bytes an operator would see on stderr.
///
/// `Arc<Mutex<Vec<u8>>>` rather than a plain `Vec`: `MakeWriter::make_writer` hands out a fresh
/// writer per event and takes `&self`, so the buffer has to be shared and interior-mutable. It
/// also has to be `Send + Sync + 'static` for `Dispatch` to accept the subscriber.
#[derive(Clone)]
struct BufferWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for BufferWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for BufferWriter {
    type Writer = BufferWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Dispatches one `SET level-key level-value` with a subscriber filtered to `level`, and returns
/// everything that subscriber wrote.
///
/// Every dispatch happens inside the `with_default` closure, on the calling thread, and that is
/// mandatory rather than stylistic: `tracing::subscriber::with_default` installs the subscriber
/// for the *current thread only*, and `cargo test` runs tests on parallel threads. Work moved
/// off this thread -- a `tokio::spawn`, a `std::thread::spawn`, or an `.await` on a multi-thread
/// runtime -- would log to the global (empty) subscriber and this function would return "".
/// These are plain `#[test]` functions for exactly that reason; `dispatch_and_log` is
/// synchronous and needs no runtime. The flip side is the useful one: per-thread scoping means
/// these tests need no serialization and cannot capture each other's output.
fn capture_at(level: &str) -> String {
    capture_frames_at(level, vec![set_frame()])
}

/// `capture_at` with the frames spelled out, for the tests that need a command other than `SET`.
fn capture_frames_at(level: &str, frames: Vec<Frame>) -> String {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_env_filter(EnvFilter::new(level))
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine::Engine::new();
        let aof = rocket_mem::aof::AofWriter::open(
            &dir.path().join("logging-test.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .expect("open aof");
        let replication = rocket_mem::replication::ReplicationHandle::default();
        let session = rocket_mem::dispatcher::Session::new();

        for frame in frames {
            rocket_mem::dispatcher::dispatch_and_log(
                &engine,
                &aof,
                &replication,
                frame,
                &session,
                1,
            );
        }
    });

    let bytes = buffer.lock().unwrap_or_else(|e| e.into_inner()).clone();
    String::from_utf8(bytes).expect("subscriber output is utf-8")
}

/// `SET level-key level-value`. A write command on purpose: it is the shape that exercises the
/// most of `dispatch_and_log_inner` (AOF append, replica fan-out) while still returning a
/// plain `+OK`.
fn set_frame() -> Frame {
    Frame::Array(vec![
        Frame::Bulk(Bytes::from_static(b"SET")),
        Frame::Bulk(Bytes::from_static(b"level-key")),
        Frame::Bulk(Bytes::from_static(b"level-value")),
    ])
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --test logging
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: both tests PASS, fmt clean, clippy clean (remember it lints this file too), full workspace suite green.

If `debug_emits_the_per_command_line` fails with an empty `output`, the likely cause is work escaping the closure's thread — re-read the `capture_frames_at` doc comment before changing anything else.

- [ ] **Step 5: Prove the test has teeth**

A guard test that cannot fail is worse than no test, and this one asserts on behavior that is already implemented, so its failure mode has to be demonstrated by hand once:

Edit `crates/server/src/dispatcher.rs` by hand — this is a temporary, deliberately-broken edit that gets reverted two steps below. Change the `tracing::debug!(` that opens the `"command dispatched"` line to `tracing::info!(`. Then:

```bash
cargo test -p rocket-mem --test logging info_emits_no_per_command_lines
```

Expected: **FAIL**, with `the per-command debug line escaped to the production default level`. That is exactly the regression this file exists to catch.

Revert the edit and confirm green again:

```bash
git checkout -- crates/server/src/dispatcher.rs
cargo test -p rocket-mem --test logging
```

Expected: PASS. Do not commit until `git diff crates/server/src/dispatcher.rs` is empty.

- [ ] **Step 6: Commit**

```bash
git add crates/server/tests/logging.rs
git commit -m "test(logging): assert per-command lines are silent at info and present at debug"
```

---

### Task 2: `trace` renders arguments, and never a credential

**Files:**
- Modify: `crates/server/tests/logging.rs`

**Interfaces:**
- Consumes: `capture_at` / `capture_frames_at` / `set_frame` (Task 1); the `trace!` line's message `"command arguments"` and its `args` field, from [plan 08](08-command-trace-args.md), Task 2.
- Produces: nothing consumed later.

This is the second half of the separation guarantee — `debug` must not leak values, and `trace` must still redact. The redaction half is a security assertion: **if it fails, stop and report; do not weaken it.**

- [ ] **Step 1: Write the failing tests**

Append to `crates/server/tests/logging.rs`:

```rust
#[test]
fn debug_logs_the_key_but_not_the_value() {
    // The level taxonomy in one assertion: `debug` is "what happened" (the key), `trace` is
    // "what the bytes were" (the value). A value leaking into `debug` would make the level an
    // operator is told is safe to leave on in production a data-exposure decision instead.
    let output = capture_at("debug");
    assert!(
        output.contains("level-key"),
        "the key is missing from the debug line; output was:\n{output}"
    );
    assert!(
        !output.contains("level-value"),
        "a stored value reached the log at `debug`; output was:\n{output}"
    );
    assert!(
        !output.contains("command arguments"),
        "the trace argument line escaped to `debug`; output was:\n{output}"
    );
}

#[test]
fn trace_renders_the_full_argument_list() {
    let output = capture_at("trace");
    assert!(
        output.contains("command arguments"),
        "the trace argument line is missing at `trace`; output was:\n{output}"
    );
    assert!(
        output.contains("args=level-key level-value"),
        "the argument list did not render as text; output was:\n{output}"
    );
}

#[test]
fn trace_never_renders_a_credential() {
    let output = capture_frames_at(
        "trace",
        vec![
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"AUTH")),
                Frame::Bulk(Bytes::from_static(b"alice")),
                Frame::Bulk(Bytes::from_static(b"hunter2")),
            ]),
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"HELLO")),
                Frame::Bulk(Bytes::from_static(b"3")),
                Frame::Bulk(Bytes::from_static(b"AUTH")),
                Frame::Bulk(Bytes::from_static(b"alice")),
                Frame::Bulk(Bytes::from_static(b"hunter2")),
            ]),
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"ACL")),
                Frame::Bulk(Bytes::from_static(b"SETUSER")),
                Frame::Bulk(Bytes::from_static(b"alice")),
                Frame::Bulk(Bytes::from_static(b">hunter2")),
            ]),
        ],
    );
    assert!(
        output.contains("<redacted>"),
        "a credential-carrying command was not redacted at all; output was:\n{output}"
    );
    // The whole point, asserted at the highest-volume level, on the real dispatch path.
    assert!(
        !output.contains("hunter2"),
        "a password reached the log at `trace`; output was:\n{output}"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run them before touching anything, since the harness already exists:

```bash
cargo test -p rocket-mem --test logging trace_
cargo test -p rocket-mem --test logging debug_logs_the_key
```

Expected: `trace_renders_the_full_argument_list` and `trace_never_renders_a_credential` FAIL with `the trace argument line is missing at 'trace'` if [plan 08](08-command-trace-args.md) has not landed. If plan 08 has landed, all three PASS immediately — that is legitimate for a regression guard, and Step 4 is what proves they have teeth.

- [ ] **Step 3: Fix whatever failed**

If plan 08 is complete and a test still fails, the failure is real and belongs to `dispatcher.rs`, not to this file:

- `command arguments` missing at `trace` → the `tracing::enabled!(tracing::Level::TRACE)` guard in `dispatch_and_log` is inverted, or the `trace!` was placed after `frame` is moved and no longer compiles/runs.
- `hunter2` present → `redact_args` is not being called, or is being called with the wrong `cmd` string (it must be the **uppercased** name from `command_name_upper`, and `args` must **exclude** the command name). Fix `dispatcher.rs`; do not relax the assertion.
- `level-value` present at `debug` → the `trace!` line was written at `debug` level.

- [ ] **Step 4: Prove these tests have teeth too**

By hand, in `crates/server/src/dispatcher.rs`, change the `trace!` line's `args` field from `%crate::logging::redact_args(name, &args, replication.log_value_max_bytes())` to `?args`, which is what an author skipping redaction would naturally write. Then:

```bash
cargo test -p rocket-mem --test logging trace_never_renders_a_credential
```

Expected: **FAIL**, with `a password reached the log at 'trace'`.

Revert and confirm green:

```bash
git checkout -- crates/server/src/dispatcher.rs
cargo test -p rocket-mem --test logging
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all five tests in the file PASS, fmt clean, clippy clean, workspace green, and `git diff crates/server/src/dispatcher.rs` empty.

- [ ] **Step 5: Commit**

```bash
git add crates/server/tests/logging.rs
git commit -m "test(logging): assert trace renders arguments and still redacts credentials"
```

---

### Task 3: The key is rendered as text, never as `Debug` bytes

**Files:**
- Modify: `crates/server/tests/logging.rs`

**Interfaces:**
- Consumes: `capture_at` (Task 1); the `cmd` span's `key` field, from [plan 07](07-command-span.md), Task 1.
- Produces: nothing consumed later.

The hot-path rule "`Bytes` is never logged via `Debug`" appears in every plan's Global Constraints, and until now nothing enforces it — `key = ?first_key` compiles, passes clippy, and passes every other test. It is also not merely cosmetic: `Bytes`'s `Debug` impl formats byte-by-byte, so the mistake costs O(len) of formatting on the project's hottest path in addition to producing an ungreppable log.

- [ ] **Step 1: Write the failing test**

Append to `crates/server/tests/logging.rs`:

```rust
#[test]
fn the_span_renders_the_key_as_text_not_as_debug_bytes() {
    let output = capture_at("debug");
    assert!(
        output.contains("key=level-key"),
        "the cmd span's key field is missing or not rendered as text; output was:\n{output}"
    );
    // The two shapes the mistake produces. `key = ?first_key` on an `Option<Bytes>` renders as
    // `Some(b"level-key")`; `key = ?key` on a bare `Bytes` renders as `b"level-key"`. Both are
    // ungreppable, and both cost O(len) of formatting on the hottest path in the project.
    assert!(
        !output.contains("Some(b\""),
        "the key was logged through Debug; output was:\n{output}"
    );
    assert!(
        !output.contains("key=b\""),
        "the key was logged through Debug; output was:\n{output}"
    );
}

#[test]
fn the_span_carries_the_command_name_and_arity() {
    // The field vocabulary the spec fixes (`cmd`, `key`, `argc`) is what makes one grep follow
    // an activity end to end -- a renamed field breaks every runbook written against it.
    let output = capture_at("debug");
    assert!(
        output.contains("cmd=SET"),
        "the cmd span lost its command name; output was:\n{output}"
    );
    assert!(
        output.contains("argc=2"),
        "the cmd span lost its arity; output was:\n{output}"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --test logging the_span_
```

Expected: both PASS immediately if [plan 07](07-command-span.md) is correct — they are regression guards over implemented behavior, and Step 4 demonstrates their failure mode. If either FAILS, the message names the problem directly; the likely cause is that the default `fmt` formatter is not printing span context, in which case check that `capture_frames_at` uses `tracing_subscriber::fmt()` unmodified (the `Full` format prints the current span scope by default, and no `.without_time()`/custom `.event_format()` should have been added).

- [ ] **Step 3: Fix whatever failed**

If `key=level-key` is absent but `Some(b"level-key")` is present, `dispatch_and_log`'s span was written as `key = ?first_key`. Change it back to the form [plan 07](07-command-span.md) specifies:

```rust
        key = %key_field(first_key.as_ref()),
```

- [ ] **Step 4: Prove the test has teeth**

By hand, in `crates/server/src/dispatcher.rs`, change the span's key field from `key = %key_field(first_key.as_ref())` to `key = ?first_key`. Then:

```bash
cargo test -p rocket-mem --test logging the_span_renders_the_key_as_text_not_as_debug_bytes
```

Expected: **FAIL**, with `the key was logged through Debug`.

Revert and confirm the whole file is green:

```bash
git checkout -- crates/server/src/dispatcher.rs
cargo test -p rocket-mem --test logging
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all seven tests in `tests/logging.rs` PASS, fmt clean, clippy clean, workspace green, and `git diff crates/server/src/dispatcher.rs` empty.

- [ ] **Step 5: Commit**

```bash
git add crates/server/tests/logging.rs
git commit -m "test(logging): assert the cmd span renders its key as text, not Debug bytes"
```

---

## Next plan

[`10-dispatch-error-events.md`](10-dispatch-error-events.md) — adds the `debug`-level events for unknown commands, WRONGTYPE, and arity errors, the remaining Dispatch rows of the spec's event catalogue.
