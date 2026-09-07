# Structured Logging Plan 6: Final Verification

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Confirm the whole structured-logging feature (Plans 1–5) is correct end-to-end across the full workspace, not just crate-by-crate.

**Architecture:** No new code — this plan is pure verification: full-workspace lint/format/test, then one combined manual smoke test exercising every listener (RESP/RMP, plaintext/TLS) together against the same running instance, confirming the startup banner is unaffected and every new/converted log line appears as designed.

**Tech Stack:** `cargo fmt`, `cargo clippy`, `cargo test`, `redis-cli`, `nc`.

**Spec:** `docs/superpowers/specs/2026-09-07-structured-logging-design.md`

## Global Constraints

- Requires Plans 1–5 already merged.
- If any verification step below fails, fix the regression in the file/plan it belongs to (don't patch around it in this plan), then re-run this plan's steps from the top.

**Next plan:** none — this is the final plan in the structured-logging series. On completion, report the feature as done; no further plan follows automatically.

---

### Task 1: Full-workspace format and lint

**Files:** none (verification only).

- [ ] **Step 1: Format check**

Run: `cargo fmt --all -- --check`
Expected: no output, exit code 0.

- [ ] **Step 2: Clippy, every crate, every target, warnings as errors**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: `Finished` with no warnings, across every crate (`common`, `engine`, `protocol`, `server`, `rmp-client`) — this is CI's exact gate per `CLAUDE.md`.

- [ ] **Step 3: If either fails**

Fix inline (formatting: run `cargo fmt --all`; clippy: address the specific lint) and re-run both commands until clean. Do not add `#[allow(...)]` to suppress a lint this plan's own changes introduced — fix the underlying code instead.

---

### Task 2: Full test suite

**Files:** none (verification only).

- [ ] **Step 1: Run every workspace test**

Run: `cargo test --workspace`
Expected: every test passes, including all of `crates/server`'s existing integration tests (`crates/server/tests/*.rs`) — none of Plans 1–5 should have changed any test's expected behavior, only how errors/events are logged.

- [ ] **Step 2: If anything fails**

Identify which plan's task introduced the regression (most likely candidates: a test in `crates/server/tests/*.rs` or `connection.rs`/`rmp_connection.rs`'s own `#[cfg(test)]` module that called `handle_connection`/`serve`/`serve_tls` with the pre-Plan-4/5 argument list). Fix the call site in that file directly, re-run this step.

---

### Task 3: Combined manual smoke test

**Files:** none (verification only).

- [ ] **Step 1: Generate a throwaway TLS cert if needed**

```bash
mkdir -p /tmp/plan6-certs
openssl req -x509 -newkey rsa:2048 -keyout /tmp/plan6-certs/key.pem -out /tmp/plan6-certs/cert.pem \
  -days 1 -nodes -subj "/CN=localhost" -addext "subjectAltName=IP:127.0.0.1"
```

- [ ] **Step 2: Start one instance with every listener enabled, at debug level**

```bash
cargo build --release -p rocket-mem
RUST_LOG=debug ./target/release/rocket-mem \
  --addr 127.0.0.1:17301 --rmp-addr 127.0.0.1:17302 --metrics-addr 127.0.0.1:17303 \
  --tls-resp-addr 127.0.0.1:17304 --tls-rmp-addr 127.0.0.1:17305 \
  --tls-cert-path /tmp/plan6-certs/cert.pem --tls-key-path /tmp/plan6-certs/key.pem \
  --aof-path /tmp/plan6-verify.aof --snapshot-path /tmp/plan6-verify.snapshot \
  > /tmp/plan6-output.log 2>&1 &
sleep 1
```

- [ ] **Step 3: Confirm the startup banner is unaffected**

Run: `head -20 /tmp/plan6-output.log`
Expected: the `rocket-mem v<version>` boxed banner (storage/acl/cluster/listeners) still appears, in its existing plain (non-`tracing`-prefixed) form, alongside the new `INFO ... rocket-mem starting version="..."` tracing line from Plan 1 Task 3 — both present, neither replacing the other.

- [ ] **Step 4: Exercise all four data-plane listeners**

```bash
redis-cli -h 127.0.0.1 -p 17301 PING
redis-cli --tls --insecure -h 127.0.0.1 -p 17304 PING
printf 'garbage\r\n' | timeout 2 nc 127.0.0.1 17301
printf 'garbage' | timeout 2 nc 127.0.0.1 17302
```

- [ ] **Step 5: Confirm the log output**

Run: `grep -E "connection accepted|decode error|tls handshake" /tmp/plan6-output.log`
Expected: at least one `connection accepted` line for the successful `PING`s (`protocol="resp" tls=false` and `protocol="resp" tls=true`), plus decode-error/rmp-decode-error warnings for the two `nc` probes — confirming Plans 4 and 5's logging fires correctly with every listener live simultaneously, not just in isolation.

- [ ] **Step 6: Tear down and clean up**

```bash
kill %1
rm -rf /tmp/plan6-certs /tmp/plan6-verify.aof /tmp/plan6-verify.snapshot /tmp/plan6-output.log
```

- [ ] **Step 7: No commit for this task**

This task is verification-only. If Task 1 or 2 required a fix, that fix should already be committed as part of re-running the relevant earlier plan's task — this plan itself produces no new commits when everything is already correct.

**On completion:** the structured-logging feature (spec: `docs/superpowers/specs/2026-09-07-structured-logging-design.md`) is complete. Report to the user that all 6 plans are done, summarizing what changed (dependencies added, config field, mechanical eprintln! conversion, new connection-lifecycle logging) and confirming the full verification gate (fmt/clippy/test/manual smoke test) passed.
