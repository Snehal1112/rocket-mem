# Snapshot Offset Handoff Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A follower that attaches to a leader mid-stream learns where in the replication stream its snapshot sits. The leader stamps its live `master_repl_offset` into the snapshot's existing 8-byte header where `serve_replica` currently hardcodes `0`, and `sync_once` seeds the follower's `slave_repl_offset` from the value `Engine::load_snapshot` already returns and currently throws away.

**Architecture:** **No wire-format change at all.** `Engine::snapshot(aof_offset)` has always written an 8-byte little-endian header, `Engine::load_snapshot` has always returned it, and the PSYNC path has always transmitted it — it was simply `0` going out and discarded coming in. This plan claims both ends of that already-existing free channel. The leader-side read of `master_repl_offset` happens inside the *same* `aof.lock_all_shards()` critical section that captures the snapshot and registers the replica, so no write can slip between the offset read and the registration. The follower-side seed is a store into a new `Arc<AtomicU64>` slot on `ReplicationHandle`, plumbed to the spawned follower task through the existing `FollowerHandles`/`FollowerStatus` bundles the same way `last_apply` and `link_up` already are.

**Tech Stack:** `std::sync::atomic` only. No new dependencies.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md) — step 1 ("Replication offsets"). The spec's finding that "every reconnect is a fresh full resync" is exactly what makes this handoff sufficient: a follower never needs to carry an offset across a reconnect, because every reconnect re-seeds it here.

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) in full before writing any code.** It is normative: every name, type, and semantic decision below is fixed there. Where this plan and the contract disagree, the contract wins and the disagreement is a bug worth reporting before you write code. §2.2 ("The snapshot's 8-byte header carries the handoff offset") and §2.4 ("Names, fixed") govern this plan directly.
- **Depends on [`01-leader-replication-offset.md`](01-leader-replication-offset.md).** `ReplicationHandle::master_repl_offset()` must already exist and be advanced at the broadcast site. Do not start this plan until plan 01 is committed.
- **No wire-format change.** Do not add a field, a length, a version byte, or a `+FULLRESYNC` line. The PSYNC reply stays: 8-byte little-endian blob length, then the blob (whose own first 8 bytes are the header), then a raw stream of pre-encoded RESP frames.
- **Atomicity is the point.** Reading the offset, capturing the snapshot, and registering the replica must all happen inside one `aof.lock_all_shards()` critical section. A write that slips between the snapshot and the registration reaches neither the blob nor the stream, and is lost with no way to detect it.
- **Do not rename the `aof_offset` parameter.** Its meaning is generalized to "the stream position this snapshot image corresponds to"; renaming it would churn the engine crate for nothing. Document the dual meaning instead — that documentation is a required deliverable of Task 2, not an optional nicety.
- **The three CI gates must be clean before every commit:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
  Clippy is strict and lints test code too; a never-read private struct field fails CI as dead code. That is why the `FollowerStatus` plumbing and its first reader land in the *same* task (Task 3).
- **Known flaky test:** `ttls_set_before_the_kill_come_back_as_absolute_deadlines_not_restarted_countdowns` in `crates/server/tests/kill_and_recover.rs` is timing-sensitive and pre-existing. If it fails, re-run, or confirm with `cargo test --workspace -- --test-threads=1`. Do not "fix" it here.
- **Comment style:** short, easy, full sentences ending in a punctuation mark. No emojis.

---

### Task 1: the follower-side offset slot on `ReplicationHandle`

**Files:**
- Modify: `crates/server/src/replication.rs` (add a field to `pub struct ReplicationHandle` — insert after the `master_repl_offset` field added by plan 01, which itself sits after `link_up` at `:187`; add the initializer to `new`'s struct literal at `:208-230`; add the three methods to the `impl ReplicationHandle` block after plan 01's `advance_master_repl_offset`; add the unit test to the existing `mod tests` at `:745`)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `pub fn ReplicationHandle::slave_repl_offset(&self) -> u64`
  - `pub fn ReplicationHandle::set_slave_repl_offset(&self, offset: u64)`
  - `pub fn ReplicationHandle::slave_repl_offset_slot(&self) -> Arc<AtomicU64>`

  All three consumed by Task 3; `slave_repl_offset()` also consumed by `03-follower-replication-offset.md`.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/replication.rs — add to the existing `mod tests`, directly after plan 01's
// `master_repl_offset_starts_at_zero_and_accumulates_byte_counts`
    #[test]
    fn slave_repl_offset_starts_at_zero_and_can_be_seeded() {
        let h = ReplicationHandle::default();
        assert_eq!(h.slave_repl_offset(), 0);
        // A seed is an absolute store, not an add: it comes from a snapshot header, which is a
        // position, not a delta.
        h.set_slave_repl_offset(4096);
        assert_eq!(h.slave_repl_offset(), 4096);
        h.set_slave_repl_offset(12);
        assert_eq!(h.slave_repl_offset(), 12);
        // The slot handed to the spawned follower task is the same atomic the getter reads.
        h.slave_repl_offset_slot().store(77, Ordering::Relaxed);
        assert_eq!(h.slave_repl_offset(), 77);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem replication::tests::slave_repl_offset_starts_at_zero`
Expected: FAIL to compile — `error[E0599]: no method named 'slave_repl_offset' found for struct 'ReplicationHandle'`

- [ ] **Step 3: Add the field**

```rust
// crates/server/src/replication.rs — add as a field of `pub struct ReplicationHandle`, directly
// after the `master_repl_offset` field added by 01-leader-replication-offset.md
    /// Follower side: how far into the leader's replication stream this node has processed.
    /// Seeded by `sync_once` from the snapshot header the leader stamped its own live
    /// `master_repl_offset` into, then advanced per applied frame by
    /// `03-follower-replication-offset.md`. An `Arc` because the spawned follower task is
    /// `'static` and needs its own handle -- the same reason `last_apply_unix` and `link_up`
    /// are `Arc`s. Meaningless while this node is a leader, and `INFO` only reports it under
    /// `role:slave`, so a value left over from a previous `REPLICAOF` is never rendered.
    slave_repl_offset: Arc<AtomicU64>,
```

- [ ] **Step 4: Add the initializer**

```rust
// crates/server/src/replication.rs — add to `new`'s struct literal, directly after plan 01's
// `master_repl_offset: Arc::new(AtomicU64::new(0)),` line
            slave_repl_offset: Arc::new(AtomicU64::new(0)),
```

- [ ] **Step 5: Add the three methods**

```rust
// crates/server/src/replication.rs — add to the existing `impl ReplicationHandle` block,
// directly after plan 01's `advance_master_repl_offset`
    /// Follower side: how far into the leader's stream this node has processed. Surfaced as
    /// `INFO REPLICATION`'s `slave_repl_offset` (and `master_repl_offset`, which on a follower
    /// reports the same number) and the `rocket_mem_slave_repl_offset` gauge.
    pub fn slave_repl_offset(&self) -> u64 {
        self.slave_repl_offset.load(Ordering::Relaxed)
    }

    /// Seeds the follower offset to an absolute position. Called once per successful sync, with
    /// the offset the leader stamped into the snapshot header -- a position in the leader's
    /// stream, not a delta, which is why this stores rather than adds.
    pub fn set_slave_repl_offset(&self, offset: u64) {
        self.slave_repl_offset.store(offset, Ordering::Relaxed);
    }

    /// The shared slot itself, for the spawned follower task to write into -- the same pattern
    /// `last_apply_slot` and `link_up_slot` already use, and for the same reason: that task is
    /// `'static` and cannot borrow from `self`.
    pub fn slave_repl_offset_slot(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.slave_repl_offset)
    }
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p rocket-mem replication::tests::slave_repl_offset_starts_at_zero`
Expected: PASS

- [ ] **Step 7: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "Add a follower replication-offset slot to ReplicationHandle"
```

---

### Task 2: the leader stamps its live offset into the PSYNC snapshot header

**Files:**
- Modify: `crates/server/src/connection.rs` (`serve_replica`'s critical section at `:324-330`; add the unit test to the existing `mod tests` at `:366`, after `psync_with_an_advertised_address_registers_it_on_the_leader` which ends at `:772`)
- Modify: `crates/engine/src/engine.rs` (the doc comment on `Engine::snapshot` at `:109-112`, and on `Engine::load_snapshot` at `:117-122`)
- Modify: `crates/engine/src/snapshot.rs` (the doc comment on `serialize` at `:71-75`, and on `deserialize` at `:93-97`)

**Interfaces:**
- Consumes: `ReplicationHandle::master_repl_offset()` (plan 01); `Engine::snapshot(&self, aof_offset: u64) -> Vec<u8>`; `AofWriter::lock_all_shards()`.
- Produces: the invariant that a PSYNC snapshot blob's 8-byte header holds the leader's `master_repl_offset` as of the moment the replica was registered. Consumed by Task 3.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/connection.rs — add to the existing `mod tests`, directly after
// `psync_with_an_advertised_address_registers_it_on_the_leader` (ends at :772)
    /// The snapshot's own 8-byte header carries the leader's live replication offset to a
    /// newly-attaching follower. This is not a wire-format change: the field has always been
    /// transmitted on this path, it was just always zero.
    #[tokio::test]
    async fn psync_stamps_the_leaders_replication_offset_into_the_snapshot_header() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-test-unused-4.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        // Take a write first, so the leader's offset is non-zero before any follower attaches.
        // A header that is still 0 here would be indistinguishable from the old hardcoded value.
        let mut client = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        client
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"k")),
                Frame::Bulk(Bytes::from_static(b"v")),
            ]))
            .await
            .unwrap();
        assert_eq!(
            client.next().await.unwrap().unwrap(),
            Frame::Simple("OK".into())
        );
        let leader_offset = replication.master_repl_offset();
        assert!(
            leader_offset > 0,
            "the write should have advanced the leader offset"
        );

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec::default());
        framed
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(
                b"PSYNC",
            ))]))
            .await
            .unwrap();
        let mut parts = framed.into_parts();

        use tokio::io::AsyncReadExt;
        let mut len_buf = [0u8; 8];
        parts.io.read_exact(&mut len_buf).await.unwrap();
        let len = u64::from_le_bytes(len_buf) as usize;
        let mut blob = vec![0u8; len];
        parts.io.read_exact(&mut blob).await.unwrap();

        // The blob's own first 8 bytes are the snapshot header, little-endian.
        let mut header = [0u8; 8];
        header.copy_from_slice(&blob[..8]);
        assert_eq!(
            u64::from_le_bytes(header),
            leader_offset,
            "the PSYNC snapshot header must carry the leader's live replication offset"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem connection::tests::psync_stamps_the_leaders_replication_offset`
Expected: FAIL — `assertion \`left == right\` failed: the PSYNC snapshot header must carry the leader's live replication offset`, with `left: 0` and `right` the encoded length of the `SET k v` frame. `serve_replica` still passes the hardcoded `0`.

- [ ] **Step 3: Pass the live offset in `serve_replica`**

```rust
// crates/server/src/connection.rs — replace the critical section in `serve_replica` (:315-330)
// with this. Only the `handoff_offset` binding and the `snapshot(...)` argument are new; the
// surrounding lock and registration are unchanged.
    // ONE critical section: read the offset, snapshot, and register, so no write can slip
    // between them. Taken separately, a write committing after the snapshot walk but before
    // registration would reach neither the blob nor the stream -- lost permanently,
    // unrepairable by reconnect, since a reconnect just snapshots a leader that has already
    // moved past it. Reading `master_repl_offset` inside the same section is what makes the
    // header the follower is about to seed itself from describe exactly this blob: the fan-out
    // site advances that counter while holding the same AOF ordering guard, so nothing can
    // advance it between this read and the registration below. Lock ordering:
    // lock_for_ordering() before the registry's own mutex, matching this plan's Global
    // Constraints and the fan-out hook in dispatcher.rs, the only other place both are taken --
    // there, the order guard for a write's shard(s) is held across both the AOF append and the
    // registry broadcast, for the same reason: neither critical section may release the order
    // guard before it has finished touching the registry.
    let (snapshot_bytes, mut rx) = {
        let _order_guard = aof.lock_all_shards();
        // The header's stream position, for a PSYNC image, is the leader's replication offset --
        // not an AOF length. See `Engine::snapshot`'s doc comment for the parameter's two
        // meanings.
        let handoff_offset = replication.master_repl_offset();
        let bytes = replication.engine().snapshot(handoff_offset);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        replication.registry.register(advertised_addr, tx);
        (bytes, rx)
    };
```

- [ ] **Step 4: Update `Engine::snapshot`'s and `Engine::load_snapshot`'s doc comments**

```rust
// crates/engine/src/engine.rs — replace the doc comment on `snapshot` (:109-112). The function
// body and signature are unchanged; the parameter keeps its `aof_offset` name.
    /// A thin facade over `snapshot::serialize`, matching `Engine`'s existing role over `Store`
    /// (see `CLAUDE.md`). `aof_offset` is opaque to `Engine`: it is whatever stream position the
    /// caller says this image corresponds to, which `Engine` has no way to compute itself. It
    /// has two meanings, one per caller. `handle_save` passes the AOF's current durable length,
    /// so recovery can replay only the tail after the snapshot. `serve_replica` passes the
    /// leader's live `master_repl_offset`, so a newly-attaching follower learns where in the
    /// replication stream its snapshot sits and can count on from there. The parameter keeps its
    /// `aof_offset` name rather than being renamed to something neutral, because renaming it
    /// would churn the whole engine crate for no behavior change; see `snapshot::serialize`'s
    /// own doc comment.
    pub fn snapshot(&self, aof_offset: u64) -> Vec<u8> {
```

```rust
// crates/engine/src/engine.rs — replace the doc comment on `load_snapshot` (:117-122). The
// function body and signature are unchanged.
    /// A thin facade over `snapshot::deserialize`. Returns the blob's header — the stream
    /// position this image corresponds to, whose meaning depends on who wrote it (see
    /// `snapshot` above): an AOF length for a disk snapshot, a replication-stream offset for a
    /// `PSYNC` snapshot. Deliberately bypasses `maxmemory` eviction — `load_snapshot_entries`
    /// goes through `Store::set`, not `Engine::set` — so a snapshot larger than a configured
    /// ceiling lands whole and is only trimmed back under it by the next write that calls
    /// `Engine::set`/`with_mut`. Evicting *while* loading would silently discard keys the
    /// operator asked to restore, which is never the right behavior for a restore path.
    pub fn load_snapshot(&self, bytes: &[u8]) -> Result<u64, crate::snapshot::SnapshotError> {
```

- [ ] **Step 5: Update `snapshot::serialize`'s and `snapshot::deserialize`'s doc comments**

```rust
// crates/engine/src/snapshot.rs — replace the doc comment on `serialize` (:71-75). The function
// body and signature are unchanged.
/// `aof_offset` is written into the blob's 8-byte little-endian header. It is the stream
/// position this image corresponds to, and which stream that is depends on the caller — the
/// parameter's name records only its original caller, not the full set. `SAVE` passes the AOF's
/// current durable length (the caller holds `AofWriter::lock_for_ordering()`, per the sprint-5
/// spec's SAVE atomicity decision, and is the only one who knows that length, so it is passed in
/// rather than discovered here). A leader answering `PSYNC` passes its live
/// `master_repl_offset`, so the follower reading this blob can seed its own `slave_repl_offset`
/// from the header and count on from there; that path used to pass `0` and discard the value on
/// receipt. Pass `0` only when there is genuinely no stream position to correlate against.
pub fn serialize(store: &Store, aof_offset: u64) -> Vec<u8> {
```

```rust
// crates/engine/src/snapshot.rs — replace the doc comment on `deserialize` (:93-97). The
// function body and signature are unchanged.
/// Replaces `store`'s entire contents with what's encoded in `bytes`, returning the stream
/// position from the blob's header — an AOF length when a `SAVE` wrote it, a replication-stream
/// offset when a leader's `PSYNC` reply did (see `serialize` above). An entry whose
/// `expires_at_unix_ms` is already in the past (compared directly as wall-clock milliseconds,
/// not via a round trip through `Instant` — see the sprint-5 spec for why that distinction
/// matters) is dropped rather than loaded and left for the expiry reaper to clean up later.
pub fn deserialize(store: &Store, bytes: &[u8]) -> Result<u64, SnapshotError> {
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p rocket-mem connection::tests::psync_stamps_the_leaders_replication_offset`
Expected: PASS

- [ ] **Step 7: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/connection.rs crates/engine/src/engine.rs crates/engine/src/snapshot.rs
git commit -m "Carry the leader replication offset in the PSYNC snapshot header"
```

---

### Task 3: the follower seeds its offset from the header

**Files:**
- Modify: `crates/server/src/replication.rs` (`struct FollowerStatus` at `:479-482`; `struct FollowerHandles` at `:486-489`; the `FollowerHandles` literal in `start_replicating_with_auth` at `:347-350`; the `FollowerStatus` literal in `replication_client_loop` at `:523-526`; the `load_snapshot` call in `sync_once` at `:708-711`; the seven `FollowerStatus` literals in `mod tests` at `:917-920`, `:982-985`, `:1028-1031`, `:1079-1082`, `:1144-1147`, `:1187-1190`, `:1299-1302`; add the new test after `sync_once_loads_the_snapshot_then_applies_streamed_frames`, which ends at `:939`)

**Interfaces:**
- Consumes: `ReplicationHandle::slave_repl_offset_slot()` (Task 1); the header value returned by `Engine::load_snapshot` (Task 2 stamps it).
- Produces: `FollowerStatus { last_apply, link_up, slave_offset }` and `FollowerHandles { last_apply, link_up, slave_offset }`, plus the invariant that a follower's `slave_repl_offset()` equals the leader's offset at sync time immediately after `sync_once` loads the snapshot. Consumed by `03-follower-replication-offset.md`, which advances that same slot per applied frame.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/replication.rs — add to the existing `mod tests`, directly after
// `sync_once_loads_the_snapshot_then_applies_streamed_frames` (ends at :939)
    /// The follower must not start counting from zero when it attaches to a leader that has
    /// already produced a replication stream. The snapshot header carries that position across,
    /// which is what makes a follower's offset comparable to its leader's.
    #[tokio::test]
    async fn sync_once_seeds_the_follower_offset_from_the_snapshot_header() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut psync_bytes = [0u8; 15];
            socket.read_exact(&mut psync_bytes).await.unwrap();

            // 4096: a leader that had already produced 4096 bytes of replication stream before
            // this follower attached.
            let blob = engine::Engine::new().snapshot(4096);
            socket
                .write_all(&(blob.len() as u64).to_le_bytes())
                .await
                .unwrap();
            socket.write_all(&blob).await.unwrap();
            // Hold the socket open just long enough for the follower to read the blob, then drop
            // it so `sync_once` returns on its own instead of needing a timeout.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });

        let engine = engine::Engine::new();
        let slave_offset = AtomicU64::new(0);
        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
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
                slave_offset: &slave_offset,
            },
            &FollowerIdentity::default(),
        )
        .await
        .unwrap();

        fake_leader.await.unwrap();
        assert_eq!(
            slave_offset.load(Ordering::Relaxed),
            4096,
            "the follower must seed its offset from the snapshot header, not start at 0"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem replication::tests::sync_once_seeds_the_follower_offset`
Expected: FAIL to compile — `error[E0560]: struct 'FollowerStatus' has no field named 'slave_offset'`

- [ ] **Step 3: Add the field to both bundles**

```rust
// crates/server/src/replication.rs — replace `struct FollowerStatus` (:479-482)
struct FollowerStatus<'a> {
    last_apply: &'a AtomicI64,
    link_up: &'a AtomicBool,
    slave_offset: &'a AtomicU64,
}
```

```rust
// crates/server/src/replication.rs — replace `struct FollowerHandles` (:486-489)
struct FollowerHandles {
    last_apply: Arc<AtomicI64>,
    link_up: Arc<AtomicBool>,
    slave_offset: Arc<AtomicU64>,
}
```

- [ ] **Step 4: Fill the new field at both production construction sites**

```rust
// crates/server/src/replication.rs — replace the `FollowerHandles` literal inside
// `start_replicating_with_auth` (:347-350)
            FollowerHandles {
                last_apply,
                link_up,
                slave_offset: self.slave_repl_offset_slot(),
            },
```

```rust
// crates/server/src/replication.rs — replace the `FollowerStatus` literal inside
// `replication_client_loop` (:523-526)
        let status = FollowerStatus {
            last_apply: &handles.last_apply,
            link_up: &handles.link_up,
            slave_offset: &handles.slave_offset,
        };
```

- [ ] **Step 5: Fill the new field at every test construction site**

Seven `FollowerStatus` literals in `mod tests` need the third field. They come in exactly two indentations. Apply both replacements across the whole file, replacing **every** occurrence. (Match on content, not on line number: Step 1 inserted a test above most of them, so the `:9xx`/`:1xxx` numbers in the **Files** block above are pre-insertion positions.)

```rust
// crates/server/src/replication.rs — replace-all, 16-space indentation (5 occurrences)
// FROM:
                last_apply: &AtomicI64::new(0),
                link_up: &AtomicBool::new(false),
// TO:
                last_apply: &AtomicI64::new(0),
                link_up: &AtomicBool::new(false),
                slave_offset: &AtomicU64::new(0),
```

```rust
// crates/server/src/replication.rs — replace-all, 20-space indentation (2 occurrences)
// FROM:
                        last_apply: &AtomicI64::new(0),
                        link_up: &AtomicBool::new(false),
// TO:
                        last_apply: &AtomicI64::new(0),
                        link_up: &AtomicBool::new(false),
                        slave_offset: &AtomicU64::new(0),
```

The new test written in Step 1 already supplies its own `slave_offset: &slave_offset` and must not be touched by these replacements — it uses a named local, not `&AtomicU64::new(0)`, so neither `FROM` pattern matches it.

- [ ] **Step 6: Seed the offset in `sync_once`**

```rust
// crates/server/src/replication.rs — replace the `load_snapshot` call and the `link_up` store
// in `sync_once` (:708-711)
    // The header is the leader's own replication offset as of the moment this blob was captured
    // and this replica was registered -- both happened inside one AOF ordering critical section
    // on the leader, so nothing was broadcast in between. Seeding from it is what makes this
    // follower's offset directly comparable to its leader's; starting from 0 instead would make
    // every follower that attached to a non-fresh leader look permanently, wrongly behind.
    let snapshot_offset = engine
        .load_snapshot(&blob)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    status.slave_offset.store(snapshot_offset, Ordering::Relaxed);
    status.link_up.store(true, Ordering::Relaxed);
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p rocket-mem replication::tests::sync_once_seeds_the_follower_offset`
Expected: PASS

- [ ] **Step 8: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 9: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "Seed the follower replication offset from the snapshot header"
```

---

## Next plan

[`03-follower-replication-offset.md`](03-follower-replication-offset.md) — advance that seeded offset per applied frame, and report it in `INFO REPLICATION` and Prometheus.
