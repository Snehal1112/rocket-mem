# Verbose Logging Plan 07: The `cmd` Span & The Per-Command `debug!` Line

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open the `cmd` span inside `dispatcher.rs`'s `dispatch_and_log`, and emit one `debug!` line per dispatched command carrying `elapsed_us` and the reply kind — without adding a single new computation, allocation, or atomic to the default `info` path.

**Architecture:** This is the hottest code in the project: every RESP command and every RMP command funnels through `dispatch_and_log`, and [`docs/benchmarks/2026-09-07-flamegraph-notes.md`](../../../benchmarks/2026-09-07-flamegraph-notes.md) already names dispatcher overhead as a contributor to the measured `redis-benchmark` gap. The spec put the `cmd` span here for exactly one reason: `dispatch_and_log` **already** computes `name` (uppercased, into a stack buffer), `(first_key, arg_count)`, `label`, and `elapsed` for the metrics and slowlog paths. The span reuses those four values verbatim. It adds correlation, not work.

Two consequences follow, and both are load-bearing:

1. **The span is opened at `DEBUG`, not `INFO`.** `tracing`'s span macros evaluate their field expressions *only* when the callsite is enabled. A `debug_span!` at the production default of `info` short-circuits on a relaxed atomic load and a branch, and the `key` field's UTF-8 validation never runs. An `info_span!` would run it on every single command in production — which is the one change in this plan most likely to fail the 2% gate.
2. **`key` is a `Bytes` and is never rendered via `Debug`.** `Bytes`'s `Debug` impl prints byte-by-byte (`b"k"` renders as a list), which is both unreadable and O(len) of allocation churn. It goes through a lossy-UTF8 `Cow` instead, which borrows rather than allocates for the overwhelmingly common case of a valid-UTF-8 key.

**Tech Stack:** Rust 2021, `tracing 0.1`, `bytes::Bytes`, `scripts/benchmark.sh`.

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see "Decision: three spans, not per-function instrumentation" and the Dispatch row of the event catalogue.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting.

The ones that decide this plan: **no new atomic counters on the hot path**, **`Bytes` is never logged via `Debug`**, **no `format!` outside a log macro's argument list**, and the **≤2% throughput regression gate** against [`docs/benchmarks/2026-09-09-pre-logging-baseline.md`](../../../benchmarks/2026-09-09-pre-logging-baseline.md).

One clarification on the `%`/`?` sigil rule, because it is easy to over-apply here: the rule exists to keep formatting lazy. Primitives that `tracing` records natively — `usize`, `u128`, `bool`, `&'static str` — are already `Value`s and are passed with no sigil. Anything that needs formatting to become text (the `key` `Cow`) takes `%`.

---

### Task 1: Open the `cmd` span

**Files:**
- Modify: `crates/server/src/dispatcher.rs` — add `key_field` beside `command_key_and_arity` (currently line 2871–2885, so insert after line 2885 and before `metric_label` at line 2890); amend `dispatch_and_log`'s body (lines 3018–3023)
- Test: `crates/server/src/dispatcher.rs` (existing `#[cfg(test)] mod tests`, opens at line 3391)

**Interfaces:**
- Consumes: `command_name_upper` (line 2848), `command_key_and_arity` (line 2871) — both already called by `dispatch_and_log`, unchanged.
- Produces: `fn key_field(key: Option<&Bytes>) -> std::borrow::Cow<'_, str>`, private to `dispatcher.rs`; and a `DEBUG`-level `cmd` span, entered for the remainder of `dispatch_and_log`. Task 2's `debug!` line and every log line emitted anywhere beneath the dispatch call inherit its `cmd`/`key`/`argc` fields. Plan 09's integration test asserts on those fields.

- [ ] **Step 1: Write the failing test**

`key_field` is the whole reason a `Bytes` never reaches the log via `Debug`, so it is the piece worth testing directly. Add to the existing `mod tests` in `crates/server/src/dispatcher.rs`:

```rust
    #[test]
    fn key_field_renders_a_key_as_text_never_as_debug_bytes() {
        let key = Bytes::from_static(b"mykey");
        assert_eq!(key_field(Some(&key)), "mykey");
        // The failure this guards: `Bytes`'s Debug impl renders byte-by-byte, so a key logged
        // with `?` comes out as `b"mykey"` or a numeric list. Neither is greppable, and both
        // are O(len) of formatting on the hottest path in the project.
        assert!(!key_field(Some(&key)).contains('['));
        assert!(!key_field(Some(&key)).contains("b\""));
    }

    #[test]
    fn key_field_renders_a_keyless_command_as_an_empty_string() {
        // PING, and every other command `command_key_and_arity` returns `None` for -- including
        // AUTH, which it deliberately reports as keyless so the password can never surface here.
        assert_eq!(key_field(None), "");
    }

    #[test]
    fn key_field_renders_a_non_utf8_key_lossily_without_panicking() {
        let key = Bytes::from_static(&[0x61, 0xff, 0x62]);
        assert_eq!(key_field(Some(&key)), "a\u{fffd}b");
    }

    #[test]
    fn key_field_borrows_a_valid_utf8_key_instead_of_allocating() {
        // This is the perf claim the span rests on: for a valid-UTF-8 key the Cow is Borrowed,
        // so entering the span copies no key bytes.
        let key = Bytes::from_static(b"mykey");
        assert!(matches!(
            key_field(Some(&key)),
            std::borrow::Cow::Borrowed(_)
        ));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem dispatcher::tests::key_field
```

Expected: FAIL to compile with `error[E0425]: cannot find function 'key_field' in this scope`.

- [ ] **Step 3: Implement `key_field`**

Insert into `crates/server/src/dispatcher.rs` directly after `command_key_and_arity` ends (line 2885) and before `metric_label`'s doc comment (line 2887):

```rust
/// Renders `command_key_and_arity`'s first key for the `cmd` span's `key` field.
///
/// A `Bytes` must never reach a log line through `Debug`: that impl renders byte-by-byte, so a
/// key logged with `?` comes out unreadable *and* costs O(len) of formatting on the hottest
/// path in the project. Lossy UTF-8 is the right rendering instead -- and for a valid-UTF-8
/// key, which is essentially all of them, `from_utf8_lossy` returns a `Cow::Borrowed` and
/// copies nothing.
///
/// `None` -- a keyless command such as `PING`, and `AUTH`, which `command_key_and_arity`
/// deliberately reports as keyless -- renders as the empty string rather than a literal
/// `"None"`, so a `key=` field is either a real key or visibly absent.
fn key_field(key: Option<&Bytes>) -> std::borrow::Cow<'_, str> {
    match key {
        Some(k) => String::from_utf8_lossy(k),
        None => std::borrow::Cow::Borrowed(""),
    }
}
```

- [ ] **Step 4: Open the span in `dispatch_and_log`**

`dispatch_and_log`'s body currently reads (lines 3018–3024):

```rust
    let name = command_name_upper(&frame); // read before `frame` is moved into the inner call
    let name = name.as_ref().map(|n| n.as_str()).unwrap_or("");
    let (first_key, arg_count) = command_key_and_arity(&frame);
    let label = metric_label(name);
    let started = std::time::Instant::now();

    let reply = dispatch_and_log_inner(engine, aof, replication, frame, session, client_id);
```

Insert the span between `let label = ...` and `let started = ...`, so span construction is outside the timed region:

```rust
    let name = command_name_upper(&frame); // read before `frame` is moved into the inner call
    let name = name.as_ref().map(|n| n.as_str()).unwrap_or("");
    let (first_key, arg_count) = command_key_and_arity(&frame);
    let label = metric_label(name);

    // The `cmd` span. Every field here is a value this function already computed for the
    // metrics and slow-log paths just above -- the span adds correlation, not computation, and
    // `first_key` is already cloned by `command_key_and_arity` (one `Bytes` refcount bump, no
    // data copy), so the span adds no clone of its own either.
    //
    // DEBUG, not INFO, and that is the load-bearing choice: `tracing`'s span macros evaluate
    // their field expressions only when the callsite is enabled, so at the production default
    // of `info` this whole statement is a relaxed atomic load and a branch, and
    // `key_field`'s UTF-8 validation never runs. An `info_span!` here would run it on every
    // command in production.
    //
    // `key` goes through `key_field`, never `?first_key`: `Bytes`'s Debug impl renders
    // byte-by-byte. See this plan's Architecture section.
    //
    // The guard must be bound to a *named* variable. `let _ = ....entered()` drops the
    // `EnteredSpan` immediately and the span closes before `dispatch_and_log_inner` is even
    // called, silently losing every nested field.
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

Three things to note about why this compiles as written:

- `key_field(first_key.as_ref())` borrows `first_key`; the `Cow` is a temporary inside the macro expression and is dropped at the end of that statement. `first_key` is still moved into `replication.slowlog.maybe_record(...)` at the bottom of the function, unchanged — there is no borrowck conflict.
- `argc = arg_count` takes no sigil: `usize` is recorded natively by `tracing-core` (`record_u64`). `cmd = %name` takes `%` because `name` is a `&str` slice of the `CommandName` stack buffer.
- `Span::entered(self)` consumes the span, so `_cmd_span` owns it. `dispatch_and_log` is a synchronous function with no `.await`, so there is no held-across-await hazard for clippy to flag.

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem dispatcher::tests::key_field
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: the four `key_field` tests PASS, fmt clean, clippy clean, and **every pre-existing test passes unchanged** — the span changes no control flow and no reply, so any test that now fails means something else moved.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(logging): open the cmd span in dispatch_and_log"
```

---

### Task 2: Emit the per-command `debug!` line

**Files:**
- Modify: `crates/server/src/dispatcher.rs` — add `reply_kind` beside `key_field` (inserted in Task 1, after line 2885); amend the tail of `dispatch_and_log` (lines 3026–3037)
- Test: `crates/server/src/dispatcher.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: the `cmd` span from Task 1 (the line is emitted while the span is entered, so it inherits `cmd`/`key`/`argc`), and `elapsed`, already computed at line 3026 for the metrics histogram and the slow log.
- Produces: `fn reply_kind(reply: &Frame) -> &'static str`, private to `dispatcher.rs`; and a `DEBUG` event with fields `elapsed_us` and `reply`, message `"command dispatched"`. Plan 09's level-separation test asserts on exactly that message text and those fields.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn reply_kind_reports_error_replies_as_errors() {
        assert_eq!(
            reply_kind(&Frame::Error("ERR unknown command 'nope'".into())),
            "error"
        );
        assert_eq!(
            reply_kind(&Frame::Error("WRONGTYPE Operation against a key".into())),
            "error"
        );
    }

    #[test]
    fn reply_kind_reports_every_other_reply_shape_as_ok() {
        // Deliberately the same two-way split `dispatch_and_log` already uses for its
        // `rocket_mem_command_errors_total` counter -- one classification, not two that can
        // drift apart. A Null reply is a successful NX/XX no-op, not an error.
        assert_eq!(reply_kind(&Frame::Simple("OK".into())), "ok");
        assert_eq!(reply_kind(&Frame::Integer(1)), "ok");
        assert_eq!(reply_kind(&Frame::Bulk(Bytes::from_static(b"v"))), "ok");
        assert_eq!(reply_kind(&Frame::Null), "ok");
        assert_eq!(reply_kind(&Frame::Array(vec![])), "ok");
        assert_eq!(reply_kind(&Frame::Map(vec![])), "ok");
    }

    #[test]
    fn reply_kind_returns_a_static_str_so_the_debug_line_allocates_nothing() {
        // The field must be a `&'static str`, not a formatted String: this runs once per
        // command whenever `debug` is on, and a per-command allocation there is exactly the
        // kind of cost the benchmark gate exists to catch.
        let kind: &'static str = reply_kind(&Frame::Null);
        assert_eq!(kind, "ok");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem dispatcher::tests::reply_kind
```

Expected: FAIL to compile with `error[E0425]: cannot find function 'reply_kind' in this scope`.

- [ ] **Step 3: Implement `reply_kind`**

Insert into `crates/server/src/dispatcher.rs` directly after `key_field` (added in Task 1):

```rust
/// The `reply` field for the per-command `debug!` line: `"error"` for an error reply, `"ok"`
/// for every other frame shape.
///
/// The same `matches!(reply, Frame::Error(_))` split `dispatch_and_log` already uses to
/// increment `rocket_mem_command_errors_total`, deliberately reused rather than restated, so
/// the log and the error-rate metric can never disagree about what an error is. A `Frame::Null`
/// is a successful `SET ... NX` no-op, not a failure, and is reported as `"ok"` by both.
///
/// Returns `&'static str`, never a formatted `String`: this is evaluated once per command
/// whenever `debug` is enabled.
fn reply_kind(reply: &Frame) -> &'static str {
    if matches!(reply, Frame::Error(_)) {
        "error"
    } else {
        "ok"
    }
}
```

- [ ] **Step 4: Emit the line**

`dispatch_and_log`'s tail currently reads (lines 3026–3038):

```rust
    let elapsed = started.elapsed();
    replication.command_executed();
    ::metrics::counter!("rocket_mem_commands_total", "cmd" => label).increment(1);
    ::metrics::histogram!("rocket_mem_command_duration_seconds", "cmd" => label)
        .record(elapsed.as_secs_f64());
    if matches!(reply, Frame::Error(_)) {
        ::metrics::counter!("rocket_mem_command_errors_total", "cmd" => label).increment(1);
    }
    replication
        .slowlog
        .maybe_record(name, first_key, arg_count, elapsed);
    reply
}
```

Add the `debug!` between the slow-log call and the `reply` return, so it is the last thing to happen while `_cmd_span` is still entered:

```rust
    let elapsed = started.elapsed();
    replication.command_executed();
    ::metrics::counter!("rocket_mem_commands_total", "cmd" => label).increment(1);
    ::metrics::histogram!("rocket_mem_command_duration_seconds", "cmd" => label)
        .record(elapsed.as_secs_f64());
    if matches!(reply, Frame::Error(_)) {
        ::metrics::counter!("rocket_mem_command_errors_total", "cmd" => label).increment(1);
    }
    replication
        .slowlog
        .maybe_record(name, first_key, arg_count, elapsed);

    // The per-command line. `cmd`, `key`, and `argc` are not repeated here -- they are on the
    // `cmd` span this event is emitted inside, so the subscriber renders them as span context.
    //
    // `elapsed` is the same `Duration` the metrics histogram and the slow log were just handed;
    // `as_micros()` is a division on an already-materialized value, and `u128` is recorded
    // natively by `tracing-core` (`record_u128`), so neither field allocates. Both fields are
    // evaluated only when the callsite is enabled, which at the default `info` it is not.
    tracing::debug!(
        elapsed_us = elapsed.as_micros(),
        reply = reply_kind(&reply),
        "command dispatched"
    );

    reply
}
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem dispatcher::tests::reply_kind
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: the three `reply_kind` tests PASS, fmt clean, clippy clean, full workspace suite green with no pre-existing test edited.

Then confirm the line actually appears, by eye, against a real server:

```bash
cargo build --release
RUST_LOG=debug ./target/release/rocket-mem &
redis-cli -p 6379 set eyeball-key eyeball-value
redis-cli -p 6379 get eyeball-key
redis-cli -p 6379 lpush eyeball-key nope   # a WRONGTYPE, to see reply=error
redis-cli -p 6379 shutdown nosave 2>/dev/null || kill %1
```

Expected on stderr: three lines shaped like

```
DEBUG cmd{cmd=SET key=eyeball-key argc=2}: rocket_mem::dispatcher: command dispatched elapsed_us=31 reply=ok
DEBUG cmd{cmd=GET key=eyeball-key argc=1}: rocket_mem::dispatcher: command dispatched elapsed_us=8 reply=ok
DEBUG cmd{cmd=LPUSH key=eyeball-key argc=2}: rocket_mem::dispatcher: command dispatched elapsed_us=11 reply=error
```

The two things to actually check: `key=eyeball-key` is **text**, not a byte list — if it renders as `key=[101, 121, 101, ...]` or `key=b"eyeball-key"`, Step 4 of Task 1 was done with `?` instead of `%key_field(...)`. And the same run at `RUST_LOG=info` must print none of these.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(logging): emit a per-command debug line with elapsed_us and reply kind"
```

---

### Task 3: Benchmark gate

**Files:**
- Modify: `docs/benchmarks/2026-09-09-pre-logging-baseline.md` (append a results section; do **not** edit the baseline table)

**Interfaces:**
- Consumes: the baseline `SET`/`GET` requests-per-second means recorded by [plan 01](01-baseline-and-dependencies.md), Task 1.
- Produces: a recorded measurement for this plan. Plan 08's own gate compares against the same baseline, not against this number.

This task has no test to write first — it is a measurement, and it is the acceptance criterion for the whole plan. **A >2% regression at the default `info` level blocks this plan.** Do not proceed to plan 08 with a failing gate: report it instead, because the span and the `debug!` line are the two changes in the entire series with real hot-path exposure, and a regression here is a design problem, not a tuning problem.

- [ ] **Step 1: Re-run the benchmark three times at the default level**

Three runs, matching plan 01's methodology exactly — same script, same machine, no `RUST_LOG` set, so the `cmd` span and the `debug!` line are both filtered out at their callsites:

```bash
cd /home/numericlabs/data/rocket/rocket-mem
cargo build --release
for i in 1 2 3; do
  echo "=== run $i ==="
  ./scripts/benchmark.sh
done 2>&1 | tee /tmp/rocket-mem-plan07.txt
```

Use `./scripts/benchmark.sh`, never a hand-rolled `redis-benchmark` invocation: rocket-mem cannot disable its AOF, and an unmatched-durability comparison makes the numbers meaningless against the baseline.

- [ ] **Step 2: Measure the cost at `debug` and `trace` too**

Not gated — the spec expects a large regression at these levels and says so — but measured and recorded, because "how expensive is the firehose" is the question an operator will ask before turning it on:

```bash
RUST_LOG=debug ./scripts/benchmark.sh 2>&1 | tee /tmp/rocket-mem-plan07-debug.txt
```

(`trace` adds nothing yet — plan 08 is what puts a `trace` line on this path — so one `debug` run is the whole picture at this point in the series.)

- [ ] **Step 3: Append the results to the baseline document**

Append to `docs/benchmarks/2026-09-09-pre-logging-baseline.md`, under a new heading. Fill from the actual output of Steps 1 and 2:

```markdown
## Plan 07 — `cmd` span + per-command `debug!` line

| Workload | Run 1 | Run 2 | Run 3 | Mean | vs baseline |
|---|---|---|---|---|---|
| SET | | | | | |
| GET | | | | | |

At `RUST_LOG=debug`: SET , GET  (not gated -- one log line per command is the
feature being asked for, and its cost is the operator's decision to accept).

Gate: PASS / FAIL (<=2% at default `info`).
```

- [ ] **Step 4: Commit**

```bash
git add docs/benchmarks/2026-09-09-pre-logging-baseline.md
git commit -m "docs(bench): record cmd-span throughput against the pre-logging baseline"
```

---

## Next plan

[`08-command-trace-args.md`](08-command-trace-args.md) — adds the `trace`-level full-argument line to the same function, and threads `log_value_max_bytes` down to it so the argument renderer has its cap.
