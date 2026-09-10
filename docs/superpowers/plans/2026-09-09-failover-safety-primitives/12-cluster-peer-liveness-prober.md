# Cluster Peer Liveness Prober Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a background task that probes every other node in the cluster topology once per interval and maintains a shared, per-node liveness map (`PeerHealth`), plus the two config fields that tune it and the `main.rs` wiring that starts it in cluster mode. This plan changes **no** `CLUSTER` reply — its whole deliverable is a correct, tested health map that plan 13 then renders.

**Architecture:** a new `crates/server/src/cluster_health.rs` holds `PeerHealth` (a fixed `HashMap<String, PeerState>` of per-peer `AtomicI64` last-success stamps, built once from the topology) and `run_peer_prober` (a `tokio` task doing TCP connect + `PING` per peer, concurrently, under a `tokio::time::timeout`). `PeerHealth` lives on `ReplicationHandle` **beside** `cluster`, never inside `ClusterConfig` — see Global Constraints. `main.rs` calls `spawn_peer_prober` only when cluster mode is on and hands the returned `Arc<PeerHealth>` to a new `with_peer_health` builder, mirroring the existing `with_cluster` pattern.

**Tech Stack:** nothing new — `tokio` (net, time), `futures_util::future::join_all` (already a `crates/server` dependency), `figment`/`clap` for the two config fields, `tracing` for logging (added in plan 14, not here).

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md) — the "Cluster mode compounds this: the topology has zero health-awareness" section, and the "Additionally, for clustered deployments" paragraph's *"cheapest honest first step"*.

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) first, in full.** It is normative; where it and this plan disagree, it wins and the disagreement is a bug worth reporting before writing code. §2.6 fixes this chain's semantics.
- **The prober is observational only.** It must not promote anything, must not rewrite `cluster.conf`, must not touch `ClusterConfig`, and must not change routing. `cluster_redirect` keeps redirecting to the configured owner even when that owner is known-dead, because the alternative is inventing a topology decision this project has no mechanism to agree on (no cluster bus, `cluster_current_epoch` pinned to `0`). If a step here seems to require promotion or reconciliation, stop — that is explicitly out of scope for the whole folder, and re-reading the spec's non-goals is the fix, not a clever workaround. **Do not "improve" this into a failover.**
- **`PeerHealth` never goes inside `ClusterConfig`.** That type's doc comment promises *"this never changes for the life of the process"* and several readers depend on it. Adding interior mutability there would weaken that guarantee for every one of them. `PeerHealth` is a sibling field on `ReplicationHandle`, set by a `with_peer_health` builder that mirrors `with_cluster`.
- **Absence of a map means "as configured", never "failed".** `peer_health` is an `Option`, `None` for every standalone deployment and every existing test. A node with no prober keeps reporting exactly what it reports today. This is what keeps this whole chain backward-compatible.
- **The probe's connect must be bounded well under the probe interval.** A peer whose host vanished without sending a RST leaves `connect` hanging until the OS TCP timeout — over two minutes on Linux. `PROBE_TIMEOUT` is `500ms`, and peers are probed concurrently, so one round costs at most one `PROBE_TIMEOUT` no matter how many peers are dead.
- **The three CI gates must be clean before every commit:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
  Clippy is strict and lints test code too — a dead-code warning fails CI.
- **Comment style** (project `CLAUDE.md`): short, easy, full sentences ending in a punctuation mark. No emojis.
- `ttls_set_before_the_kill_come_back_as_absolute_deadlines_not_restarted_countdowns` in `crates/server/tests/kill_and_recover.rs` is a known pre-existing flake (contract §4). Not yours; do not "fix" it.

---

### Task 1: The two config fields and their startup validation

**Files:**
- Modify: `crates/server/src/config.rs`

**Interfaces:**
- Produces: `Config::cluster_probe_interval_secs: u64` (default `1`) and `Config::cluster_node_timeout_secs: u64` (default `15`), both layered through defaults < TOML < `ROCKET_MEM_*` env < CLI flags, exactly as contract §2.4's table fixes them; `pub fn validate_cluster_health(config: &Config) -> Result<(), std::io::Error>`.
- Consumes: nothing new.
- Both fields are numeric, so both use the **manual** `if let Some(v) = cli.field` pattern in `cli_overrides`, never the `set!` macro (which only handles `Option<String>` — contract §1.9).

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/config.rs`'s `mod tests`:

```rust
#[test]
fn default_config_has_the_documented_cluster_health_timers() {
    let cfg = Config::default();
    assert_eq!(cfg.cluster_probe_interval_secs, 1);
    assert_eq!(cfg.cluster_node_timeout_secs, 15);
}

#[test]
fn the_cluster_health_timers_are_layered_like_every_other_numeric_field() {
    figment::Jail::expect_with(|jail| {
        jail.create_file(
            "rocket-mem.toml",
            "cluster_probe_interval_secs = 2\ncluster_node_timeout_secs = 20\n",
        )?;
        let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
        assert_eq!(cfg.cluster_probe_interval_secs, 2, "file overrides default");
        assert_eq!(cfg.cluster_node_timeout_secs, 20, "file overrides default");

        jail.set_env("ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS", "3");
        jail.set_env("ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS", "30");
        let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
        assert_eq!(cfg.cluster_probe_interval_secs, 3, "env overrides file");
        assert_eq!(cfg.cluster_node_timeout_secs, 30, "env overrides file");

        let cli = Cli::parse_from([
            "rocket-mem",
            "--config",
            "rocket-mem.toml",
            "--cluster-probe-interval-secs",
            "5",
            "--cluster-node-timeout-secs",
            "50",
        ]);
        let cfg = load_with_cli(cli).unwrap();
        assert_eq!(cfg.cluster_probe_interval_secs, 5, "CLI overrides env");
        assert_eq!(cfg.cluster_node_timeout_secs, 50, "CLI overrides env");
        Ok(())
    });
}

#[test]
fn validate_cluster_health_rejects_a_zero_probe_interval() {
    let cfg = Config {
        cluster_probe_interval_secs: 0,
        ..Config::default()
    };
    let err = validate_cluster_health(&cfg).unwrap_err();
    assert!(
        err.to_string().contains("cluster_probe_interval_secs"),
        "{err}"
    );
}

#[test]
fn validate_cluster_health_rejects_a_zero_node_timeout() {
    let cfg = Config {
        cluster_node_timeout_secs: 0,
        ..Config::default()
    };
    let err = validate_cluster_health(&cfg).unwrap_err();
    assert!(err.to_string().contains("cluster_node_timeout_secs"), "{err}");
}

#[test]
fn validate_cluster_health_accepts_the_defaults() {
    assert!(validate_cluster_health(&Config::default()).is_ok());
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p rocket-mem config::tests::`
Expected failure: compile errors — `error[E0560]: struct 'Config' has no field named 'cluster_probe_interval_secs'` and `error[E0425]: cannot find function 'validate_cluster_health' in this scope`.

- [ ] **Step 3: Add the fields, the CLI flags, the overrides, and the validator**

In `crates/server/src/config.rs`, add to `pub struct Config` (after `cluster_node_id`):

```rust
    /// How often, in seconds, the cluster peer prober probes every other node in the topology.
    /// Read only in cluster mode -- a standalone node has no peers and never starts a prober.
    /// Must be at least 1; see `validate_cluster_health`.
    pub cluster_probe_interval_secs: u64,
    /// How long, in seconds, a peer may go without answering a probe before this node reports it
    /// failed in `CLUSTER NODES`/`SHARDS`/`INFO`. Reporting only: nothing is promoted, no
    /// topology is rewritten, and routing is unchanged. Must be at least 1.
    pub cluster_node_timeout_secs: u64,
```

Add to `impl Default for Config`, in the same position:

```rust
            cluster_probe_interval_secs: 1,
            cluster_node_timeout_secs: 15,
```

Add to `pub struct Cli`, after `cluster_node_id`:

```rust
    /// Seconds between cluster peer liveness probes; cluster mode only [default: 1]
    #[arg(long)]
    pub cluster_probe_interval_secs: Option<u64>,
    /// Seconds without a successful probe before a peer is reported failed; cluster mode only [default: 15]
    #[arg(long)]
    pub cluster_node_timeout_secs: Option<u64>,
```

In `cli_overrides`, beside the existing `slowlog_threshold_micros` block (numeric fields cannot use `set!`):

```rust
    if let Some(v) = cli.cluster_probe_interval_secs {
        map.insert("cluster_probe_interval_secs", Value::from(v));
    }
    if let Some(v) = cli.cluster_node_timeout_secs {
        map.insert("cluster_node_timeout_secs", Value::from(v));
    }
```

And add the validator next to `validate_replicaof`:

```rust
/// Enforces that both cluster-health timers are usable at all. Zero is rejected on both, for two
/// different reasons: `tokio::time::interval` panics outright on a zero period, and a zero node
/// timeout would report every peer failed the moment its last-success stamp aged by a single
/// second -- a permanent false alarm spelled as a config typo, the same class of mistake
/// `min_replicas_max_lag_secs == 0` is rejected for. Validated unconditionally rather than only
/// in cluster mode: a value that would crash or lie should fail startup wherever it is set, not
/// only once someone also turns cluster mode on. `main.rs` calls this before it spawns the
/// prober.
pub fn validate_cluster_health(config: &Config) -> Result<(), std::io::Error> {
    if config.cluster_probe_interval_secs == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cluster_probe_interval_secs must be at least 1",
        ));
    }
    if config.cluster_node_timeout_secs == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cluster_node_timeout_secs must be at least 1",
        ));
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p rocket-mem config::tests::`
Expected: all green, including the pre-existing config tests (the new fields are additive and defaulted, so `default_config_matches_todays_hardcoded_main_rs_values` is unaffected).

- [ ] **Step 5: Gate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

```bash
git add crates/server/src/config.rs
```
Commit through the `1-git-commit` skill (this project's standing convention for Superpowers work). Suggested subject: `Add cluster_probe_interval_secs and cluster_node_timeout_secs`.

---

### Task 2: `PeerHealth` — the shared liveness map

**Files:**
- Create: `crates/server/src/cluster_health.rs`
- Modify: `crates/server/src/lib.rs` (register the module)
- Modify: `crates/server/src/replication.rs` (`unix_now_secs` visibility; the `peer_health` field and its accessor/builder)

**Interfaces:**
- Produces: `pub struct PeerHealth` with `new<I: IntoIterator<Item = String>>(peer_ids, node_timeout)`, `for_cluster(&ClusterConfig, node_timeout)`, `record_ok(&str)`, `last_ok_unix(&str) -> i64`, `set_last_ok_unix(&str, i64)`, `is_reachable(&str) -> bool`, `node_timeout() -> Duration`, `reachable_count()`/`unreachable_count() -> usize`.
- Produces: `ReplicationHandle::with_peer_health(self, Arc<PeerHealth>) -> Self` and `ReplicationHandle::peer_health(&self) -> Option<&Arc<PeerHealth>>`.
- Consumes: `crate::cluster::ClusterConfig` (read-only — `nodes()`, `myself()`), `crate::replication::unix_now_secs` (raised to `pub(crate)` here).
- Plan 13 consumes `is_reachable`; plan 14 consumes `reachable_count`/`unreachable_count`/`node_timeout`.

- [ ] **Step 1: Write the failing tests**

Create `crates/server/src/cluster_health.rs` containing only the test module for now (the implementation lands in step 3):

```rust
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
        assert!(health.is_reachable("shard-c"), "only the stale peer changes");
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
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p rocket-mem cluster_health::`
Expected failure: `error[E0433]: failed to resolve: use of undeclared crate or module 'cluster_health'` from `lib.rs`'s missing `pub mod`, and — once that is added — `cannot find type 'PeerHealth' in this scope` plus `no method named 'peer_health'/'with_peer_health' found for struct 'ReplicationHandle'`.

- [ ] **Step 3: Implement `PeerHealth` and wire it onto `ReplicationHandle`**

In `crates/server/src/lib.rs`, add the module in alphabetical order (right after `pub mod cluster;`):

```rust
pub mod cluster_health;
```

In `crates/server/src/replication.rs`, raise `unix_now_secs`'s visibility and note the new caller:

```rust
/// Unix seconds now, or 0 if the system clock is somehow before the epoch. Never panics: a
/// bogus clock must not take down a server over a metrics field. Used by `record_save`, by
/// `sync_once`'s last-apply stamp, and by `cluster_health`'s probe stamps, so there is exactly
/// one implementation of this expression.
pub(crate) fn unix_now_secs() -> i64 {
```

Add the field to `pub struct ReplicationHandle`, immediately after `cluster`:

```rust
    /// Live liveness of this cluster's *other* nodes, when a prober is running -- `main.rs`
    /// starts one whenever cluster mode is on. `None`, the default for `new`/`Default` and so for
    /// every existing test and every standalone deployment, means no liveness information exists
    /// at all, and the `CLUSTER` reply builders fall back to reporting the topology exactly as
    /// configured -- what they did before the prober existed. Deliberately a sibling of `cluster`
    /// rather than a field inside `ClusterConfig`: that type promises it "never changes for the
    /// life of the process" and several readers depend on that, so it gets no interior
    /// mutability. See the failover-safety design contract, §2.6.
    peer_health: Option<Arc<crate::cluster_health::PeerHealth>>,
```

Add `peer_health: None,` to `ReplicationHandle::new`'s struct literal, right after `cluster: None,`. Then add the builder and accessor next to `with_cluster`/`cluster`:

```rust
    /// Attaches the peer-health map a running prober maintains. Only `main.rs` (in cluster mode)
    /// and cluster tests call this; everything else leaves it `None`. Mirrors `with_cluster`, and
    /// for the same reason: the ~25 existing `ReplicationHandle::new` call sites stay untouched.
    pub fn with_peer_health(
        mut self,
        health: Arc<crate::cluster_health::PeerHealth>,
    ) -> Self {
        self.peer_health = Some(health);
        self
    }

    /// `None` when no prober is running, which the `CLUSTER` reply builders read as "no liveness
    /// information", never as "failed".
    pub fn peer_health(&self) -> Option<&Arc<crate::cluster_health::PeerHealth>> {
        self.peer_health.as_ref()
    }
```

Now put the implementation at the top of `crates/server/src/cluster_health.rs`, above the `mod tests` block written in step 1:

```rust
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
use std::sync::Arc;
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
```

Note on the unused `Arc` import: it is used by the prober added in Task 3. If clippy flags it as unused at this point, add the prober's `use` in Task 3 instead and drop it here — do not leave a warning behind, `-D warnings` fails CI on it.

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p rocket-mem cluster_health::`
Expected: all seven tests green.

- [ ] **Step 5: Gate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green. Nothing outside the new module changed behavior, so every existing test still passes untouched.

```bash
git add crates/server/src/cluster_health.rs crates/server/src/lib.rs crates/server/src/replication.rs
```
Commit through the `1-git-commit` skill. Suggested subject: `Add PeerHealth, the cluster peer liveness map`.

---

### Task 3: The prober task and its `main.rs` wiring

**Files:**
- Modify: `crates/server/src/cluster_health.rs`
- Modify: `crates/server/src/main.rs`

**Interfaces:**
- Produces: `pub async fn run_peer_prober(cluster: Arc<ClusterConfig>, health: Arc<PeerHealth>, interval: Duration)`, `pub fn spawn_peer_prober(cluster: &Arc<ClusterConfig>, probe_interval: Duration, node_timeout: Duration) -> Arc<PeerHealth>`, and the private `probe_once(addr: &str, timeout: Duration) -> bool`.
- Consumes: `PeerHealth` (Task 2), `Config::cluster_probe_interval_secs`/`cluster_node_timeout_secs` and `validate_cluster_health` (Task 1), `ReplicationHandle::with_peer_health` (Task 2).
- Plan 14 refactors `run_peer_prober`'s loop body into a `probe_round` helper to add transition logging; nothing else consumes these.

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/cluster_health.rs`'s `mod tests`:

```rust
    /// A minimal server that answers one `PING` per connection with `+PONG`, so the prober has a
    /// real socket to talk to. The spawned task lives as long as the test process.
    async fn spawn_ping_responder() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 64];
                    if socket.read(&mut buf).await.unwrap_or(0) > 0 {
                        let _ = socket.write_all(b"+PONG\r\n").await;
                    }
                });
            }
        });
        addr
    }

    /// An address nothing listens on: bound to claim an ephemeral port, then dropped. A connect
    /// to it is refused immediately on loopback, so no test here ever waits on a real network
    /// timeout.
    async fn dead_addr() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        addr
    }

    #[tokio::test]
    async fn a_probe_of_a_live_node_succeeds() {
        let addr = spawn_ping_responder().await;
        assert!(probe_once(&addr, Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn a_probe_of_an_address_nothing_listens_on_fails() {
        let addr = dead_addr().await;
        assert!(!probe_once(&addr, Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn a_probe_of_a_peer_that_accepts_but_never_answers_times_out() {
        // The case the timeout exists for: the socket is open, so connect succeeds, and without a
        // bound the read would hang until the OS gave up minutes later, stalling the prober.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let _accepted = listener.accept().await;
            std::future::pending::<()>().await; // hold the connection open, answer nothing
        });
        assert!(!probe_once(&addr, Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn the_prober_marks_a_peer_that_stops_answering_as_unreachable() {
        let dead = dead_addr().await;
        let config = std::sync::Arc::new(
            crate::cluster::ClusterConfig::parse(
                &format!("me 127.0.0.1:1 0 8000\npeer {dead} 8001 16383\n"),
                "me",
            )
            .unwrap(),
        );
        let health = std::sync::Arc::new(PeerHealth::for_cluster(&config, Duration::from_secs(1)));
        // Start from a stale stamp so one failed round is decisive, instead of waiting out a real
        // node timeout in a unit test.
        health.set_last_ok_unix("peer", unix_now_secs() - 3600);
        let task = tokio::spawn(run_peer_prober(
            std::sync::Arc::clone(&config),
            std::sync::Arc::clone(&health),
            Duration::from_millis(20),
        ));
        tokio::time::sleep(Duration::from_millis(150)).await;
        task.abort();
        assert!(
            !health.is_reachable("peer"),
            "a refused connection must never refresh the last-success stamp"
        );
    }

    #[tokio::test]
    async fn the_prober_brings_a_peer_back_when_it_starts_answering_again() {
        let live = spawn_ping_responder().await;
        let config = std::sync::Arc::new(
            crate::cluster::ClusterConfig::parse(
                &format!("me 127.0.0.1:1 0 8000\npeer {live} 8001 16383\n"),
                "me",
            )
            .unwrap(),
        );
        let health = std::sync::Arc::new(PeerHealth::for_cluster(&config, Duration::from_secs(1)));
        health.set_last_ok_unix("peer", unix_now_secs() - 3600);
        assert!(!health.is_reachable("peer"), "starts out failed");
        let task = tokio::spawn(run_peer_prober(
            std::sync::Arc::clone(&config),
            std::sync::Arc::clone(&health),
            Duration::from_millis(20),
        ));
        tokio::time::sleep(Duration::from_millis(150)).await;
        task.abort();
        assert!(health.is_reachable("peer"), "one answered probe is enough");
    }

    #[tokio::test]
    async fn spawn_peer_prober_returns_a_map_its_own_task_keeps_current() {
        let live = spawn_ping_responder().await;
        let config = std::sync::Arc::new(
            crate::cluster::ClusterConfig::parse(
                &format!("me 127.0.0.1:1 0 8000\npeer {live} 8001 16383\n"),
                "me",
            )
            .unwrap(),
        );
        let health = spawn_peer_prober(&config, Duration::from_millis(20), Duration::from_secs(1));
        health.set_last_ok_unix("peer", unix_now_secs() - 3600);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(health.is_reachable("peer"));
        assert_eq!(health.reachable_count(), 1);
        assert_eq!(health.unreachable_count(), 0);
    }
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p rocket-mem cluster_health::`
Expected failure: `error[E0425]: cannot find function 'probe_once' in this scope`, plus the same for `run_peer_prober` and `spawn_peer_prober`.

- [ ] **Step 3: Implement the prober**

Append to `crates/server/src/cluster_health.rs`, below the `impl PeerHealth` block:

```rust
/// How long one probe (connect, send `PING`, read a reply) may take before it counts as a
/// failure. Deliberately far below the smallest allowed probe interval of one second: a peer
/// whose host vanished without sending a RST leaves `connect` hanging until the OS TCP timeout,
/// which is over two minutes on Linux, and an unbounded connect would stall the whole prober
/// behind one dead peer.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// One probe of `addr`: connect, send `PING`, read the first bytes of a reply, all inside
/// `timeout`. `true` means a process at that address answered.
///
/// **Any RESP reply counts as alive, not only `+PONG`, and that is deliberate -- do not tighten
/// this to require a literal `+PONG`.** A node with ACL users configured answers an
/// unauthenticated `PING` with `-NOAUTH Authentication required.`, so a `+PONG`-only check would
/// report every node of an ACL-protected cluster permanently failed -- a self-inflicted
/// cluster-wide false alarm on exactly the deployments most likely to be production. A peer that
/// answers at all is up, and "is it up" is the entire question being asked here.
///
/// The prober deliberately never authenticates: it needs liveness, not access, and giving this
/// loop cluster-wide credentials would be a new secret to manage for no extra information. See
/// the failover-safety design contract, §2.6.
async fn probe_once(addr: &str, timeout: Duration) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let probe = async {
        let mut socket = tokio::net::TcpStream::connect(addr).await.ok()?;
        socket.write_all(b"*1\r\n$4\r\nPING\r\n").await.ok()?;
        let mut buf = [0u8; 32];
        let read = socket.read(&mut buf).await.ok()?;
        // A zero-length read is the peer closing the connection, not answering it.
        (read > 0 && (buf[0] == b'+' || buf[0] == b'-')).then_some(())
    };
    tokio::time::timeout(timeout, probe)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Probes every peer in `cluster` forever, one round every `interval`, recording each success in
/// `health`.
///
/// Observational only. It writes nothing but timestamps: no promotion, no `cluster.conf` rewrite,
/// no routing change. A slot's configured owner stays its owner while it is dead, and
/// `cluster_redirect` keeps sending clients there -- see this module's doc comment.
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
    let mut ticker = tokio::time::interval(interval);
    // Delay, not the default Burst: after a slow round the next tick should be a fresh interval
    // away, not a backlog of missed ticks firing back to back.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        // Concurrently, not one peer after another: a round then costs one `PROBE_TIMEOUT` at
        // worst however many peers are dead, so a large cluster's round cannot outlast its own
        // interval.
        let results = futures_util::future::join_all(
            peers
                .iter()
                .map(|(id, addr)| async move { (id.as_str(), probe_once(addr, PROBE_TIMEOUT).await) }),
        )
        .await;
        for (id, answered) in results {
            if answered {
                health.record_ok(id);
            }
        }
    }
}

/// Builds the peer-health map for `cluster` and spawns the prober that keeps it current, then
/// returns the map so the caller can hand it to `ReplicationHandle::with_peer_health`.
///
/// Called only in cluster mode: a standalone node has no peers, so it gets no map at all and its
/// `CLUSTER` replies keep reporting exactly what they reported before this existed.
pub fn spawn_peer_prober(
    cluster: &Arc<ClusterConfig>,
    probe_interval: Duration,
    node_timeout: Duration,
) -> Arc<PeerHealth> {
    let health = Arc::new(PeerHealth::for_cluster(cluster, node_timeout));
    tokio::spawn(run_peer_prober(
        Arc::clone(cluster),
        Arc::clone(&health),
        probe_interval,
    ));
    health
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p rocket-mem cluster_health::`
Expected: all green. The two timing tests sleep 150ms each against a 20ms interval, so they see roughly seven rounds — there is no race on one probe landing.

- [ ] **Step 5: Wire it into `main.rs`**

In `crates/server/src/main.rs`, immediately **before** the `let cluster = match (&config.cluster_config, &config.cluster_node_id) {` block, add:

```rust
    // Before the cluster block, because `spawn_peer_prober` builds a `tokio::time::interval`,
    // which panics outright on a zero period. A bad timer must fail startup with a readable
    // error, not abort the process from inside a spawned task.
    rocket_mem::config::validate_cluster_health(&config)?;
```

Then replace the existing `if let Some(cluster) = cluster { handle = handle.with_cluster(cluster); }` with:

```rust
    if let Some(cluster) = cluster {
        // Cluster mode only: the prober needs peers, and a standalone node has none. It is purely
        // observational -- it makes `CLUSTER NODES`/`SHARDS`/`INFO` tell the truth about which
        // peers are answering, and changes nothing about routing, promotion, or the topology
        // file. See docs/superpowers/plans/2026-09-09-failover-safety-primitives/.
        let peer_health = rocket_mem::cluster_health::spawn_peer_prober(
            &cluster,
            std::time::Duration::from_secs(config.cluster_probe_interval_secs),
            std::time::Duration::from_secs(config.cluster_node_timeout_secs),
        );
        handle = handle.with_cluster(cluster).with_peer_health(peer_health);
    }
```

- [ ] **Step 6: Verify the wiring by hand**

The prober changes no reply yet (that is plan 13), so the observable result here is a clean start and a rejected bad timer.

```bash
cargo build --workspace
cd "$(mktemp -d)"
cat > cluster.conf <<'EOF'
solo 127.0.0.1:7401 0 16383
EOF
ROCKET_MEM_ADDR=127.0.0.1:7401 ROCKET_MEM_RMP_ADDR=127.0.0.1:7402 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:7403 ROCKET_MEM_CLUSTER_CONFIG=cluster.conf \
ROCKET_MEM_CLUSTER_NODE_ID=solo \
  timeout 2 /path/to/target/debug/rocket-mem || true
```
Expected: the normal startup banner with the `cluster` line, then killed by `timeout`. A single-node cluster has no peers, so `run_peer_prober` returns immediately — a clean start is the pass condition.

```bash
ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS=0 timeout 2 /path/to/target/debug/rocket-mem || true
```
Expected: exits immediately with `Custom { kind: InvalidInput, error: "cluster_probe_interval_secs must be at least 1" }`, before any listener binds.

- [ ] **Step 7: Gate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

```bash
git add crates/server/src/cluster_health.rs crates/server/src/main.rs
```
Commit through the `1-git-commit` skill. Suggested subject: `Probe cluster peers for liveness in the background`.

---

## Next plan

[`13-cluster-health-replies.md`](13-cluster-health-replies.md) — render this map into `CLUSTER NODES`, `CLUSTER SHARDS`, and `CLUSTER INFO`, replacing the hardcoded `connected` / `health: online` / `cluster_state:ok`.
