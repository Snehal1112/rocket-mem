# Structured Logging Plan 3: Convert `eprintln!` in dispatcher/AOF/replication

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert the remaining 8 `eprintln!` sites (`dispatcher.rs` ×2, `aof.rs` ×3, `replication.rs` ×3) to structured `tracing` calls, completing the Part A mechanical conversion from the spec.

**Architecture:** Same as Plan 2 — pure output-mechanism swaps, no control-flow changes. Grouped by file, one task per file.

**Tech Stack:** `tracing` (wired up in Plan 1).

**Spec:** `docs/superpowers/specs/2026-09-07-structured-logging-design.md`

## Global Constraints

- Scope is `crates/server` only.
- Every conversion is 1:1 with the spec's Part A table — same level, same fields.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` must all stay clean after every task.
- Requires Plan 1 already merged.

**Next plan:** `docs/superpowers/plans/2026-09-07-structured-logging-plans/04-resp-connection-lifecycle.md`

---

### Task 1: `dispatcher.rs` — AOF encode/append failures

**Files:**
- Modify: `crates/server/src/dispatcher.rs:2606`, `:2616`

**Interfaces:** none.

- [ ] **Step 1: Make the edits**

In `crates/server/src/dispatcher.rs`, around line 2606, change:

```rust
        let encoded = match crate::aof::encode_frame(&frame_to_log) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("aof encode failed: {e}");
                aof_failed = true;
                continue; // nothing to append or broadcast without a successful encode
            }
        };
```

to:

```rust
        let encoded = match crate::aof::encode_frame(&frame_to_log) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::error!(error = %e, "aof encode failed");
                aof_failed = true;
                continue; // nothing to append or broadcast without a successful encode
            }
        };
```

And around line 2616, change:

```rust
        if let Err(e) = aof.append_encoded(encoded.clone()) {
            eprintln!("aof append failed: {e}");
            aof_failed = true;
        }
```

to:

```rust
        if let Err(e) = aof.append_encoded(encoded.clone()) {
            tracing::error!(error = %e, "aof append failed");
            aof_failed = true;
        }
```

- [ ] **Step 2: Confirm existing tests still pass**

Run: `cargo test -p rocket-mem`

- [ ] **Step 3: Lint and format**

Run: `cargo clippy -p rocket-mem --all-targets -- -D warnings && cargo fmt --all -- --check`

- [ ] **Step 4: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "$(cat <<'EOF'
Convert AOF encode/append failure logs to tracing::error!

Part of the eprintln!-to-tracing conversion (see
docs/superpowers/specs/2026-09-07-structured-logging-design.md).
EOF
)"
```

---

### Task 2: `aof.rs` — writer-thread append, snapshot-offset, snapshot-unreadable

**Files:**
- Modify: `crates/server/src/aof.rs:110`, `:478-483`, `:491-494`

**Interfaces:** none.

- [ ] **Step 1: Convert the writer-thread append failure**

Around line 110, change:

```rust
                        AofMsg::Append(bytes) => {
                            if let Err(e) = writer.write_all(&bytes) {
                                eprintln!("aof append failed: {e}");
                            }
                        }
```

to:

```rust
                        AofMsg::Append(bytes) => {
                            if let Err(e) = writer.write_all(&bytes) {
                                tracing::error!(error = %e, "aof append failed");
                            }
                        }
```

This line runs on a plain OS thread (`thread::Builder::new().spawn(...)`, not a tokio task) — `tracing`'s macros work identically outside async/tokio contexts, no special handling needed.

- [ ] **Step 2: Convert the snapshot-offset-past-end-of-AOF warning**

Around line 478, change:

```rust
                    Some(len) if offset > len => {
                        eprintln!(
                            "snapshot at {} names an AOF offset ({offset}) past the AOF's \
                             actual length ({len}) -- discarding the snapshot and replaying \
                             the full AOF from byte 0 instead",
                            snapshot_path.display()
                        );
                        let fresh = engine::Engine::new();
                        replay(aof_path, &fresh, 0)?;
                        return Ok(fresh);
                    }
```

to:

```rust
                    Some(len) if offset > len => {
                        tracing::warn!(
                            snapshot_path = %snapshot_path.display(),
                            offset,
                            aof_len = len,
                            "snapshot offset past end of AOF; discarding snapshot and replaying full AOF"
                        );
                        let fresh = engine::Engine::new();
                        replay(aof_path, &fresh, 0)?;
                        return Ok(fresh);
                    }
```

- [ ] **Step 3: Convert the snapshot-unreadable warning**

Around line 491, change:

```rust
            Err(e) => {
                eprintln!(
                    "snapshot at {} is unreadable ({e}); falling back to full AOF replay",
                    snapshot_path.display()
                );
                0
            }
```

to:

```rust
            Err(e) => {
                tracing::warn!(
                    snapshot_path = %snapshot_path.display(),
                    error = %e,
                    "snapshot unreadable; falling back to full AOF replay"
                );
                0
            }
```

- [ ] **Step 4: Confirm existing tests still pass**

Run: `cargo test -p rocket-mem`
Expected: the AOF recovery tests (including the generation-cleanup tests mentioned in this repo's recent commit history) must still pass unchanged — this task only swaps the log call inside each branch, not the branch's logic or return value.

- [ ] **Step 5: Lint and format**

Run: `cargo clippy -p rocket-mem --all-targets -- -D warnings && cargo fmt --all -- --check`

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/aof.rs
git commit -m "$(cat <<'EOF'
Convert AOF writer/recovery logs to tracing

Part of the eprintln!-to-tracing conversion (see
docs/superpowers/specs/2026-09-07-structured-logging-design.md).
EOF
)"
```

---

### Task 3: `replication.rs` — connection closed/lost, apply failure

**Files:**
- Modify: `crates/server/src/replication.rs:433-434`, `:565`

**Interfaces:** none.

- [ ] **Step 1: Convert the connection closed/lost logs**

Around line 433, change:

```rust
            Ok(()) => eprintln!("replication: connection to {host_port} closed; reconnecting"),
            Err(e) => eprintln!("replication: lost connection to {host_port}: {e}; reconnecting"),
```

to:

```rust
            Ok(()) => tracing::warn!(%host_port, "replication connection closed, reconnecting"),
            Err(e) => tracing::warn!(%host_port, error = %e, "replication connection lost, reconnecting"),
```

- [ ] **Step 2: Convert the apply-failure log**

Around line 565, change:

```rust
        if let protocol::Frame::Error(e) = reply {
            eprintln!("replication: applying a replicated command failed: {e}");
        }
```

to:

```rust
        if let protocol::Frame::Error(e) = reply {
            tracing::error!(error = %e, "failed to apply replicated command");
        }
```

- [ ] **Step 3: Confirm existing tests still pass**

Run: `cargo test -p rocket-mem`
Expected: replication tests unaffected — same reconnect/apply logic, only the log call changed.

- [ ] **Step 4: Lint and format**

Run: `cargo clippy -p rocket-mem --all-targets -- -D warnings && cargo fmt --all -- --check`

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "$(cat <<'EOF'
Convert replication reconnect/apply logs to tracing

Completes the eprintln!-to-tracing conversion (see
docs/superpowers/specs/2026-09-07-structured-logging-design.md).
Every eprintln! in crates/server is now a structured tracing call.
EOF
)"
```

**On completion of this plan:** proceed automatically to `docs/superpowers/plans/2026-09-07-structured-logging-plans/04-resp-connection-lifecycle.md` without waiting for further confirmation.
