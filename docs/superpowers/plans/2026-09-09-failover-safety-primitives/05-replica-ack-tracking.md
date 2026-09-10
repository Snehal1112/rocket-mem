# Replica Ack Tracking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The leader learns, and reports, how caught up each connected replica is. `ReplicaRegistry` grows from a list of `(Option<String>, Sender)` tuples into a list of `Arc<ReplicaEntry>` carrying an acked offset and an ack timestamp; `serve_replica` parses the `REPLCONF ACK <offset>` frames plan 04 currently logs and drops, recording them on that replica's own entry; and `INFO REPLICATION`'s slave lines gain `offset=` and `lag=` fields. This is what turns "which replica is most caught up" from an unanswerable question into a number an operator can read.

**Architecture:** `register` returns the `Arc<ReplicaEntry>` it just pushed, so `serve_replica` updates acks with a plain relaxed atomic store — no id, no lookup, no registry lock on the ack path. `len()`, `addrs()`, `is_empty()` and `broadcast()` keep their exact current signatures and behavior because `INFO`, `main.rs`'s startup banner and `metrics::refresh_sampled_gauges` all call them. Two new read methods, `good_replicas(max_lag)` and `states()`, render without holding the lock afterwards. Nothing here promotes anything, rejects anything, or changes routing — reporting is the whole deliverable. `07-fencing-config.md` is what later consumes `good_replicas`.

**Tech Stack:** Rust, `std::sync::atomic`, `std::sync::Mutex`. No new dependencies.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md) — "Decision: v1 is not failover", step 1 ("Replication offsets ... echoed by each follower via a periodic `REPLCONF ACK <offset>`-equivalent frame; surfaced in `INFO`"), is authoritative for why this exists.

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) in full before writing any code.** It is normative: every name, type, and semantic decision below is fixed there. Where this plan and the contract disagree, the contract wins and the disagreement is a bug worth reporting before you write code. §2.3 (`REPLCONF ACK` on the existing socket), §2.4 (the `ReplicaEntry` shape, the registry's new methods, and the `INFO` key format) govern this plan directly.
- **`04-bidirectional-replica-connection.md` must be landed first.** This plan's Task 2 edits the `select!` loop that plan introduced. If `serve_replica` still calls `framed.into_parts()` and only writes, stop — plan 04 has not run.
- **`len()`, `addrs()`, `is_empty()` and `broadcast()` keep their exact signatures.** Their call sites are `crates/server/src/metrics.rs:56` (`registry.len()`), `crates/server/src/dispatcher.rs:1918` (`registry.addrs()`), `crates/server/src/main.rs:367` (`registry.addrs()`) and `crates/server/src/dispatcher.rs:3252` (`registry.broadcast(...)`). Task 3 moves the `dispatcher.rs:1918` caller onto `states()`; the other three must keep compiling untouched.
- **`register`'s new return value is additive.** Six existing call sites ignore it — `crates/server/src/connection.rs` (`serve_replica` plus one test), `crates/server/src/dispatcher.rs:5120`, `:5135`, `:5136`, `:7910`, `:7943`, `:7991`, `:8133`, and `crates/server/src/replication.rs`'s own registry tests. An ignored non-`#[must_use]` return value is not a warning, so none of them need editing. Do **not** add `#[must_use]` to `register`.
- **A replica that has never acked is not "good" and does not report `lag=0`.** `last_ack_unix == 0` means unknown. `INFO` reports `offset=0,lag=-1` for it and `good_replicas` never counts it. `-1` means "unknown", never "zero lag". A follower build that never sends an ack must keep working as a replica; it just has no ack information.
- **Do not weaken an existing test.** Extending the slave line breaks the `contains` assertion at `crates/server/src/dispatcher.rs:5142`. Task 3 updates it to the new correct format, in the same commit as the change that broke it.
- **No per-replica metric labels.** Replica addresses are unbounded-cardinality. Per-replica detail belongs in `INFO`; metrics stay aggregate. This plan adds no metric at all.
- **Timing tests are bounded polls with a deadline**, never a bare `sleep` + assert. Copy the shape of `wait_for` at `crates/server/tests/replication.rs:42-60`.
- **The three CI gates must be clean before every commit:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
  Clippy is strict (`-D warnings`) and lints test code too; a dead-code warning fails CI.
- **Known flaky test:** `ttls_set_before_the_kill_come_back_as_absolute_deadlines_not_restarted_countdowns` in `crates/server/tests/kill_and_recover.rs` is timing-sensitive and pre-existing. If it fails, re-run, or confirm with `cargo test --workspace -- --test-threads=1`. Do not "fix" it here.
- **Comment style:** short, easy, full sentences ending in a punctuation mark. No emojis.

---

### Task 1: restructure `ReplicaRegistry` around `ReplicaEntry`

**Files:**
- Modify: `crates/server/src/replication.rs:18-86` (the `ReplicaRegistry` doc comment, struct and `impl` block — replaced wholesale)
- Test: `crates/server/src/replication.rs` (existing `#[cfg(test)] mod tests`, after `addrs_returns_registered_addresses_in_registration_order`, which ends at `:830`)

**Interfaces:**
- Consumes: `unix_now_secs() -> i64` (already private in this module, `replication.rs:11-16`).
- Produces:
  - `pub struct ReplicaEntry { pub addr: Option<String>, tx: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>, pub ack_offset: AtomicU64, pub last_ack_unix: AtomicI64 }`
  - `pub fn ReplicaEntry::record_ack(&self, offset: u64)`
  - `pub struct ReplicaState { pub addr: Option<String>, pub ack_offset: u64, pub last_ack_unix: i64 }` (`Debug + Clone + PartialEq + Eq`)
  - `pub fn ReplicaRegistry::register(&self, addr: Option<String>, sender: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>) -> Arc<ReplicaEntry>` — return type changed, parameters unchanged.
  - `pub fn ReplicaRegistry::good_replicas(&self, max_lag: std::time::Duration) -> usize` — consumed by `07-fencing-config.md`.
  - `pub fn ReplicaRegistry::states(&self) -> Vec<ReplicaState>` — consumed by Task 3.
  - Unchanged: `pub fn broadcast(&self, bytes: bytes::Bytes)`, `pub fn len(&self) -> usize`, `pub fn addrs(&self) -> Vec<Option<String>>`, `pub fn is_empty(&self) -> bool`.

- [ ] **Step 1: Write the failing test**

Append to `crates/server/src/replication.rs`'s test module, after `addrs_returns_registered_addresses_in_registration_order`:

```rust
    #[test]
    fn register_returns_an_entry_that_starts_out_unacked() {
        let registry = ReplicaRegistry::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let entry = registry.register(Some("127.0.0.1:6480".to_string()), tx);

        assert_eq!(entry.addr.as_deref(), Some("127.0.0.1:6480"));
        assert_eq!(entry.ack_offset.load(Ordering::Relaxed), 0);
        // 0, not "now": a replica that has never acked must be distinguishable from one that
        // acked this instant, or `INFO`'s lag field would report a lie.
        assert_eq!(entry.last_ack_unix.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn record_ack_stores_the_offset_and_stamps_the_time() {
        let registry = ReplicaRegistry::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let entry = registry.register(None, tx);

        entry.record_ack(4096);

        assert_eq!(entry.ack_offset.load(Ordering::Relaxed), 4096);
        assert!(
            entry.last_ack_unix.load(Ordering::Relaxed) > 1_700_000_000,
            "record_ack should stamp a real unix timestamp, got {}",
            entry.last_ack_unix.load(Ordering::Relaxed)
        );
        // The registry sees the same entry, because `register` handed back an `Arc` of the one
        // it pushed rather than a copy.
        assert_eq!(registry.states()[0].ack_offset, 4096);
    }

    #[test]
    fn good_replicas_counts_only_replicas_that_acked_recently() {
        let registry = ReplicaRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        let (tx3, _rx3) = tokio::sync::mpsc::unbounded_channel();
        let fresh = registry.register(Some("fresh".to_string()), tx1);
        let stale = registry.register(Some("stale".to_string()), tx2);
        let _never = registry.register(Some("never".to_string()), tx3);

        fresh.record_ack(100);
        stale.record_ack(50);
        // Backdate the stale one well past the window, rather than sleeping for it.
        stale
            .last_ack_unix
            .store(unix_now_secs() - 60, Ordering::Relaxed);

        // Only `fresh`. `stale` acked too long ago, and `never` has no ack at all -- "unknown"
        // must never count as "healthy", which is the whole reason this number exists.
        assert_eq!(registry.good_replicas(std::time::Duration::from_secs(10)), 1);
        // A window wide enough to cover the backdated ack picks up both, proving the filter is
        // the lag comparison and not something incidental.
        assert_eq!(
            registry.good_replicas(std::time::Duration::from_secs(600)),
            2
        );
        // A zero-second window admits nothing that isn't acked this very second, and still
        // never admits the never-acked one.
        assert!(registry.good_replicas(std::time::Duration::ZERO) <= 1);
    }

    #[test]
    fn states_snapshots_every_replica_in_registration_order() {
        let registry = ReplicaRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        let first = registry.register(Some("127.0.0.1:6480".to_string()), tx1);
        registry.register(None, tx2); // a bare PSYNC advertised no address
        first.record_ack(7);

        let states = registry.states();

        assert_eq!(states.len(), 2);
        assert_eq!(states[0].addr.as_deref(), Some("127.0.0.1:6480"));
        assert_eq!(states[0].ack_offset, 7);
        assert!(states[0].last_ack_unix > 0);
        assert_eq!(states[1].addr, None);
        assert_eq!(states[1].ack_offset, 0);
        assert_eq!(states[1].last_ack_unix, 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem replication::tests::register_returns_an_entry_that_starts_out_unacked replication::tests::record_ack_stores_the_offset_and_stamps_the_time replication::tests::good_replicas_counts_only_replicas_that_acked_recently replication::tests::states_snapshots_every_replica_in_registration_order`

Expected: FAIL to compile — `error[E0599]: no method named 'states' found for struct 'ReplicaRegistry'` (and `good_replicas`), plus `error[E0308]: mismatched types` / `no field 'addr'` on `register`'s current `()` return. That is the correct failure — none of this exists yet.

- [ ] **Step 3: Replace the registry**

Replace `crates/server/src/replication.rs:18-86` — everything from the `/// Holds one outbound channel per connected replica, ...` doc comment through the closing `}` of `impl ReplicaRegistry` — with:

```rust
/// One connected replica, from the leader's side: the outbound channel its `serve_replica` task
/// drains, the address (if any) it advertised in its `PSYNC` -- see `ReplicationHandle::own_addr`'s
/// doc comment -- and how caught up it last told this leader it was.
///
/// `ReplicaRegistry::register` hands the caller an `Arc` of the very entry it pushed, so
/// `serve_replica` records an ack with a relaxed atomic store: no id, no lookup, and no contention
/// with `broadcast` on the registry's own mutex. That matters because acks arrive on the hot
/// replication path.
pub struct ReplicaEntry {
    /// What this replica advertised in its `PSYNC` frame -- `None` for a bare `PSYNC` (an old
    /// client, or a test). Immutable for the entry's life, so it needs no synchronization.
    pub addr: Option<String>,
    tx: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
    /// The replication-stream offset this replica last acknowledged, in bytes. 0 until its
    /// first ack, which is indistinguishable from a genuine ack of 0 -- use `last_ack_unix` to
    /// tell those apart.
    pub ack_offset: AtomicU64,
    /// Unix seconds at which `ack_offset` was last updated; 0 for a replica that has never
    /// acked at all. Never treat 0 as "acked at the epoch": it means unknown.
    pub last_ack_unix: AtomicI64,
}

impl ReplicaEntry {
    /// Records one `REPLCONF ACK <offset>` from this replica. Called from `serve_replica`'s
    /// inbound arm, once per ack. Relaxed ordering throughout: these two fields are only ever
    /// read for reporting and for the `min-replicas-to-write` count, neither of which orders
    /// anything else against them.
    pub fn record_ack(&self, offset: u64) {
        self.ack_offset.store(offset, Ordering::Relaxed);
        self.last_ack_unix.store(unix_now_secs(), Ordering::Relaxed);
    }
}

/// A plain, lock-free snapshot of one replica's state, so `INFO REPLICATION` can render its
/// `slaveN:` lines without holding the registry's mutex across a `format!`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaState {
    pub addr: Option<String>,
    pub ack_offset: u64,
    /// 0 means this replica has never acked. See `ReplicaEntry::last_ack_unix`.
    pub last_ack_unix: i64,
}

/// Holds one `ReplicaEntry` per connected replica. The `Mutex` is a plain `std::sync::Mutex`,
/// not `tokio::sync::Mutex`: every access is a quick, synchronous push/retain/map, never held
/// across an `.await`, so the lighter std lock is the right tool — matching `AofWriter::order`'s
/// existing choice for the same reason. Recording an ack does not take this lock at all; it goes
/// straight through the `Arc<ReplicaEntry>` `register` returned.
#[derive(Default)]
pub struct ReplicaRegistry {
    replicas: std::sync::Mutex<Vec<Arc<ReplicaEntry>>>,
}

impl ReplicaRegistry {
    /// Registers a newly-synced replica's outbound channel, alongside the address (if any) it
    /// advertised in its `PSYNC` frame -- `None` for a bare `PSYNC` (an old client, or a test).
    /// Called only from `serve_replica`, while it still holds `AofWriter::lock_all_shards()` —
    /// see this plan's Global Constraints for why registration must happen inside that same
    /// critical section as the snapshot walk, not after it.
    ///
    /// Returns the entry it just pushed so the caller can record acks on it directly. Ignoring
    /// the return value is fine and is what every pre-ack call site does.
    pub fn register(
        &self,
        addr: Option<String>,
        sender: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
    ) -> Arc<ReplicaEntry> {
        let entry = Arc::new(ReplicaEntry {
            addr,
            tx: sender,
            ack_offset: AtomicU64::new(0),
            last_ack_unix: AtomicI64::new(0),
        });
        self.replicas
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::clone(&entry));
        entry
    }

    /// Fans `bytes` out to every registered replica, pruning any whose receiver has been
    /// dropped (the replica connection died). Never itself returns an error: a delivery
    /// failure to one dead replica must not affect delivery to the others, and must never
    /// roll back the write that already committed on the leader.
    pub fn broadcast(&self, bytes: bytes::Bytes) {
        let mut replicas = self.replicas.lock().unwrap_or_else(|e| e.into_inner());
        replicas.retain(|entry| entry.tx.send(bytes.clone()).is_ok());
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
    /// for a replica whose `PSYNC` carried no address. Feeds `main.rs`'s startup banner; subject
    /// to the same lazy-pruning lag as `len`. `INFO REPLICATION` uses `states` instead, because
    /// it needs the ack fields too.
    pub fn addrs(&self) -> Vec<Option<String>> {
        self.replicas
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|entry| entry.addr.clone())
            .collect()
    }

    /// Required by `clippy::len_without_is_empty`, which `-D warnings` makes a hard error.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many replicas have acknowledged something within `max_lag` of now.
    ///
    /// A replica that has never acked is **never** good: "we have no idea where this replica is"
    /// must not read as "this replica is caught up", which is the entire reason the number
    /// exists. That also means an older follower build, which sends no acks at all, never counts
    /// -- deliberately, since fencing on a replica whose position is unknowable would be
    /// fencing on nothing. Feeds `min-replicas-to-write` (`07-fencing-config.md`) and the
    /// `rocket_mem_good_replicas` gauge.
    pub fn good_replicas(&self, max_lag: std::time::Duration) -> usize {
        let now = unix_now_secs();
        let max_lag = max_lag.as_secs() as i64;
        self.replicas
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|entry| {
                let last = entry.last_ack_unix.load(Ordering::Relaxed);
                // `saturating_sub` guards a clock that stepped backwards: that yields a
                // negative difference, which compares as "not lagging" rather than panicking or
                // wrapping into an enormous lag.
                last > 0 && now.saturating_sub(last) <= max_lag
            })
            .count()
    }

    /// A snapshot of every registered replica, in registration order, for `INFO REPLICATION` and
    /// metrics to render after the lock is released. Subject to the same lazy-pruning lag as
    /// `len` and `addrs`.
    pub fn states(&self) -> Vec<ReplicaState> {
        self.replicas
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|entry| ReplicaState {
                addr: entry.addr.clone(),
                ack_offset: entry.ack_offset.load(Ordering::Relaxed),
                last_ack_unix: entry.last_ack_unix.load(Ordering::Relaxed),
            })
            .collect()
    }
}
```

- [ ] **Step 4: Run the new tests to verify they pass**

Run: `cargo test -p rocket-mem replication::tests::register_returns_an_entry_that_starts_out_unacked replication::tests::record_ack_stores_the_offset_and_stamps_the_time replication::tests::good_replicas_counts_only_replicas_that_acked_recently replication::tests::states_snapshots_every_replica_in_registration_order`

Expected: PASS.

- [ ] **Step 5: Run the pre-existing registry tests to prove the kept signatures really were kept**

Run: `cargo test -p rocket-mem replication::tests::broadcast replication::tests::addrs_returns_registered_addresses_in_registration_order replication::tests::registry_len_tracks_registered_replicas`

Expected: PASS, with **no edits** to any of them. `broadcast_delivers_to_every_registered_sender`, `broadcast_prunes_a_sender_whose_receiver_was_dropped`, `broadcast_with_no_registered_replicas_does_nothing`, `addrs_returns_registered_addresses_in_registration_order` and `registry_len_tracks_registered_replicas` all call `register` and ignore its return value. If any of them needs a change, the signature contract was broken — fix the registry, not the test.

- [ ] **Step 6: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

Expected: all clean/green. `crates/server/src/metrics.rs:56`, `crates/server/src/main.rs:367` and `crates/server/src/dispatcher.rs:1918`/`:3252` must still compile untouched.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "Track per-replica ack offset and timestamp in the registry"
```

---

### Task 2: parse `REPLCONF ACK` on the replica connection

**Files:**
- Modify: `crates/server/src/connection.rs` (`serve_replica`: the registration block and the inbound `select!` arm; add the `parse_replconf_ack` helper next to `psync_advertised_addr`, which ends at `:300`)
- Test: `crates/server/src/connection.rs` (existing `#[cfg(test)] mod tests`, after the tests plan 04 added)

**Interfaces:**
- Consumes: `ReplicaRegistry::register(...) -> Arc<ReplicaEntry>` and `ReplicaEntry::record_ack(&self, offset: u64)` (Task 1); `ReplicaRegistry::states()` (Task 1, for the tests); `serve_replica`'s `select!` loop (plan 04).
- Produces:
  - `fn parse_replconf_ack(frame: &protocol::Frame) -> Option<u64>` (private to `connection.rs`)
  - The invariant that a `REPLCONF ACK <offset>` arriving on a replica connection — including one pipelined behind the `PSYNC` frame itself — updates that replica's registry entry. Consumed by Task 3's `INFO` rendering and by `06-follower-periodic-ack.md`'s end-to-end test.

- [ ] **Step 1: Write the failing tests**

Append to `crates/server/src/connection.rs`'s test module, after the tests plan 04 added:

```rust
    /// The point of making the connection bidirectional: an ack a follower sends up the same
    /// socket lands on that replica's registry entry, where `INFO` and (later) fencing can read
    /// it.
    #[tokio::test]
    async fn a_replconf_ack_from_a_replica_is_recorded_on_its_registry_entry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-ack-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec::default());
        framed
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"PSYNC")),
                Frame::Bulk(Bytes::from_static(b"127.0.0.1:6480")),
            ]))
            .await
            .unwrap();
        let mut parts = framed.into_parts();

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut len_buf = [0u8; 8];
        parts.io.read_exact(&mut len_buf).await.unwrap();
        let mut blob = vec![0u8; u64::from_le_bytes(len_buf) as usize];
        parts.io.read_exact(&mut blob).await.unwrap();

        parts
            .io
            .write_all(b"*3\r\n$8\r\nREPLCONF\r\n$3\r\nACK\r\n$4\r\n4096\r\n")
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let states = replication.registry.states();
            if states.len() == 1 && states[0].ack_offset == 4096 {
                assert!(
                    states[0].last_ack_unix > 1_700_000_000,
                    "an ack must stamp a real timestamp, got {}",
                    states[0].last_ack_unix
                );
                assert_eq!(states[0].addr.as_deref(), Some("127.0.0.1:6480"));
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the leader never recorded the replica's ack: {states:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// The `read_buf` hand-off, made observable. A follower is free to pipeline its first ack
    /// into the same write as its `PSYNC`, in which case the RESP codec has already pulled those
    /// bytes off the socket while decoding `PSYNC` -- they live in `FramedParts::read_buf` and
    /// nowhere else. `serve_replica` seeds its inbound reader with that buffer; if it ever stops
    /// doing so, the ack is gone with no error anywhere and this test is what catches it.
    ///
    /// One `write_all` of both frames, so on loopback they land in a single read and the codec
    /// genuinely reads ahead. Sending them as two writes would usually put the ack in a separate
    /// read, where a dropped `read_buf` would not show up.
    #[tokio::test]
    async fn an_ack_pipelined_behind_psync_is_not_lost() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-pipelined-ack-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        use tokio::io::AsyncWriteExt;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"*1\r\n$5\r\nPSYNC\r\n\
                  *3\r\n$8\r\nREPLCONF\r\n$3\r\nACK\r\n$1\r\n7\r\n",
            )
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let states = replication.registry.states();
            if states.len() == 1 && states[0].ack_offset == 7 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the ack pipelined behind PSYNC was dropped -- read_buf was not carried into \
                 the inbound reader: {states:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(stream);
    }

    /// A follower that never acks -- an older build, or any test that just PSYNCs -- stays a
    /// perfectly good replica with no ack information. `PING` here stands in for any well-formed
    /// frame this leader has no handler for: it must be ignored, not answered, and not fatal.
    #[tokio::test]
    async fn an_unrecognised_inbound_frame_leaves_the_replica_registered_and_unacked() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-unknown-frame-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec::default());
        framed
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(
                b"PSYNC",
            ))]))
            .await
            .unwrap();
        let mut parts = framed.into_parts();

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut len_buf = [0u8; 8];
        parts.io.read_exact(&mut len_buf).await.unwrap();
        let mut blob = vec![0u8; u64::from_le_bytes(len_buf) as usize];
        parts.io.read_exact(&mut blob).await.unwrap();

        // A frame with no handler, a REPLCONF subcommand this leader does not implement, and an
        // ack whose offset is not a number. None of the three may be recorded or answered.
        parts
            .io
            .write_all(
                b"*1\r\n$4\r\nPING\r\n\
                  *3\r\n$8\r\nREPLCONF\r\n$14\r\nlistening-port\r\n$4\r\n6480\r\n\
                  *3\r\n$8\r\nREPLCONF\r\n$3\r\nACK\r\n$3\r\nabc\r\n",
            )
            .await
            .unwrap();

        // A real write, driven after them, must still arrive -- and be the very next bytes on
        // this socket, proving nothing above drew a reply.
        let mut client = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        client
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"new")),
                Frame::Bulk(Bytes::from_static(b"value")),
            ]))
            .await
            .unwrap();
        assert_eq!(
            client.next().await.unwrap().unwrap(),
            Frame::Simple("OK".into())
        );

        let expected = b"*3\r\n$3\r\nSET\r\n$3\r\nnew\r\n$5\r\nvalue\r\n";
        let mut streamed = vec![0u8; expected.len()];
        parts.io.read_exact(&mut streamed).await.unwrap();
        assert_eq!(streamed, expected);

        let states = replication.registry.states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].ack_offset, 0);
        assert_eq!(
            states[0].last_ack_unix, 0,
            "nothing above is a well-formed REPLCONF ACK, so this replica has never acked"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem connection::tests::a_replconf_ack_from_a_replica_is_recorded_on_its_registry_entry connection::tests::an_ack_pipelined_behind_psync_is_not_lost connection::tests::an_unrecognised_inbound_frame_leaves_the_replica_registered_and_unacked`

Expected: the first two FAIL with `the leader never recorded the replica's ack: [ReplicaState { addr: Some("127.0.0.1:6480"), ack_offset: 0, last_ack_unix: 0 }]` and `the ack pipelined behind PSYNC was dropped ... [ReplicaState { addr: None, ack_offset: 0, last_ack_unix: 0 }]`. That is the correct failure — plan 04's inbound arm logs every frame at `debug` and records nothing. The third test PASSES already; it is the characterization guard that Task 2's parser does not become over-eager, and it must keep passing after Step 4.

- [ ] **Step 3: Add the ack parser**

Insert into `crates/server/src/connection.rs`, immediately after `psync_advertised_addr` (which ends at `:300`) and before `serve_replica`:

```rust
/// Pulls the offset out of a follower's `REPLCONF ACK <offset>` frame -- the ack shape fixed by
/// the failover-safety design contract's §2.3, a plain RESP array on the existing replication
/// socket. `None` for anything else at all: a different frame type, a different arity, a
/// `REPLCONF` subcommand this leader does not implement, or an offset that is not decimal ASCII.
/// The caller logs those at `debug` and ignores them -- an unrecognised inbound frame must never
/// draw an error reply and must never cost the follower its connection.
fn parse_replconf_ack(frame: &protocol::Frame) -> Option<u64> {
    let protocol::Frame::Array(items) = frame else {
        return None;
    };
    if items.len() != 3 {
        return None;
    }
    let protocol::Frame::Bulk(name) = &items[0] else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"REPLCONF") {
        return None;
    }
    let protocol::Frame::Bulk(subcommand) = &items[1] else {
        return None;
    };
    if !subcommand.eq_ignore_ascii_case(b"ACK") {
        return None;
    }
    let protocol::Frame::Bulk(offset) = &items[2] else {
        return None;
    };
    std::str::from_utf8(offset).ok()?.parse().ok()
}
```

- [ ] **Step 4: Keep the registry entry and record acks on it**

In `serve_replica`, capture the entry `register` now returns. Replace the registration block:

```rust
    let (snapshot_bytes, mut rx, entry) = {
        let _order_guard = aof.lock_all_shards();
        // The header's stream position, for a PSYNC image, is the leader's replication offset --
        // not an AOF length. See `Engine::snapshot`'s doc comment for the parameter's two
        // meanings.
        let handoff_offset = replication.master_repl_offset();
        let bytes = replication.engine().snapshot(handoff_offset);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        // The entry, not just a registration: acks arriving below are recorded straight through
        // this handle, with no registry lookup and no registry lock.
        let entry = replication.registry.register(advertised_addr, tx);
        (bytes, rx, entry)
    };
```

and replace the inbound arm's `Some(Ok(frame))` case:

```rust
                Some(Ok(frame)) => match parse_replconf_ack(&frame) {
                    Some(offset) => entry.record_ack(offset),
                    // Logged and dropped, never answered: an unrecognised frame must not draw an
                    // error reply and must not cost the follower its connection. A follower that
                    // never sends a recognisable ack stays a replica with no ack information.
                    // Kind and length only, never contents -- a replica's frames are arbitrary
                    // client bytes, and `logging.rs` forbids a `Bytes` reaching a log through
                    // `Debug`. (Corrected 2026-09-10: this block said `?frame`, which plan 04's
                    // Task 2 review caught as a redaction violation and fixed in the shipped
                    // code. Keep the shape below.)
                    None => tracing::debug!(
                        kind = frame.kind(),
                        len = frame.log_len(),
                        "ignoring inbound frame from a replica"
                    ),
                },
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem connection::tests::a_replconf_ack_from_a_replica_is_recorded_on_its_registry_entry connection::tests::an_ack_pipelined_behind_psync_is_not_lost connection::tests::an_unrecognised_inbound_frame_leaves_the_replica_registered_and_unacked`

Expected: all three PASS.

- [ ] **Step 6: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

Expected: all clean/green.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/connection.rs
git commit -m "Record REPLCONF ACK offsets from connected replicas"
```

---

### Task 3: report each replica's offset and lag in `INFO REPLICATION`

**Files:**
- Modify: `crates/server/src/replication.rs:8-16` (`unix_now_secs`'s visibility)
- Modify: `crates/server/src/dispatcher.rs:1917-1931` (the master branch of `info_text`'s replication section)
- Test: `crates/server/src/dispatcher.rs:5127-5149` (`info_lists_each_connected_slaves_advertised_address` — updated, not weakened) and a new test after it

**Interfaces:**
- Consumes: `ReplicaRegistry::states() -> Vec<ReplicaState>` and `ReplicaEntry::record_ack` (Task 1); `split_addr(&str) -> (&str, i64)` (`dispatcher.rs:1582`); `ReplicationHandle::master_repl_offset()` (plan 01 — already emitted on this branch, left exactly as plan 01 wrote it).
- Produces:
  - `pub(crate) fn unix_now_secs() -> i64` in `crates/server/src/replication.rs` (visibility widened from private).
  - The `INFO REPLICATION` line format `slave{i}:ip={ip},port={port},state=online,offset={ack},lag={secs}`, with `offset=0,lag=-1` for a replica that has never acked. Consumed by `06-follower-periodic-ack.md`'s end-to-end test and by the operator runbook in chain C.

- [ ] **Step 1: Update the existing test to the new format and add the ack case**

Replace `info_lists_each_connected_slaves_advertised_address` at `crates/server/src/dispatcher.rs:5127-5149` with:

```rust
    #[test]
    fn info_lists_each_connected_slaves_advertised_address() {
        let engine = Engine::new();
        let replication = ReplicationHandle::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        replication
            .registry
            .register(Some("127.0.0.1:6480".to_string()), tx1);
        replication.registry.register(None, tx2); // a bare PSYNC advertised no address

        let text = info_text_for(&replication, &engine, &[b"replication"]);

        assert!(text.contains("connected_slaves:2\r\n"), "{text}");
        // Neither replica has acked, so both report the unknown sentinel. `lag=-1` is
        // deliberately not `lag=0`: "we have never heard from this replica" must not render as
        // "this replica is perfectly caught up".
        assert!(
            text.contains("slave0:ip=127.0.0.1,port=6480,state=online,offset=0,lag=-1\r\n"),
            "{text}"
        );
        assert!(
            text.contains("slave1:ip=?,port=0,state=online,offset=0,lag=-1\r\n"),
            "{text}"
        );
    }

    #[test]
    fn info_reports_a_replicas_acked_offset_and_lag() {
        let engine = Engine::new();
        let replication = ReplicationHandle::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let entry = replication
            .registry
            .register(Some("127.0.0.1:6480".to_string()), tx);
        entry.record_ack(4096);

        let text = info_text_for(&replication, &engine, &[b"replication"]);

        // Acked this instant, so the lag is 0 seconds -- a real 0, not the -1 sentinel.
        assert!(
            text.contains("slave0:ip=127.0.0.1,port=6480,state=online,offset=4096,lag=0\r\n"),
            "{text}"
        );
    }

    #[test]
    fn info_reports_a_stale_replicas_lag_in_whole_seconds() {
        let engine = Engine::new();
        let replication = ReplicationHandle::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let entry = replication.registry.register(None, tx);
        entry.record_ack(10);
        // Backdated rather than slept for: the lag is derived from the stamp, so moving the
        // stamp is the whole experiment.
        entry.last_ack_unix.store(
            crate::replication::unix_now_secs() - 42,
            std::sync::atomic::Ordering::Relaxed,
        );

        let text = info_text_for(&replication, &engine, &[b"replication"]);

        assert!(
            text.contains("slave0:ip=?,port=0,state=online,offset=10,lag=42\r\n"),
            "{text}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::info_lists_each_connected_slaves_advertised_address dispatcher::tests::info_reports_a_replicas_acked_offset_and_lag dispatcher::tests::info_reports_a_stale_replicas_lag_in_whole_seconds`

Expected: FAIL. `info_lists_each_connected_slaves_advertised_address` fails its `contains` assertion (the emitted line is still `slave0:ip=127.0.0.1,port=6480,state=online\r\n`, with no `offset`/`lag`), and `info_reports_a_stale_replicas_lag_in_whole_seconds` additionally fails to compile — `error[E0603]: function 'unix_now_secs' is private`.

- [ ] **Step 3: Widen `unix_now_secs`'s visibility**

```rust
// crates/server/src/replication.rs — replace the doc comment and signature at :8-11
/// Unix seconds now, or 0 if the system clock is somehow before the epoch. Never panics: a
/// bogus clock must not take down a server over a metrics field. Used by `record_save`, by
/// `sync_once`'s last-apply stamp, by `ReplicaEntry::record_ack`, and by `info_text`'s replica
/// lag calculation, so there is exactly one implementation of this expression.
pub(crate) fn unix_now_secs() -> i64 {
```

- [ ] **Step 4: Render the offset and lag**

Replace the master branch of `info_text`'s replication section, `crates/server/src/dispatcher.rs:1917-1931` — from `out.push_str("role:master\r\n");` through the closing brace of the `for (i, addr) in addrs.iter().enumerate()` loop. Keep the `master_repl_offset:` line exactly where plan 01 put it.

> **This step contradicts itself — corrected 2026-09-10.** The sentence above is right: plan 01 put
> `master_repl_offset:` **after** the per-replica `slaveN:` lines, matching real Redis's field order.
> The code block below puts it **before** them. Pasting the block therefore silently reorders `INFO`
> output while claiming not to, and **no test catches it**, because every assertion on this section
> is `contains`-based and order-blind. Reconcile against the live file: emit `connected_slaves`, then
> the `slaveN:` lines, then `master_repl_offset`. The shipped code (`cf8ef2b`) does this correctly.

```rust
            out.push_str("role:master\r\n");
            out.push_str(&format!(
                "master_repl_offset:{}\r\n",
                replication.master_repl_offset()
            ));
            let states = replication.registry.states();
            out.push_str(&format!("connected_slaves:{}\r\n", states.len()));
            let now = crate::replication::unix_now_secs();
            // One `slaveN:` line per connected replica, real Redis's format -- `ip`/`port` come
            // from the address the replica advertised in its own `PSYNC` (see
            // `ReplicationHandle::own_addr`'s doc comment), not this connection's ephemeral
            // source port. `ip=?,port=0` for a replica that advertised none (a bare `PSYNC`,
            // from an old client or a test) rather than silently omitting the line.
            //
            // `offset` is the last position this replica acknowledged, and `lag` is whole
            // seconds since it did. A replica that has never acked -- an older follower build,
            // or one that has only just attached -- reports `offset=0,lag=-1`. The `-1` means
            // "unknown" and must never be read as "zero lag"; that distinction is the entire
            // reason this line was extended.
            for (i, state) in states.iter().enumerate() {
                let (ip, port) = match &state.addr {
                    Some(a) => split_addr(a),
                    None => ("?", 0),
                };
                let lag = if state.last_ack_unix == 0 {
                    -1
                } else {
                    // Clamped at 0 so a clock that stepped backwards reports "caught up"
                    // rather than a negative lag that reads as the unknown sentinel.
                    now.saturating_sub(state.last_ack_unix).max(0)
                };
                out.push_str(&format!(
                    "slave{i}:ip={ip},port={port},state=online,offset={},lag={lag}\r\n",
                    state.ack_offset
                ));
            }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests::info_lists_each_connected_slaves_advertised_address dispatcher::tests::info_reports_a_replicas_acked_offset_and_lag dispatcher::tests::info_reports_a_stale_replicas_lag_in_whole_seconds`

Expected: all three PASS.

- [ ] **Step 6: Sweep for any other assertion on the old slave-line format**

Run: `rg -n "state=online" --glob '!docs/superpowers/plans/2026-09-09-failover-safety-primitives/**'`

Expected: hits only in `crates/server/src/dispatcher.rs` (the emitting `format!` and the three tests above) and in `00-design-contract.md`/`01-leader-replication-offset.md`, which are documentation of the change, not assertions. If any other test or doc pins the old spelling, update it in this same commit. Do not weaken an assertion to make it pass.

- [ ] **Step 7: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

Expected: all clean/green.

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/dispatcher.rs crates/server/src/replication.rs
git commit -m "Report each replica's acked offset and lag in INFO REPLICATION"
```

---

## Next plan

[`06-follower-periodic-ack.md`](06-follower-periodic-ack.md) — the follower side: `sync_once` sends `REPLCONF ACK <offset>` up the same socket about once a second, so the fields this plan added stop reading `offset=0,lag=-1` forever.
