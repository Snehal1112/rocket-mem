# Cluster Health Observability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** make the peer prober's findings reachable without a `CLUSTER` command — two Prometheus gauges, exactly one log line per peer state *transition* (never one per probe), and the operator documentation for both new config fields plus a worked "kill a shard and watch it get reported" procedure.

**Architecture:** `metrics::refresh_sampled_gauges` gains a `refresh_cluster_gauges` sibling that emits `rocket_mem_cluster_peers_reachable`/`rocket_mem_cluster_peers_unreachable` only when a health map exists. `run_peer_prober`'s loop body moves into a `probe_round` helper that samples each peer's state once per round and returns only the peers whose state changed since the previous round; the loop logs that list. Sampling per round, not per probe, is what makes the timeout-crossing edge — which happens between probes, not at one — detectable exactly once.

**Tech Stack:** nothing new — the `metrics` crate's `gauge!` macro as used throughout `metrics.rs`, and `tracing::info!`/`warn!`.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md) — the "Cluster mode compounds this: the topology has zero health-awareness" section, and the "cheapest honest first step" sentence in the "Additionally, for clustered deployments" paragraph.

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) first, in full.** It is normative. §2.4's metric table fixes both gauge names; §2.6 fixes the prober's observational-only scope.
- **Depends on plans 12 and 13 being landed.** This plan adds no new behavior to the reply builders and no new state to `PeerHealth`.
- **No per-peer metric labels.** Contract §2.4: node addresses and ids are unbounded-cardinality from Prometheus's point of view. These are aggregate gauges only; per-peer detail belongs in `CLUSTER NODES`, which already carries it.
- **One log line per transition, never one per probe.** A one-second probe interval means 86,400 probes per peer per day; logging each one would bury the single line that matters. The transition test in Task 2 exists to pin this and must not be relaxed.
- **Still observational only.** Nothing in this plan may promote a node, rewrite `cluster.conf`, or influence routing. The warn-level log line explicitly tells the reader that nothing was promoted, so an operator reading it at 3am does not assume a failover happened.
- **The three CI gates must be clean before every commit:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
- **Comment style** (project `CLAUDE.md`): short, easy, full sentences ending in a punctuation mark. No emojis.
- `ttls_set_before_the_kill_come_back_as_absolute_deadlines_not_restarted_countdowns` in `crates/server/tests/kill_and_recover.rs` is a known pre-existing flake (contract §4). Not yours.

---

### Task 1: The two peer gauges

**Files:**
- Modify: `crates/server/src/metrics.rs`

**Interfaces:**
- Produces: `pub fn refresh_cluster_gauges(replication: &ReplicationHandle)`, called from `refresh_sampled_gauges`; the metrics `rocket_mem_cluster_peers_reachable` and `rocket_mem_cluster_peers_unreachable`.
- Consumes: `ReplicationHandle::peer_health` (plan 12), `PeerHealth::reachable_count`/`unreachable_count` (plan 12).

- [ ] **Step 1: Write the failing test**

Add to `crates/server/src/metrics.rs`'s `mod tests`:

```rust
    #[test]
    fn the_cluster_peer_gauges_count_reachable_and_unreachable_peers() {
        let handle = recorder_handle();
        let config = std::sync::Arc::new(
            crate::cluster::ClusterConfig::parse(
                "shard-a 127.0.0.1:7001 0 5460\n\
                 shard-b 127.0.0.1:7002 5461 10922\n\
                 shard-c 127.0.0.1:7003 10923 16383\n",
                "shard-a",
            )
            .unwrap(),
        );
        let health = std::sync::Arc::new(crate::cluster_health::PeerHealth::for_cluster(
            &config,
            std::time::Duration::from_secs(15),
        ));
        health.set_last_ok_unix("shard-c", crate::replication::unix_now_secs() - 3600);
        let replication = crate::replication::ReplicationHandle::default()
            .with_cluster(config)
            .with_peer_health(health);

        // Deliberately not `refresh_sampled_gauges`: the recorder is process-wide and the test
        // binary runs many servers in it, so touching the shared key/client gauges here would
        // race the endpoint test above. This function writes only the two cluster gauges.
        refresh_cluster_gauges(&replication);
        let rendered = handle.render();
        assert!(
            rendered.contains("rocket_mem_cluster_peers_reachable 1"),
            "{rendered}"
        );
        assert!(
            rendered.contains("rocket_mem_cluster_peers_unreachable 1"),
            "{rendered}"
        );
    }

    #[test]
    fn a_node_with_no_prober_has_no_peer_health_to_report() {
        // The gauges are emitted only when a health map exists, so a standalone node publishes
        // neither. Asserting their absence in the rendered output would depend on which other
        // test ran first in this shared recorder, so the condition itself is what gets pinned.
        let replication = crate::replication::ReplicationHandle::default();
        assert!(replication.peer_health().is_none());
        refresh_cluster_gauges(&replication); // must not panic and must write nothing
    }
```

- [ ] **Step 2: Run the test and watch it fail**

Run: `cargo test -p rocket-mem metrics::tests::`
Expected failure: `error[E0425]: cannot find function 'refresh_cluster_gauges' in this scope`.

- [ ] **Step 3: Emit the gauges**

In `crates/server/src/metrics.rs`, add the function directly below `refresh_sampled_gauges`:

```rust
/// Publishes the peer prober's aggregate view. Only emitted when a health map exists -- that is,
/// only in cluster mode with a prober running. A standalone node has no peers at all, and
/// publishing `0`/`0` there would put an empty cluster panel on every standalone dashboard; the
/// metrics' absence is the honest signal that this process has no peers to report on.
///
/// Aggregate counts, never per-peer labels: node addresses are unbounded-cardinality from
/// Prometheus's point of view, and `CLUSTER NODES` already carries the per-peer detail.
pub fn refresh_cluster_gauges(replication: &ReplicationHandle) {
    let Some(health) = replication.peer_health() else {
        return;
    };
    ::metrics::gauge!("rocket_mem_cluster_peers_reachable").set(health.reachable_count() as f64);
    ::metrics::gauge!("rocket_mem_cluster_peers_unreachable")
        .set(health.unreachable_count() as f64);
}
```

And call it from `refresh_sampled_gauges`, as its last line:

```rust
    refresh_cluster_gauges(replication);
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p rocket-mem metrics::tests::`
Expected: green, including the pre-existing `the_metrics_endpoint_serves_the_rendered_registry_and_404s_everything_else` — its handle is a plain `ReplicationHandle::default()`, so `refresh_cluster_gauges` returns immediately and writes nothing.

- [ ] **Step 5: Gate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

```bash
git add crates/server/src/metrics.rs
```
Commit through the `1-git-commit` skill. Suggested subject: `Export reachable and unreachable cluster peer gauges`.

---

### Task 2: One log line per peer state transition

**Files:**
- Modify: `crates/server/src/cluster_health.rs`

**Interfaces:**
- Produces: `async fn probe_round(peers: &[(String, String)], health: &PeerHealth, probe_timeout: Duration, last_reported: &mut HashMap<String, bool>) -> Vec<(String, bool)>` (private), returning only the peers whose reachability changed since the previous round.
- Consumes: `probe_once`, `PeerHealth` (plan 12).
- Modifies: `run_peer_prober`'s loop body, which now calls `probe_round` and logs its result.

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/cluster_health.rs`'s `mod tests`:

```rust
    #[tokio::test]
    async fn a_peer_going_down_is_reported_once_not_once_per_probe() {
        let dead = dead_addr().await;
        let peers = vec![("peer".to_string(), dead)];
        let health = PeerHealth::new(["peer".to_string()], Duration::from_secs(1));
        // Already past the timeout, so the very first round is the one that notices.
        health.set_last_ok_unix("peer", unix_now_secs() - 3600);
        let mut last_reported = std::collections::HashMap::new();

        let first = probe_round(
            &peers,
            &health,
            Duration::from_millis(50),
            &mut last_reported,
        )
        .await;
        assert_eq!(first, vec![("peer".to_string(), false)]);

        for round in 2..=4 {
            let later = probe_round(
                &peers,
                &health,
                Duration::from_millis(50),
                &mut last_reported,
            )
            .await;
            assert!(
                later.is_empty(),
                "round {round} re-reported a peer that was already failed"
            );
        }
    }

    #[tokio::test]
    async fn a_peer_coming_back_is_reported_once() {
        let live = spawn_ping_responder().await;
        let peers = vec![("peer".to_string(), live)];
        let health = PeerHealth::new(["peer".to_string()], Duration::from_secs(1));
        health.set_last_ok_unix("peer", unix_now_secs() - 3600);
        let mut last_reported = std::collections::HashMap::from([("peer".to_string(), false)]);

        let first = probe_round(
            &peers,
            &health,
            Duration::from_millis(500),
            &mut last_reported,
        )
        .await;
        assert_eq!(first, vec![("peer".to_string(), true)]);

        let second = probe_round(
            &peers,
            &health,
            Duration::from_millis(500),
            &mut last_reported,
        )
        .await;
        assert!(
            second.is_empty(),
            "a peer that is still answering is not a new event"
        );
    }

    #[tokio::test]
    async fn a_peer_that_never_changes_state_is_never_reported() {
        let live = spawn_ping_responder().await;
        let peers = vec![("peer".to_string(), live)];
        let health = PeerHealth::new(["peer".to_string()], Duration::from_secs(15));
        let mut last_reported = std::collections::HashMap::new();
        for round in 1..=3 {
            let changed = probe_round(
                &peers,
                &health,
                Duration::from_millis(500),
                &mut last_reported,
            )
            .await;
            assert!(changed.is_empty(), "round {round} reported a healthy peer");
        }
    }
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p rocket-mem cluster_health::`
Expected failure: `error[E0425]: cannot find function 'probe_round' in this scope`, three times.

- [ ] **Step 3: Extract `probe_round` and log transitions**

In `crates/server/src/cluster_health.rs`, add `probe_round` directly above `run_peer_prober`:

```rust
/// One probe round: probes every peer concurrently, records the successes, and returns the peers
/// whose reachability changed since the previous round, updating `last_reported` as it goes.
///
/// Sampling state once per round, rather than reacting to each probe's result, is what makes the
/// "went down" edge detectable at all. A peer only *becomes* failed when `cluster_node_timeout_secs`
/// elapses, which happens as time passes between rounds, not at any one probe -- comparing this
/// round's sampled state against what the previous round saw catches that edge exactly once.
///
/// A peer missing from `last_reported` is assumed to have been reachable, matching
/// `PeerHealth::new`'s optimistic seeding: a freshly started process should log the moment a peer
/// goes quiet, not announce at startup that everything is fine.
///
/// Probing is concurrent, not sequential: a round then costs one `probe_timeout` at worst however
/// many peers are dead, so a large cluster's round cannot outlast its own interval.
async fn probe_round(
    peers: &[(String, String)],
    health: &PeerHealth,
    probe_timeout: Duration,
    last_reported: &mut HashMap<String, bool>,
) -> Vec<(String, bool)> {
    let results = futures_util::future::join_all(
        peers
            .iter()
            .map(|(id, addr)| async move { (id.as_str(), probe_once(addr, probe_timeout).await) }),
    )
    .await;
    let mut changed = Vec::new();
    for (id, answered) in results {
        if answered {
            health.record_ok(id);
        }
        let reachable = health.is_reachable(id);
        let previously = last_reported.insert(id.to_string(), reachable).unwrap_or(true);
        if reachable != previously {
            changed.push((id.to_string(), reachable));
        }
    }
    changed
}
```

Then replace `run_peer_prober`'s body in full — the peer-list snapshot is unchanged, the loop now delegates and logs:

```rust
pub async fn run_peer_prober(
    cluster: Arc<ClusterConfig>,
    health: Arc<PeerHealth>,
    interval: Duration,
) {
    let my_id = cluster.myself().id.clone();
    // The peer list is snapshotted once: `ClusterConfig` never changes for the life of the
    // process, so re-reading it every round would buy nothing.
    let peers: Vec<(String, String)> = cluster
        .nodes()
        .iter()
        .filter(|n| n.id != my_id)
        .map(|n| (n.id.clone(), n.addr.clone()))
        .collect();
    if peers.is_empty() {
        return; // a single-node cluster has nothing to probe
    }
    // What the previous round reported for each peer, so only *changes* are logged. At one probe
    // a second, logging every probe would be 86,400 lines per peer per day and would bury the one
    // line that matters.
    let mut last_reported: HashMap<String, bool> = HashMap::new();
    let mut ticker = tokio::time::interval(interval);
    // Delay, not the default Burst: after a slow round the next tick should be a fresh interval
    // away, not a backlog of missed ticks firing back to back.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        for (id, reachable) in
            probe_round(&peers, &health, PROBE_TIMEOUT, &mut last_reported).await
        {
            if reachable {
                tracing::info!(
                    peer = %id,
                    "cluster peer answered a probe again; reporting it connected"
                );
            } else {
                tracing::warn!(
                    peer = %id,
                    node_timeout_secs = health.node_timeout().as_secs(),
                    "cluster peer has not answered a probe within cluster_node_timeout_secs; \
                     reporting it failed in CLUSTER NODES/SHARDS/INFO. Nothing was promoted and \
                     routing is unchanged -- clients are still redirected to this peer's \
                     configured address."
                );
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p rocket-mem cluster_health::`
Expected: green, including plan 12's `the_prober_marks_a_peer_that_stops_answering_as_unreachable` and `the_prober_brings_a_peer_back_when_it_starts_answering_again` — the loop's observable effect on the health map is unchanged by this refactor.

- [ ] **Step 5: Gate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

```bash
git add crates/server/src/cluster_health.rs
```
Commit through the `1-git-commit` skill. Suggested subject: `Log a line each time a cluster peer changes state`.

---

### Task 3: Operator documentation — config reference and manual testing

**Files:**
- Modify: `docs/config-reference.md`
- Modify: `.claude/manual-testing.md`

**Interfaces:**
- Consumes: `Config::cluster_probe_interval_secs`/`cluster_node_timeout_secs` and `validate_cluster_health` (plan 12, Task 1); the reply formats from plan 13; the gauges and log lines from Tasks 1 and 2 above.
- Produces: no code. The verification is running the documented procedure and confirming the real output matches what is written.

- [ ] **Step 1: Add the two rows and a note to `docs/config-reference.md`**

In the `## Fields` table, directly after the `cluster_node_id` row, add:

```markdown
| `cluster_probe_interval_secs` | `ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS` | `--cluster-probe-interval-secs` | `1` | How often, in seconds, this node probes every other node in `cluster_config`'s topology for liveness (a TCP connect plus a `PING`). Cluster mode only — a standalone node has no peers and never starts a prober. Must be at least 1. |
| `cluster_node_timeout_secs` | `ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS` | `--cluster-node-timeout-secs` | `15` | How long, in seconds, a peer may go without answering a probe before this node reports it failed in `CLUSTER NODES`, `CLUSTER SHARDS`, and `CLUSTER INFO`. Reporting only — see the note below. Must be at least 1. |
```

Then add a note section directly after the "### `replicaof`'s auth pair is all-or-nothing; the target itself is not validated" section:

```markdown
### Cluster peer health is reported, never acted on

In cluster mode, `rocket-mem` probes every other node in the topology every
`cluster_probe_interval_secs` and reports any peer that has not answered within
`cluster_node_timeout_secs` as `master,fail?`/`disconnected` in `CLUSTER NODES`, `health: failed`
in `CLUSTER SHARDS`, and `cluster_state:fail` with a non-zero `cluster_slots_pfail` in
`CLUSTER INFO`. Two Prometheus gauges, `rocket_mem_cluster_peers_reachable` and
`rocket_mem_cluster_peers_unreachable`, carry the same information, and each state change is
logged once — once per change, not once per probe.

`cluster_slots_fail` stays `0` even then, and that is correct rather than a bug. Redis's *pfail*
(`fail?`) means one node suspects a peer; *fail* means a majority agreed over the cluster bus.
`rocket-mem` has no cluster bus and no quorum, so a suspicion here can never be promoted — nothing
can ever agree — and the counter has no value it could honestly take but zero. Read
`cluster_slots_pfail` and `cluster_state`; `cluster_slots_fail` is structurally always zero.

That is the entire feature. **Nothing is promoted and no routing changes.** A slot's owner stays
its configured owner while it is dead, so clients keep getting `-MOVED` to a dead address until an
operator intervenes: picking a different owner is a topology decision this project has no
mechanism to agree on (there is no cluster bus, and `cluster_current_epoch` is pinned to `0`).
Recovering write access to a dead shard's slots still means hand-editing `cluster.conf` on every
node and restarting every node.

`cluster_state:fail` here is a report, not a mode — unlike real Redis, this node keeps serving its
own slots and keeps redirecting for everyone else's. That is a deliberate wire-compatibility
divergence: a cluster-aware client that checks `cluster_state` before sending commands may treat
this node as unusable when it is still serving normally.

Both timers are rejected at startup if set to `0`: a zero probe interval is not a valid timer at
all, and a zero node timeout would report every peer failed permanently. If you want faster
detection, lower `cluster_node_timeout_secs` — but keep it comfortably above
`cluster_probe_interval_secs`, or a single slow round will flap a healthy peer.
```

- [ ] **Step 2: Verify the documented defaults against the code**

Run: `cargo test -p rocket-mem config::tests::default_config_has_the_documented_cluster_health_timers`
Expected: green — this is the test that pins `1` and `15`, the two numbers just written into the table. Also confirm the table rows landed:

Run: `grep -c 'cluster_probe_interval_secs\|cluster_node_timeout_secs' docs/config-reference.md`
Expected: at least `2` lines matched.

- [ ] **Step 3: Add the kill-a-shard procedure to `.claude/manual-testing.md`**

In the `## Cluster mode` section, directly before its closing `kill %1 %2 %3` block, add:

````markdown
### Killing a shard: what the survivors report

Before this existed, killing shard-a left every surviving node reporting it as `master` /
`connected` forever — nothing probed liveness, so a dead node was indistinguishable from a healthy
one. Now each node probes its peers every `cluster_probe_interval_secs` (default 1) and reports a
peer that has gone quiet for `cluster_node_timeout_secs` (default 15) as failed.

Start the three nodes with a shorter timeout so this takes seconds instead of a quarter minute —
add these to each node's env in the block above:

```bash
ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS=1 ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS=3
```

Then kill shard-a outright — no shutdown, the way a crash looks:

```bash
ss -tlnp | grep ':7001'          # find shard-a's PID
kill -9 <pid>
sleep 5                          # one node timeout plus a probe interval
```

If you are running the cluster under systemd (`systemctl --user status rocket-mem-shard-a`), stop
it that way instead — `systemctl --user stop rocket-mem-shard-a` — or systemd will restart it out
from under you and you will watch it come straight back.

```bash
redis-cli -p 7002 cluster nodes
# -> shard-a 127.0.0.1:7001@17001 master,fail? - 0 0 0 disconnected 0-5460
#    shard-b 127.0.0.1:7002@17002 myself,master - 0 0 0 connected 5461-10922
#    shard-c 127.0.0.1:7003@17003 master - 0 0 0 connected 10923-16383

redis-cli -p 7002 cluster info | grep -E 'cluster_state|cluster_slots_'
# -> cluster_state:fail
#    cluster_slots_assigned:16384
#    cluster_slots_ok:10923
#    cluster_slots_pfail:5461      <- shard-a's whole span, 0-5460
#    cluster_slots_fail:0          <- always 0: `fail` means a quorum agreed, and there is no
#                                     cluster bus here for anyone to agree over. Read pfail.

redis-cli -p 7002 cluster shards   # shard-a's node entry now reads health: failed

curl -s localhost:9122/metrics | grep cluster_peers
# -> rocket_mem_cluster_peers_reachable 1
#    rocket_mem_cluster_peers_unreachable 1
```

shard-b's log carries exactly one line for the change, not one per probe:

```
WARN cluster peer has not answered a probe within cluster_node_timeout_secs; reporting it failed
     in CLUSTER NODES/SHARDS/INFO. Nothing was promoted and routing is unchanged -- clients are
     still redirected to this peer's configured address. peer="shard-a" node_timeout_secs=3
```

**Routing is deliberately unchanged**, and this is the part to internalize before relying on any
of it:

```bash
redis-cli -p 7002 get hello        # slot 866, owned by the dead shard-a
# -> MOVED 866 127.0.0.1:7001      still the dead address, on purpose
```

Nothing here is a failover. There is no promotion, `cluster.conf` is never rewritten, and no
replica takes over shard-a's slots. Restoring write access to slots 0-5460 is still the manual
runbook: promote shard-a's replica by hand, hand-edit `cluster.conf` on *every* node to name the
promoted address, and restart *every* node — cluster mode has no live topology-reload path. What
changed is only that you can now see which node is dead instead of guessing.

Bring shard-a back and the report reverses within one probe interval, with one `INFO` line:

```bash
# restart shard-a with the same env as before
sleep 2
redis-cli -p 7002 cluster nodes | head -1
# -> shard-a 127.0.0.1:7001@17001 master - 0 0 0 connected 0-5460
redis-cli -p 7002 cluster info | grep cluster_state
# -> cluster_state:ok
```
````

- [ ] **Step 4: Run the documented procedure and confirm every quoted output**

Build, then follow the section exactly as written, in a scratch directory:

```bash
cargo build --release
```

Then start the three nodes per the existing "Cluster mode" block with the two extra env vars,
`kill -9` shard-a, and compare each command's real output against the `#->` lines above. Fix the
document wherever they differ — the quoted output is the assertion, so a mismatch means the
document is wrong, not the server. Clean up with the `## Cleanup` section's `ss`-based
kill-by-PID procedure; do not use a broad `pkill -f rocket-mem`.

- [ ] **Step 5: Gate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green (documentation-only changes, so this is a regression check on the previous two tasks).

```bash
git add docs/config-reference.md .claude/manual-testing.md
```
Commit through the `1-git-commit` skill. Suggested subject: `Document the cluster health timers and the kill-a-shard procedure`.

---

## Next plan

Continue with [`15-doc-sample-reconciliation.md`](15-doc-sample-reconciliation.md).

Chain D's *code* is complete here, and with chains A (01-06), B (07-09), and C (10-11) that finishes
every behavioral change in the failover-safety-primitives set. Plan 15 is the last plan in the
folder: it reconciles the committed documentation, which by this point asserts several
known-limitations that plans 01-14 have just made false — including README bullets and
`docs/qa-playbook.md` rows claiming `cluster_state` is always `ok` and that there are no
replication offsets. Do not skip it; those claims are exactly the kind of confident falsehood this
spec exists to remove, and nothing in CI catches a stale doc.

What deliberately remains unbuilt across the whole set, and must stay that way until someone
revisits the spec:

- **Automatic promotion.** No plan here promotes a replica. The spec's central finding is that automatic failover on this project's replication would silently discard acknowledged writes — strictly worse than today's honest "no failover." Offsets (chain A) and `min-replicas-to-write` fencing (chain B) are the prerequisites that make a *correct* promotion designable later; they are not a promotion.
- **Cluster-topology reconciliation on promotion.** Nothing updates any node's topology when a replica is promoted. `ClusterConfig` is still parsed once at startup and never mutated, `cluster.conf`'s four-field format still has no field for a replica, and with no cluster bus and every epoch pinned to `0` a per-node config push would be a non-atomic multi-node write whose partial application leaves nodes disagreeing about a slot's owner with nothing detecting it. Restoring cluster-wide write access after any promotion is still: hand-edit `cluster.conf` on every node, restart every node.
- **Automatic client-redirect on failover**, and **embedded consensus / Raft**.

All four are named in the spec's [Non-goals](../../specs/2026-09-09-sentinel-failover-spec.md) section and in the design contract's §0. The prober built in plans 12-14 is the spec's *"cheapest honest first step"* and nothing more: it makes the cluster stop lying about which nodes are alive. Anyone extending it into a failover is starting a different project than this spec scoped, and should re-run the scoping first.
