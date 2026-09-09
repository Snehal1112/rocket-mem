# Plan 23: Deferred cleanups and recorded decisions

Shared requirements: see [`## Global Constraints`](01-baseline-and-dependencies.md#global-constraints).
Spec: [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md).
Preceded by [plan 22](22-log-field-consistency-sweep.md); see its header for why these two
plans run before [plan 21](21-docs-and-final-verification.md).

---

### Task 1: Log the silent error paths in recovery

The series instrumented recovery's success paths (plans 15 and 16) but several `recover()`
paths still propagate an `Err` upward with **no log at all**. A startup that fails to recover
is exactly the moment an operator needs a log line, and right now some of those failures are
visible only as a process exit.

**Files:** `crates/server/src/` — find them rather than trusting a list here:
`rg -n 'fn recover' crates/server/src/` then trace each `?` and each `return Err(...)` in those
functions and their helpers.

**Requirements:**
- Emit at `error` for a failure that aborts startup, `warn` for one the server recovers from
  (e.g. a truncated trailing AOF record that is deliberately tolerated — check which of these
  the code already treats as recoverable before assigning a level; do not promote a tolerated
  condition to `error`).
- Include the failing path and the error, using the spec's existing field vocabulary
  (`error`, and the path field name already used by the recovery events from plans 15/16 —
  match them rather than inventing a new name).
- **Never log file *contents*.** A corrupt-record failure logs an offset and a length, not the
  bytes.
- Do not change control flow. This task adds observability to existing paths; if you believe a
  path's error handling is itself wrong, say so in your report rather than fixing it here.

**Steps:**
- [ ] Enumerate every silent `Err` path in recovery and list them in your report.
- [ ] Write failing tests for the ones that are reachable from a test (a corrupt/truncated
      AOF fixture, a missing/unreadable snapshot). Some paths may be unreachable without
      fault injection — say which, and do not contort the code to reach them.
- [ ] Implement, then confirm the pre-existing recovery tests still pass unchanged.

---

### Task 2: Two small deferred cleanups

**A — eviction call-site comment.** Commit `e10c88f` extracted an `evicted_entry_size` helper
in `crates/engine/src/engine.rs`'s `maybe_evict` rather than computing freed bytes as a
`memory_used()` delta, because a concurrent mutation on another shard makes that delta wrong.
That reasoning currently lives only in the commit message, so the next reader is liable to
"simplify" it back. Add a short comment at the call site recording why the delta form is wrong.
Read `e10c88f` for the actual argument; keep the comment to a few plain sentences per the
project's comment conventions.

**B — a missing log assertion.** `recover_with_a_snapshot_and_no_aof_keeps_the_snapshot_state`
exercises the snapshot-only recovery path whose summary event was added in plan 16
(`5337ce5`), but asserts nothing about the log. Add a `CapturedLogs` assertion so a regression
in that event is caught.

**Placement constraint — this has bitten the series twice.** `tracing` caches per-callsite
`Interest` process-globally; a callsite first reached in a binary with no subscriber installed
can be cached `never` for the whole process, silently breaking a later capture assertion
(fixed once in `4e646d2`). Capture assertions belong in `crates/server/tests/logging.rs`, not
in a `mod tests` inside `crates/server/src/`. If the target test lives in a source file, move
the *assertion* to `tests/logging.rs` and leave a short "moved, don't re-add" comment.

Note `tests/logging.rs` already holds ~20 capture tests and its scaling behaviour is under
watch — plan 21's Task 2 checks it. Keep the addition minimal.

---

### Task 3: Record this series' decisions in the spec

Several rulings were established empirically during execution and exist only in commit messages
and review reports. Anyone reading the spec afterwards would re-litigate them. Add them to
[`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md).

- [ ] **Span-field duplication rule.** "Field duplication is only real when the event's level is
      at or below the span's level. The `cmd` span is a `debug_span!`, so at the `info`
      production default it is never entered and a nested `warn!` inherits none of its fields;
      an event at info/warn/error inside a `debug_span!` must carry its own `cmd`/`key`."
      Cite plan 19's slowlog event as the worked example. This was verified empirically under
      both `with_max_level` and `EnvFilter`.
- [ ] **Redaction: what is structural and what is residue.** `acl::AclUser` (hash) and
      `config::AclUserConfig` (plaintext password, raw rule tokens) both have hand-written
      redacting `Debug` impls. The two differ deliberately: `AclUser::rules` is `Vec<AclRule>`,
      a parsed enum that structurally cannot hold a credential, so it renders verbatim;
      `AclUserConfig::rules` is `Vec<String>` of raw tokens where `>password` is a plaintext
      secret, so it renders as a count. **`Config`'s own `Debug` stays derived** — a
      hand-written impl over its field count has no compile-time check for a forgotten field,
      and a cert/key *path* is a filename, not key material. Residue to record honestly: a
      `?config` would still render the ACL **username** and the TLS **paths**. The primary
      guard is and remains the standing rule at the call site — enumerate fields explicitly,
      never `?config`.
- [ ] **Redaction tests must be mutation-checked.** An absence-assertion is only as strong as
      its fixture: asserting "no password appears" against a config containing no password
      proves nothing. This was a real defect, caught in plan 20's review.
- [ ] **Correct a spec claim plan 22 made untrue.** Around line 87 the spec says the `cmd` span's
      fields are "values that exist at that point regardless — the span adds correlation, not
      computation". `logged_key` breaks that: the key-spec selection is genuinely computed per
      command, and it is **eager**, not deferred behind the span's level check, because the
      slowlog `warn!` needs the same value after `frame` has been moved. Record what is actually
      true — the *rendering* (`key_field`, the O(len) UTF-8 pass) stays lazy and is proven so by
      probe; the *selection* does not. Both facts were established empirically in plan 22, not
      argued.
- [ ] **Record the `SLOWLOG GET` exposure as a known, deliberate non-fix.** Plan 22 closed the
      `ECHO`/`PING` value leak in the log fields but not in `SlowLogEntry.key`, which still stores
      the command's first argument — so `SLOWLOG GET` hands an uncapped client-supplied payload to
      any client permitted to run `SLOWLOG`. The same leak, on a surface reachable without
      filesystem access. `command_key_and_arity` already special-cases `AUTH` for exactly this
      reason ("would leak the password through `SLOWLOG GET`"), so the argument for extending it
      to every `KeySpec::None` command is the codebase's own. It was **not** fixed here because it
      changes a client-visible surface, which is outside this series' scope, and because
      rocket-mem's slowlog already stores key+arity rather than Redis's full args — so the right
      shape is a design question, not a cleanup. Write it up as an open decision, name Sprint 8's
      ACL work as the natural home, and be explicit that the constraint which protected it during
      plan 22 was a scope guard and not a defence of the behaviour.
- [ ] **Extend the "Field vocabulary" list** with the names added during execution that were
      never recorded: from plan 18, `node_id` / `first_slot` / `node_count`; from plan 20 and
      its fixes, `addr` / `rmp_addr` / `metrics_addr` / `aof_path` / `snapshot_path` /
      `log_filter` / `cluster_mode` / `acl_enabled` / `acl_user_count` / `tls_enabled` /
      `tls_replication_enabled` / `log_value_max_bytes` / `slowlog_threshold_micros`, plus
      `protocol` at event level. Verify each against the code rather than trusting this list —
      it was assembled from reports, and plan 22 Task 2 may have changed `protocol`'s rendering.

Documentation only; no code changes in this task.

---

## Next plan

[21-docs-and-final-verification.md](21-docs-and-final-verification.md) — the final plan:
operator documentation, the full verification sweep, and the spec-coverage audit. It runs last
so that its verification and audit see the finished state.
