# Verbose Logging Plan 19: Slowlog and Metrics Events

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Warn when a command actually crosses the slow-log threshold (`cmd`/`key`/`elapsed_us`), and trace every served `/metrics` scrape — the two events the spec's Event catalogue names for `server/slowlog.rs` and `server/metrics.rs`.

**Architecture:** Two independent, unrelated insertion points, so this plan has two tasks. The slowlog event is the more delicate of the two: `SlowLog::maybe_record` is called from `dispatch_and_log` on **every** dispatched command (line 3034–3036), and returns early on the overwhelmingly common case — a command under threshold. The log call must sit strictly after that early return, inside the branch that actually records an entry, never before it; getting this backwards would put a `warn!` on every single command the server handles. This plan proves that placement with a dedicated negative test. The metrics event is the simpler of the two: a single `trace!` inside the one branch of `serve_one_scrape` that actually renders the registry.

**Tech Stack:** Rust 2021, `tracing 0.1`, `tracing-subscriber 0.3` (test-only capture, already a `crates/server` dependency).

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the Event catalogue's "Slowlog" and "Metrics" rows.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting. The load-bearing one for Task 1 specifically: `maybe_record` runs on every command dispatched through `dispatch_and_log`, so the new `warn!` must be provably confined to the already-slow branch — this plan's negative test exists precisely to catch the mistake the task description warns about.

**Note on test infrastructure:** the shared `CapturedLogs` log-capture helper already exists from plan 17's first task, defined once in `crates/server/src/logging.rs` under `#[cfg(test)] pub(crate) mod test_support` (a `tracing_subscriber::fmt` writer over a shared buffer, using only the already-present `tracing-subscriber` dependency). Every task below imports it with `use crate::logging::test_support::CapturedLogs;` rather than redefining it. It is distinct from plan 09's `capture_at` helper in `crates/server/tests/logging.rs`: files under `tests/` compile as separate crates, so a helper there cannot be shared with `src/` unit tests.

---

### Task 1: Warn when a slow command is actually recorded

**Files:**
- Modify: `crates/server/src/slowlog.rs` — `SlowLog::maybe_record` (lines 67–99)
- Modify: `crates/server/src/slowlog.rs` — `#[cfg(test)] mod tests` (starts line 145)

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing consumed by a later plan — a leaf log line, confined to the recording branch.

- [ ] **Step 1: Write the failing test**

Add to the existing `mod tests` in `crates/server/src/slowlog.rs` (after the last existing test, before the closing `}`):

```rust
    use crate::logging::test_support::CapturedLogs;

    #[test]
    fn maybe_record_only_warns_when_the_command_actually_gets_recorded() {
        let log = SlowLog::with_threshold(Duration::from_millis(10));

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        // Under the threshold: must not log anything at all. This is the case that runs on
        // essentially every command the server handles, so getting this backwards would put a
        // warn! on the hottest path in the project.
        log.maybe_record("GET", key(b"fast"), 1, Duration::from_micros(50));
        assert!(
            captured.text().is_empty(),
            "a command under the threshold must not log a warning, got:\n{}",
            captured.text()
        );

        // At/over the threshold: must log, with the command, key, and elapsed microseconds.
        log.maybe_record("LRANGE", key(b"mylist"), 3, Duration::from_millis(25));
        let text = captured.text();
        assert!(text.contains("LRANGE"), "expected the command name in the slowlog warning:\n{text}");
        assert!(text.contains("mylist"), "expected the key in the slowlog warning:\n{text}");
        assert!(text.contains("25000"), "expected elapsed_us (25000) in the slowlog warning:\n{text}");
    }
```

This test module already imports `Bytes`, `Duration`, and defines the `key()` helper (see the existing `use super::*; use std::time::Duration;` and `fn key(s: &'static [u8]) -> Option<Bytes>` at the top of `mod tests`).

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem slowlog::tests::maybe_record_only_warns_when_the_command_actually_gets_recorded
```

Expected: FAIL on the second half — after the at-threshold call, `captured.text()` is still empty (no `warn!` exists yet), so all three `assert!`s on `text` fail. The first assertion (empty text after the under-threshold call) passes vacuously already, which is fine — it is the guard, not the driver, matching the reasoning `fmt_value_renders_an_empty_value_as_an_empty_string` used against a stub in plan 02.

- [ ] **Step 3: Implement the log line, confined to the recording branch**

In `crates/server/src/slowlog.rs`, replace `maybe_record` (lines 67–99):

```rust
    /// Records `command` if it took at least the configured threshold. A no-op otherwise, which
    /// is the overwhelmingly common case -- this is the only slow-log work on the hot path.
    /// `Duration::ZERO` means disabled, not "record everything"; see this plan's Global
    /// Constraints.
    pub fn maybe_record(
        &self,
        command: &str,
        key: Option<Bytes>,
        arg_count: usize,
        elapsed: Duration,
    ) {
        if self.threshold.is_zero() || elapsed < self.threshold {
            return;
        }
        let unix_time_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let duration_micros = elapsed.as_micros().min(i64::MAX as u128) as i64;

        // Only reached past the early return above, i.e. only when a command actually crosses
        // the operator-set threshold -- never on the common under-threshold path every other
        // command takes. `key` uses `%` over a lossy UTF-8 render rather than `?` (Debug) on the
        // raw bytes, per the hot-path guardrail against byte-by-byte Bytes rendering.
        match &key {
            Some(k) => tracing::warn!(
                cmd = %command,
                key = %String::from_utf8_lossy(k),
                elapsed_us = duration_micros,
                "slow command recorded"
            ),
            None => tracing::warn!(
                cmd = %command,
                elapsed_us = duration_micros,
                "slow command recorded"
            ),
        }

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let entry = SlowLogEntry {
            id: state.next_id,
            unix_time_secs,
            duration_micros,
            command: command.to_string(),
            key,
            arg_count,
        };
        state.next_id += 1;
        if state.entries.len() == SLOWLOG_CAPACITY {
            state.entries.pop_front();
        }
        state.entries.push_back(entry);
        drop(state);
        ::metrics::counter!("rocket_mem_slowlog_entries_total").increment(1);
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem slowlog::tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: the new test PASSes alongside every pre-existing `slowlog.rs` test (`a_command_under_the_threshold_is_not_recorded`, `a_zero_threshold_disables_recording_entirely`, `concurrent_maybe_record_calls_keep_id_and_insertion_order_in_agreement`, etc. — none assert on log output, and none change behavior), fmt clean, clippy clean, full workspace suite green.

- [ ] **Step 5: Manual check**

Per `.claude/manual-testing.md`'s "Standalone mode" section, start a server with `RUST_LOG=warn` (the default `slowlog-threshold-micros` is 10000, i.e. 10ms). Run `redis-cli -p 6399 auth admin adminpw` against an ACL-configured instance (per the ACL section: "`AUTH` costs ~20ms", reliably crossing the default threshold) and confirm a `slow command recorded` warning appears naming `cmd="AUTH"` and `elapsed_us` near 20000, with no key field leaking the password (matches the existing `redact_args` guarantee, since `AUTH`'s key extraction already never surfaces its password — see `command_key_and_arity`'s doc comment). Then run a fast `PING` and confirm nothing is logged.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/slowlog.rs
git commit -m "feat(logging): warn when a command crosses the slowlog threshold"
```

---

### Task 2: Trace a served metrics scrape

**Files:**
- Modify: `crates/server/src/metrics.rs` — `serve_one_scrape` (lines 101–132, specifically the `/metrics` branch at lines 118–127)
- Modify: `crates/server/src/metrics.rs` — `#[cfg(test)] mod tests` (starts line 134)

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing consumed by a later plan — a leaf log line, confined to the branch that actually renders the registry.

- [ ] **Step 1: Write the failing test**

Add to the existing `mod tests` in `crates/server/src/metrics.rs` (after the last existing test, before the closing `}`):

```rust
    use crate::logging::test_support::CapturedLogs;

    #[tokio::test]
    async fn a_metrics_scrape_is_traced_and_a_404_is_not() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let handle = recorder_handle();
        let engine = std::sync::Arc::new(engine::Engine::new());
        let replication = std::sync::Arc::new(crate::replication::ReplicationHandle::default());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve_metrics(
            listener,
            handle,
            std::sync::Arc::clone(&engine),
            std::sync::Arc::clone(&replication),
        ));

        async fn get(addr: std::net::SocketAddr, path: &str) -> String {
            let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
            socket
                .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).await.unwrap();
            response
        }

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let missing = get(addr, "/nope").await;
        assert!(missing.starts_with("HTTP/1.1 404 Not Found\r\n"), "{missing}");
        assert!(
            captured.text().is_empty(),
            "a 404 must not trigger the scrape-served trace, got:\n{}",
            captured.text()
        );

        let body = get(addr, "/metrics").await;
        assert!(body.starts_with("HTTP/1.1 200 OK\r\n"), "{body}");

        drop(_guard);
        let text = captured.text();
        assert!(
            text.contains("metrics scrape served") && text.contains("bytes"),
            "expected a trace-level scrape event carrying a byte count:\n{text}"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem metrics::tests::a_metrics_scrape_is_traced_and_a_404_is_not
```

Expected: FAIL — `serve_one_scrape` emits no log line yet, so the final assertion (`text.contains("metrics scrape served")`) fails against an empty buffer. The 404-produces-no-log assertion passes vacuously already, which is expected and not the one driving this task.

- [ ] **Step 3: Implement the log line, confined to the render branch**

In `crates/server/src/metrics.rs`, inside `serve_one_scrape` (lines 118–127):

```rust
    let response = if path == "/metrics" || path.starts_with("/metrics?") {
        refresh_sampled_gauges(&engine, &replication);
        let body = handle.render();
        tracing::trace!(bytes = body.len(), "metrics scrape served");
        format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/plain; version=0.0.4; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
    } else {
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
    };
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem metrics::tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: the new test PASSes alongside the pre-existing `recorder_handle_is_idempotent_and_renders_what_was_recorded` and `the_metrics_endpoint_serves_the_rendered_registry_and_404s_everything_else` tests (neither asserts on log output, and neither's behavior changed), fmt clean, clippy clean, full workspace suite green — including `crates/server/tests/metrics.rs`'s own integration test, which scrapes `/metrics` and is unaffected by an added `trace!` call.

- [ ] **Step 5: Manual check**

Per `.claude/manual-testing.md`'s "Standalone mode" section, start a server with `RUST_LOG=trace` and its metrics port reachable, then run `curl -s http://127.0.0.1:9199/metrics | tail -1` a few times. Confirm each scrape produces one `metrics scrape served` trace line with a `bytes` field, and that hitting an unrelated path (`curl -s http://127.0.0.1:9199/nope`) produces no such line.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/metrics.rs
git commit -m "feat(logging): trace a served metrics scrape"
```

---

## Next plan

[`20-startup-and-listener-events.md`](20-startup-and-listener-events.md) — logs the resolved config summary, per-protocol listener bind, and shutdown from `server/main.rs`.
