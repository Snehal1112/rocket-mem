# Plan 22: Log field consistency sweep

Shared requirements: see [`## Global Constraints`](01-baseline-and-dependencies.md#global-constraints).
Spec: [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md).

**Why this plan exists, and why it is numbered 22 but runs before 21.** Plans 01–20 each
surfaced a small defect or inconsistency that was deliberately deferred rather than fixed
in place, so that a review of plan N stayed a review of plan N. Those deferrals are now
due. They must land *before* [plan 21](21-docs-and-final-verification.md), because plan
21's Task 2 is the final verification sweep and Task 3 the spec-coverage audit — both of
which must run against the finished state, not a state with six known defects in it. The
file numbering is execution order 20 → 22 → 23 → 21; the chain links say so explicitly.

Every item here was found by a reviewer or implementer during execution, not invented now.

---

### Task 1: Make the `cmd` span's `key` field key-spec aware

**Two independent motivations. An implementer who fixes only one has not finished the task.**

*Motivation A — the field is wrong.* `command_key_and_arity` (`crates/server/src/dispatcher.rs`,
~line 2945) takes `items.get(1)` unconditionally, with no key-spec awareness. But `key_spec`
(~line 1432) maps `"MEMORY" | "OBJECT"` to `KeySpec::Second`. So the `cmd` span added in plan
07 emits `key=USAGE` for `MEMORY USAGE somekey` and `key=ENCODING` for `OBJECT ENCODING somekey`
— a log field that confidently reports a wrong value. An operator grepping by key never finds
those commands.

*Motivation B — the field leaks values.* `key_spec` also returns `KeySpec::None` for
`PING | ECHO | SELECT | COMMAND | INFO | HELLO | KEYS | SCAN | RANDOMKEY | CLUSTER | SAVE |
BGREWRITEAOF | REPLICAOF | PSYNC | SLOWLOG | DEBUG | AUTH | ACL | DBSIZE | CONFIG | CLIENT`,
yet `items.get(1)` renders an argument for all of them. For `ECHO <payload>` and
`PING <message>` that argument is a client-supplied **value**, and `key_field` applies **no
length cap** (`log_value_max_bytes` gates only the trace-level args line). The spec's invariant
is that logs carry key names and byte lengths, never value contents.

As of plan 19 this reaches `warn` level via the slowlog event, i.e. the production default,
not just a `debug` span an operator may never enable.

**Files:**
- Modify: `crates/server/src/dispatcher.rs` — the `debug_span!("cmd", ...)` key field (~3181)
  and the slowlog `warn!`'s key at the `SlowLog::maybe_record` call site (~3219).
- Modify: `crates/server/src/logging.rs`, `crates/server/src/slowlog.rs` — see Task 2 below
  for the `key_field` relocation; do that first if you prefer, the two interact.
- Test: `crates/server/tests/logging.rs`.

**Hard constraints:**
- **Do NOT change `command_key_and_arity`'s return value.** It also feeds the slowlog's
  *stored* entry, which `SLOWLOG GET` exposes and existing tests may pin. Fix the **rendering**
  used by the log fields, not the shared accessor. Verify this claim before you start:
  `rg 'command_key_and_arity' crates/server/src/` and read every call site.
- The `cmd` span's key sits in a `debug_span!` field position, so a more expensive key-spec-aware
  lookup costs nothing at the `info` default. **Confirm that before committing** — read plan 01's
  Global Constraints on hot-path field evaluation and satisfy yourself the new expression is not
  evaluated when `debug` is disabled. The slowlog `warn!`'s key is different: it is inside the
  already-taken slow branch, so cost there is irrelevant.
- `AUTH` must remain keyless. It already is, via an explicit exception in
  `command_key_and_arity`; do not regress it.

**Steps:**
- [ ] Write failing tests first: `MEMORY USAGE somekey` and `OBJECT ENCODING somekey` log
      `key=somekey`; `ECHO <payload>` and `PING <message>` log an **empty** key; `GET k` still
      logs `key=k`; `AUTH` still logs an empty key. Cover both the `cmd` span (at `debug`) and
      the slowlog `warn!` (force it by setting a zero-ish threshold, per that test's existing
      pattern).
- [ ] Implement, reusing `key_spec` rather than restating its table.
- [ ] Confirm the slowlog's *stored* entry behaviour is unchanged: run the pre-existing slowlog
      tests and say so explicitly in the commit message.

---

### Task 2: Move `key_field` into `logging.rs` and unify the `protocol` field's rendering

**Part A — `key_field` relocation.** Plan 19 widened `key_field` to `pub(crate)` so `slowlog.rs`
could reuse it, making it a second cross-module log renderer living in a 10k-line dispatcher.
The spec designates `crates/server/src/logging.rs` as *the single auditable place a secret could
reach a log*. Reuse over duplication was right; the destination was not. Move it, keeping its
doc comment (which explains why `Bytes` must never reach a log through `Debug`). This pairs
naturally with Task 1, since the key-spec-aware rendering lands in the same helper.

**Part B — `protocol` renders two different ways for the same field name.** Captured from a
real run:

```
INFO rocket_mem: listener bound protocol=metrics addr=http://127.0.0.1:39873/metrics
INFO handle_connection{conn_id=1 peer=127.0.0.1:60768 protocol="resp" tls=false}: connection accepted
```

`crates/server/src/main.rs` (~295, 306, 328, 350, 363) renders unquoted and uppercase;
`crates/server/src/connection.rs:222` and `crates/server/src/rmp_connection.rs:136` render
`Debug`-quoted and lowercase. Neither `grep protocol=RESP` nor `grep 'protocol="resp"'` finds
both. That directly defeats the spec's stated reason for a fixed field vocabulary: *"so a single
`grep` follows an activity end to end"* — which is the whole point of the correlation work in
plans 05–07.

**Pick one rendering and justify it in the commit message.** Weigh: `main.rs`'s sites are the
only `%`-on-string-literal sites in the workspace; the connection sites are on the hot path and
changing them touches more tests. Note that a bare `&str` field renders through `Debug` (quoted)
by default — that is why the existing line reads `version="0.1.4"` — so matching them means
choosing a sigil deliberately, not just editing string literals.

**Files:** `crates/server/src/{logging,dispatcher,slowlog,main,connection,rmp_connection}.rs`,
plus whichever tests assert on these fields.

**Steps:**
- [ ] Move `key_field`, adjusting both call sites; confirm no behaviour change.
- [ ] Decide the `protocol` rendering, change all five-plus sites to match, update assertions.
- [ ] Grep the whole workspace afterwards to prove one rendering remains:
      `rg 'protocol\s*=' crates/` and include the output in your report.

---

### Task 3: Remove genuinely duplicated span fields

Some events re-log a field the enclosing span already carries. **The test for whether this is
real duplication was established empirically in plan 19 and you must apply it, not delete
fields on sight:**

> Field duplication is only real when the event's level is at or below the span's level. The
> `cmd` span is a `debug_span!`, so at the `info` production default it is never entered and a
> nested `warn!`/`info!` inherits **none** of its fields. An event at info/warn/error inside a
> `debug_span!` **must** carry its own `cmd`/`key`.

So: `debug!`/`trace!` events inside the `cmd` span duplicate genuinely. Anything at
`info`/`warn`/`error` does not — leave those alone. Plan 19's slowlog `warn!` is the worked
example of the second case.

**In scope (both are `debug!` inside the `debug_span!`):**
- `crates/server/src/dispatcher.rs` ~1129 — the catch-all unknown-command event re-logs `cmd`.
- The `require_args!` macro's `cmd = %$name` — overlaps the span's `cmd`, differing only in case.

**Explicitly NOT in scope — do not touch:** the cold-path unknown-command event's `cmd = %raw`.
When `upper_name` fails, the span's `cmd` field is **empty**, so that event is the only place
the raw command name appears at all. Removing it loses information outright. Verify this for
yourself before touching anything nearby.

**Steps:**
- [ ] Confirm each candidate's level and its enclosing span's level before editing.
- [ ] Remove only the genuinely duplicated fields; update any assertions.
- [ ] State in the commit message which candidates you left alone and why.

---

## Next plan

[23-deferred-cleanups-and-decisions.md](23-deferred-cleanups-and-decisions.md) — the remaining
deferred items: an eviction call-site comment, a missing recovery-path log assertion, and
recording this series' redaction decisions in the spec. After that,
[21-docs-and-final-verification.md](21-docs-and-final-verification.md) closes the series.
