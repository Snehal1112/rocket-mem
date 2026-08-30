use crate::aof::AofWriter;
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Holds one outbound channel per connected replica. `senders` is a plain `std::sync::Mutex`,
/// not `tokio::sync::Mutex`: every access is a quick, synchronous push/retain, never held
/// across an `.await`, so the lighter std lock is the right tool — matching `AofWriter::order`'s
/// existing choice for the same reason.
#[derive(Default)]
pub struct ReplicaRegistry {
    senders: std::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<bytes::Bytes>>>,
}

impl ReplicaRegistry {
    /// Registers a newly-synced replica's outbound channel. Called only from `serve_replica`
    /// (Task 4), while it still holds `AofWriter::lock_for_ordering()` — see this plan's
    /// Global Constraints for why registration must happen inside that same critical section
    /// as the snapshot walk, not after it.
    pub fn register(&self, sender: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>) {
        self.senders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(sender);
    }

    /// Fans `bytes` out to every registered replica, pruning any whose receiver has been
    /// dropped (the replica connection died). Never itself returns an error: a delivery
    /// failure to one dead replica must not affect delivery to the others, and must never
    /// roll back the write that already committed on the leader.
    pub fn broadcast(&self, bytes: bytes::Bytes) {
        let mut senders = self.senders.lock().unwrap_or_else(|e| e.into_inner());
        senders.retain(|tx| tx.send(bytes.clone()).is_ok());
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
            cluster: None,
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

    /// Puts this node into cluster mode with the given static topology. Only `main.rs` and
    /// `crates/server/tests/cluster.rs` call this; everything else leaves cluster mode off.
    pub fn with_cluster(mut self, cluster: Arc<crate::cluster::ClusterConfig>) -> Self {
        self.cluster = Some(cluster);
        self
    }

    /// `None` when cluster mode is off. `dispatch_and_log`'s redirection gate short-circuits on
    /// this before extracting any key, so a standalone node pays one `Option` check per command.
    pub fn cluster(&self) -> Option<&Arc<crate::cluster::ClusterConfig>> {
        self.cluster.as_ref()
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
        let mut task = self.follower_task.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = task.take() {
            old.abort();
        }
        let my_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let engine = Arc::clone(&self.engine);
        let generation = Arc::clone(&self.generation);
        let aof = self.aof.clone();
        *task = Some(tokio::spawn(replication_client_loop(
            host_port,
            engine,
            generation,
            my_generation,
            aof,
        )));
        self.is_replica.store(true, Ordering::Relaxed);
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

/// Connects to `host_port`, syncs, applies the leader's stream forever, and reconnects (after
/// a fixed ~1s backoff) on any failure — including the leader simply closing the connection.
/// There is no distinction between "first sync" and "resync after disconnect": both run this
/// same loop body. `generation`/`my_generation` let this task detect it has been superseded by
/// a later `start_replicating`/`stop_replicating` call and stop applying state — see
/// `ReplicationHandle::generation`'s doc comment.
async fn replication_client_loop(
    host_port: String,
    engine: Arc<Engine>,
    generation: Arc<AtomicU64>,
    my_generation: u64,
    aof: Option<Arc<AofWriter>>,
) {
    loop {
        if generation.load(Ordering::SeqCst) != my_generation {
            return; // superseded before even starting this iteration's sync
        }
        match sync_once(
            &host_port,
            &engine,
            &generation,
            my_generation,
            aof.as_deref(),
        )
        .await
        {
            Ok(()) => eprintln!("replication: connection to {host_port} closed; reconnecting"),
            Err(e) => eprintln!("replication: lost connection to {host_port}: {e}; reconnecting"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// One full sync: connect, `PSYNC`, load the snapshot, then apply every subsequent frame
/// until the connection ends (cleanly or with an error). Never called `dispatch_and_log` —
/// see this plan's Global Constraints. Checks `generation` against `my_generation` immediately
/// before `load_snapshot` and before each `dispatch` call, bailing out the moment this task has
/// been superseded rather than after a whole `sync_once` call — see
/// `ReplicationHandle::generation`'s doc comment for why `abort()` alone isn't sufficient.
async fn sync_once(
    host_port: &str,
    engine: &Engine,
    generation: &AtomicU64,
    my_generation: u64,
    aof: Option<&AofWriter>,
) -> std::io::Result<()> {
    let stream = tokio::net::TcpStream::connect(host_port).await?;
    let mut framed = tokio_util::codec::Framed::new(stream, protocol::codec::RespCodec::default());
    framed
        .send(protocol::Frame::Array(vec![protocol::Frame::Bulk(
            bytes::Bytes::from_static(b"PSYNC"),
        )]))
        .await?;

    // Reclaim the raw socket to read the length-prefixed snapshot blob, which is NOT a RESP
    // frame — decoding it through RespCodec would desync the stream entirely. `read_buf` is
    // guaranteed empty here: this Framed has never had `next()`/`decode` called on it, only
    // `send()`, so nothing has been read from the socket yet on the codec side.
    let mut parts = framed.into_parts();
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 8];
    parts.io.read_exact(&mut len_buf).await?;
    let len = u64::from_le_bytes(len_buf) as usize;
    let mut blob = vec![0u8; len];
    parts.io.read_exact(&mut blob).await?;

    if generation.load(Ordering::SeqCst) != my_generation {
        return Ok(()); // superseded while reading the blob -- do not clobber the newer task's state
    }
    engine
        .load_snapshot(&blob)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

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
        let _order_guard = aof.map(|a| a.lock_for_ordering());
        let reply = crate::dispatcher::dispatch(engine, frame, &mut protocol, 0);
        // A leader only ever fans out a command whose local execution already succeeded, so
        // an error applying it here means the two sides have genuinely diverged (a bug, or
        // version skew) — logged and skipped, not a reason to tear down and resync, which
        // would just reproduce the same error against the same divergence.
        if let protocol::Frame::Error(e) = reply {
            eprintln!("replication: applying a replicated command failed: {e}");
        }
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
        registry.register(tx1);
        registry.register(tx2);

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
        registry.register(tx1);
        registry.register(tx2);

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
            tokio::spawn(async move { sync_once(&host_port, &engine, &generation, 0, None).await })
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
        sync_once(&host_port, &engine, &generation, 0, None)
            .await
            .unwrap();
        fake_leader.await.unwrap();

        assert_eq!(engine.get(b"from-snapshot"), None); // stale task must not load its snapshot
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
            tokio::spawn(
                async move { sync_once(&host_port, &engine, &generation, 0, Some(&aof)).await },
            )
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
                        &mut protocol::codec::Protocol::default(),
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
    fn a_handle_is_not_in_cluster_mode_by_default() {
        let h = ReplicationHandle::default();
        assert!(h.cluster().is_none());
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
}
