# Structured Logging Plan 2: Convert `eprintln!` in connection/RMP/metrics

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert the 3 single-`eprintln!` files (`connection.rs`, `rmp_connection.rs`, `metrics.rs`) to structured `tracing` calls.

**Architecture:** Each is a pure output-mechanism swap — same trigger condition, same control flow, only `eprintln!("... {e}")` becomes `tracing::warn!`/`error!(error = %e, "...")`. There is no new behavior to drive with a failing test first (that's the TDD pattern for new logic, not a formatting swap), so each task's steps are: make the edit, confirm the crate's existing tests still pass unchanged, lint, commit.

**Tech Stack:** `tracing` (wired up in Plan 1).

**Spec:** `docs/superpowers/specs/2026-09-07-structured-logging-design.md`

## Global Constraints

- Scope is `crates/server` only.
- Every conversion in this plan is 1:1 with the spec's Part A table — same level (`error!`/`warn!`) and same fields the spec specifies, don't improvise different levels.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` must all stay clean after every task.
- Requires Plan 1 (`01-deps-config-field.md`) already merged — `tracing` must already be a dependency of `crates/server`.

**Next plan:** `docs/superpowers/plans/2026-09-07-structured-logging-plans/03-convert-dispatcher-aof-replication.md`

---

### Task 1: `connection.rs` — AOF fsync failure

**Files:**
- Modify: `crates/server/src/connection.rs:71`

**Interfaces:** none (leaf conversion, no interaction with other tasks).

- [ ] **Step 1: Make the edit**

In `crates/server/src/connection.rs`, inside `periodic_fsync_loop` (around line 71), change:

```rust
        if let Err(e) = aof.fsync() {
            eprintln!("aof fsync failed: {e}");
        }
```

to:

```rust
        if let Err(e) = aof.fsync() {
            tracing::error!(error = %e, "aof fsync failed");
        }
```

- [ ] **Step 2: Confirm existing tests still pass**

Run: `cargo test -p rocket-mem`
Expected: same pass/fail outcome as before this change (this file's tests don't assert on stderr output, so nothing should change).

- [ ] **Step 3: Lint and format**

Run: `cargo clippy -p rocket-mem --all-targets -- -D warnings && cargo fmt --all -- --check`

- [ ] **Step 4: Commit**

```bash
git add crates/server/src/connection.rs
git commit -m "$(cat <<'EOF'
Convert AOF fsync failure log to tracing::error!

Part of the eprintln!-to-tracing conversion (see
docs/superpowers/specs/2026-09-07-structured-logging-design.md).
EOF
)"
```

---

### Task 2: `rmp_connection.rs` — RMP decode error

**Files:**
- Modify: `crates/server/src/rmp_connection.rs:128`

**Interfaces:** none for this task. Note for later: Plan 5 will extend this same call site to add a `peer` field once the connection's peer address is threaded through — that's expected, not a conflict.

- [ ] **Step 1: Make the edit**

In `crates/server/src/rmp_connection.rs`, inside `handle_connection`'s read loop (around line 128), change:

```rust
            Err(e) => {
                eprintln!("rmp decode error: {e}");
                break;
            }
```

to:

```rust
            Err(e) => {
                tracing::warn!(error = %e, "rmp decode error");
                break;
            }
```

- [ ] **Step 2: Confirm existing tests still pass**

Run: `cargo test -p rocket-mem`

- [ ] **Step 3: Lint and format**

Run: `cargo clippy -p rocket-mem --all-targets -- -D warnings && cargo fmt --all -- --check`

- [ ] **Step 4: Commit**

```bash
git add crates/server/src/rmp_connection.rs
git commit -m "$(cat <<'EOF'
Convert RMP decode error log to tracing::warn!

Part of the eprintln!-to-tracing conversion (see
docs/superpowers/specs/2026-09-07-structured-logging-design.md).
EOF
)"
```

---

### Task 3: `metrics.rs` — recorder-already-installed

**Files:**
- Modify: `crates/server/src/metrics.rs:33-35`

**Interfaces:** none.

- [ ] **Step 1: Make the edit**

In `crates/server/src/metrics.rs` (around line 33), change:

```rust
            if ::metrics::set_global_recorder(recorder).is_err() {
                eprintln!(
                    "metrics: a global recorder was already installed; metrics may be incomplete"
                );
            }
```

to:

```rust
            if ::metrics::set_global_recorder(recorder).is_err() {
                tracing::warn!("global metrics recorder already installed; metrics may be incomplete");
            }
```

Note: this is `warn!`, not `error!` — the existing code comment right above this block ("That is not fatal... the alternative -- panicking -- would take down a server over an observability detail") already establishes this is a degraded-but-fine condition, matching `warn!`'s severity, not `error!`'s.

- [ ] **Step 2: Confirm existing tests still pass**

Run: `cargo test -p rocket-mem`

- [ ] **Step 3: Lint and format**

Run: `cargo clippy -p rocket-mem --all-targets -- -D warnings && cargo fmt --all -- --check`

- [ ] **Step 4: Commit**

```bash
git add crates/server/src/metrics.rs
git commit -m "$(cat <<'EOF'
Convert metrics recorder-install log to tracing::warn!

Part of the eprintln!-to-tracing conversion (see
docs/superpowers/specs/2026-09-07-structured-logging-design.md).
EOF
)"
```

**On completion of this plan:** proceed automatically to `docs/superpowers/plans/2026-09-07-structured-logging-plans/03-convert-dispatcher-aof-replication.md` without waiting for further confirmation.
