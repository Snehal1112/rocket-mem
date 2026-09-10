# Failover Safety Primitives — Plan Index

Fifteen chained implementation plans building the safety primitives that a correct failover would
need, plus honest reporting of what this system actually knows about its own health.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md)
**Design contract (read first, normative):** [`00-design-contract.md`](00-design-contract.md)

## This does not build failover

The spec's central finding: automatic promotion built on rocket-mem's current offset-less,
ack-less replication would silently discard acknowledged writes — **strictly worse than today's
honest "no failover."** No plan here promotes a replica, redirects a client, or rewrites cluster
topology. If a plan appears to require that, stop and re-read the spec.

## How to execute

**`00-design-contract.md` is NOT an executable plan.** It contains zero tasks. It is a reference
document — the fixed names, types, and rulings every plan must agree on. Read it; never "run" it.

The executable plans are **`01` through `15`**. Execute this one:

```
docs/superpowers/plans/2026-09-09-failover-safety-primitives/01-leader-replication-offset.md
```

Each plan ends with a `## Next plan` pointer, so the chain runs 01 → 02 → … → 15 without a human
choosing the next step.

### Status

| Plan | State |
|---|---|
| 01 leader replication offset | ✅ merged to `main` (`6308a6f`..`7c27cf8`) |
| 02 snapshot offset handoff | ✅ merged to `main` (`9f1cea9`..`c860394`) |
| 03 follower replication offset | ✅ built on `main` (`ad4bb1f`..`872f004`) |
| 04 bidirectional replica connection | ✅ built on `main` (`e60d761`..`79d3745`) |
| 05-15 | not started |

**Chain A is complete.** Plans 01-03 together deliver the invariant the rest of this work rests on:
a leader's `master_repl_offset` and a caught-up follower's `slave_repl_offset` converge on the
**same number**, proven end to end by
`a_followers_replication_offset_converges_on_its_leaders` in `crates/server/tests/replication.rs`.
Both of §2.4's traps were live-tripped and closed during plan 03; do not re-derive them.

**Next up: [`05-replica-ack-tracking.md`](05-replica-ack-tracking.md).** Read these first:

- **MANDATORY (plan 04's Important 1): pipeline the first `REPLCONF ACK` behind the `PSYNC` frame,
  in the same write.** Contract §2.3's one non-negotiable — the `parts.read_buf` carry-over at
  `connection.rs:429` — is currently unguarded: deleting that line leaves the whole 1005-test suite
  green, because nothing in the repo pipelines anything behind a `PSYNC`. Plan 05 is both the first
  change that makes the line load-bearing and the change that rewrites the arm consuming it. The
  plan already names a test `an_ack_pipelined_behind_psync_is_not_lost` — it is now required, and it
  must genuinely pipeline, not send the ack as a second write after draining the blob.
- **Plan 05 has seven invalid `cargo test` commands** (`:144`, `:315`, `:321`, `:556`, `:631`,
  `:739`, `:799`), some with four positional filters. `cargo test` accepts one; a second is a hard
  `error: unexpected argument`. Put multiple filters after `--`.
- **Any quoted replacement block in a plan is a diff to reconcile against the live tree, never a
  literal paste.** This bit three times in plans 03 and 04 — each time the quoted code predated the
  verbose-logging merge and would have silently reverted merged instrumentation. Before editing,
  diff the plan's block against the live function and list what the live code has that the plan's
  copy lacks.
- Plan 05's quoted code at `:620-635` was **corrected on 2026-09-10** to drop `?frame`, which
  Debug-renders client bytes in violation of `logging.rs`. Keep the `kind`/`log_len` shape.
- **Do not run a task review concurrently with a sibling task that adds tests.** Plan 04 did, and the
  reviewer measured a tree that had moved under it and reported a false test-count finding.
- Contract §2.3 carries a **dated correction**: `continue`-ing past a decode error does *not* spin
  forever — `FramedRead` fuses and the next poll yields `None`. Read the note, not the retracted
  claim, if any plan text still repeats it.

---

### Superseded — plan 04's pre-flight notes, kept for provenance

Six things were gathered from plan 03's reviews before plan 04 ran:
- **§2.3's `read_buf` snippet must be verified against the pinned tokio-util before use.** (Written
  before plan 04 ran, and it turned out to matter twice: the original form did not compile, and the
  replacement compiled but silently failed to decode. See rule 2 below for the working form.)
- **The plans' `cargo test` commands are invalid.** They pass two or more positional filters and
  `cargo test` accepts one — a second is a hard `error: unexpected argument`, not a filter. Plan 05
  has seven such occurrences (`:144`, `:315`, `:321`, `:556`, `:631`, `:739`, `:799`). Put multiple
  filters after `--`.
- **Do not trust the plans' predicted red-phase strings** where a test uses
  `assert!(cond, "{text}")`. That custom message *replaces* Rust's `assertion failed: ...` line
  rather than appending to it, so the predicted text never appears.
- `replication.rs:105`/`:110` already cite **Sprint 6's** `04-`/`05-` plan files. Prefer naming the
  symbol over citing a plan file; plan 03 had to fix two comments that cited plan filenames which
  then landed.
- The byte-exactness test covers one frame shape (`SET k v`). A table-driven case for an empty
  bulk, a binary payload and a large-arity `MSET` belongs in plan 04, which is already in that file.
- **Run the final clippy gate in an isolated `CARGO_TARGET_DIR`.** Plan 03's final reviewer caught
  that a passing clippy run was a cache replay, and re-ran it from scratch to confirm.

**Before running any plan, make sure only one Claude session is working in this repo folder.** Plan
02 had to be paused mid-flight because a second session moved the checked-out branch, and plan 01
had a commit land on `main` by accident for the same reason.

> **Line numbers in plans 02-15 drift as earlier plans land.** Each plan was written against the
> tree as it stood in 2026-09-09. Plan 01 alone added ~85 lines to `crates/server/src/replication.rs`.
> Anchor on quoted code and symbol names, never on the line numbers, and stop if what you find does
> not match what the brief describes.

Each plan holds **at most 3 tasks**, and each task is an independently testable, committed
deliverable. Use `superpowers:subagent-driven-development` (one fresh subagent per task, reviewed
between tasks) or `superpowers:executing-plans` for inline batch execution.

Before every commit, all three CI gates must be clean:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## The plans

| # | Plan | What it delivers |
|---|---|---|
| 00 | [design contract](00-design-contract.md) | Normative names, types, wire format, and rulings. Not a plan — read first. |
| | **Chain A — replication offsets** (spec step 1) | |
| 01 | [leader replication offset](01-leader-replication-offset.md) | `master_repl_offset` advanced by encoded byte length at the broadcast site; `INFO` key; gauge. |
| 02 | [snapshot offset handoff](02-snapshot-offset-handoff.md) | Carries the leader's offset to a new follower through the snapshot's **existing** 8-byte header. No wire-format change. |
| 03 | [follower replication offset](03-follower-replication-offset.md) | Follower advances `slave_repl_offset` per applied frame; `INFO` keys; gauge. |
| 04 | [bidirectional replica connection](04-bidirectional-replica-connection.md) | Makes the leader **read** from a replica socket at all — today it is provably write-only after PSYNC. Prerequisite for any ack. |
| 05 | [replica ack tracking](05-replica-ack-tracking.md) | `ReplicaEntry` registry restructure; parses `REPLCONF ACK`; per-replica `offset=`/`lag=` in `INFO`. |
| 06 | [follower periodic ack](06-follower-periodic-ack.md) | Follower sends `REPLCONF ACK` ~1/s; end-to-end acceptance test. |
| | **Chain B — write fencing** (spec step 2) | |
| 07 | [fencing config](07-fencing-config.md) | `min_replicas_to_write` / `min_replicas_max_lag_secs` through all four config layers, with validation. |
| 08 | [fencing enforcement](08-fencing-enforcement.md) | The `NOREPLICAS` gate, immediately after the READONLY gate and before the AOF ordering lock. |
| 09 | [fencing observability](09-fencing-observability.md) | Good-replica and min-ack-offset gauges, rejection counter, fenced-state transition logging. |
| | **Chain C — runbook** (spec step 3) | |
| 10 | [replication health probe](10-replication-health-probe.md) | An **alerting-only** health script. Never issues `REPLICAOF`; enforced three independent ways. |
| 11 | [manual promotion runbook](11-manual-promotion-runbook.md) | The operator runbook, including cluster-mode surgery and the failback hazard. |
| | **Chain D — cluster health honesty** | |
| 12 | [cluster peer liveness prober](12-cluster-peer-liveness-prober.md) | Point-to-point peer probing into an `Arc<PeerHealth>` map. Observational only. |
| 13 | [cluster health replies](13-cluster-health-replies.md) | Real health in `CLUSTER NODES`/`SHARDS`/`INFO` instead of hardcoded `connected`/`online`/`ok`. |
| 14 | [cluster health observability](14-cluster-health-observability.md) | Peer gauges, transition logging, config-reference and manual-testing docs. |
| | **Documentation reconciliation** | |
| 15 | [doc sample reconciliation](15-doc-sample-reconciliation.md) | Fixes committed docs that plans 01-14 make false. **Do not skip** — nothing in CI catches a stale doc. |

## Dependencies between chains

The chains are listed in priority order, **not** as independent tracks:

- **B depends on A.** Fencing consumes `good_replicas()`/`ReplicaEntry` from plan 05.
- **D's plan 13 depends on A's plan 01** for the real `master_repl_offset` in `CLUSTER SHARDS`
  (Task 2 only).
- **15 depends on everything**, since it reconciles docs against the finished behavior.

Chain C (10-11) is documentation and tooling; it reads the `INFO` fields chain A adds, so run it
after A.

## Rulings made during planning

Recorded here because each overrode something an author or an earlier contract draft got wrong,
and the reasoning matters more than the decision:

1. **`cluster_slots_fail` is structurally always `0`** (contract §2.6). Redis distinguishes *pfail*
   (this node suspects) from *fail* (a quorum agreed over the cluster bus). This project has no bus
   and no quorum, so a suspicion can never be promoted. An earlier contract draft asked for both at
   once; an author resolved the contradiction by emitting the pfail span in both fields. Overridden:
   `cluster_slots_fail > 0` asserts a consensus that cannot exist, and this spec exists because the
   cluster commands were emitting confident falsehoods. Every field stays individually true and a
   doc comment carries the explanation.
2. **`FramedRead::from_parts` does not exist** — the contract's original `read_buf`-preservation
   snippet did not compile. **Corrected twice**, and the second correction matters more than the
   first: `*inbound.read_buffer_mut() = parts.read_buf;` compiles and preserves the bytes but leaves
   them *undecoded* until a later socket read, silently costing a follower its first ack after every
   attach. The working form is
   `let rd = std::io::Cursor::new(parts.read_buf).chain(rd);` before `FramedRead::new`. The general
   lesson, recorded in contract §2.3: a normative snippet that compiles has been checked for the
   wrong property — pin it with a test that fails without it.
3. **"Log and ignore, never disconnect" is unimplementable for a codec decode error.** `RespCodec`
   returns `Err` on an unknown type byte *without consuming bytes*, so ignoring it spins a hot
   loop. Resolved in the rule's spirit: unparseable bytes stop the inbound reader while the
   outbound stream continues, leaving the leader in the same state as for a follower that never
   acks. Still never a disconnect.
4. **A promoted replica is not a cluster member.** The replica configs deliberately omit
   `cluster_config`/`cluster_node_id`, so `cluster_redirect` returns `None` for every command and
   the promoted node silently accepts keys for slots it does not own. The spec's original
   "hand-edit `cluster.conf` everywhere" instruction was incomplete; both the spec and plan 11 now
   carry the missing step.
5. **Fenced-state transition logging is event-driven**, tied to write attempts rather than
   `/metrics` scrapes, so log timing does not depend on whether Prometheus happens to be polling.
   The blind spot (fencing engages but nobody writes) is covered by the scrape-time
   `rocket_mem_good_replicas` gauge.

## Known flaky test

`ttls_set_before_the_kill_come_back_as_absolute_deadlines_not_restarted_countdowns` in
`crates/server/tests/kill_and_recover.rs` is timing-sensitive and fails intermittently under
parallel load. It is pre-existing and unrelated to any plan here. Re-run, or confirm with
`--test-threads=1`. Do not "fix" it as part of this work, and do not treat it as a regression.
