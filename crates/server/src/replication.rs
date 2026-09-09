use crate::aof::AofWriter;
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Unix seconds now, or 0 if the system clock is somehow before the epoch. Never panics: a
/// bogus clock must not take down a server over a metrics field. Used by `record_save` and by
/// `sync_once`'s last-apply stamp, so there is exactly one implementation of this expression.
fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Holds one outbound channel per connected replica, paired with the address (if any) that
/// replica advertised in its `PSYNC` -- see `ReplicationHandle::own_addr`'s doc comment. The
/// `Mutex` is a plain `std::sync::Mutex`, not `tokio::sync::Mutex`: every access is a quick,
/// synchronous push/retain, never held across an `.await`, so the lighter std lock is the right
/// tool — matching `AofWriter::order`'s existing choice for the same reason.
#[derive(Default)]
pub struct ReplicaRegistry {
    replicas: std::sync::Mutex<
        Vec<(
            Option<String>,
            tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
        )>,
    >,
}

impl ReplicaRegistry {
    /// Registers a newly-synced replica's outbound channel, alongside the address (if any) it
    /// advertised in its `PSYNC` frame -- `None` for a bare `PSYNC` (an old client, or a test).
    /// Called only from `serve_replica` (Task 4), while it still holds
    /// `AofWriter::lock_for_ordering()` — see this plan's Global Constraints for why
    /// registration must happen inside that same critical section as the snapshot walk, not
    /// after it.
    pub fn register(
        &self,
        addr: Option<String>,
        sender: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
    ) {
        self.replicas
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((addr, sender));
    }

    /// Fans `bytes` out to every registered replica, pruning any whose receiver has been
    /// dropped (the replica connection died). Never itself returns an error: a delivery
    /// failure to one dead replica must not affect delivery to the others, and must never
    /// roll back the write that already committed on the leader.
    pub fn broadcast(&self, bytes: bytes::Bytes) {
        let mut replicas = self.replicas.lock().unwrap_or_else(|e| e.into_inner());
        replicas.retain(|(_, tx)| tx.send(bytes.clone()).is_ok());
    }

    /// How many replicas are currently registered. Note this counts senders, which are pruned
    /// lazily by `broadcast`, so a replica that died since the last write may still be counted
    /// until the next one -- an acceptable lag for a gauge, and cheaper than probing sockets.
    pub fn len(&self) -> usize {
        self.replicas
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Every currently-registered replica's advertised address, in registration order -- `None`
    /// for a replica whose `PSYNC` carried no address. Feeds `INFO REPLICATION`'s `slaveN:`
    /// lines on the leader side; subject to the same lazy-pruning lag as `len`.
    pub fn addrs(&self) -> Vec<Option<String>> {
        self.replicas
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(addr, _)| addr.clone())
            .collect()
    }

    /// Required by `clippy::len_without_is_empty`, which `-D warnings` makes a hard error.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Threads leader/follower replication state through `dispatch_and_log` without adding a
/// parameter to plain `dispatch` — see the sprint-5 spec's `ReplicationHandle` decision for
/// why `dispatch`'s ~250 call sites must stay untouched.
pub struct ReplicationHandle {
    /// Leader side: connected replicas to fan writes out to. Empty until `serve_replica`
    /// (`04-replica-registry-and-leader-fanout.md`, Task 4) calls `ReplicaRegistry::register`
    /// during `PSYNC` handling.
    pub registry: ReplicaRegistry,
    /// Follower side: gates client-originated writes once this node is replicating from a
    /// leader. Read by `dispatch_and_log`'s `-READONLY` check, added in
    /// `05-replicaof-and-follower-apply-loop.md`. A plain field, not `Arc<AtomicBool>`: the
    /// whole handle is already behind one `Arc` wherever it's shared, so a second layer of
    /// sharing buys nothing.
    pub is_replica: AtomicBool,
    /// Follower side: the running `replication_client_loop`, if any — set and aborted by
    /// `start_replicating`/`stop_replicating` below.
    follower_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The engine this handle's leader-side `PSYNC` snapshots from and follower-side
    /// `replication_client_loop` applies into. An owned `Arc`, not a borrow: a spawned
    /// follower task is `'static` and cannot hold a borrow of `dispatch_and_log`'s own
    /// `engine: &Engine` parameter. Invariant, enforced only by convention (there is no way to
    /// assert it in the type system): this must be the *same* `Engine` `serve()` was handed.
    engine: Arc<Engine>,
    /// Where `SAVE` writes, from `ROCKET_MEM_SNAPSHOT_PATH`.
    snapshot_path: PathBuf,
    /// Bumped under `follower_task`'s mutex every time `start_replicating`/`stop_replicating`
    /// changes which task (if any) owns `follower_task`. A spawned task captures the
    /// generation it was started with and checks it against this counter's live value before
    /// applying any state. This closes a race `abort()` alone leaves open: `JoinHandle::abort`
    /// only cancels at the task's *next* await point, so a task already mid-poll when abort()
    /// is called can still finish that poll — e.g. complete a `read_exact` and go on to call
    /// `load_snapshot`, clobbering a newer task's already-loaded state. `Arc` because a
    /// spawned task is `'static` and needs its own handle to the shared counter, independent
    /// of `self`. Note this check-then-act is still not fully atomic: a superseding task would
    /// need to connect, `PSYNC`, and read a whole snapshot blob inside the few instructions
    /// between a stale task's check and its `load_snapshot` call, which is not reachable in
    /// practice — closing that theoretical residual would require taking `follower_task`'s
    /// mutex around the apply itself, which isn't worth the added contention for this sprint.
    generation: Arc<AtomicU64>,
    /// Follower side: the `AofWriter` whose `lock_for_ordering()` this node's apply loop takes
    /// around each replicated frame it applies. This is the follower-side counterpart to
    /// `handle_save`'s own use of that lock: a replicated multi-key command (`MSET`, `RENAME`,
    /// `SINTERSTORE`, ...) mutates more than one shard, while `SAVE`'s `Store::snapshot_entries`
    /// walk locks the 16 shards one at a time, so without a lock spanning the whole apply a
    /// concurrent `SAVE` on this same node could capture such a command half-applied and write a
    /// torn snapshot. Client-originated writes already close that race via `dispatch_and_log`;
    /// replicated ones go through plain `dispatch` and so need this. `Option` because only
    /// `main.rs` (via `with_aof`) has a real `AofWriter` to hand over — test-constructed handles
    /// leave it `None` and their apply loops take no lock, which is correct since no `SAVE` runs
    /// against them.
    aof: Option<Arc<AofWriter>>,
    /// Follower side: when set, `sync_once` upgrades its connection to the leader to TLS,
    /// pinned to exactly the one certificate this `ClientConfig` trusts (see
    /// `tls::load_client_config`). `None` -- the default for `new`/`Default` -- means plaintext
    /// replication, matching every existing test and pre-this-fix deployment.
    replication_tls_client_config: Option<Arc<rustls::ClientConfig>>,
    /// The static cluster topology, when this node was started in cluster mode. `None` -- the
    /// default for `new`/`Default`, i.e. every existing test and every standalone deployment --
    /// means cluster mode is off: no `-MOVED`, no `-CROSSSLOT`, `cluster_enabled:0` in `INFO`.
    /// A builder-set `Option` rather than a third `new` parameter, mirroring `with_aof` above
    /// and for the same reason: the existing `ReplicationHandle::new`/`::default()` call sites
    /// (all of them tests) stay untouched.
    ///
    /// Naming note: this struct now carries a snapshot path, an AOF handle, and a cluster
    /// config -- it is shared *server* state, not a replication handle. Renaming it to
    /// `ServerState` is deferred to Sprint 7, whose dual-protocol work has to touch these
    /// signatures anyway; see ../../docs/superpowers/specs/2026-08-30-sprint-6-spec.md.
    cluster: Option<Arc<crate::cluster::ClusterConfig>>,
    /// Live client connections, kept by a Drop guard in `handle_connection` so it is decremented
    /// on every one of that function's early returns, including the `serve_replica` path.
    connected_clients: AtomicUsize,
    /// Every connection ever accepted; never decremented.
    total_connections: AtomicU64,
    /// Every command that reached `dispatch_and_log`. Replicated commands applied through plain
    /// `dispatch` are deliberately *not* counted -- they are not client traffic.
    total_commands: AtomicU64,
    /// Keys removed by the active expiry sweep. Passively expired keys (a read finding a key
    /// already dead) are not counted: that would mean a counter on the hottest read path in the
    /// project, inside `Shard`, for a statistic nothing gates on.
    expired_keys: AtomicU64,
    /// Unix seconds at which this node last applied a replicated frame; 0 if it never has. An
    /// `Arc` because the spawned follower task is `'static` and needs its own handle.
    last_apply_unix: Arc<AtomicI64>,
    /// When this handle was built, which for `main.rs`'s single handle is process start. Feeds
    /// `INFO`'s `uptime_in_seconds`.
    started_at: std::time::Instant,
    /// Unix seconds of the last successful `SAVE`; 0 if none has run in this process. This is
    /// per-process state, not read back from the snapshot file: the file has no timestamp field
    /// (Sprint 5 deliberately gave it no header beyond the AOF offset), so reporting anything
    /// else would be a guess.
    last_save_unix: AtomicI64,
    /// `Some(host:port)` while this node is a follower. Set by `start_replicating`, cleared by
    /// `stop_replicating`; feeds `INFO`'s `master_host`/`master_port`.
    master_addr: Mutex<Option<String>>,
    /// Whether the follower's link to its leader is currently up -- set true once a sync has
    /// loaded a snapshot, false when that connection ends or fails. An `Arc` because the spawned
    /// follower task is `'static`. This is the honest counterpart to real Redis's
    /// `master_link_status`: it tracks the connection, not a byte offset, because this project
    /// has no replication offsets at all.
    link_up: Arc<AtomicBool>,
    /// Leader side: how many bytes of replication stream this node has produced since it
    /// started. Advanced by `dispatch_and_log_inner`'s fan-out loop by the encoded length of
    /// every frame it hands to `ReplicaRegistry::broadcast`, under the same AOF ordering guard
    /// the broadcast itself is under, so the advance and broadcast are atomic as observed under
    /// lock_all_shards. It counts the write stream this leader produced, not what any replica
    /// received, so it advances even when no replica is connected. Process-local: it resets to 0
    /// on restart, which is safe only because every reconnect is a full resync that re-seeds the
    /// follower from the snapshot header, so a follower can never carry a stale offset across a
    /// leader restart. An `Arc` for symmetry with the follower-side counter added in
    /// `03-follower-replication-offset.md`, whose spawned task is `'static`.
    master_repl_offset: Arc<AtomicU64>,
    /// Recently-slow commands, recorded by the `dispatch_and_log` wrapper. A plain field, not an
    /// `Option`: it is always present and always cheap when nothing is slow, so there is nothing
    /// to configure away. `main.rs` sets its threshold from the environment via
    /// `with_slowlog_threshold`; `new`/`Default` use the 10ms default.
    pub slowlog: crate::slowlog::SlowLog,
    /// The truncation cap `logging::fmt_value`/`logging::redact_args` apply to each rendered
    /// argument on the `trace`-level dispatch line. Stored as `usize` so the call site needs no
    /// cast on the hot path. `main.rs` sets it from `Config::log_value_max_bytes` via
    /// `with_log_value_max_bytes`; `new`/`Default` use the same 128-byte default `Config` does,
    /// so the ~25 test-constructed handles behave identically to a real server.
    log_value_max_bytes: usize,
    /// In-memory ACL users. Empty by default -- every existing test and deployment through
    /// Sprint 7 -- populated only via `with_acl_bootstrap` (from the TOML config's
    /// `[[acl.users]]`) and at runtime via `ACL SETUSER` (plan 08). Never persisted; see
    /// ../../../docs/superpowers/plans/2026-08-31-sprint-8-plans/04-acl-store-and-bootstrap-wiring.md.
    pub acl: crate::acl::AclStore,
    /// This node's own RESP listen address, advertised to a leader via `PSYNC <addr>` once this
    /// node is a follower -- lets the leader's `INFO REPLICATION` report which address to reach
    /// this replica at (`slaveN:ip=...,port=...`), the same purpose real Redis's `REPLCONF
    /// listening-port` serves. `None` -- the default -- sends a bare `PSYNC`, matching every
    /// pre-this-feature test and any deployment that never calls `with_own_addr`.
    own_addr: Option<String>,
}

impl ReplicationHandle {
    pub fn new(engine: Arc<Engine>, snapshot_path: PathBuf) -> Self {
        Self {
            registry: ReplicaRegistry::default(),
            is_replica: AtomicBool::new(false),
            follower_task: Mutex::new(None),
            engine,
            snapshot_path,
            generation: Arc::new(AtomicU64::new(0)),
            aof: None,
            replication_tls_client_config: None,
            cluster: None,
            connected_clients: AtomicUsize::new(0),
            total_connections: AtomicU64::new(0),
            total_commands: AtomicU64::new(0),
            expired_keys: AtomicU64::new(0),
            last_apply_unix: Arc::new(AtomicI64::new(0)),
            started_at: std::time::Instant::now(),
            last_save_unix: AtomicI64::new(0),
            master_addr: Mutex::new(None),
            link_up: Arc::new(AtomicBool::new(false)),
            master_repl_offset: Arc::new(AtomicU64::new(0)),
            slowlog: crate::slowlog::SlowLog::default(),
            log_value_max_bytes: 128,
            acl: crate::acl::AclStore::default(),
            own_addr: None,
        }
    }

    /// Configures the `AofWriter` this node's own replication apply loop synchronizes against a
    /// concurrent `SAVE` through — see the `aof` field's doc comment for the torn-snapshot race
    /// this closes. A builder method rather than a third `new` parameter so the ~25 existing
    /// `ReplicationHandle::new` call sites (all of them tests, none of which run a `SAVE`
    /// against a follower) stay untouched. Only `main.rs` calls this, with the same `AofWriter`
    /// `serve()` was handed. Every test-constructed handle (via `new` alone, or `Default`)
    /// leaves this `None`, so its apply loop, if any, takes no lock — matching the pre-fix
    /// behavior for those.
    pub fn with_aof(mut self, aof: Arc<AofWriter>) -> Self {
        self.aof = Some(aof);
        self
    }

    /// Sets this node's own RESP listen address, advertised on every `PSYNC` this node's
    /// follower loop sends -- see the `own_addr` field's doc comment. Only `main.rs` calls this,
    /// with `config.addr`.
    pub fn with_own_addr(mut self, addr: String) -> Self {
        self.own_addr = Some(addr);
        self
    }

    /// Configures this node's follower-side replication connection to upgrade to TLS, pinned
    /// to `config`'s one trusted certificate (built via `tls::load_client_config`). A builder
    /// method, matching `with_aof`/`with_cluster`'s existing pattern, so the ~25 existing
    /// `ReplicationHandle::new` call sites (all tests, none configuring TLS) stay untouched.
    pub fn with_replication_tls_client_config(mut self, config: Arc<rustls::ClientConfig>) -> Self {
        self.replication_tls_client_config = Some(config);
        self
    }

    /// Puts this node into cluster mode with the given static topology. Only `main.rs` and
    /// `crates/server/tests/cluster.rs` call this; everything else leaves cluster mode off.
    pub fn with_cluster(mut self, cluster: Arc<crate::cluster::ClusterConfig>) -> Self {
        self.cluster = Some(cluster);
        self
    }

    /// Overrides the slow-log threshold. `Duration::ZERO` disables recording entirely -- see
    /// ../../../docs/superpowers/specs/2026-08-30-sprint-6-spec.md for why that differs from real
    /// Redis's meaning for 0.
    pub fn with_slowlog_threshold(mut self, threshold: std::time::Duration) -> Self {
        self.slowlog = crate::slowlog::SlowLog::with_threshold(threshold);
        self
    }

    /// Sets the `trace`-level argument truncation cap -- see the `log_value_max_bytes` field.
    /// A builder method, matching `with_aof`/`with_cluster`/`with_slowlog_threshold`'s existing
    /// pattern, so the ~25 existing `ReplicationHandle::new` call sites stay untouched.
    ///
    /// Takes the `u64` `Config` declares and saturates into `usize`: on a 32-bit target a cap
    /// larger than the address space would otherwise wrap to a small one and silently log
    /// *less* than configured.
    pub fn with_log_value_max_bytes(mut self, cap: u64) -> Self {
        self.log_value_max_bytes = usize::try_from(cap).unwrap_or(usize::MAX);
        self
    }

    /// Seeds the ACL store from the config file's `[[acl.users]]` bootstrap list. A builder
    /// method, matching `with_aof`/`with_cluster`/`with_slowlog_threshold`'s existing pattern, so
    /// the ~25 existing `ReplicationHandle::new` call sites (all tests, none configuring ACLs)
    /// stay untouched.
    pub fn with_acl_bootstrap(self, users: Vec<crate::acl::AclUser>) -> Self {
        for user in users {
            self.acl.insert_bootstrap(user);
        }
        self
    }

    /// `None` when cluster mode is off. `dispatch_and_log`'s redirection gate short-circuits on
    /// this before extracting any key, so a standalone node pays one `Option` check per command.
    pub fn cluster(&self) -> Option<&Arc<crate::cluster::ClusterConfig>> {
        self.cluster.as_ref()
    }

    /// The `trace`-level argument truncation cap, read once per command by `dispatch_and_log`
    /// but only when `trace` is actually enabled.
    pub fn log_value_max_bytes(&self) -> usize {
        self.log_value_max_bytes
    }

    /// For `SAVE` and (later) `PSYNC` handling, which need the shared `Engine` to snapshot
    /// from, and for the follower apply loop, which needs it to apply replicated frames into.
    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// For `SAVE`, which needs to know where to write.
    pub fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }

    /// Cancels any currently-running replication task, then spawns a new one against
    /// `host_port` and sets `is_replica`. The whole sequence — abort old, bump the generation,
    /// spawn new, store, set the flag — happens under `follower_task`'s mutex, so two clients
    /// issuing `REPLICAOF` concurrently can only serialize, never leave two apply loops racing
    /// into the same `Engine`. Bumping the generation here (not just relying on `abort()`) is
    /// what stops a stale task's already-in-flight poll from mutating state after this call
    /// returns — see the `generation` field's doc comment.
    pub fn start_replicating(&self, host_port: String) {
        self.start_replicating_with_auth(host_port, None);
    }

    /// Same as `start_replicating`, but also authenticates against the leader with `AUTH
    /// <username> <password>` before `PSYNC` -- required when the leader has ACL users
    /// configured, since an unauthenticated `PSYNC` is otherwise rejected with `NOAUTH` and
    /// replication can never complete. See `handle_replicaof`'s `REPLICAOF ... AUTH user pass`
    /// clause, the only production caller of this with `Some`.
    pub fn start_replicating_with_auth(&self, host_port: String, auth: Option<(String, String)>) {
        let mut task = self.follower_task.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = task.take() {
            old.abort();
        }
        let my_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let engine = Arc::clone(&self.engine);
        let generation = Arc::clone(&self.generation);
        let aof = self.aof.clone();
        let tls_client_config = self.replication_tls_client_config.clone();
        let last_apply = self.last_apply_slot();
        *self.master_addr.lock().unwrap_or_else(|e| e.into_inner()) = Some(host_port.clone());
        let link_up = self.link_up_slot();
        let identity = FollowerIdentity {
            own_addr: self.own_addr.clone(),
            auth,
        };
        *task = Some(tokio::spawn(replication_client_loop(
            host_port,
            engine,
            Generation {
                counter: generation,
                mine: my_generation,
            },
            aof,
            FollowerHandles {
                last_apply,
                link_up,
            },
            tls_client_config,
            identity,
        )));
        self.is_replica.store(true, Ordering::Relaxed);
    }

    /// Config-driven equivalent of the "if `replicaof` is set, auto-connect" startup wiring
    /// `main.rs` runs on every launch: derives the AUTH tuple via `config::replicaof_auth` and
    /// calls `start_replicating_with_auth`, the same way `main.rs` does. A no-op when
    /// `config.replicaof` is unset -- standalone startup takes no replication action at all.
    /// `main.rs` calls this after `config::validate_replicaof`; tests drive it with a hand-built
    /// `Config` instead of hand-building `start_replicating_with_auth`'s arguments directly, so
    /// they exercise the actual `Config` -> connection mapping, not just the connection itself.
    pub fn start_replicating_from_config(&self, config: &crate::config::Config) {
        if let Some(target) = &config.replicaof {
            let auth = crate::config::replicaof_auth(config);
            self.start_replicating_with_auth(target.clone(), auth);
        }
    }

    /// Cancels the running replication task (if any) and returns this node to normal,
    /// writable operation. Also bumps the generation so a stale task's in-flight poll can no
    /// longer apply state even when nothing new replaces it.
    pub fn stop_replicating(&self) {
        let mut task = self.follower_task.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = task.take() {
            old.abort();
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.is_replica.store(false, Ordering::Relaxed);
        *self.master_addr.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.link_up.store(false, Ordering::Relaxed);
    }

    /// Called once per accepted client connection. Bumps both the live gauge and the lifetime
    /// total; `connection_closed` is its Drop-guarded pair.
    pub fn connection_opened(&self) {
        self.connected_clients.fetch_add(1, Ordering::Relaxed);
        self.total_connections.fetch_add(1, Ordering::Relaxed);
    }
    pub fn connection_closed(&self) {
        self.connected_clients.fetch_sub(1, Ordering::Relaxed);
    }
    pub fn connected_clients(&self) -> usize {
        self.connected_clients.load(Ordering::Relaxed)
    }
    pub fn total_connections(&self) -> u64 {
        self.total_connections.load(Ordering::Relaxed)
    }
    pub fn command_executed(&self) {
        self.total_commands.fetch_add(1, Ordering::Relaxed);
    }
    pub fn total_commands(&self) -> u64 {
        self.total_commands.load(Ordering::Relaxed)
    }
    pub fn record_expired(&self, removed: usize) {
        if removed > 0 {
            self.expired_keys
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
    }
    pub fn expired_keys(&self) -> u64 {
        self.expired_keys.load(Ordering::Relaxed)
    }
    pub fn last_apply_unix(&self) -> i64 {
        self.last_apply_unix.load(Ordering::Relaxed)
    }
    /// The shared slot itself, for the spawned follower task to write into.
    pub fn last_apply_slot(&self) -> Arc<AtomicI64> {
        Arc::clone(&self.last_apply_unix)
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
    pub fn last_save_unix(&self) -> i64 {
        self.last_save_unix.load(Ordering::Relaxed)
    }
    /// Called by `handle_save` after a snapshot has landed on disk.
    pub fn record_save(&self) {
        self.last_save_unix
            .store(unix_now_secs(), Ordering::Relaxed);
    }
    pub fn master_addr(&self) -> Option<String> {
        self.master_addr
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn link_up(&self) -> bool {
        self.link_up.load(Ordering::Relaxed)
    }
    /// The shared flag itself, for the spawned follower task to write into.
    pub fn link_up_slot(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.link_up)
    }

    /// Leader side: total replication-stream bytes this node has produced since process start.
    /// Surfaced as `INFO REPLICATION`'s `master_repl_offset` and the
    /// `rocket_mem_master_repl_offset` gauge.
    pub fn master_repl_offset(&self) -> u64 {
        self.master_repl_offset.load(Ordering::Relaxed)
    }

    /// Adds `bytes` to the leader's replication offset and returns the new value. Called once
    /// per broadcast frame from `dispatch_and_log_inner`, while it still holds the AOF ordering
    /// guard, so the offset advances in the same order the frames are fanned out. `Relaxed` is
    /// enough: that guard already provides the mutual exclusion, and nothing orders other memory
    /// against this counter.
    pub fn advance_master_repl_offset(&self, bytes: u64) -> u64 {
        self.master_repl_offset.fetch_add(bytes, Ordering::Relaxed) + bytes
    }
}

/// An idle handle: no replicas registered, not a replica, no follower task running, its own
/// throwaway `Engine`, and the `./dump.snapshot` default path relative to the process's
/// current directory. Exists only so `dispatch_and_log`'s and `serve`'s existing tests stay
/// one-liners. Any test that actually exercises `SAVE` or `REPLICAOF` must use `new` instead
/// with an explicit `tempfile::tempdir()` path — see this plan's Global Constraints.
impl Default for ReplicationHandle {
    fn default() -> Self {
        Self::new(Arc::new(Engine::new()), PathBuf::from("./dump.snapshot"))
    }
}

/// Pairs the shared generation counter with the value a task was started with -- every caller
/// down this call chain needs both together to detect it has been superseded (see
/// `ReplicationHandle::generation`'s doc comment), and bundling them here is what keeps
/// `replication_client_loop`/`connect_and_sync` under clippy's argument-count limit.
struct Generation {
    counter: Arc<AtomicU64>,
    mine: u64,
}

impl Generation {
    fn is_stale(&self) -> bool {
        self.counter.load(Ordering::SeqCst) != self.mine
    }
}

/// Bundles the two follower-status slots `sync_once`'s apply loop keeps live -- every caller
/// down this call chain needs both together, and bundling them here is what keeps
/// `replication_client_loop`/`connect_and_sync`/`sync_once` under clippy's argument-count limit,
/// the same reasoning as `Generation` above.
struct FollowerStatus<'a> {
    last_apply: &'a AtomicI64,
    link_up: &'a AtomicBool,
}

/// Owned counterpart of `FollowerStatus`, held across the whole `replication_client_loop`
/// (which needs its own `Arc`s, being `'static`) -- bundled for the same argument-count reason.
struct FollowerHandles {
    last_apply: Arc<AtomicI64>,
    link_up: Arc<AtomicBool>,
}

/// What this follower tells the leader about itself when it `PSYNC`s: the address it
/// advertises (see `ReplicationHandle::own_addr`'s doc comment) and, when the leader has ACL
/// users configured, `AUTH` credentials -- `(username, password)`. Without this, `PSYNC` against
/// an ACL-protected leader is rejected with `NOAUTH` and replication can never complete (see
/// `sync_once`'s handling of a RESP error reply). Bundled into one struct, rather than two more
/// parameters, so `replication_client_loop`/`connect_and_sync`/`sync_once` stay under clippy's
/// argument-count limit -- the same reasoning as `Generation`/`FollowerStatus` above.
#[derive(Clone, Default)]
struct FollowerIdentity {
    own_addr: Option<String>,
    auth: Option<(String, String)>,
}

/// Connects to `host_port`, syncs, applies the leader's stream forever, and reconnects (after
/// a fixed ~1s backoff) on any failure — including the leader simply closing the connection.
/// There is no distinction between "first sync" and "resync after disconnect": both run this
/// same loop body. `generation` lets this task detect it has been superseded by a later
/// `start_replicating`/`stop_replicating` call and stop applying state — see
/// `ReplicationHandle::generation`'s doc comment.
async fn replication_client_loop(
    host_port: String,
    engine: Arc<Engine>,
    generation: Generation,
    aof: Option<Arc<AofWriter>>,
    handles: FollowerHandles,
    tls_client_config: Option<Arc<rustls::ClientConfig>>,
    identity: FollowerIdentity,
) {
    loop {
        if generation.is_stale() {
            return; // superseded before even starting this iteration's sync
        }
        let status = FollowerStatus {
            last_apply: &handles.last_apply,
            link_up: &handles.link_up,
        };
        match connect_and_sync(
            &host_port,
            &engine,
            &generation,
            aof.as_deref(),
            status,
            tls_client_config.as_ref(),
            &identity,
        )
        .await
        {
            Ok(()) => tracing::warn!(%host_port, "replication connection closed, reconnecting"),
            Err(e) => {
                tracing::warn!(%host_port, error = %e, "replication connection lost, reconnecting")
            }
        }
        handles.link_up.store(false, Ordering::Relaxed);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Connects to `host_port`, optionally upgrading to TLS (pinned to `tls_client_config`'s one
/// trusted certificate) when the follower was configured for it, then runs `sync_once` over
/// whichever stream type resulted. Two monomorphizations of the generic `sync_once` rather than
/// a boxed trait object, matching this codebase's existing avoidance of dynamic dispatch on the
/// hot connection-setup path.
async fn connect_and_sync(
    host_port: &str,
    engine: &Engine,
    generation: &Generation,
    aof: Option<&AofWriter>,
    status: FollowerStatus<'_>,
    tls_client_config: Option<&Arc<rustls::ClientConfig>>,
    identity: &FollowerIdentity,
) -> std::io::Result<()> {
    let tcp = tokio::net::TcpStream::connect(host_port).await?;
    match tls_client_config {
        Some(config) => {
            let host = host_port.rsplit_once(':').map_or(host_port, |(h, _)| h);
            let server_name =
                rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())
                })?;
            let tls_stream = tokio_rustls::TlsConnector::from(Arc::clone(config))
                .connect(server_name, tcp)
                .await?;
            sync_once(
                tls_stream,
                engine,
                &generation.counter,
                generation.mine,
                aof,
                status,
                identity,
            )
            .await
        }
        None => {
            sync_once(
                tcp,
                engine,
                &generation.counter,
                generation.mine,
                aof,
                status,
                identity,
            )
            .await
        }
    }
}

/// One full sync: `PSYNC`, load the snapshot, then apply every subsequent frame until the
/// connection ends (cleanly or with an error). Never called `dispatch_and_log` — see this
/// plan's Global Constraints. Checks `generation` against `my_generation` immediately before
/// `load_snapshot` and before each `dispatch` call, bailing out the moment this task has been
/// superseded rather than after a whole `sync_once` call — see `ReplicationHandle::generation`'s
/// doc comment for why `abort()` alone isn't sufficient. Generic over the stream type so the
/// same body serves both plaintext and TLS-upgraded replication connections (`connect_and_sync`
/// above), mirroring `connection::handle_connection`'s existing genericization for the
/// server-accept side.
async fn sync_once<S>(
    stream: S,
    engine: &Engine,
    generation: &AtomicU64,
    my_generation: u64,
    aof: Option<&AofWriter>,
    status: FollowerStatus<'_>,
    identity: &FollowerIdentity,
) -> std::io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut framed = tokio_util::codec::Framed::new(stream, protocol::codec::RespCodec::default());

    // Authenticate first, when configured -- an ACL-protected leader rejects an unauthenticated
    // PSYNC with NOAUTH (handled below as a RESP error reply), so replication can never
    // otherwise complete against one. AUTH's reply is a normal RESP frame (unlike PSYNC's, which
    // is a raw length-prefixed blob), so it round-trips through the codec's ordinary
    // send/next -- no need for the raw-socket handling PSYNC's reply requires.
    if let Some((username, password)) = &identity.auth {
        framed
            .send(protocol::Frame::Array(vec![
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"AUTH")),
                protocol::Frame::Bulk(bytes::Bytes::copy_from_slice(username.as_bytes())),
                protocol::Frame::Bulk(bytes::Bytes::copy_from_slice(password.as_bytes())),
            ]))
            .await?;
        match framed.next().await {
            Some(Ok(protocol::Frame::Error(e))) => {
                return Err(std::io::Error::other(format!("leader rejected AUTH: {e}")))
            }
            Some(Ok(_)) => {} // +OK -- proceed to PSYNC
            Some(Err(e)) => return Err(e),
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "leader closed the connection during AUTH",
                ))
            }
        }
    }

    // Advertising `own_addr` (when configured) is what lets the leader's `INFO REPLICATION`
    // report a `slaveN:ip=...,port=...` line naming an address an operator can actually
    // reconnect to -- the connection's own peer address is an ephemeral source port, not this
    // node's listening port, so the leader has no way to learn it otherwise. A bare `PSYNC`
    // (`own_addr: None`) matches every pre-this-feature test and deployment.
    let psync_frame = match &identity.own_addr {
        Some(addr) => protocol::Frame::Array(vec![
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"PSYNC")),
            protocol::Frame::Bulk(bytes::Bytes::copy_from_slice(addr.as_bytes())),
        ]),
        None => protocol::Frame::Array(vec![protocol::Frame::Bulk(bytes::Bytes::from_static(
            b"PSYNC",
        ))]),
    };
    framed.send(psync_frame).await?;

    // Reclaim the raw socket to read the length-prefixed snapshot blob, which is NOT a RESP
    // frame — decoding it through RespCodec would desync the stream entirely. `read_buf` is
    // guaranteed empty here: this Framed has never had `next()`/`decode` called on it, only
    // `send()`, so nothing has been read from the socket yet on the codec side.
    let mut parts = framed.into_parts();
    use tokio::io::AsyncReadExt;

    // The leader can reject PSYNC outright (e.g. NOAUTH, when it has ACL users configured and
    // this connection never authenticated) with a plain RESP error line instead of the raw
    // length prefix expected next. A `-` first byte is never a valid length-prefix byte in
    // practice (it would require an implausible ~2^56-magnitude blob), so it reliably
    // distinguishes the two cases. Without this check, the error line's bytes get read as a
    // little-endian u64 length and the following `vec![0u8; len]` aborts the process.
    let mut first_byte = [0u8; 1];
    parts.io.read_exact(&mut first_byte).await?;
    if first_byte[0] == b'-' {
        let mut line = Vec::new();
        loop {
            let mut b = [0u8; 1];
            parts.io.read_exact(&mut b).await?;
            if b[0] == b'\n' {
                break;
            }
            if b[0] != b'\r' {
                line.push(b[0]);
            }
        }
        return Err(std::io::Error::other(format!(
            "leader rejected PSYNC: {}",
            String::from_utf8_lossy(&line)
        )));
    }
    let mut len_buf = [0u8; 8];
    len_buf[0] = first_byte[0];
    parts.io.read_exact(&mut len_buf[1..]).await?;
    let len = u64::from_le_bytes(len_buf) as usize;
    let mut blob = vec![0u8; len];
    parts.io.read_exact(&mut blob).await?;

    if generation.load(Ordering::SeqCst) != my_generation {
        return Ok(()); // superseded while reading the blob -- do not clobber the newer task's state
    }
    engine
        .load_snapshot(&blob)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    status.link_up.store(true, Ordering::Relaxed);

    // From here on the leader sends plain RESP frames, byte-for-byte what its own AOF
    // received — rebuild a Framed over the same socket (whose read position is exactly past
    // the blob) to resume decoding normally.
    let mut framed = tokio_util::codec::Framed::from_parts(parts);
    while let Some(result) = framed.next().await {
        if generation.load(Ordering::SeqCst) != my_generation {
            return Ok(()); // superseded -- stop applying frames to state a newer task now owns
        }
        let frame = result?;
        let mut protocol = protocol::codec::Protocol::default();
        // Mutual exclusion with a concurrent SAVE on this same node: SAVE's shard-by-shard
        // snapshot walk (Store::snapshot_entries) must not observe a multi-key replicated
        // command (MSET, RENAME, SINTERSTORE, ...) half-applied across shards. Holding the same
        // lock_for_ordering() handle_save already takes closes that race. Deliberately wraps
        // only the dispatch call — not the framed.next() await, not the generation check —
        // matching handle_save's own pattern of holding the lock across the mutating work and
        // nothing else. None when this node has no AofWriter configured (test-only handles),
        // which matches the pre-fix behavior for those.
        let _order_guard = aof.map(|a| a.lock_all_shards());
        let reply = crate::dispatcher::dispatch(engine, frame, &mut protocol, 0);
        // A leader only ever fans out a command whose local execution already succeeded, so
        // an error applying it here means the two sides have genuinely diverged (a bug, or
        // version skew) — logged and skipped, not a reason to tear down and resync, which
        // would just reproduce the same error against the same divergence.
        if let protocol::Frame::Error(e) = reply {
            tracing::error!(error = %e, "failed to apply replicated command");
        }
        status.last_apply.store(unix_now_secs(), Ordering::Relaxed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::Engine;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    #[test]
    fn new_starts_as_not_a_replica() {
        let h = ReplicationHandle::new(Arc::new(Engine::new()), "/tmp/does-not-matter".into());
        assert!(!h.is_replica.load(Ordering::Relaxed));
    }

    #[test]
    fn engine_and_snapshot_path_return_what_new_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.snapshot");
        let engine = Arc::new(Engine::new());
        engine.set(
            bytes::Bytes::from_static(b"k"),
            engine::Value::String(bytes::Bytes::from_static(b"v")),
        );
        let h = ReplicationHandle::new(Arc::clone(&engine), path.clone());
        assert_eq!(h.snapshot_path(), path.as_path());
        assert_eq!(
            h.engine().get(b"k"),
            Some(engine::Value::String(bytes::Bytes::from_static(b"v")))
        );
    }

    #[test]
    fn default_is_idle_with_no_replicas_and_is_not_a_replica() {
        let h = ReplicationHandle::default();
        assert!(!h.is_replica.load(Ordering::Relaxed));
    }

    #[test]
    fn broadcast_delivers_to_every_registered_sender() {
        let registry = ReplicaRegistry::default();
        let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.register(None, tx1);
        registry.register(None, tx2);

        registry.broadcast(bytes::Bytes::from_static(b"hello"));

        assert_eq!(rx1.try_recv().unwrap().as_ref(), b"hello");
        assert_eq!(rx2.try_recv().unwrap().as_ref(), b"hello");
    }

    #[test]
    fn broadcast_prunes_a_sender_whose_receiver_was_dropped() {
        let registry = ReplicaRegistry::default();
        let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
        drop(rx1); // simulate a dead replica connection
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.register(None, tx1);
        registry.register(None, tx2);

        registry.broadcast(bytes::Bytes::from_static(b"a"));
        registry.broadcast(bytes::Bytes::from_static(b"b")); // the dead sender must be pruned by now

        // rx2 saw both broadcasts; nothing panicked or errored over rx1's drop
        assert_eq!(rx2.try_recv().unwrap().as_ref(), b"a");
        assert_eq!(rx2.try_recv().unwrap().as_ref(), b"b");
    }

    #[test]
    fn broadcast_with_no_registered_replicas_does_nothing() {
        let registry = ReplicaRegistry::default();
        registry.broadcast(bytes::Bytes::from_static(b"hello")); // must not panic
    }

    #[test]
    fn addrs_returns_registered_addresses_in_registration_order() {
        let registry = ReplicaRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.register(Some("127.0.0.1:6480".to_string()), tx1);
        registry.register(None, tx2); // a bare PSYNC advertised no address

        assert_eq!(
            registry.addrs(),
            vec![Some("127.0.0.1:6480".to_string()), None]
        );
    }

    #[test]
    fn uptime_starts_at_zero_and_never_goes_backwards() {
        let h = ReplicationHandle::default();
        let first = h.uptime_secs();
        assert!(
            first < 2,
            "a just-built handle should report ~0s, got {first}"
        );
        assert!(h.uptime_secs() >= first);
    }

    #[test]
    fn last_save_unix_is_zero_until_a_save_records_one() {
        let h = ReplicationHandle::default();
        assert_eq!(h.last_save_unix(), 0);
        h.record_save();
        assert!(
            h.last_save_unix() > 1_700_000_000,
            "record_save should store a real unix timestamp, got {}",
            h.last_save_unix()
        );
    }

    #[test]
    fn master_repl_offset_starts_at_zero_and_accumulates_byte_counts() {
        let h = ReplicationHandle::default();
        assert_eq!(h.master_repl_offset(), 0);
        assert_eq!(h.advance_master_repl_offset(31), 31);
        assert_eq!(h.advance_master_repl_offset(11), 42);
        assert_eq!(h.master_repl_offset(), 42);
        // A zero-length advance is a no-op, not an error: an empty encode never reaches the
        // fan-out, but the counter must not care either way.
        assert_eq!(h.advance_master_repl_offset(0), 42);
    }

    #[tokio::test]
    async fn master_addr_and_link_up_follow_the_replica_role() {
        let h = ReplicationHandle::default();
        assert_eq!(h.master_addr(), None);
        assert!(!h.link_up());

        h.start_replicating("127.0.0.1:1".to_string()); // nothing is listening; that's fine
        assert_eq!(h.master_addr(), Some("127.0.0.1:1".to_string()));

        h.stop_replicating();
        assert_eq!(h.master_addr(), None);
        assert!(!h.link_up());
    }

    #[tokio::test]
    async fn sync_once_loads_the_snapshot_then_applies_streamed_frames() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // consume the PSYNC frame the follower sends: `*1\r\n$5\r\nPSYNC\r\n` is exactly 15 bytes
            let mut psync_bytes = [0u8; 15];
            socket.read_exact(&mut psync_bytes).await.unwrap();

            let snapshot_engine = engine::Engine::new();
            snapshot_engine.set(
                bytes::Bytes::from_static(b"from-snapshot"),
                engine::Value::String(bytes::Bytes::from_static(b"v")),
            );
            let blob = snapshot_engine.snapshot(0);
            socket
                .write_all(&(blob.len() as u64).to_le_bytes())
                .await
                .unwrap();
            socket.write_all(&blob).await.unwrap();

            socket
                .write_all(b"*3\r\n$3\r\nSET\r\n$11\r\nfrom-stream\r\n$1\r\nv\r\n")
                .await
                .unwrap();
            // keep the socket open long enough for the follower to read and apply that frame
            // before this task (and its socket) drops
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let engine = std::sync::Arc::new(engine::Engine::new());
        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
        let sync_task = {
            let engine = std::sync::Arc::clone(&engine);
            let generation = Arc::clone(&generation);
            tokio::spawn(async move {
                let stream = tokio::net::TcpStream::connect(&host_port).await.unwrap();
                sync_once(
                    stream,
                    &engine,
                    &generation,
                    0,
                    None,
                    FollowerStatus {
                        last_apply: &AtomicI64::new(0),
                        link_up: &AtomicBool::new(false),
                    },
                    &FollowerIdentity::default(),
                )
                .await
            })
        };

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        sync_task.abort();
        fake_leader.await.unwrap();

        assert_eq!(
            engine.get(b"from-snapshot"),
            Some(engine::Value::String(bytes::Bytes::from_static(b"v")))
        );
        assert_eq!(
            engine.get(b"from-stream"),
            Some(engine::Value::String(bytes::Bytes::from_static(b"v")))
        );
    }

    #[tokio::test]
    async fn sync_once_does_not_load_the_snapshot_when_its_generation_is_already_stale() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut psync_bytes = [0u8; 15];
            socket.read_exact(&mut psync_bytes).await.unwrap();

            let snapshot_engine = engine::Engine::new();
            snapshot_engine.set(
                bytes::Bytes::from_static(b"from-snapshot"),
                engine::Value::String(bytes::Bytes::from_static(b"v")),
            );
            let blob = snapshot_engine.snapshot(0);
            socket
                .write_all(&(blob.len() as u64).to_le_bytes())
                .await
                .unwrap();
            socket.write_all(&blob).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });

        let engine = engine::Engine::new();
        let host_port = addr.to_string();
        // The shared counter is already ahead of the generation this call is running as --
        // simulating a task that has been superseded by a newer start_replicating/
        // stop_replicating call before it even finished reading the blob. Without the
        // generation check, this would go on to call load_snapshot and clobber whatever a
        // newer task has already loaded.
        let generation = Arc::new(AtomicU64::new(1));
        let stream = tokio::net::TcpStream::connect(&host_port).await.unwrap();
        sync_once(
            stream,
            &engine,
            &generation,
            0,
            None,
            FollowerStatus {
                last_apply: &AtomicI64::new(0),
                link_up: &AtomicBool::new(false),
            },
            &FollowerIdentity::default(),
        )
        .await
        .unwrap();
        fake_leader.await.unwrap();

        assert_eq!(engine.get(b"from-snapshot"), None); // stale task must not load its snapshot
    }

    #[tokio::test]
    async fn sync_once_returns_an_error_instead_of_crashing_on_a_resp_error_reply() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Simulates an ACL-protected leader rejecting the follower's unauthenticated PSYNC:
        // a plain RESP error frame, not the raw 8-byte length prefix `sync_once` otherwise
        // expects next. Before this fix, the first 8 bytes of that error text were read as a
        // little-endian u64 length ("-NOAUTH " -> 2326201932681530925) and the subsequent
        // `vec![0u8; len]` aborted the whole process with an allocation failure.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut psync_bytes = [0u8; 15];
            socket.read_exact(&mut psync_bytes).await.unwrap();
            socket
                .write_all(b"-NOAUTH Authentication required.\r\n")
                .await
                .unwrap();
        });

        let engine = engine::Engine::new();
        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
        let stream = tokio::net::TcpStream::connect(&host_port).await.unwrap();

        let result = sync_once(
            stream,
            &engine,
            &generation,
            0,
            None,
            FollowerStatus {
                last_apply: &AtomicI64::new(0),
                link_up: &AtomicBool::new(false),
            },
            &FollowerIdentity::default(),
        )
        .await;

        fake_leader.await.unwrap();
        assert!(
            result.is_err(),
            "sync_once must reject a non-length-prefixed reply instead of misreading it as a blob length"
        );
    }

    #[tokio::test]
    async fn sync_once_advertises_its_own_address_in_the_psync_frame_when_configured() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut framed =
                tokio_util::codec::Framed::new(socket, protocol::codec::RespCodec::default());
            let frame = framed.next().await.unwrap().unwrap();
            assert_eq!(
                frame,
                protocol::Frame::Array(vec![
                    protocol::Frame::Bulk(bytes::Bytes::from_static(b"PSYNC")),
                    protocol::Frame::Bulk(bytes::Bytes::from_static(b"127.0.0.1:6479")),
                ])
            );
            // Reject it so `sync_once` returns quickly instead of hanging on a length prefix
            // that will never come -- this test only cares about the outgoing PSYNC frame.
            framed
                .send(protocol::Frame::Error("ERR test stub".into()))
                .await
                .unwrap();
        });

        let engine = engine::Engine::new();
        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
        let stream = tokio::net::TcpStream::connect(&host_port).await.unwrap();

        let _ = sync_once(
            stream,
            &engine,
            &generation,
            0,
            None,
            FollowerStatus {
                last_apply: &AtomicI64::new(0),
                link_up: &AtomicBool::new(false),
            },
            &FollowerIdentity {
                own_addr: Some("127.0.0.1:6479".to_string()),
                auth: None,
            },
        )
        .await;

        fake_leader.await.unwrap();
    }

    #[tokio::test]
    async fn sync_once_authenticates_before_psync_when_credentials_are_configured() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut framed =
                tokio_util::codec::Framed::new(socket, protocol::codec::RespCodec::default());
            let auth_frame = framed.next().await.unwrap().unwrap();
            assert_eq!(
                auth_frame,
                protocol::Frame::Array(vec![
                    protocol::Frame::Bulk(bytes::Bytes::from_static(b"AUTH")),
                    protocol::Frame::Bulk(bytes::Bytes::from_static(b"app")),
                    protocol::Frame::Bulk(bytes::Bytes::from_static(b"changeme")),
                ]),
                "AUTH must be sent, with the configured credentials, before PSYNC"
            );
            framed
                .send(protocol::Frame::Simple("OK".into()))
                .await
                .unwrap();

            let psync_frame = framed.next().await.unwrap().unwrap();
            assert_eq!(
                psync_frame,
                protocol::Frame::Array(vec![protocol::Frame::Bulk(bytes::Bytes::from_static(
                    b"PSYNC"
                ))]),
                "PSYNC must follow a successful AUTH"
            );
            // Reject it so sync_once returns quickly -- this test only cares about the AUTH and
            // PSYNC frames sent, not a full snapshot handshake.
            framed
                .send(protocol::Frame::Error("ERR test stub".into()))
                .await
                .unwrap();
        });

        let engine = engine::Engine::new();
        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
        let stream = tokio::net::TcpStream::connect(&host_port).await.unwrap();

        let _ = sync_once(
            stream,
            &engine,
            &generation,
            0,
            None,
            FollowerStatus {
                last_apply: &AtomicI64::new(0),
                link_up: &AtomicBool::new(false),
            },
            &FollowerIdentity {
                own_addr: None,
                auth: Some(("app".to_string(), "changeme".to_string())),
            },
        )
        .await;

        fake_leader.await.unwrap();
    }

    #[tokio::test]
    async fn sync_once_fails_without_sending_psync_when_auth_is_rejected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut framed =
                tokio_util::codec::Framed::new(socket, protocol::codec::RespCodec::default());
            let _auth_frame = framed.next().await.unwrap().unwrap();
            framed
                .send(protocol::Frame::Error(
                    "WRONGPASS invalid username-password pair or user is disabled.".into(),
                ))
                .await
                .unwrap();
        });

        let engine = engine::Engine::new();
        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
        let stream = tokio::net::TcpStream::connect(&host_port).await.unwrap();

        let result = sync_once(
            stream,
            &engine,
            &generation,
            0,
            None,
            FollowerStatus {
                last_apply: &AtomicI64::new(0),
                link_up: &AtomicBool::new(false),
            },
            &FollowerIdentity {
                own_addr: None,
                auth: Some(("app".to_string(), "wrong".to_string())),
            },
        )
        .await;

        fake_leader.await.unwrap();
        assert!(
            result.is_err(),
            "sync_once must fail when the leader rejects AUTH, not proceed to PSYNC anyway"
        );
    }

    /// Proves the apply loop's `lock_for_ordering()` guard is load-bearing, not decorative.
    /// `SAVE` is explicitly allowed on a follower, but its `Store::snapshot_entries` walk locks
    /// the 16 shards one at a time, while a replicated multi-key command mutates several shards
    /// with no single lock spanning the whole thing — so without the guard a `SAVE` can capture
    /// a half-applied `MSET` and write a torn snapshot that `aof::recover` would later load
    /// without complaint. `MSET` over 16 keys is the probe because `commands::string::mset`
    /// takes and releases one shard write lock *per key*, making the window wide enough to hit
    /// reliably; every key always carries the same value, so any snapshot in which two of them
    /// disagree is proof of a torn observation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_save_racing_the_apply_loop_never_observes_a_half_applied_multi_key_write() {
        use std::sync::atomic::AtomicBool;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const KEYS: usize = 16;
        const SAVES: usize = 500;
        let keys: Vec<bytes::Bytes> = (0..KEYS)
            .map(|k| bytes::Bytes::from(format!("k{k}")))
            .collect();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));

        // A leader that first hands over a snapshot with every key at "0", then streams
        // `MSET k0 <i> k1 <i> ... k15 <i>` for an ever-increasing `i` until told to stop. The
        // socket's own backpressure paces it to whatever the follower can apply.
        let fake_leader = {
            let stop = Arc::clone(&stop);
            let keys = keys.clone();
            tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut psync_bytes = [0u8; 15];
                socket.read_exact(&mut psync_bytes).await.unwrap();

                let seed = engine::Engine::new();
                for key in &keys {
                    seed.set(
                        key.clone(),
                        engine::Value::String(bytes::Bytes::from_static(b"0")),
                    );
                }
                let blob = seed.snapshot(0);
                socket
                    .write_all(&(blob.len() as u64).to_le_bytes())
                    .await
                    .unwrap();
                socket.write_all(&blob).await.unwrap();

                let mut i: u64 = 1;
                while !stop.load(Ordering::Relaxed) {
                    let mut parts = vec![protocol::Frame::Bulk(bytes::Bytes::from_static(b"MSET"))];
                    for key in &keys {
                        parts.push(protocol::Frame::Bulk(key.clone()));
                        parts.push(protocol::Frame::Bulk(bytes::Bytes::from(i.to_string())));
                    }
                    let encoded = crate::aof::encode_frame(&protocol::Frame::Array(parts)).unwrap();
                    if socket.write_all(&encoded).await.is_err() {
                        return; // the follower went away; nothing left to stream to
                    }
                    i += 1;
                }
            })
        };

        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(engine::Engine::new());
        let aof = Arc::new(
            crate::aof::AofWriter::open(
                &dir.path().join("test.aof"),
                crate::aof::FsyncPolicy::Never,
            )
            .unwrap(),
        );
        let snapshot_path = dir.path().join("test.snapshot");
        let replication = Arc::new(
            ReplicationHandle::new(Arc::clone(&engine), snapshot_path.clone())
                .with_aof(Arc::clone(&aof)),
        );
        replication.is_replica.store(true, Ordering::Relaxed);

        let sync_task = {
            let engine = Arc::clone(&engine);
            let aof = Arc::clone(&aof);
            let host_port = addr.to_string();
            let generation = Arc::new(AtomicU64::new(0));
            tokio::spawn(async move {
                let stream = tokio::net::TcpStream::connect(&host_port).await.unwrap();
                sync_once(
                    stream,
                    &engine,
                    &generation,
                    0,
                    Some(&aof),
                    FollowerStatus {
                        last_apply: &AtomicI64::new(0),
                        link_up: &AtomicBool::new(false),
                    },
                    &FollowerIdentity::default(),
                )
                .await
            })
        };

        // The SAVE loop runs on a blocking thread, not a runtime worker: `handle_save` fsyncs
        // and writes a file, and parking a worker on that would starve the very apply loop this
        // test needs to be racing against.
        let observations = {
            let engine = Arc::clone(&engine);
            let aof = Arc::clone(&aof);
            let replication = Arc::clone(&replication);
            let keys = keys.clone();
            let snapshot_path = snapshot_path.clone();
            let stop = Arc::clone(&stop);
            tokio::task::spawn_blocking(move || {
                // Wait for the first *replicated* MSET to land before sampling. Until then the
                // apply loop may still be inside `load_snapshot`, whose own clear-then-reinsert
                // is a separate (one-off, pre-stream) non-atomic window this test isn't about.
                while engine.get(b"k0")
                    == Some(engine::Value::String(bytes::Bytes::from_static(b"0")))
                {
                    std::thread::yield_now();
                }

                let mut observations = Vec::with_capacity(SAVES);
                for _ in 0..SAVES {
                    let reply = crate::dispatcher::dispatch_and_log(
                        &engine,
                        &aof,
                        &replication,
                        protocol::Frame::Array(vec![protocol::Frame::Bulk(
                            bytes::Bytes::from_static(b"SAVE"),
                        )]),
                        &crate::dispatcher::Session::new(),
                        1,
                    );
                    assert_eq!(reply, protocol::Frame::Simple("OK".into()));
                    // Read back what this iteration actually wrote before the next SAVE
                    // overwrites the same path, and reconstruct it the way recovery would.
                    let bytes = std::fs::read(&snapshot_path).unwrap();
                    let restored = engine::Engine::new();
                    restored.load_snapshot(&bytes).unwrap();
                    observations.push(
                        keys.iter()
                            .map(|k| match restored.get(k) {
                                Some(engine::Value::String(v)) => Some(v),
                                _ => None,
                            })
                            .collect::<Vec<_>>(),
                    );
                }
                stop.store(true, Ordering::Relaxed);
                observations
            })
            .await
            .unwrap()
        };

        sync_task.abort();
        fake_leader.abort();

        for (n, values) in observations.iter().enumerate() {
            let first = values[0].clone();
            assert!(
                values.iter().all(|v| *v == first),
                "snapshot {n} of {SAVES} is torn: an MSET was captured half-applied, {values:?}"
            );
        }
    }

    #[tokio::test]
    async fn start_replicating_sets_is_replica_and_stop_replicating_clears_it() {
        let handle = ReplicationHandle::new(
            std::sync::Arc::new(engine::Engine::new()),
            "/tmp/unused.snapshot".into(),
        );
        assert!(!handle.is_replica.load(std::sync::atomic::Ordering::Relaxed));

        // "127.0.0.1:1" is a real address nothing listens on — start_replicating doesn't wait
        // for the connection to succeed, so this returns immediately regardless
        handle.start_replicating("127.0.0.1:1".to_string());
        assert!(handle.is_replica.load(std::sync::atomic::Ordering::Relaxed));

        handle.stop_replicating();
        assert!(!handle.is_replica.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn start_replicating_twice_cancels_the_first_task_before_starting_the_second() {
        let handle = ReplicationHandle::new(
            std::sync::Arc::new(engine::Engine::new()),
            "/tmp/unused.snapshot".into(),
        );
        handle.start_replicating("127.0.0.1:1".to_string());
        handle.start_replicating("127.0.0.1:2".to_string()); // must not panic or leave two tasks running
        assert!(handle.is_replica.load(std::sync::atomic::Ordering::Relaxed));
        handle.stop_replicating();
    }

    #[test]
    fn registry_len_tracks_registered_replicas() {
        let registry = ReplicaRegistry::default();
        assert!(registry.is_empty());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register(None, tx);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn connection_counters_move_with_open_and_close() {
        let h = ReplicationHandle::default();
        assert_eq!(h.connected_clients(), 0);
        assert_eq!(h.total_connections(), 0);
        h.connection_opened();
        h.connection_opened();
        assert_eq!(h.connected_clients(), 2);
        assert_eq!(h.total_connections(), 2);
        h.connection_closed();
        assert_eq!(h.connected_clients(), 1);
        assert_eq!(h.total_connections(), 2); // total never goes down
    }

    #[test]
    fn command_and_expiry_counters_accumulate() {
        let h = ReplicationHandle::default();
        h.command_executed();
        h.command_executed();
        assert_eq!(h.total_commands(), 2);
        h.record_expired(5);
        h.record_expired(0);
        h.record_expired(2);
        assert_eq!(h.expired_keys(), 7);
    }

    #[test]
    fn last_apply_unix_starts_at_zero_and_follows_the_shared_slot() {
        let h = ReplicationHandle::default();
        assert_eq!(h.last_apply_unix(), 0);
        h.last_apply_slot()
            .store(1_756_512_000, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(h.last_apply_unix(), 1_756_512_000);
    }

    #[test]
    fn a_handle_is_not_in_cluster_mode_by_default() {
        let h = ReplicationHandle::default();
        assert!(h.cluster().is_none());
    }

    #[test]
    fn a_new_handle_has_an_empty_acl_store() {
        let h = ReplicationHandle::default();
        assert!(h.acl.is_empty());
    }

    #[test]
    fn with_acl_bootstrap_populates_the_store() {
        let h = ReplicationHandle::new(Arc::new(Engine::new()), "/tmp/does-not-matter".into())
            .with_acl_bootstrap(vec![crate::acl::AclUser {
                username: "seed".to_string(),
                password_hash: None,
                enabled: true,
                rules: vec![
                    crate::acl::AclRule::AllCommands,
                    crate::acl::AclRule::AllKeys,
                ],
            }]);
        assert!(!h.acl.is_empty());
        assert!(h.acl.get_user("seed").unwrap().enabled);
    }

    #[test]
    fn with_cluster_puts_the_handle_into_cluster_mode() {
        let config = crate::cluster::ClusterConfig::parse(
            "shard-a 127.0.0.1:7001 0 8000\nshard-b 127.0.0.1:7002 8001 16383\n",
            "shard-b",
        )
        .unwrap();
        let h = ReplicationHandle::new(Arc::new(Engine::new()), "/tmp/does-not-matter".into())
            .with_cluster(Arc::new(config));
        let cluster = h.cluster().expect("cluster mode should be on");
        assert_eq!(cluster.myself().id, "shard-b");
        assert!(cluster.owns(8001));
        assert!(!cluster.owns(8000));
    }

    #[test]
    fn log_value_max_bytes_defaults_to_128() {
        // Every existing `ReplicationHandle::new`/`default()` call site -- ~25 of them, all
        // tests -- must keep working untouched, with the same cap `Config::default()` uses.
        assert_eq!(ReplicationHandle::default().log_value_max_bytes(), 128);
    }

    #[test]
    fn with_log_value_max_bytes_overrides_the_default() {
        let handle = ReplicationHandle::default().with_log_value_max_bytes(16);
        assert_eq!(handle.log_value_max_bytes(), 16);
    }

    #[test]
    fn with_log_value_max_bytes_saturates_an_absurd_config_value() {
        // The config field is a u64 and `fmt_value` takes a usize. On a 32-bit target a large
        // configured cap would otherwise truncate to a small one -- silently logging *less*
        // than asked. Saturate to usize::MAX instead: "no truncation" is the honest reading of
        // "cap larger than this machine can index".
        let handle = ReplicationHandle::default().with_log_value_max_bytes(u64::MAX);
        assert_eq!(handle.log_value_max_bytes(), usize::MAX);
    }
}
