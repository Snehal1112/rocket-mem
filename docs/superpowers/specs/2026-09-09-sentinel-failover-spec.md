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

### Cluster mode compounds this: the topology has zero health-awareness (live-verified 2026-09-09)

This project's cluster mode (`crates/server/src/cluster.rs`, static `cluster.conf` topology,
hash-slot routing) is orthogonal to replication by design, but the interaction was never
investigated until a live failover test on a running 3-shard/3-replica deployment surfaced a
second, independent gap:

- **`CLUSTER NODES`/`CLUSTER SHARDS` have zero health-awareness.** They report the topology
  exactly as configured in `cluster.conf`, with no liveness probing at all. When shard-a's leader
  process was killed outright, every surviving node kept reporting shard-a's dead address as
  `master`/connected — indefinitely, with no timeout, no flag, nothing distinguishing it from a
  healthy node.
- **This produces a hard, total outage for the dead node's entire slot range, cluster-wide.**
  Because routing is driven by that same stale topology, every command touching shard-a's slots —
  reads included, and from any node in the cluster, not just clients talking to shard-a directly —
  got `-MOVED` to the dead address, via the same `ClusterConfig::owns`/`owner_of` gate in
  `cluster_redirect` (called from `dispatch_and_log_inner`) that serves healthy redirects. There
  is no fallback to the healthy
  replica sitting right next to it; `-MOVED` doesn't know a replica exists, let alone that it
  could serve the slot.
- **Manual promotion does not fix cluster-wide routing.** Running `REPLICAOF NO ONE` on shard-a's
  replica makes it writable, but nothing updates `cluster.conf` or any node's in-memory topology —
  `ClusterConfig` is parsed once at startup and, by its own doc comment, "never changes for the
  life of the process." Every other node keeps sending `-MOVED` to the now-doubly-wrong old
  address. Restoring cluster-wide write access requires hand-editing `cluster.conf` on **every**
  node to point shard-a's slot range at the promoted replica's address, then restarting **every**
  node in the cluster — cluster mode has no live topology-reload path.
- **The promoted replica is not a cluster member, and pointing `cluster.conf` at it is not enough
  to make it one.** Every `rocket-mem-shard-*-replica.toml` deliberately omits `cluster_config` and
  `cluster_node_id` (their header comments say so outright), which is correct while the node is a
  replica — but it means `ReplicationHandle::cluster()` is `None` on the promoted node, so
  `cluster_redirect` returns `None` for **every** command. The consequence is worse than a missing
  feature: the promoted node silently accepts keys from *any* slot, including slots it does not
  own, while every other node in the cluster still enforces the routing invariant and redirects to
  it. Writes land on a node that has no claim to them and no other node will ever look there for
  them. So the manual failover procedure has a step the "hand-edit `cluster.conf` everywhere"
  instruction alone misses: **the promoted node's own config must gain `cluster_config` and
  `cluster_node_id` before it is routed to.**
- **Nothing promoted the replica, and nothing ever would have.** shard-a's replica stayed a
  read-only follower of a dead address for the whole outage, `sync_once` reconnecting into
  nothing on its retry loop. The only component in the system that knew was the replica itself:
  `ReplicationHandle::link_up` (surfaced as `INFO REPLICATION`'s `master_link_status:down`). That
  signal is follower-local and nothing consumes it — not the leader, not `cluster.rs`, not any
  third party — so the outage ended only when a human ran `REPLICAOF NO ONE` by hand.
- **Failback silently destroys the promoted node's writes.** A write accepted by the promoted
  replica while it was standalone was gone the moment the restored original leader was made its
  leader again: `sync_once` unconditionally calls `Engine::load_snapshot`, whose
  `Store::load_snapshot_entries` clears all 16 shards before re-inserting the leader's snapshot.
  This is symmetric with the "when the old leader returns" bullet above, now live-verified in the
  other direction — whichever node ends up the follower loses everything it accepted while
  divergent, with no log line and no offset or replication-ID mismatch that could detect it
  afterwards.

This means the offset-less/ack-less findings above are not the only blocker to automatic
failover in this project: even a perfect replication-level promotion (offsets, fencing, the
works) does nothing for cluster-wide write availability by itself, because nothing in
`cluster.rs` reconciles topology with a replica's role change. Any failover story that includes
cluster mode has to solve *both* problems, not just the replication one.

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
   **For a clustered deployment, this runbook must also carry the cluster-mode steps and their
   hazards**: give the promoted node its own `cluster_config`/`cluster_node_id` (it had none as a
   replica, and without them it accepts keys for slots it does not own), hand-edit `cluster.conf`
   on every node, and restart every node (see "Cluster mode compounds this" above). It must also
   treat failback as destructive — re-pointing the promoted node back at a restored leader
   discards everything written since promotion. Note for this project's own deployment: only the
   three leaders have `systemd --user` units, so "restart every node" is not uniformly a
   `systemctl --user restart` and the runbook must give the hand-start form for replicas.

**Additionally, for clustered deployments — explicitly not part of this v1: cluster-topology
reconciliation on promotion.** Independent of steps 1-3 (it doesn't depend on offsets): nothing
updates any node's topology when a replica is promoted, because `ClusterConfig` is parsed once at
startup and never mutated, so a promoted node's writes stay unroutable cluster-wide until the
manual step-3 runbook is followed. A real fix needs either an automated topology-write step or a
live topology-reload command in `cluster.rs` (a `CLUSTER SETSLOT`/`CLUSTER MEET`-equivalent), and
either way two things the current design lacks: `cluster.conf`'s four-field format (`<node-id>
<host:port> <first-slot> <last-slot>`, `ClusterConfig::parse`) has no field for a replica at all,
so the replica set has to come from the replication layer's `slaveN:ip=…` lines; and with
`cluster_my_epoch`/`cluster_current_epoch` pinned to `0` and no cluster bus, a per-node config
push is a non-atomic multi-node write whose partial application leaves nodes disagreeing about a
slot range's owner, with nothing detecting the disagreement — real Redis resolves exactly this
with `configEpoch`. The cheapest honest first step is smaller than any of that: stop hardcoding
`connected` in `cluster_nodes_text`, `health: online`/`role: master` in `cluster_shards_reply`,
and `cluster_state:ok`/`cluster_slots_fail:0` in `cluster_info_text`. This is a hard blocker to
ever calling this project's failover story "automatic" for a clustered deployment, even after
steps 1-2 ship.

Automatic, quorum-based promotion (option (b), a `rocket-sentinel` crate) becomes a realistic,
safe feature to build **after** step 1 exists — not before, and not as part of this v1. For
clustered deployments it also needs the cluster-topology reconciliation described above: a
sentinel that promotes a replica but never updates `cluster.conf` recreates the exact cluster-wide
outage documented above, just with a robot doing the promotion instead of a human.

## Non-goals (for v1 and likely much later)

- Embedded Raft / consensus (option (c)) — out of scope indefinitely; not worth the rewrite this
  project's size/stage.
- Automatic client-redirect-on-failover — depends on automatic promotion existing first (option
  (b) above, itself gated on step 1's offsets).
- **Automated cluster-topology reconciliation on promotion** (see the clustered-deployment
  paragraph above) — deferred alongside automatic promotion itself. The v1 runbook (step 3) only
  covers this manually: cluster operators must still hand-edit `cluster.conf` on every node and
  restart every node after any promotion, automatic or manual.
- Any of this shipping as part of the current 8-sprint roadmap (`docs/rocket-mem-sprint-plan.md`)
  — this is explicitly post-Sprint-8, future work.
- **The sentinel control plane's TLS and authentication design — an open question, not decided
  here.** Option (b) above dials the addresses replicas announce about themselves, discovered via
  `INFO REPLICATION`'s `slaveN:ip=...` lines. What gets announced is now controlled by
  `replica_announce_addr` (defaulting to the plaintext `addr`) — see
  [`../../specs/2026-09-10-replica-announce-addr-spec.md`](../../specs/2026-09-10-replica-announce-addr-spec.md).
  This spec has no answer for whether sentinel probes speak TLS, whether they authenticate, or
  what a sentinel does when a replica announces an address whose protocol it cannot determine (an
  address string carries no protocol marker). Blocks nothing today, because nothing dials the
  announced address yet — but it must be answered before option (b) is built, not discovered
  while building it.
