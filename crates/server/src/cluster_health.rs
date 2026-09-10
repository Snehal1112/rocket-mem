//! Cluster peer liveness. Observational only: this module probes peers and records what it saw,
//! so `CLUSTER NODES`/`SHARDS`/`INFO` can stop hardcoding `connected`/`online`/`ok`. It never
//! promotes a node, never rewrites `cluster.conf`, and never influences routing -- a slot's owner
//! stays its owner while it is dead, because deciding otherwise is a topology decision this
//! project has no mechanism to agree on. See
//! `docs/superpowers/plans/2026-09-09-failover-safety-primitives/00-design-contract.md`, §2.6.

use crate::cluster::ClusterConfig;
use crate::replication::unix_now_secs;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

/// One peer's liveness state. Only the last-success stamp is stored: "failed" is derived from it
/// and the clock at read time, so a peer goes stale on its own with nothing running.
struct PeerState {
    last_ok_unix: AtomicI64,
}

/// Per-peer liveness for the configured topology: one entry per *other* node. This process's own
/// entry is never probed and never stored -- it is the one answering the command.
///
/// The map is built once and never resized, so only the per-entry atomics ever change and no read
/// path takes a lock.
pub struct PeerHealth {
    peers: HashMap<String, PeerState>,
    node_timeout: Duration,
}

impl PeerHealth {
    /// One entry per peer id, each seeded as if it had just answered a probe.
    ///
    /// Seeding to "now" rather than 0 is deliberate and matches Redis: a node is only suspected
    /// after `node_timeout` passes with no successful probe. Seeding to 0 would make every node
    /// report its entire cluster failed for the first probe round after any restart.
    pub fn new<I>(peer_ids: I, node_timeout: Duration) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        let now = unix_now_secs();
        Self {
            peers: peer_ids
                .into_iter()
                .map(|id| {
                    (
                        id,
                        PeerState {
                            last_ok_unix: AtomicI64::new(now),
                        },
                    )
                })
                .collect(),
            node_timeout,
        }
    }

    /// A map for every node in `cluster` except this process's own entry.
    pub fn for_cluster(cluster: &ClusterConfig, node_timeout: Duration) -> Self {
        let my_id = &cluster.myself().id;
        Self::new(
            cluster
                .nodes()
                .iter()
                .filter(|n| &n.id != my_id)
                .map(|n| n.id.clone()),
            node_timeout,
        )
    }

    /// Records that `node_id` answered a probe just now. An id this map does not hold is ignored.
    pub fn record_ok(&self, node_id: &str) {
        if let Some(state) = self.peers.get(node_id) {
            state.last_ok_unix.store(unix_now_secs(), Ordering::Relaxed);
        }
    }

    /// The unix second of `node_id`'s last successful probe; 0 for an id this map does not hold.
    pub fn last_ok_unix(&self, node_id: &str) -> i64 {
        self.peers
            .get(node_id)
            .map_or(0, |s| s.last_ok_unix.load(Ordering::Relaxed))
    }

    /// Overrides a peer's last-success stamp. This exists for tests, which need a peer to be
    /// stale without waiting out a real timeout; the prober itself only ever calls `record_ok`.
    pub fn set_last_ok_unix(&self, node_id: &str, unix: i64) {
        if let Some(state) = self.peers.get(node_id) {
            state.last_ok_unix.store(unix, Ordering::Relaxed);
        }
    }

    /// Whether `node_id` has answered a probe within `node_timeout`.
    ///
    /// An id this map does not hold reports **reachable**, not failed. That covers this process's
    /// own entry (never probed) and any lookup miss: reporting a node dead because of a missing
    /// map entry would be exactly the confident lie this chain exists to remove.
    pub fn is_reachable(&self, node_id: &str) -> bool {
        let Some(state) = self.peers.get(node_id) else {
            return true;
        };
        let elapsed = unix_now_secs().saturating_sub(state.last_ok_unix.load(Ordering::Relaxed));
        // Whole seconds on both sides. `last_ok_unix` has one-second resolution, so a timeout
        // finer than a second could only ever be rounded; clamping to one second keeps a mis-set
        // sub-second value from reporting every peer failed forever.
        elapsed < self.node_timeout.as_secs().max(1) as i64
    }

    /// How long a peer may go without answering before `is_reachable` turns false.
    pub fn node_timeout(&self) -> Duration {
        self.node_timeout
    }

    /// Peers currently answering probes.
    pub fn reachable_count(&self) -> usize {
        self.peers.keys().filter(|id| self.is_reachable(id)).count()
    }

    /// Peers that have not answered within `node_timeout`.
    pub fn unreachable_count(&self) -> usize {
        self.peers.len() - self.reachable_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::unix_now_secs;

    const THREE_SHARDS: &str = "\
shard-a 127.0.0.1:7001 0     5460
shard-b 127.0.0.1:7002 5461  10922
shard-c 127.0.0.1:7003 10923 16383
";

    fn health_for(node_id: &str) -> PeerHealth {
        let config = crate::cluster::ClusterConfig::parse(THREE_SHARDS, node_id).unwrap();
        PeerHealth::for_cluster(&config, Duration::from_secs(15))
    }

    #[test]
    fn for_cluster_holds_every_node_except_this_one() {
        let health = health_for("shard-b");
        assert_eq!(health.reachable_count(), 2);
        assert_eq!(health.unreachable_count(), 0);
        // This node is not in the map at all: it is never probed, and it is the one answering.
        assert_eq!(health.last_ok_unix("shard-b"), 0);
        assert!(health.last_ok_unix("shard-a") > 0);
    }

    #[test]
    fn a_fresh_map_reports_every_peer_reachable() {
        // Seeded to "now", not 0: a peer is only failed after a full node_timeout with no
        // successful probe, so a restart must not report the whole cluster down for one round.
        let health = health_for("shard-b");
        assert!(health.is_reachable("shard-a"));
        assert!(health.is_reachable("shard-c"));
    }

    #[test]
    fn a_peer_whose_last_success_is_older_than_the_timeout_is_not_reachable() {
        let health = health_for("shard-b");
        health.set_last_ok_unix("shard-a", unix_now_secs() - 3600);
        assert!(!health.is_reachable("shard-a"));
        assert!(
            health.is_reachable("shard-c"),
            "only the stale peer changes"
        );
        assert_eq!(health.reachable_count(), 1);
        assert_eq!(health.unreachable_count(), 1);
    }

    #[test]
    fn recording_a_probe_brings_a_failed_peer_back() {
        let health = health_for("shard-b");
        health.set_last_ok_unix("shard-a", unix_now_secs() - 3600);
        assert!(!health.is_reachable("shard-a"));
        health.record_ok("shard-a");
        assert!(health.is_reachable("shard-a"));
        assert!(health.last_ok_unix("shard-a") >= unix_now_secs() - 1);
    }

    #[test]
    fn an_unknown_node_id_reports_reachable_and_is_never_written() {
        // Reporting a node dead because of a lookup miss would be exactly the confident lie this
        // chain exists to remove. Unknown ids read as reachable and writes to them are no-ops.
        let health = health_for("shard-b");
        assert!(health.is_reachable("shard-z"));
        health.record_ok("shard-z");
        health.set_last_ok_unix("shard-z", 1);
        assert_eq!(health.last_ok_unix("shard-z"), 0);
        assert_eq!(health.reachable_count(), 2, "the map itself never grows");
    }

    #[test]
    fn a_sub_second_timeout_is_treated_as_one_second() {
        // `last_ok_unix` has one-second resolution, so a finer timeout could only ever be
        // rounded. Clamping to one second keeps a mis-set value from reporting every peer failed
        // forever; `validate_cluster_health` rejects a zero `cluster_node_timeout_secs` outright.
        let config = crate::cluster::ClusterConfig::parse(THREE_SHARDS, "shard-b").unwrap();
        let health = PeerHealth::for_cluster(&config, Duration::from_millis(10));
        assert!(health.is_reachable("shard-a"));
    }

    #[test]
    fn the_handle_carries_the_map_beside_the_cluster_config() {
        let config = std::sync::Arc::new(
            crate::cluster::ClusterConfig::parse(THREE_SHARDS, "shard-b").unwrap(),
        );
        let plain = crate::replication::ReplicationHandle::default();
        assert!(
            plain.peer_health().is_none(),
            "no prober means no liveness information, which the reply builders report as \
             'exactly as configured'"
        );
        let handle = crate::replication::ReplicationHandle::default()
            .with_cluster(std::sync::Arc::clone(&config))
            .with_peer_health(std::sync::Arc::new(PeerHealth::for_cluster(
                &config,
                Duration::from_secs(15),
            )));
        assert!(handle.peer_health().is_some());
        assert!(handle.peer_health().unwrap().is_reachable("shard-a"));
    }
}
