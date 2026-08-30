# README Update & Sprint 5 Close Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** close out Sprint 5's Definition of Done — update the README's Status/persistence sections and known-limits notes, run the full workspace verification one more time end-to-end, and mark the sprint complete in the sprint plan.

**Architecture:** documentation-only — no source changes. Kept as its own last plan (not folded into plan `01`) because it needs the *complete* Sprint 5 picture, which isn't final until plans `01`–`06` have all landed — matching Sprint 4's own `10-readme-and-sprint-close.md` pattern exactly.

**Tech Stack:** none.

**Spec:** `../../specs/2026-08-30-sprint-5-spec.md` — every "Decision" section is authoritative for what this closes out. `../../../rocket-mem-sprint-plan.md`'s Sprint 5 section is authoritative for the sprint's own Definition of Done (recovery-time benchmark, 1-leader-2-follower propagation, kill-and-reconnect-follower).

**Depends on:** `01-snapshot-serialization.md` through `06-replication-integration-tests.md` must all be complete.

---

### Task 1: Update the README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the "Status" section**

Add a new paragraph after the existing Sprint 4 paragraph (leave Sprints 1–4's text unchanged), replacing the current trailing "Remaining sprints..." line:

```markdown
**Sprint 5 (snapshotting & replication) — done.** `SAVE` writes a full, consistent
point-in-time snapshot (`bincode`-encoded, atomically written via write-then-rename) to
`ROCKET_MEM_SNAPSHOT_PATH`; startup loads that snapshot plus only the AOF bytes written
after it — the offset is embedded in the snapshot itself — instead of Sprint 4's
full-AOF-replay-from-empty, cutting recovery time (numbers below). `PSYNC`/`REPLICAOF <host>
<port>` add real leader→follower replication over the server's normal RESP port: a follower
receives a full snapshot, then applies every subsequent write the leader's AOF already logs —
inheriting the `SPOP`→`SREM`/`EXPIRE`-family→`PEXPIREAT` rewrites for free — while rejecting
client-originated writes of its own with a `READONLY` error until `REPLICAOF NO ONE` returns
it to normal operation. Every (re)sync, first or after a dropped connection, is a full resync;
there is no partial-resync/offset-resume support this sprint. See
`docs/superpowers/specs/2026-08-30-sprint-5-spec.md` for the full set of design decisions (why
replication piggybacks on the AOF's already-rewritten frame stream instead of a separate
mechanism, why a follower keeps no AOF of its own, the `SAVE`/`PSYNC` atomicity arguments).

Known limits, called out explicitly rather than left to be discovered: no partial resync (a
dropped follower connection always triggers a full resnapshot, per above); no authentication
on `PSYNC` (any client that sends it is treated as a legitimate replica — Sprint 8 is the
first point auth exists anywhere in this project); a stalled replica's fan-out queue is
unbounded and grows the leader's memory invisibly to `MAXMEMORY` accounting rather than
stalling every writer; `HELLO`/`INFO` still hardcode `role: master`/a bare `# Server` section
regardless of actual replica status.

Remaining sprints (clustering, a custom protocol, ACLs/TLS) are scoped in the
[sprint plan](docs/rocket-mem-sprint-plan.md) but not started.
```

- [ ] **Step 2: Run the recovery-time benchmark and add its results as a new paragraph**

Run: `cargo test -p rocket-mem --test replication snapshot_plus_tail_recovery -- --nocapture`

This prints a line of the shape `recovery benchmark (5000 keys): full AOF replay <D1>, snapshot+tail <D2>`. Insert a new paragraph into the README's Status section, directly after the Sprint 5 paragraph from Step 1, using the two durations this run actually printed in place of `<D1>`/`<D2>`:

```markdown
Recovery-time benchmark (5,000 keys, `cargo test -p rocket-mem --test replication
snapshot_plus_tail_recovery -- --nocapture`): full AOF replay took `<D1>`, snapshot+tail took
`<D2>`.
```

- [ ] **Step 3: Extend the "Running with persistence" section**

Replace the existing section (currently titled "Running with persistence," documenting only `ROCKET_MEM_ADDR`/`ROCKET_MEM_AOF_PATH`) with:

```markdown
### Running with persistence and replication

The server binary reads three environment variables at startup:

| Variable | Default | Purpose |
|---|---|---|
| `ROCKET_MEM_ADDR` | `127.0.0.1:6379` | TCP address to bind |
| `ROCKET_MEM_AOF_PATH` | `./appendonly.aof` | Append-only file path — replayed on startup if it already exists, then opened for appending with an `EverySecond` fsync policy |
| `ROCKET_MEM_SNAPSHOT_PATH` | `./dump.snapshot` | Snapshot file path — loaded on startup if present (together with only the AOF bytes written after the offset embedded in it), written by the `SAVE` command |

Turn a running node into a follower with `REPLICAOF <host> <port>` (sent over its own RESP
connection, e.g. via `redis-cli -p <port> replicaof <host> <port>`); `REPLICAOF NO ONE`
returns it to normal, writable operation. A follower rejects client-originated writes with a
`READONLY` error for as long as it's replicating.
```

- [ ] **Step 4: Fix the `server` workspace-layout bullet**

```markdown
- **`server`** — the binary (package name `rocket-mem`): Tokio TCP accept loop, per-connection task, command dispatcher, AOF writer/replayer, snapshotting, leader/follower replication, and the active-expiry and fsync background loops.
```

Leave the `common`/`engine`/`protocol` bullets as they are.

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`README.md` — do not compose the commit message freeform. Suggested subject:
`docs: update README for Sprint 5 completion`.

---

### Task 2: Full workspace verification and Sprint 5 close note

**Files:**
- Modify: `docs/rocket-mem-sprint-plan.md`

**Interfaces:** none — this task is verification plus one status annotation, matching the
pattern this file already uses for Sprints 1–4.

- [ ] **Step 1: Run the complete verification suite**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all three clean/green — this is the same trio CI (`.github/workflows/ci.yml`) runs
on every push, so a clean local run here means CI will pass too. This run must include
`crates/server/tests/replication.rs` (from `06-replication-integration-tests.md`) — it's a
`cargo test --workspace` target like any other integration test, no special invocation needed.

- [ ] **Step 2: Confirm the sprint's Definition of Done against the spec and sprint plan**

Manually check off each item:
- Recovery time benchmark recorded, showing clear improvement →
  `06-replication-integration-tests.md`'s `snapshot_plus_tail_recovery_reconstructs_identical_state_to_full_aof_replay`
  test, confirmed passing in Step 1's run, numbers already copied into the README in Task 1
  Step 2
- 1 leader + 2 follower integration test passes, writes visible within a bounded time window →
  `06`'s `one_leader_two_followers_propagates_writes_within_a_bounded_time_window`, confirmed
  passing
- Kill-and-reconnect-follower test passes (even if it falls back to full resync) → `06`'s
  `a_follower_reconnects_and_resyncs_after_its_connection_drops`, confirmed passing

- [ ] **Step 3: Update `docs/rocket-mem-sprint-plan.md`'s Sprint 5 section**

Following the exact pattern already used for Sprints 1–4 in this same file, add a
**Status** line right under the `**Sprint goal:**` line:

```markdown
**Status:** ✅ Complete — full P0/P1 scope shipped. See
`docs/superpowers/specs/2026-08-30-sprint-5-spec.md` and
`docs/superpowers/plans/2026-08-30-sprint-5-plans/`.
```

And tick its Definition of Done:

```markdown
### Definition of done
- [x] Recovery time benchmark (snapshot+AOF vs full AOF replay) recorded, showing clear improvement
- [x] 1 leader + 2 follower integration test passes, writes visible within a bounded time window
- [x] Kill-and-reconnect-follower test passes (even if it falls back to full resync)
```

- [ ] **Step 4: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`docs/rocket-mem-sprint-plan.md` — do not compose the commit message freeform. Suggested
subject: `docs: mark Sprint 5 complete in the sprint plan`.
