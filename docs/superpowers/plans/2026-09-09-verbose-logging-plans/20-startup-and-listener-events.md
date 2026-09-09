# Verbose Logging Plan 20: Startup and Listener Events

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the Startup row's `info`-level events from the spec's event catalogue —
resolved config summary and per-protocol listener-bound events — as the machine-readable
counterpart to `main.rs`'s existing boxed startup banner, and settle the catalogue's
`shutdown (info)` event honestly against a binary that has no shutdown code path to log from.

**Architecture:** All three tasks touch only `crates/server/src/main.rs`, which already
computes every value these events need (`config`, and the `(&str, String)` pairs already
pushed into `listeners`) — no new state, no new module. **The boxed startup banner
(`print_banner`, `paint`, `banner_label`, `visible_width`) stays exactly as it is: plain
`println!` to stdout, never routed through `tracing`.** This was settled by the prior
[2026-09-07 structured logging spec](../../specs/2026-09-07-structured-logging-design.md)'s
"Decision: the startup banner stays separate" and remains in force here — the events this
plan adds are additive `tracing::info!` lines to stderr alongside the banner, not a
replacement for any part of it. **An implementer who finds themselves converting a
`println!`/`paint`/`print_banner` call into a `tracing` call in this plan has misread the
brief — stop and re-read this paragraph.**

Task 3 is a deliberate scope boundary, not an implementation. `crates/server/src` has no
`tokio::signal` handler anywhere in the workspace (confirmed by grep — see Task 3), and
`rocket_mem::serve` (`connection.rs`'s `serve`, called at `main.rs:378`) is an unconditional
`loop { ... }` that never returns under normal operation; the process only ever ends via an
external `SIGKILL`/`SIGTERM`, which terminates it before any Rust code — including a
`tracing::info!` call — would run. There is therefore no reachable code path for a
`shutdown (info)` event to log from today. Building one means adding real signal handling,
which is a graceful-shutdown feature in its own right, not a logging change, and is well
beyond this plan's (and this series') scope. Task 3 documents this rather than inventing a
signal-handling subsystem to justify one log line.

**Tech Stack:** Rust 2021, `tracing 0.1` (already a `server` dependency; this plan adds call
sites, not the dependency), `tokio::net::TcpListener`, the existing
`std::process::Command`-based binary-spawning integration test pattern already established in
`crates/server/tests/kill_and_recover.rs`.

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md)

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting.

---

### Task 1: Resolved config summary event

**Files:**
- Modify: `crates/server/src/main.rs` — directly beneath the existing
  `tracing::info!(version = env!("CARGO_PKG_VERSION"), "rocket-mem starting");` at line 107
- Test: Create `crates/server/tests/startup_logging.rs`

**Interfaces:**
- Consumes: `config: rocket_mem::config::Config`, already loaded at `main.rs:84` and in scope
  at line 107 — every field this event logs is read directly off it, so this event needs no
  data that doesn't already exist at that point in `main`.
- Produces: one `info`-level `resolved config summary` tracing event on stderr. Nothing
  downstream in-process consumes it — it exists for an operator or log aggregator reading
  the stream.

- [ ] **Step 1: Write the failing integration test**

Create `crates/server/tests/startup_logging.rs`:

```rust
// crates/server/tests/startup_logging.rs
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

/// Spawns the real compiled binary with its ports bound to `127.0.0.1:0` (OS-assigned) and
/// captures up to `max_lines` of its stderr -- where the `tracing` subscriber writes, per
/// `main.rs`'s `.with_writer(std::io::stderr)` -- into one buffer. Mirrors
/// `kill_and_recover.rs`'s `spawn_server`, but reads the log stream instead of the plain
/// `println!` startup banner on stdout.
fn spawn_and_capture_stderr(
    aof_path: &std::path::Path,
    extra_env: &[(&str, &str)],
    max_lines: usize,
) -> (Child, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rocket-mem"));
    cmd.env("ROCKET_MEM_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_AOF_PATH", aof_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("failed to spawn the rocket-mem binary");

    let stderr = child.stderr.take().expect("child stderr was not piped");
    let mut reader = BufReader::new(stderr);
    let mut captured = String::new();
    for _ in 0..max_lines {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => captured.push_str(&line),
            Err(_) => break,
        }
    }
    (child, captured)
}

#[test]
fn resolved_config_summary_is_logged_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("startup-log-test.aof");

    let (mut child, stderr) = spawn_and_capture_stderr(&aof_path, &[], 5);
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        stderr.contains("resolved config summary"),
        "expected a 'resolved config summary' info line, got:\n{stderr}"
    );
    assert!(stderr.contains("cluster_mode=false"), "got:\n{stderr}");
    assert!(stderr.contains("acl_enabled=false"), "got:\n{stderr}");
    assert!(stderr.contains("tls_enabled=false"), "got:\n{stderr}");
    assert!(
        !stderr.to_lowercase().contains("password"),
        "config summary must never log credential material, got:\n{stderr}"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem --test startup_logging
```

Expected: FAIL. The test compiles and the binary spawns fine, but the first assertion
panics — today's stderr contains only the pre-existing `rocket-mem starting` line, not the
new event:

```
thread 'resolved_config_summary_is_logged_at_startup' panicked at crates/server/tests/startup_logging.rs:NN:5:
expected a 'resolved config summary' info line, got:
2026-09-09T00:00:00.000000Z  INFO rocket_mem: rocket-mem starting version="0.x.x"
```

- [ ] **Step 3: Add the event**

In `crates/server/src/main.rs`, directly beneath the existing `tracing::info!(version = ...)`
call at line 107:

```rust
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "rocket-mem starting");

    // The machine-readable counterpart to the boxed startup banner printed further down this
    // function, not a replacement for it -- see the verbose logging spec's "Decision: the
    // startup banner stays separate". No secrets: `config.acl.users` carries passwords, so
    // only its *presence* is surfaced here as `acl_enabled`, never the users themselves.
    tracing::info!(
        addr = %config.addr,
        rmp_addr = %config.rmp_addr,
        metrics_addr = %config.metrics_addr,
        aof_path = %config.aof_path,
        snapshot_path = %config.snapshot_path,
        log_level = %config.log_level,
        cluster_mode = config.cluster_config.is_some(),
        acl_enabled = !config.acl.users.is_empty(),
        tls_enabled = config.tls_cert_path.is_some() && config.tls_key_path.is_some(),
        "resolved config summary"
    );

    let metrics_handle = rocket_mem::metrics::recorder_handle();
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p rocket-mem --test startup_logging
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: the new test passes; fmt and clippy are clean; every pre-existing test still
passes unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/main.rs crates/server/tests/startup_logging.rs
git commit -m "feat(logging): log a resolved config summary at startup"
```

---

### Task 2: Listener-bound events, per protocol

**Files:**
- Modify: `crates/server/src/main.rs` — the five `listeners.push((...))` call sites: metrics
  (~line 259-263), RMP (~line 271-272), RESP+TLS (~line 291-292), RMP+TLS (~line 311-312),
  RESP (~line 322-323). Line numbers shift by the lines Task 1 inserted; match by the code
  shown below, not by number.
- Modify: `crates/server/tests/startup_logging.rs` (created in Task 1)

**Interfaces:**
- Consumes: the `TcpListener`s already bound at each site and the `&str` protocol labels
  already used as the first element of each pushed tuple (`"metrics"`, `"RMP"`,
  `"RESP+TLS"`, `"RMP+TLS"`, `"RESP"`) — this task reuses those labels verbatim as the
  `protocol` field so the log and the banner never disagree about a listener's name.
- Produces: one `info`-level `listener bound` event per listener, each carrying `protocol`
  and `addr`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/server/tests/startup_logging.rs`:

```rust
#[test]
fn listener_bound_is_logged_for_the_always_on_listeners() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("listener-log-test.aof");

    let (mut child, stderr) = spawn_and_capture_stderr(&aof_path, &[], 10);
    let _ = child.kill();
    let _ = child.wait();

    for protocol in ["metrics", "RMP", "RESP"] {
        assert!(
            stderr.contains(&format!("protocol={protocol}")),
            "expected a 'listener bound' line for protocol={protocol}, got:\n{stderr}"
        );
    }
    assert_eq!(
        stderr.matches("listener bound").count(),
        3,
        "expected exactly 3 listener-bound lines with no TLS configured, got:\n{stderr}"
    );
}

#[test]
fn listener_bound_is_logged_for_tls_listeners_when_configured() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("listener-tls-log-test.aof");
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let cert = fixtures.join("test-cert.pem");
    let key = fixtures.join("test-key.pem");

    let (mut child, stderr) = spawn_and_capture_stderr(
        &aof_path,
        &[
            ("ROCKET_MEM_TLS_RESP_ADDR", "127.0.0.1:0"),
            ("ROCKET_MEM_TLS_RMP_ADDR", "127.0.0.1:0"),
            ("ROCKET_MEM_TLS_CERT_PATH", cert.to_str().unwrap()),
            ("ROCKET_MEM_TLS_KEY_PATH", key.to_str().unwrap()),
        ],
        10,
    );
    let _ = child.kill();
    let _ = child.wait();

    for protocol in ["metrics", "RMP", "RESP+TLS", "RMP+TLS", "RESP"] {
        assert!(
            stderr.contains(&format!("protocol={protocol}")),
            "expected a 'listener bound' line for protocol={protocol}, got:\n{stderr}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --test startup_logging
```

Expected: FAIL. `resolved_config_summary_is_logged_at_startup` still passes (Task 1 is
already implemented), but both new tests panic — no `listener bound` text exists in stderr
yet:

```
thread 'listener_bound_is_logged_for_the_always_on_listeners' panicked at crates/server/tests/startup_logging.rs:NN:9:
expected a 'listener bound' line for protocol=metrics, got:
...
resolved config summary addr=127.0.0.1:0 ...
```

- [ ] **Step 3: Add the events**

In `crates/server/src/main.rs`, at each of the five listener sites. Metrics:

```rust
    let metrics_listener = tokio::net::TcpListener::bind(&config.metrics_addr).await?;
    let metrics_addr_str = format!("http://{}/metrics", metrics_listener.local_addr()?);
    tracing::info!(protocol = "metrics", addr = %metrics_addr_str, "listener bound");
    listeners.push(("metrics", metrics_addr_str));
```

RMP:

```rust
    let rmp_listener = tokio::net::TcpListener::bind(&config.rmp_addr).await?;
    let rmp_addr_str = rmp_listener.local_addr()?.to_string();
    tracing::info!(protocol = "RMP", addr = %rmp_addr_str, "listener bound");
    listeners.push(("RMP", rmp_addr_str));
```

RESP+TLS (inside the `if let (Some(tls_addr), ...)` block):

```rust
        let tls_listener = tokio::net::TcpListener::bind(tls_addr).await?;
        let tls_addr_str = tls_listener.local_addr()?.to_string();
        tracing::info!(protocol = "RESP+TLS", addr = %tls_addr_str, "listener bound");
        listeners.push(("RESP+TLS", tls_addr_str));
```

RMP+TLS (inside its own `if let (Some(tls_rmp_addr), ...)` block):

```rust
        let tls_rmp_listener = tokio::net::TcpListener::bind(tls_rmp_addr).await?;
        let tls_rmp_addr_str = tls_rmp_listener.local_addr()?.to_string();
        tracing::info!(protocol = "RMP+TLS", addr = %tls_rmp_addr_str, "listener bound");
        listeners.push(("RMP+TLS", tls_rmp_addr_str));
```

RESP (plaintext, the last listener bound before the banner prints):

```rust
    let listener = tokio::net::TcpListener::bind(&config.addr).await?;
    let resp_addr_str = listener.local_addr()?.to_string();
    tracing::info!(protocol = "RESP", addr = %resp_addr_str, "listener bound");
    listeners.push(("RESP", resp_addr_str));
```

Each site now computes the address string once and both logs and pushes it, instead of
calling `.local_addr()?` twice — a small, harmless side effect of this change, not a
behavior change (the string pushed into `listeners`, and therefore the banner's `listeners`
block, is byte-for-byte identical to before).

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --test startup_logging
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three `startup_logging` tests pass; fmt and clippy clean; full workspace
suite passes unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/main.rs crates/server/tests/startup_logging.rs
git commit -m "feat(logging): log a listener-bound event for each protocol at startup"
```

---

### Task 3: Document why `shutdown (info)` is deferred

**Files:**
- Modify: `crates/server/src/main.rs` — directly above the final
  `rocket_mem::serve(listener, engine, aof, replication).await;` line (line 378 before Tasks
  1-2 shift it)

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing consumed by other code. This documents a scope boundary so plan 21's
  spec-coverage audit (Task 3 of that plan) records this catalogue row as "deferred", not as
  a silently-dropped gap, and so no future contributor "helpfully" bolts on a shutdown
  subsystem inside a logging-scoped plan to make one log line possible.

- [ ] **Step 1: Confirm there is genuinely no signal handling to hook**

```bash
grep -rn "ctrl_c\|SIGTERM\|SIGINT\|signal::" crates/server/src/
```

Expected: no output. There is no `tokio::signal` usage anywhere in the crate, confirming
`main`'s final call has no shutdown path to log from — the process only ever ends via an
external kill signal that terminates it before any of this binary's own code, `tracing`
included, runs.

- [ ] **Step 2: Confirm `serve` never returns under normal operation**

```bash
grep -n "pub async fn serve" -A 20 crates/server/src/connection.rs | head -25
```

Expected: the `loop { ... }` body shown earlier in this plan's Architecture section — an
unconditional loop with no `break`, confirming it does not return control to `main` for any
reachable shutdown log line to run after it.

- [ ] **Step 3: Add the deferral comment**

In `crates/server/src/main.rs`, directly above the final `rocket_mem::serve(...)` call:

```rust
    print_banner(&title, &body, color);

    // No `shutdown (info)` event: the spec's Startup catalogue row names one, but the two
    // greps above show there is nothing to log it from -- `rocket_mem::serve` is an
    // unconditional `loop` (connection.rs) and this binary installs no `tokio::signal`
    // handler anywhere, so a `kill -9`/SIGTERM ends the process before any Rust code,
    // including this line, would run. Logging a shutdown event requires adding real signal
    // handling first, which is a graceful-shutdown feature in its own right, not a logging
    // change -- deferred. See the spec-coverage audit in
    // docs/superpowers/plans/2026-09-09-verbose-logging-plans/21-docs-and-final-verification.md.
    rocket_mem::serve(listener, engine, aof, replication).await;
    Ok(())
}
```

- [ ] **Step 4: Verify nothing else changed**

```bash
grep -n "No .shutdown (info). event" crates/server/src/main.rs
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: one hit for the grep (the comment landed); fmt/clippy/test all clean and
unchanged — a comment-only edit changes no behavior, so every test that passed before still
passes now.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/main.rs
git commit -m "docs(logging): record that shutdown logging is deferred pending signal handling"
```

---

## Next plan

[`21-docs-and-final-verification.md`](21-docs-and-final-verification.md) — documents the
logging capability for operators, runs the full verification sweep (fmt/clippy/test/
benchmark/manual) against the whole series' cumulative cost, and audits the spec's Event
catalogue row by row. It is the last plan in the series.
