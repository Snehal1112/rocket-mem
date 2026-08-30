use engine::Engine;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
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
    /// `REPLICAOF`/`REPLICAOF NO ONE` in `05-replicaof-and-follower-apply-loop.md`. Not used
    /// by this plan; present now so the struct's shape doesn't change again later.
    /// `#[allow(dead_code)]`: write-only until `05-replicaof-and-follower-apply-loop.md` reads
    /// it via `.lock()` in `start_replicating`/`stop_replicating` — same reasoning as the
    /// engine crate's `pub mod commands` staying public ahead of Sprint 2's dispatcher.
    #[allow(dead_code)]
    follower_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The engine this handle's leader-side `PSYNC` snapshots from and follower-side
    /// `replication_client_loop` applies into. An owned `Arc`, not a borrow: a spawned
    /// follower task is `'static` and cannot hold a borrow of `dispatch_and_log`'s own
    /// `engine: &Engine` parameter. Invariant, enforced only by convention (there is no way to
    /// assert it in the type system): this must be the *same* `Engine` `serve()` was handed.
    engine: Arc<Engine>,
    /// Where `SAVE` writes, from `ROCKET_MEM_SNAPSHOT_PATH`.
    snapshot_path: PathBuf,
}

impl ReplicationHandle {
    pub fn new(engine: Arc<Engine>, snapshot_path: PathBuf) -> Self {
        Self {
            registry: ReplicaRegistry::default(),
            is_replica: AtomicBool::new(false),
            follower_task: Mutex::new(None),
            engine,
            snapshot_path,
        }
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
}
