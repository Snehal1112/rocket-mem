# Sentinel-style Automatic Failover — Scoping Spec

**Status: scoping only, and re-scoped by investigation — not yet broken into an implementation
plan, and the originally-requested feature ("automatic failover") is deliberately NOT this
spec's v1.** See "Decision: v1 is not failover" below before reading further — this is the most
important finding in this document.

## Why

rocket-mem currently supports basic leader→follower replication (`crates/server/src/
replication.rs`: `REPLICAOF` live command, full-resync-only, no offsets, no chaining) but has
zero automatic failover: no health-based leader-death detection, no automatic promotion of a
replica, no client-redirect-on-failover. Every promotion today is a human running `REPLICAOF NO
ONE` on a chosen replica and re-pointing every other follower and every client by hand.

## Investigation findings (opus-level design assessment, 2026-09-09)

### Architecture options, lightest to heaviest

- **(a) External health-checker script.** Polls `INFO REPLICATION`/`/metrics`, on N failed
  probes issues `REPLICAOF NO ONE` + re-points survivors. A single observer has no quorum, so it
  guarantees split-brain under a network partition (the observer can't distinguish "leader is
  dead" from "leader is fine but I can't reach it"). Fine as an ops runbook, not a shippable
  feature.
- **(b) A companion `rocket-sentinel` crate in this workspace.** N≥3 processes, each probing
  every node, agreeing by majority vote that the leader is down, electing one sentinel to drive
  promotion, then reconfiguring survivors. This is the Redis Sentinel shape and is realistically
  buildable by a solo/small team — reuses `INFO REPLICATION`'s existing `role:`/
  `master_link_status:`/`slaveN:ip=...` lines as a discovery mechanism, `handle_replicaof`
  (including its `AUTH` form) for promotion/reconfiguration, and the generation-counter supersede
  mechanism in `start_replicating_with_auth`/`stop_replicating` (already safe under concurrent
  reconfiguration — exactly the race a failover triggers).
- **(c) Embedded Raft inside rocket-mem itself.** Correct in principle, but a project-sized
  rewrite: the replication stream would need to become an indexed log, which it explicitly is
  not today (`replication.rs` has no offset/index concept at all).

### The hard problem this project's *current* replication design makes worse

This is the finding that changes the recommendation. rocket-mem's replication is:
- **Offset-less.** `sync_once` does snapshot-then-stream with no sequence numbers; every
  reconnect is a fresh full resync. There is no way to compare two replicas' "how caught up am I"
  — `last_apply_unix` is a wall-clock timestamp of the last *applied* frame, not a count of frames
  received, so it says nothing about how many writes a replica might have missed.
- **Ack-less.** The leader's write fan-out (`ReplicaRegistry::broadcast`) is fire-and-forget into
  unbounded channels, called *after* the client's write already succeeded and was replied to.
  There is no `WAIT`, no `REPLCONF ACK`, nothing resembling real Redis's `min-replicas-to-write`
  self-fencing.

Consequences if automatic promotion were built on top of this **today**:
- **You cannot pick the most-caught-up replica** — there's no offset to compare, only a
  timestamp that doesn't measure lag in writes.
- **You cannot tell, after promoting a replica, whether acknowledged writes were lost** — a
  client that got `+OK` for a write the old leader never got around to broadcasting has no way to
  detect this after failover. Real Redis with offsets at least surfaces a replication-ID
  mismatch; here it would be silent.
- **Split-brain is unbounded, not just possible.** A partitioned-but-still-alive old leader keeps
  accepting and "acking" writes indefinitely — nothing fences it, because there is no
  `min-replicas-to-write`-equivalent.
- **When the old leader returns** and gets re-pointed at the new one, its divergent writes are
  silently discarded by the mandatory full resync — data loss with no log line marking it.

## Decision: v1 is not failover — it's the safety primitives failover needs

Building automatic promotion on rocket-mem's current offset-less, ack-less replication would
produce a system that silently discards acknowledged writes on every failover — **strictly worse
than today's honest "no failover."** The recommended real v1, in order:

1. **Replication offsets.** A monotonic `u64` counter on the leader, echoed by each follower via
   a periodic `REPLCONF ACK <offset>`-equivalent frame; surfaced in `INFO` as
   `master_repl_offset`/`slave_repl_offset` and as new Prometheus metrics. This is the
   prerequisite every later step depends on — it's what makes "which replica is most caught up"
   and "did we lose acknowledged writes" answerable questions instead of unknowable ones.
2. **`min-replicas-to-write` self-fencing.** A leader with fewer than N acked replicas within M
   seconds refuses writes with a new error. This is the single change that makes split-brain
   *bounded* (the partitioned old leader stops accepting writes on its own) rather than unbounded.
3. **A manual promotion runbook + a health-probe/alerting script** (option (a) above, explicitly
   scoped as alerting-only, not auto-acting) — gets operators a fast, correct manual failover
   using the new offset information to pick the right replica, without pretending it's automatic.

Automatic, quorum-based promotion (option (b), a `rocket-sentinel` crate) becomes a realistic,
safe feature to build **after** step 1 exists — not before, and not as part of this v1.

## Non-goals (for v1 and likely much later)

- Embedded Raft / consensus (option (c)) — out of scope indefinitely; not worth the rewrite this
  project's size/stage.
- Automatic client-redirect-on-failover — depends on automatic promotion existing first (step
  above this one), which itself depends on offsets.
- Any of this shipping as part of the current 8-sprint roadmap (`docs/rocket-mem-sprint-plan.md`)
  — this is explicitly post-Sprint-8, future work.
