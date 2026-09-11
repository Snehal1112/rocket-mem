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
///
/// `tls_client_config` mirrors replication's own TLS gating (`tls::load_client_config`, built
/// from this node's `tls_ca_path`): `None` dials `addr` in plaintext, `Some` wraps the connection
/// in TLS first. This must match what `addr` actually is -- a plaintext PING sent straight at a
/// TLS listener is not a valid TLS record, so the peer's handshake fails and this probe never
/// sees a reply, permanently misreporting a healthy peer as down.
async fn probe_once(
    addr: &str,
    timeout: Duration,
    tls_client_config: Option<&Arc<rustls::ClientConfig>>,
) -> bool {
    let probe = async {
        let tcp = tokio::net::TcpStream::connect(addr).await.ok()?;
        match tls_client_config {
            Some(config) => {
                let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
                let server_name = rustls::pki_types::ServerName::try_from(host.to_string()).ok()?;
                let tls = tokio_rustls::TlsConnector::from(Arc::clone(config))
                    .connect(server_name, tcp)
                    .await
                    .ok()?;
                probe_ping(tls).await
            }
            None => probe_ping(tcp).await,
        }
    };
    tokio::time::timeout(timeout, probe)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// The RESP argument every peer-liveness probe's `PING` carries, so the probed peer's
/// `connection.rs` can recognize this connection as this node's own internal health check
/// (never a real client) and log its accept/close pair at `debug` instead of `info`. The sender
/// (`probe_ping`, below) and the recognizer (`connection::is_probe_ping`) must agree on this
/// exact byte string — it is a log-verbosity signal only, never an authentication or security
/// boundary: a client that happens to send this by coincidence just gets a quieter log line for
/// that one connection, nothing more.
pub(crate) const PROBE_MARKER: &[u8] = b"__rocket_mem_peer_probe__";

/// Sends one `PING` on `socket` and reads the first bytes of a reply. `true` means something
/// answered. Generic over the stream type so `probe_once` can share this between its plaintext
/// and TLS branches without a boxed trait object -- see `replication::connect_and_sync`'s
/// `sync_once` for the same monomorphization-over-dynamic-dispatch choice in this codebase.
async fn probe_ping<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut socket: S,
) -> Option<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut request = Vec::with_capacity(24 + PROBE_MARKER.len());
    request.extend_from_slice(b"*2\r\n$4\r\nPING\r\n$");
    request.extend_from_slice(PROBE_MARKER.len().to_string().as_bytes());
    request.extend_from_slice(b"\r\n");
    request.extend_from_slice(PROBE_MARKER);
    request.extend_from_slice(b"\r\n");
    socket.write_all(&request).await.ok()?;
    let mut buf = [0u8; 64];
    let read = socket.read(&mut buf).await.ok()?;
    // `PING <msg>` replies with a Bulk string (`$...`), not the Simple-string `+PONG` a bare
    // PING gets — probe_ping now always sends the two-argument form, so `$` is the expected
    // success prefix. `+`/`-` stay accepted too, for robustness against a peer that doesn't
    // recognize rocket-mem's own marker at all (a plain RESP server, or a future protocol
    // change) and just answers with an ordinary PONG or an error — either still proves the peer
    // is alive and speaking RESP, which is all this check is for. A zero-length read is the peer
    // closing the connection, not answering it, hence the `read > 0` check.
    let answered = read > 0 && matches!(buf[0], b'+' | b'-' | b'$');
    // Best-effort graceful close. Over TLS this sends a `close_notify` alert before the socket
    // closes; dropping the stream without it leaves the peer's rustls session reading a bare TCP
    // EOF, which it reports as an error ("peer closed connection without sending TLS
    // close_notify") rather than a clean shutdown, once per probe. Harmless on the plaintext path
    // (just shuts down the write half), so this runs unconditionally for both branches.
    let _ = socket.shutdown().await;
    answered.then_some(())
}

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
    tls_client_config: Option<&Arc<rustls::ClientConfig>>,
    last_reported: &mut HashMap<String, bool>,
) -> Vec<(String, bool)> {
    let results = futures_util::future::join_all(peers.iter().map(|(id, addr)| async move {
        (
            id.as_str(),
            probe_once(addr, probe_timeout, tls_client_config).await,
        )
    }))
    .await;
    let mut changed = Vec::new();
    for (id, answered) in results {
        if answered {
            health.record_ok(id);
        }
        let reachable = health.is_reachable(id);
        let previously = last_reported
            .insert(id.to_string(), reachable)
            .unwrap_or(true);
        if reachable != previously {
            changed.push((id.to_string(), reachable));
        }
    }
    changed
}

/// Probes every peer in `cluster` forever, one round every `interval`, recording each success in
/// `health`.
///
/// Observational only. It writes nothing but timestamps: no promotion, no `cluster.conf` rewrite,
/// no routing change. A slot's configured owner stays its owner while it is dead, and
/// `cluster_redirect` keeps sending clients there -- see this module's doc comment.
///
/// `tls_client_config` is `Some` exactly when this node's `tls_ca_path` is set, matching
/// replication's own TLS gating -- see `probe_once`. Every peer is probed the same way: this
/// project's TLS story is one shared trust cert across the whole deployment, not a per-peer
/// setting, so there is no per-peer plaintext/TLS mix to account for.
pub async fn run_peer_prober(
    cluster: Arc<ClusterConfig>,
    health: Arc<PeerHealth>,
    interval: Duration,
    tls_client_config: Option<Arc<rustls::ClientConfig>>,
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
        for (id, reachable) in probe_round(
            &peers,
            &health,
            PROBE_TIMEOUT,
            tls_client_config.as_ref(),
            &mut last_reported,
        )
        .await
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

/// Builds the peer-health map for `cluster` and spawns the prober that keeps it current, then
/// returns the map so the caller can hand it to `ReplicationHandle::with_peer_health`.
///
/// Called only in cluster mode: a standalone node has no peers, so it gets no map at all and its
/// `CLUSTER` replies keep reporting exactly what they reported before this existed.
///
/// `tls_client_config` should be this node's own replication TLS client config (built from
/// `tls_ca_path`) when it has one -- see `probe_once`'s doc comment for why the prober must match
/// it rather than always dialing plaintext.
pub fn spawn_peer_prober(
    cluster: &Arc<ClusterConfig>,
    probe_interval: Duration,
    node_timeout: Duration,
    tls_client_config: Option<Arc<rustls::ClientConfig>>,
) -> Arc<PeerHealth> {
    let health = Arc::new(PeerHealth::for_cluster(cluster, node_timeout));
    tokio::spawn(run_peer_prober(
        Arc::clone(cluster),
        Arc::clone(&health),
        probe_interval,
        tls_client_config,
    ));
    health
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
        assert!(probe_once(&addr, Duration::from_secs(1), None).await);
    }

    #[tokio::test]
    async fn a_probe_of_an_address_nothing_listens_on_fails() {
        let addr = dead_addr().await;
        assert!(!probe_once(&addr, Duration::from_secs(1), None).await);
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
        assert!(!probe_once(&addr, Duration::from_millis(50), None).await);
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// A minimal TLS-wrapped responder: accepts a TLS handshake using the repo's self-signed test
    /// cert, then answers one `PING` per connection with `+PONG\r\n` over the encrypted stream.
    /// Returns a `host:port` string using the cert's `localhost` SAN, not the raw IP, since
    /// `ServerName` validation needs a name the certificate actually covers.
    async fn spawn_tls_ping_responder() -> (String, Arc<rustls::ClientConfig>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_config =
            crate::tls::load_server_config(&fixture("test-cert.pem"), &fixture("test-key.pem"))
                .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(server_config);
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    if let Ok(mut tls) = acceptor.accept(socket).await {
                        let mut buf = [0u8; 64];
                        if tls.read(&mut buf).await.unwrap_or(0) > 0 {
                            let _ = tls.write_all(b"+PONG\r\n").await;
                        }
                    }
                });
            }
        });
        let client_config = crate::tls::load_client_config(&fixture("test-cert.pem")).unwrap();
        (format!("localhost:{port}"), client_config)
    }

    #[tokio::test]
    async fn a_probe_without_a_tls_client_config_fails_against_a_tls_listener() {
        // Reproduces the bug: a plaintext PING sent straight at a TLS listener is not a valid TLS
        // record, so the peer's handshake fails and the prober never sees a reply.
        let (addr, _client_config) = spawn_tls_ping_responder().await;
        assert!(!probe_once(&addr, Duration::from_secs(1), None).await);
    }

    #[tokio::test]
    async fn a_probe_with_a_matching_tls_client_config_succeeds_against_a_tls_listener() {
        let (addr, client_config) = spawn_tls_ping_responder().await;
        assert!(probe_once(&addr, Duration::from_secs(1), Some(&client_config)).await);
    }

    /// Like `spawn_tls_ping_responder`, but after answering the `PING` it attempts one more read
    /// and reports over `tx` whether that read saw a clean EOF (`Ok(0)`) or an error. A client that
    /// drops its `TlsStream` without an explicit shutdown never sends a TLS `close_notify` alert,
    /// which rustls surfaces on this side as an error rather than a graceful close -- exactly the
    /// "peer closed connection without sending TLS close_notify" warning this test guards against.
    async fn spawn_tls_ping_responder_reporting_shutdown() -> (
        String,
        Arc<rustls::ClientConfig>,
        tokio::sync::oneshot::Receiver<bool>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_config =
            crate::tls::load_server_config(&fixture("test-cert.pem"), &fixture("test-key.pem"))
                .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(server_config);
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Ok((socket, _)) = listener.accept().await {
                if let Ok(mut tls) = acceptor.accept(socket).await {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 64];
                    if tls.read(&mut buf).await.unwrap_or(0) > 0 {
                        let _ = tls.write_all(b"+PONG\r\n").await;
                    }
                    let clean_close = matches!(tls.read(&mut buf).await, Ok(0));
                    let _ = tx.send(clean_close);
                }
            }
        });
        let client_config = crate::tls::load_client_config(&fixture("test-cert.pem")).unwrap();
        (format!("localhost:{port}"), client_config, rx)
    }

    #[tokio::test]
    async fn a_tls_probe_closes_its_connection_with_a_close_notify() {
        let (addr, client_config, rx) = spawn_tls_ping_responder_reporting_shutdown().await;
        assert!(probe_once(&addr, Duration::from_secs(1), Some(&client_config)).await);
        assert!(
            rx.await.unwrap(),
            "prober must send a TLS close_notify on shutdown, not just drop the connection"
        );
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
            None,
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
            None,
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
        let health = spawn_peer_prober(
            &config,
            Duration::from_millis(20),
            Duration::from_secs(1),
            None,
        );
        health.set_last_ok_unix("peer", unix_now_secs() - 3600);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(health.is_reachable("peer"));
        assert_eq!(health.reachable_count(), 1);
        assert_eq!(health.unreachable_count(), 0);
    }

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
            None,
            &mut last_reported,
        )
        .await;
        assert_eq!(first, vec![("peer".to_string(), false)]);

        for round in 2..=4 {
            let later = probe_round(
                &peers,
                &health,
                Duration::from_millis(50),
                None,
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
            None,
            &mut last_reported,
        )
        .await;
        assert_eq!(first, vec![("peer".to_string(), true)]);

        let second = probe_round(
            &peers,
            &health,
            Duration::from_millis(500),
            None,
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
                None,
                &mut last_reported,
            )
            .await;
            assert!(changed.is_empty(), "round {round} reported a healthy peer");
        }
    }

    /// Like `spawn_ping_responder`, but captures the raw bytes the prober actually sent instead
    /// of blindly replying — this is what proves `probe_ping` sends the marker, not just that
    /// probing still works. Replies with a `PING`-style Bulk echo of whatever second argument it
    /// parsed out of the request, mirroring what a real rocket-mem peer's `PING <msg>` handling
    /// does, so the round trip stays representative.
    async fn spawn_capturing_responder() -> (String, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 128];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let _ = tx.send(buf[..n].to_vec());
                let _ = socket
                    .write_all(b"$25\r\n__rocket_mem_peer_probe__\r\n")
                    .await;
            }
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn probe_ping_sends_the_ping_marker_as_a_two_element_array() {
        let (addr, rx) = spawn_capturing_responder().await;
        assert!(probe_once(&addr, Duration::from_secs(1), None).await);
        let sent = rx.await.unwrap();
        let expected = b"*2\r\n$4\r\nPING\r\n$25\r\n__rocket_mem_peer_probe__\r\n";
        assert_eq!(
            sent,
            expected,
            "expected the exact PING <PROBE_MARKER> wire encoding, got: {}",
            String::from_utf8_lossy(&sent)
        );
    }

    #[test]
    fn probe_marker_is_the_length_the_wire_encoding_assumes() {
        // The test above hardcodes `$25\r\n` for the marker's RESP bulk-length prefix; this
        // guards that assumption so a future edit to PROBE_MARKER's text fails loudly here
        // instead of silently breaking the wire-format assertion above.
        assert_eq!(PROBE_MARKER.len(), 25);
    }
}
