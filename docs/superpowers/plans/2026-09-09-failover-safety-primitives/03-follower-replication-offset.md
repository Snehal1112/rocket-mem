# Follower Replication Offset Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A follower counts the replication stream it has processed. `sync_once`'s apply loop advances `slave_repl_offset` by the byte length of every streamed frame it applies, `INFO REPLICATION` reports `slave_repl_offset:<n>` and `master_repl_offset:<n>` under `role:slave`, and the `rocket_mem_slave_repl_offset` gauge exports it. With plan 01's leader counter and plan 02's snapshot handoff, this closes the loop: a leader's offset and a caught-up follower's offset become the *same number*, which is what makes "which replica is most caught up" answerable.

**Architecture:** The follower learns each frame's replication-stream length by re-encoding it: `crate::aof::encode_frame(&frame)?.len()`. That is byte-exact, not an estimate — see Global Constraints for the invariant that guarantees it — and it is deliberately done before the frame is moved into `dispatch`. The advance sits next to the existing `status.last_apply.store(...)` at the bottom of the apply loop, writing the same `Arc<AtomicU64>` slot plan 02 seeds from the snapshot header. `INFO` and the gauge read it through `ReplicationHandle::slave_repl_offset()`.

**Tech Stack:** `std::sync::atomic` only. No new dependencies.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md) — step 1 ("Replication offsets"), and the finding it exists to fix: "`last_apply_unix` is a wall-clock timestamp of the last *applied* frame, not a count of frames received, so it says nothing about how many writes a replica might have missed."

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) in full before writing any code.** It is normative: every name, type, and semantic decision below is fixed there. Where this plan and the contract disagree, the contract wins and the disagreement is a bug worth reporting before you write code. §2.1 ("The offset counts bytes, not frames") and §2.4 ("Names, fixed") govern this plan directly.
- **Depends on [`01-leader-replication-offset.md`](01-leader-replication-offset.md) and [`02-snapshot-offset-handoff.md`](02-snapshot-offset-handoff.md).** `ReplicationHandle::{master_repl_offset, slave_repl_offset, set_slave_repl_offset, slave_repl_offset_slot}` and `FollowerStatus::slave_offset` must already exist, and the snapshot header must already carry the leader's offset. Do not start this plan until both are committed. Line numbers cited in **Files** blocks below are as of the start of this chain, before plans 01 and 02 inserted their code; match on content, not on line number.
- **Re-encoding is byte-exact, and this is load-bearing — do not "optimize" it away.** `crate::aof::encode_frame(&frame)?.len()` reproduces the leader's `encoded.len()` exactly, because only `crate::aof::WRITE_COMMANDS` frames are ever replicated and those are always a `Frame::Array` of `Frame::Bulk` — a shape whose RESP encoding is identical under RESP2 and RESP3. (`Frame::Null` and `Frame::Map` *do* encode differently per protocol version; neither can appear in a replicated write command.) A future change that widens what gets replicated breaks this invariant and must revisit the decision. Carry this reasoning into the source comment, so the next reader does not replace it with a decoder-side byte count or a frame count.
- **The offset advances for every frame taken off the stream, including one whose apply errors.** An errored apply is logged and skipped, but the bytes were still consumed; not advancing would leave the follower permanently, wrongly behind.
- **A follower reports the same number under both keys.** `slave_repl_offset` and `master_repl_offset` are equal on a follower — that is what real Redis does, and the number is this node's own processed position, the only leader position it can honestly claim to know.
- **Do not weaken an existing test.** The `info_text` unit tests near `dispatcher.rs:5089`, `:5110` and `:5142` are `contains`-based, so adding new keys does not break them; Task 2 nonetheless strengthens the `role:slave` one with positive assertions for the new keys, in the same commit as the format change. The `slave{i}:...state=online` line keeps its exact current spelling in this plan — extending it is `05-replica-ack-tracking.md`'s job.
- **The three CI gates must be clean before every commit:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
  Clippy is strict and lints test code too; a dead-code warning fails CI.
- **Known flaky test:** `ttls_set_before_the_kill_come_back_as_absolute_deadlines_not_restarted_countdowns` in `crates/server/tests/kill_and_recover.rs` is timing-sensitive and pre-existing. If it fails, re-run, or confirm with `cargo test --workspace -- --test-threads=1`. Do not "fix" it here.
- **Comment style:** short, easy, full sentences ending in a punctuation mark. No emojis.

---

### Task 1: advance the follower offset per applied frame

**Files:**
- Modify: `crates/server/src/replication.rs` (add `advance_slave_repl_offset` to the `impl ReplicationHandle` block, after plan 02's `slave_repl_offset_slot`; the apply loop in `sync_once` at `:717-741`; add the new test to the existing `mod tests` after plan 02's `sync_once_seeds_the_follower_offset_from_the_snapshot_header`)

**Interfaces:**
- Consumes: `FollowerStatus::slave_offset: &AtomicU64` (plan 02); `crate::aof::encode_frame(&Frame) -> std::io::Result<Vec<u8>>` (`crates/server/src/aof.rs:48`).
- Produces:
  - `pub fn ReplicationHandle::advance_slave_repl_offset(&self, bytes: u64) -> u64`
  - the invariant that a follower's `slave_repl_offset()` equals the snapshot-header seed plus the summed encoded length of every frame it has since applied.

  Both consumed by Task 2 and Task 3.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/replication.rs — add to the existing `mod tests`, directly after plan 02's
// `sync_once_seeds_the_follower_offset_from_the_snapshot_header`
    /// Proves the follower's byte count is exact, not approximate: it asserts the offset equals
    /// the header seed plus the *literal wire length* of the frame the leader wrote. If the
    /// re-encode in the apply loop ever stopped reproducing the leader's bytes, this would fail
    /// by exactly the drift.
    #[tokio::test]
    async fn sync_once_advances_the_follower_offset_by_each_applied_frames_wire_length() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const STREAMED: &[u8] = b"*3\r\n$3\r\nSET\r\n$11\r\nfrom-stream\r\n$1\r\nv\r\n";

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut psync_bytes = [0u8; 15];
            socket.read_exact(&mut psync_bytes).await.unwrap();

            // 1000: this leader had already produced 1000 bytes of stream before the follower
            // attached, so the test proves the seed and the per-frame advance compose.
            let blob = engine::Engine::new().snapshot(1000);
            socket
                .write_all(&(blob.len() as u64).to_le_bytes())
                .await
                .unwrap();
            socket.write_all(&blob).await.unwrap();
            socket.write_all(STREAMED).await.unwrap();
            // Hold the socket open long enough for the follower to read and apply the frame,
            // then drop it so `sync_once` returns on its own instead of needing a timeout.
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
            engine.get(b"from-stream"),
            Some(engine::Value::String(bytes::Bytes::from_static(b"v"))),
            "the streamed frame must actually have been applied"
        );
        assert_eq!(
            slave_offset.load(Ordering::Relaxed),
            1000 + STREAMED.len() as u64,
            "the follower must advance by the applied frame's exact wire length"
        );
    }

    #[test]
    fn advance_slave_repl_offset_accumulates_from_the_seeded_position() {
        let h = ReplicationHandle::default();
        h.set_slave_repl_offset(1000);
        assert_eq!(h.advance_slave_repl_offset(38), 1038);
        assert_eq!(h.advance_slave_repl_offset(2), 1040);
        assert_eq!(h.slave_repl_offset(), 1040);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem replication::tests::sync_once_advances_the_follower_offset replication::tests::advance_slave_repl_offset_accumulates`
Expected: `advance_slave_repl_offset_accumulates_from_the_seeded_position` FAILs to compile — `error[E0599]: no method named 'advance_slave_repl_offset' found for struct 'ReplicationHandle'`. Once that compiles, `sync_once_advances_the_follower_offset_by_each_applied_frames_wire_length` FAILs on `assertion \`left == right\` failed: the follower must advance by the applied frame's exact wire length`, with `left: 1000` — the seed landed but nothing advances it.

- [ ] **Step 3: Add `advance_slave_repl_offset`**

```rust
// crates/server/src/replication.rs — add to the existing `impl ReplicationHandle` block,
// directly after plan 02's `slave_repl_offset_slot`
    /// Adds `bytes` to the follower offset and returns the new value. The apply loop writes the
    /// shared slot directly (it holds only an `&AtomicU64`, like `last_apply` and `link_up`);
    /// this is the same operation for a caller that holds the whole handle.
    pub fn advance_slave_repl_offset(&self, bytes: u64) -> u64 {
        self.slave_repl_offset.fetch_add(bytes, Ordering::Relaxed) + bytes
    }
```

- [ ] **Step 4: Advance the offset in the apply loop**

```rust
// crates/server/src/replication.rs — replace the body of `sync_once`'s apply loop (:717-741)
// with this. Only the `frame_len` binding and the final `slave_offset` store are new;
// everything else is unchanged.
    while let Some(result) = framed.next().await {
        if generation.load(Ordering::SeqCst) != my_generation {
            return Ok(()); // superseded -- stop applying frames to state a newer task now owns
        }
        let frame = result?;
        // Re-encode to learn this frame's replication-stream length, before the frame is moved
        // into `dispatch` below. This is byte-exact, not an approximation, and the exactness is
        // load-bearing: it is what makes this follower's offset directly comparable to the
        // leader's `master_repl_offset`, which counted the same bytes on the way out. It holds
        // because only `aof::WRITE_COMMANDS` frames are ever replicated, and those are always a
        // Frame::Array of Frame::Bulk -- a shape whose RESP encoding is identical under RESP2
        // and RESP3. (Frame::Null and Frame::Map do encode differently per protocol version;
        // neither can appear in a replicated write command.) Do not replace this with a
        // decoder-side byte count or a frame count: widening what gets replicated is what would
        // break the invariant, not this re-encode.
        let frame_len = crate::aof::encode_frame(&frame)?.len() as u64;
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
        // Advanced even when the apply above errored: those bytes were still consumed from the
        // stream, and an offset that skipped them would report this follower as permanently
        // behind a leader it is actually level with.
        status.slave_offset.fetch_add(frame_len, Ordering::Relaxed);
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem replication::tests::sync_once_advances_the_follower_offset replication::tests::advance_slave_repl_offset_accumulates`
Expected: PASS, both

- [ ] **Step 6: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "Advance the follower replication offset per applied frame"
```

---

### Task 2: report both offset keys in `INFO REPLICATION` on a follower

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (`info_text`'s `role:slave` branch at `:1904-1915`; the existing test `info_reports_role_slave_on_a_replica` at `:5102-5113`; add one new test directly after it)

**Interfaces:**
- Consumes: `ReplicationHandle::slave_repl_offset()` (plan 02), kept live by Task 1.
- Produces: `INFO REPLICATION`'s `slave_repl_offset:<n>` and `master_repl_offset:<n>` lines on a follower.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — replace the existing `info_reports_role_slave_on_a_replica`
// (:5102-5113) with this strengthened version
    #[test]
    fn info_reports_role_slave_on_a_replica() {
        let engine = Engine::new();
        let replication = ReplicationHandle::default();
        replication
            .is_replica
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let text = info_text_for(&replication, &engine, &[b"replication"]);
        assert!(text.contains("role:slave\r\n"), "{text}");
        assert!(text.contains("master_link_status:down\r\n"), "{text}");
        assert!(text.contains("slave_repl_offset:0\r\n"), "{text}");
        assert!(text.contains("master_repl_offset:0\r\n"), "{text}");
        assert!(!text.contains("connected_slaves:"), "{text}");
    }
```

```rust
// crates/server/src/dispatcher.rs — add to the existing `mod tests`, directly after
// `info_reports_role_slave_on_a_replica`
    /// A follower reports the same number under both keys, exactly as real Redis does: it is
    /// this node's own processed position, and the only leader position it can honestly claim to
    /// know. The leader-side counter is irrelevant here and must not leak into the slave lines.
    #[test]
    fn info_on_a_follower_reports_the_same_offset_under_both_keys() {
        let engine = Engine::new();
        let replication = ReplicationHandle::default();
        replication
            .is_replica
            .store(true, std::sync::atomic::Ordering::Relaxed);
        replication.set_slave_repl_offset(4134);

        let text = info_text_for(&replication, &engine, &[b"replication"]);

        assert!(text.contains("slave_repl_offset:4134\r\n"), "{text}");
        assert!(text.contains("master_repl_offset:4134\r\n"), "{text}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::info_reports_role_slave_on_a_replica dispatcher::tests::info_on_a_follower_reports_the_same_offset`
Expected: both FAIL on `assertion failed: text.contains("slave_repl_offset:...")` — `info_text`'s `role:slave` branch emits no offset keys at all.

- [ ] **Step 3: Emit both keys**

```rust
// crates/server/src/dispatcher.rs — replace the `if is_replica` (role:slave) branch of
// `info_text`'s replication section (:1904-1915) with this. Only the trailing offset push is
// new; everything above it is unchanged.
        if is_replica {
            // `slave`, not `replica`: real Redis still emits the legacy word and every client
            // library parses for it. Matching the wire is the point.
            out.push_str("role:slave\r\n");
            if let Some(addr) = replication.master_addr() {
                let (host, port) = split_addr(&addr);
                out.push_str(&format!("master_host:{host}\r\nmaster_port:{port}\r\n"));
            }
            out.push_str(&format!(
                "master_link_status:{}\r\n",
                if replication.link_up() { "up" } else { "down" }
            ));
            // The same number twice, exactly as real Redis does. `slave_repl_offset` is how far
            // this node has processed; `master_repl_offset` is the leader position that
            // corresponds to, and it is the only leader position this node can honestly claim
            // to know -- nothing tells a follower how far ahead its leader has since run. Both
            // are seeded from the snapshot header at sync time and advanced per applied frame.
            let offset = replication.slave_repl_offset();
            out.push_str(&format!(
                "slave_repl_offset:{offset}\r\nmaster_repl_offset:{offset}\r\n"
            ));
        } else {
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests::info_reports_role_slave_on_a_replica dispatcher::tests::info_on_a_follower_reports_the_same_offset`
Expected: PASS, both

- [ ] **Step 5: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "Report the follower replication offset in INFO REPLICATION"
```

---

### Task 3: the Prometheus gauge and an end-to-end convergence test

**Files:**
- Modify: `crates/server/src/metrics.rs` (`refresh_sampled_gauges` at `:50-62`; the existing test `the_metrics_endpoint_serves_the_rendered_registry_and_404s_everything_else` at `:151-212`)
- Modify: `crates/server/tests/replication.rs` (add one integration test after `one_leader_two_followers_propagates_writes_within_a_bounded_time_window`, which ends at `:129`)

**Interfaces:**
- Consumes: `ReplicationHandle::{master_repl_offset, slave_repl_offset}`; the `spawn_node()` and `wait_for()` helpers at `crates/server/tests/replication.rs:11-40` and `:42-60`.
- Produces: the `rocket_mem_slave_repl_offset` Prometheus gauge, and end-to-end proof that a caught-up follower's offset equals its leader's — the whole point of plans 01-03 together.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/metrics.rs — add to the existing test
// `the_metrics_endpoint_serves_the_rendered_registry_and_404s_everything_else`, directly after
// plan 01's `assert!(body.contains("rocket_mem_master_repl_offset"), "{body}");`
        assert!(body.contains("rocket_mem_slave_repl_offset"), "{body}");
```

```rust
// crates/server/tests/replication.rs — add directly after
// `one_leader_two_followers_propagates_writes_within_a_bounded_time_window` (ends at :129)

/// The payoff of the offset chain: a leader and a caught-up follower report the *same* number.
/// Both halves are exercised -- a write taken before the follower attached (carried across in
/// the snapshot header) and one taken after (counted in the apply loop) -- because either half
/// alone would let the two sides agree by accident.
#[tokio::test]
async fn a_followers_replication_offset_converges_on_its_leaders() {
    let (_leader_dir, _leader_engine, _leader_aof, leader_replication, leader_addr) =
        spawn_node().await;
    let (_f_dir, f_engine, _f_aof, f_replication, _f_addr) = spawn_node().await;

    let client = redis::Client::open(format!("redis://{leader_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    // Write *before* the follower attaches, so the leader's offset is already non-zero when the
    // snapshot header carries it across.
    let _: () = con.set("before", "1").await.unwrap();
    assert!(
        leader_replication.master_repl_offset() > 0,
        "the pre-attach write should have advanced the leader's offset"
    );

    f_replication.start_replicating(leader_addr.clone());
    wait_for(&f_engine, b"before", b"1").await;

    // And one *after*, so the apply loop's per-frame advance is exercised too.
    let _: () = con.set("after", "2").await.unwrap();
    wait_for(&f_engine, b"after", b"2").await;

    // A bounded poll rather than a bare assertion: the apply loop stores the new offset just
    // after the dispatch that `wait_for` observes, so the two can race by a few instructions.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let leader = leader_replication.master_repl_offset();
        let follower = f_replication.slave_repl_offset();
        if leader == follower {
            assert!(follower > 0, "both offsets converged on zero, which proves nothing");
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "follower offset {follower} never caught up to leader offset {leader}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem metrics::tests::the_metrics_endpoint_serves && cargo test -p rocket-mem --test replication a_followers_replication_offset_converges`
Expected: the metrics test FAILs on `assertion failed: body.contains("rocket_mem_slave_repl_offset")` — no such gauge is registered. The integration test PASSes already if Tasks 1 and 2 are correct; run it here to confirm the end-to-end invariant holds before the gauge is added, and treat a failure as a bug in Task 1's apply loop or plan 02's handoff, not in this test.

- [ ] **Step 3: Add the gauge**

```rust
// crates/server/src/metrics.rs — add to `refresh_sampled_gauges`, directly after plan 01's
// `rocket_mem_master_repl_offset` gauge
    // Zero on a node that has never been a follower. `INFO` hides this behind `role:slave`;
    // a gauge cannot, so it simply reads 0 there, which is the honest value.
    ::metrics::gauge!("rocket_mem_slave_repl_offset").set(replication.slave_repl_offset() as f64);
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem metrics::tests::the_metrics_endpoint_serves && cargo test -p rocket-mem --test replication a_followers_replication_offset_converges`
Expected: PASS, both

- [ ] **Step 5: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/metrics.rs crates/server/tests/replication.rs
git commit -m "Export the follower replication offset and prove it converges on the leader's"
```

---

## Next plan

[`04-bidirectional-replica-connection.md`](04-bidirectional-replica-connection.md) — make the leader *read* from a replica connection, so a follower can send `REPLCONF ACK <offset>` back up the socket it already has. Today `serve_replica` never reads again after `PSYNC`, so an ack would sit unread in the kernel forever.
