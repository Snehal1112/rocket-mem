# Verbose Logging Plan 08: `trace`-Level Argument Logging

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `trace`-level line that renders a command's full argument list through `logging::redact_args`, and thread `Config::log_value_max_bytes` down to `dispatch_and_log` so that renderer has its truncation cap — with the default `info` path paying nothing at all.

**Architecture:** Two problems have to be solved together, and getting either wrong is the failure mode of this plan.

**The move.** `dispatch_and_log` takes `frame: Frame` by value and moves it into `dispatch_and_log_inner` at line 3024. Any argument the log line needs must be read out *before* that move. The existing code already does exactly this, and says so, at line 3018: `let name = command_name_upper(&frame); // read before 'frame' is moved into the inner call`. The trace line's arguments follow the same rule.

**The cost of obeying it.** Reading them out means building a `Vec<Bytes>` — one heap allocation plus a refcount bump per argument. Done unconditionally, that allocation happens on *every command at every level*, including the production default of `info` where the line will never be printed. Unlike a `tracing` macro's field expressions, code written before the macro is not lazy: nothing short-circuits it. So the extraction is wrapped in `tracing::enabled!(tracing::Level::TRACE)`, which is the same relaxed atomic load and branch the macro itself would do. **At `info`, the whole thing is one branch and no allocation.** This is the single most important point in the plan; a reviewer should check it first.

The cap is threaded through `ReplicationHandle` rather than a `OnceLock` — see Task 1 for the argument.

**Tech Stack:** Rust 2021, `tracing 0.1`, `bytes::Bytes`, `figment` (via `Config`), `scripts/benchmark.sh`.

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see "Decision: log content — values at `trace`, secrets never" and the Dispatch row of the event catalogue.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting.

The ones that decide this plan: **no new atomic counters on the hot path**, **`Bytes` is never logged via `Debug`**, **redaction policy lives only in `crates/server`**, and the **≤2% throughput regression gate** against [`docs/benchmarks/2026-09-09-pre-logging-baseline.md`](../../../benchmarks/2026-09-09-pre-logging-baseline.md).

**Additional constraint specific to this plan:** the redaction assertion in Task 2 is a security test. If it fails, stop and report — do not weaken it.

---

### Task 1: Thread `log_value_max_bytes` to the dispatcher

**Files:**
- Modify: `crates/server/src/replication.rs` — add a field to `ReplicationHandle` (struct ends at line 204, so insert beside `slowlog` at line 192); initialize it in `ReplicationHandle::new` (lines 207–231); add the builder beside `with_slowlog_threshold` (lines 270–276); add the accessor beside `cluster()` (line 291)
- Modify: `crates/server/src/main.rs` — the builder chain at lines 241–248
- Test: `crates/server/src/replication.rs` (existing `#[cfg(test)] mod tests`, opens at line 731)

**Interfaces:**
- Consumes: `Config::log_value_max_bytes: u64` (default `128`), added by [plan 04](04-config-log-value-max-bytes.md).
- Produces: `ReplicationHandle::with_log_value_max_bytes(self, cap: u64) -> Self` and `ReplicationHandle::log_value_max_bytes(&self) -> usize`. Task 2 calls the accessor from inside `dispatch_and_log`.

**Why `ReplicationHandle` and not a `OnceLock`.** Both were considered:

- A `static LOG_VALUE_MAX_BYTES: OnceLock<u64>` in `logging.rs`, set once in `main.rs`. Cheapest possible read (one acquire load), and needs no signature changes. But it is process-global mutable state that every test shares: the first test to set it wins for the whole binary, so no test can exercise two different caps, and a test that sets it silently changes the behavior of every other test in the same process. It also splits configuration into two mechanisms — one field of `Config` reaching the dispatcher by a route no other field uses.
- `ReplicationHandle`, which is **already the exact same problem, already solved**: `slowlog_threshold_micros` is a `Config` field that a `dispatch_and_log`-adjacent code path needs, and it gets there via `with_slowlog_threshold` (`main.rs:247` → `replication.rs:273`), constructed once in `main.rs` and passed to `dispatch_and_log` as `replication: &ReplicationHandle` — a parameter the function *already has*. Per-handle, so tests configure it freely; discoverable, because a reader looking for how a config value reaches the dispatcher finds one pattern rather than two.

`ReplicationHandle` wins on consistency, and the extra cost is one field read off a struct already in cache. (The struct's own doc comment at `replication.rs:151` already concedes it is "shared *server* state, not a replication handle" — this is one more piece of exactly that, not a new misuse.)

- [ ] **Step 1: Write the failing test**

Add to the existing `mod tests` in `crates/server/src/replication.rs`:

```rust
    #[test]
    fn log_value_max_bytes_defaults_to_128() {
        // Every existing `ReplicationHandle::new`/`default()` call site -- ~25 of them, all
        // tests -- must keep working untouched, with the same cap `Config::default()` uses.
        assert_eq!(ReplicationHandle::default().log_value_max_bytes(), 128);
    }

    #[test]
    fn with_log_value_max_bytes_overrides_the_default() {
        let handle = ReplicationHandle::default().with_log_value_max_bytes(16);
        assert_eq!(handle.log_value_max_bytes(), 16);
    }

    #[test]
    fn with_log_value_max_bytes_saturates_an_absurd_config_value() {
        // The config field is a u64 and `fmt_value` takes a usize. On a 32-bit target a large
        // configured cap would otherwise truncate to a small one -- silently logging *less*
        // than asked. Saturate to usize::MAX instead: "no truncation" is the honest reading of
        // "cap larger than this machine can index".
        let handle = ReplicationHandle::default().with_log_value_max_bytes(u64::MAX);
        assert_eq!(handle.log_value_max_bytes(), usize::MAX);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem replication::tests::log_value_max_bytes
cargo test -p rocket-mem replication::tests::with_log_value_max_bytes
```

Expected: FAIL to compile with `error[E0599]: no method named 'log_value_max_bytes' found for struct 'ReplicationHandle'`.

- [ ] **Step 3: Add the field, the builder, and the accessor**

**3a.** In `crates/server/src/replication.rs`, add the field to the `ReplicationHandle` struct. Put it directly after the `slowlog` field (line 192), since it is the same kind of thing — a `Config` value the dispatch path reads:

```rust
    /// The truncation cap `logging::fmt_value`/`logging::redact_args` apply to each rendered
    /// argument on the `trace`-level dispatch line. Stored as `usize` so the call site needs no
    /// cast on the hot path. `main.rs` sets it from `Config::log_value_max_bytes` via
    /// `with_log_value_max_bytes`; `new`/`Default` use the same 128-byte default `Config` does,
    /// so the ~25 test-constructed handles behave identically to a real server.
    log_value_max_bytes: usize,
```

**3b.** Initialize it in `ReplicationHandle::new`'s struct literal (lines 208–230), directly after `slowlog: crate::slowlog::SlowLog::default(),` at line 227:

```rust
            log_value_max_bytes: 128,
```

**3c.** Add the builder directly after `with_slowlog_threshold` (which ends at line 276):

```rust
    /// Sets the `trace`-level argument truncation cap -- see the `log_value_max_bytes` field.
    /// A builder method, matching `with_aof`/`with_cluster`/`with_slowlog_threshold`'s existing
    /// pattern, so the ~25 existing `ReplicationHandle::new` call sites stay untouched.
    ///
    /// Takes the `u64` `Config` declares and saturates into `usize`: on a 32-bit target a cap
    /// larger than the address space would otherwise wrap to a small one and silently log
    /// *less* than configured.
    pub fn with_log_value_max_bytes(mut self, cap: u64) -> Self {
        self.log_value_max_bytes = usize::try_from(cap).unwrap_or(usize::MAX);
        self
    }
```

**3d.** Add the accessor beside `cluster()` (line 291) — a method rather than a `pub` field, matching `cluster()`/`engine()`:

```rust
    /// The `trace`-level argument truncation cap, read once per command by `dispatch_and_log`
    /// but only when `trace` is actually enabled.
    pub fn log_value_max_bytes(&self) -> usize {
        self.log_value_max_bytes
    }
```

**3e.** In `crates/server/src/main.rs`, add one line to the existing builder chain (lines 241–248), directly after `.with_slowlog_threshold(slowlog_threshold)`:

```rust
    let mut handle = rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&engine),
        snapshot_path.to_path_buf(),
    )
    .with_aof(Arc::clone(&aof))
    .with_own_addr(config.addr.clone())
    .with_slowlog_threshold(slowlog_threshold)
    .with_log_value_max_bytes(config.log_value_max_bytes)
    .with_acl_bootstrap(acl_users);
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem replication::tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: the three new tests PASS alongside every pre-existing replication test, fmt clean, clippy clean, full workspace suite green.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/replication.rs crates/server/src/main.rs
git commit -m "feat(logging): thread log_value_max_bytes to the dispatcher via ReplicationHandle"
```

---

### Task 2: The `trace`-level argument line

**Files:**
- Modify: `crates/server/src/dispatcher.rs` — add `command_args` beside `key_field`/`reply_kind` (added by [plan 07](07-command-span.md), after line 2885); amend `dispatch_and_log`'s body (lines 3018–3024, as plan 07 left it)
- Test: `crates/server/src/dispatcher.rs` (existing `#[cfg(test)] mod tests`, opens at line 3391)

**Interfaces:**
- Consumes: `rocket_mem::logging::redact_args(cmd: &str, args: &[Bytes], cap: usize) -> String` ([plan 03](03-logging-module-redaction.md)); `ReplicationHandle::log_value_max_bytes()` (Task 1); the `cmd` span from plan 07, which this event is emitted inside.
- Produces: `fn command_args(frame: &Frame) -> Vec<Bytes>`, private to `dispatcher.rs`; and a `TRACE` event with field `args`, message `"command arguments"`. Plan 09's integration test asserts on that message and that field.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `crates/server/src/dispatcher.rs`. The last test is the security one: it renders an `AUTH` through **the exact composition `dispatch_and_log` uses** — `command_name_upper` → `command_args` → `redact_args` — rather than calling `redact_args` with hand-written inputs, so it would catch a dispatcher that passes the wrong `cmd` string or fails to strip the command name.

```rust
    #[test]
    fn command_args_excludes_the_command_name() {
        assert_eq!(
            command_args(&cmd(&[b"SET", b"k", b"v"])),
            vec![Bytes::from_static(b"k"), Bytes::from_static(b"v")]
        );
        assert_eq!(command_args(&cmd(&[b"PING"])), Vec::<Bytes>::new());
    }

    #[test]
    fn command_args_of_a_non_array_frame_is_empty() {
        assert_eq!(command_args(&Frame::Simple("PONG".into())), Vec::<Bytes>::new());
        assert_eq!(command_args(&Frame::Array(vec![])), Vec::<Bytes>::new());
    }

    #[test]
    fn command_args_skips_non_bulk_arguments_rather_than_panicking() {
        // A client can send `*3\r\n$3\r\nSET\r\n:1\r\n$1\r\nv\r\n`. `dispatch` rejects it later;
        // the log renderer must not be the thing that falls over on it first.
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Integer(1),
            Frame::Bulk(Bytes::from_static(b"v")),
        ]);
        assert_eq!(command_args(&frame), vec![Bytes::from_static(b"v")]);
    }

    #[test]
    fn the_trace_argument_line_never_renders_an_auth_password() {
        // Composed exactly as `dispatch_and_log` composes it, so this covers the wiring and not
        // just `redact_args` in isolation.
        for frame in [
            cmd(&[b"AUTH", b"hunter2"]),
            cmd(&[b"AUTH", b"alice", b"hunter2"]),
            cmd(&[b"HELLO", b"3", b"AUTH", b"alice", b"hunter2"]),
            cmd(&[b"ACL", b"SETUSER", b"alice", b">hunter2"]),
        ] {
            let name = command_name_upper(&frame).unwrap();
            let args = command_args(&frame);
            let rendered = crate::logging::redact_args(name.as_str(), &args, 128);
            assert_eq!(rendered, "<redacted>", "leaked for {name:?}", name = name.as_str());
            assert!(!rendered.contains("hunter2"));
        }
    }

    #[test]
    fn the_trace_argument_line_renders_an_ordinary_command_in_full() {
        let frame = cmd(&[b"SET", b"k", b"v"]);
        let name = command_name_upper(&frame).unwrap();
        let args = command_args(&frame);
        assert_eq!(
            crate::logging::redact_args(name.as_str(), &args, 128),
            "k v"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem dispatcher::tests::command_args
cargo test -p rocket-mem dispatcher::tests::the_trace_argument_line
```

Expected: FAIL to compile with `error[E0425]: cannot find function 'command_args' in this scope`.

- [ ] **Step 3: Implement `command_args`**

Insert into `crates/server/src/dispatcher.rs` directly after `reply_kind` (added by plan 07):

```rust
/// The command's arguments, excluding the command name -- the same slice `dispatch` calls
/// `rest`, and the shape `logging::redact_args` expects.
///
/// Allocates a `Vec` and bumps one `Bytes` refcount per argument (no data is copied). That cost
/// is why the only caller guards this behind `tracing::enabled!(Level::TRACE)`: unlike a
/// `tracing` macro's field expressions, ordinary code before the macro is not lazy, and calling
/// this unconditionally would allocate on every command at every level.
///
/// Non-`Bulk` arguments are skipped rather than rendered: a client can legally frame an integer
/// where a bulk string belongs, `dispatch` rejects it a moment later, and the log renderer must
/// not be what falls over on it first.
fn command_args(frame: &Frame) -> Vec<Bytes> {
    let Frame::Array(items) = frame else {
        return Vec::new();
    };
    items
        .iter()
        .skip(1)
        .filter_map(|f| match f {
            Frame::Bulk(b) => Some(b.clone()),
            _ => None,
        })
        .collect()
}
```

- [ ] **Step 4: Emit the line in `dispatch_and_log`**

After plan 07, `dispatch_and_log`'s head reads:

```rust
    let name = command_name_upper(&frame); // read before `frame` is moved into the inner call
    let name = name.as_ref().map(|n| n.as_str()).unwrap_or("");
    let (first_key, arg_count) = command_key_and_arity(&frame);
    let label = metric_label(name);

    let _cmd_span = tracing::debug_span!(
        "cmd",
        cmd = %name,
        key = %key_field(first_key.as_ref()),
        argc = arg_count,
    )
    .entered();

    let started = std::time::Instant::now();

    let reply = dispatch_and_log_inner(engine, aof, replication, frame, session, client_id);
```

Insert the trace line between the span guard and `let started`, so it is emitted inside the span and, crucially, **before `frame` is moved**:

```rust
    let _cmd_span = tracing::debug_span!(
        "cmd",
        cmd = %name,
        key = %key_field(first_key.as_ref()),
        argc = arg_count,
    )
    .entered();

    // Read before `frame` is moved into the inner call below -- the same constraint the `name`
    // binding at the top of this function carries, for the same reason: `dispatch_and_log_inner`
    // consumes the frame.
    //
    // The `enabled!` guard is not an optimization, it is the point. `command_args` allocates a
    // `Vec` and bumps a refcount per argument; a `tracing` macro would never evaluate its field
    // expressions at a disabled level, but this extraction has to happen *outside* the macro to
    // beat the move, and ordinary code is not lazy. `enabled!` is the same relaxed atomic load
    // and branch the macro's own check performs, so at the production default of `info` this
    // whole block costs one branch and allocates nothing.
    if tracing::enabled!(tracing::Level::TRACE) {
        let args = command_args(&frame);
        tracing::trace!(
            args = %crate::logging::redact_args(name, &args, replication.log_value_max_bytes()),
            "command arguments"
        );
    }

    let started = std::time::Instant::now();

    let reply = dispatch_and_log_inner(engine, aof, replication, frame, session, client_id);
```

Note that `args` is rendered through `redact_args`, never `?args`: the `Vec<Bytes>` would render byte-by-byte through `Debug`, and — far worse — `Debug` performs no redaction, so an `AUTH` password would land in the log verbatim.

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem dispatcher::tests::command_args
cargo test -p rocket-mem dispatcher::tests::the_trace_argument_line
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all five new tests PASS, fmt clean, clippy clean, full workspace suite green with no pre-existing test edited.

Then confirm by eye against a real server, including that the cap is actually wired:

```bash
cargo build --release
RUST_LOG=trace ROCKET_MEM_LOG_VALUE_MAX_BYTES=8 ./target/release/rocket-mem &
redis-cli -p 6379 set trace-key abcdefghijklmnop
redis-cli -p 6379 auth hunter2
redis-cli -p 6379 shutdown nosave 2>/dev/null || kill %1
```

Expected on stderr:

```
TRACE cmd{cmd=SET key=trace-key argc=2}: rocket_mem::dispatcher: command arguments args=trace-key abcdefgh…(8 more)
TRACE cmd{cmd=AUTH key= argc=1}: rocket_mem::dispatcher: command arguments args=<redacted>
```

`hunter2` must not appear anywhere in the output. Grep for it to be sure rather than reading:

```bash
RUST_LOG=trace ./target/release/rocket-mem 2>&1 | grep -c hunter2   # must stay 0
```

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(logging): add the trace-level redacted argument line"
```

---

### Task 3: Benchmark gate

**Files:**
- Modify: `docs/benchmarks/2026-09-09-pre-logging-baseline.md` (append a results section; do **not** edit the baseline table, and do not edit plan 07's section)

**Interfaces:**
- Consumes: the baseline `SET`/`GET` requests-per-second means recorded by [plan 01](01-baseline-and-dependencies.md), Task 1.
- Produces: a recorded measurement for this plan.

No test to write first — this is the acceptance criterion for the plan. **A >2% regression at the default `info` level blocks this plan.** Compare against plan 01's baseline, not against plan 07's number: the gate is cumulative across the series, so a series of individually-tiny regressions cannot creep past it.

If this gate fails while plan 07's passed, the `enabled!` guard in Task 2, Step 4 is the first thing to check — an extraction that escaped the guard is exactly what a per-command allocation at `info` looks like.

- [ ] **Step 1: Re-run the benchmark three times at the default level**

```bash
cd /home/numericlabs/data/rocket/rocket-mem
cargo build --release
for i in 1 2 3; do
  echo "=== run $i ==="
  ./scripts/benchmark.sh
done 2>&1 | tee /tmp/rocket-mem-plan08.txt
```

Same script as plan 01, no `RUST_LOG` set, same machine. Never substitute a hand-rolled `redis-benchmark` invocation — rocket-mem cannot disable its AOF, and an unmatched-durability run is not comparable to the baseline.

- [ ] **Step 2: Measure `debug` and `trace`, which is now a real firehose**

```bash
RUST_LOG=debug ./scripts/benchmark.sh 2>&1 | tee /tmp/rocket-mem-plan08-debug.txt
RUST_LOG=trace ./scripts/benchmark.sh 2>&1 | tee /tmp/rocket-mem-plan08-trace.txt
```

Neither is gated. `trace` now renders and writes every argument of every command to stderr, so a large regression there is the expected and correct result — but it is the number an operator needs before enabling `trace` on a live node, so record it honestly.

- [ ] **Step 3: Append the results to the baseline document**

```markdown
## Plan 08 — `trace`-level argument line

| Workload | Run 1 | Run 2 | Run 3 | Mean | vs baseline |
|---|---|---|---|---|---|
| SET | | | | | |
| GET | | | | | |

At `RUST_LOG=debug`: SET , GET
At `RUST_LOG=trace`: SET , GET  (every argument of every command is rendered and
written to stderr -- not gated; this is the cost of the firehose, recorded so an
operator can decide whether to pay it on a live node.)

Gate: PASS / FAIL (<=2% at default `info`, against plan 01's baseline).
```

- [ ] **Step 4: Commit**

```bash
git add docs/benchmarks/2026-09-09-pre-logging-baseline.md
git commit -m "docs(bench): record trace-argument-line throughput against the baseline"
```

---

## Next plan

[`09-level-separation-test.md`](09-level-separation-test.md) — the integration test that proves the lines added by plans 07 and 08 stay silent at the production default of `info`, so a level regression cannot quietly enable the firehose.
