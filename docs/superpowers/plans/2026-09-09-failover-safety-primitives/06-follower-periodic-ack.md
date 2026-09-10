# Follower Periodic Ack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the loop. The leader can now read a replica connection (plan 04) and record acks on it (plan 05), but nothing sends any — every `INFO REPLICATION` slave line still reads `offset=0,lag=-1` forever. This plan makes the follower send `REPLCONF ACK <offset>` up the same replication socket: once immediately after it loads its snapshot, and about once a second thereafter. It ends with an end-to-end test that writes on a leader and asserts the leader's own `INFO REPLICATION` shows that follower's `offset` advancing with a small `lag`.

**Architecture:** `Framed` is both a `Sink` and a `Stream`, so the follower can send on the very handle `sync_once` already reads frames from — no second socket, no second task, no `Arc<Mutex<..>>` around the writer. The apply loop stops being `while let Some(result) = framed.next().await` and becomes a `tokio::select!` between `framed.next()` and a `tokio::time::interval` tick. Both branch futures are cancel-safe; the `send` happens in the tick arm's body, after the other future has been dropped, which is why one `&mut framed` serves both. The offset sent is whatever `slave_repl_offset` holds — seeded from the snapshot header by plan 02, advanced per applied frame by plan 03. This plan neither computes nor advances it.

**Tech Stack:** Rust, tokio (`macros` for `select!`, `time` for `interval_at`), tokio-util (`codec`), futures-util (`SinkExt`/`StreamExt`).

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md) — "Decision: v1 is not failover", step 1 ("a monotonic `u64` counter on the leader, **echoed by each follower via a periodic `REPLCONF ACK <offset>`-equivalent frame**"), is authoritative for why this exists.

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) in full before writing any code.** It is normative: every name, type, and semantic decision below is fixed there. Where this plan and the contract disagree, the contract wins and the disagreement is a bug worth reporting before you write code. §2.1 (byte semantics), §2.2 (the snapshot header seeds the follower) and §2.3 (`REPLCONF ACK` is a normal RESP array on the existing socket) govern this plan directly.
- **Plans 01–05 must all be landed first.** In particular:
  - `ReplicationHandle::slave_repl_offset()` / `set_slave_repl_offset()` / `advance_slave_repl_offset()` exist (plans 01–03).
  - `struct FollowerStatus<'a>` carries a **`slave_offset: &'a AtomicU64`** field, and `struct FollowerHandles` carries `slave_offset: Arc<AtomicU64>` (plan 02, Task 3). That is the exact spelling this plan writes against.
  - `sync_once` seeds that slot from `Engine::load_snapshot`'s return value (plan 02) and advances it by `crate::aof::encode_frame(&frame)?.len()` per applied frame (plan 03).
  - The leader parses `REPLCONF ACK` and renders `offset=`/`lag=` in `INFO` (plans 04–05).

  **Verify before writing code** (Task 1, Step 1). If `FollowerStatus` has no `slave_offset` field, or spells it differently, **stop and report** rather than renaming anything: the contract fixes the `ReplicationHandle` accessor names but not this internal plumbing name, so a mismatch is a real gap between plans, not a licence to improvise.
- **One ack per interval, not one per applied frame.** A frame per write would double the replication stream's packet count for a number nothing reads more than once a second. `MissedTickBehavior::Delay` is required so a busy apply loop cannot produce a burst of catch-up acks the moment it frees up.
- **A failed ack send ends `sync_once` with its I/O error**, which drops into `replication_client_loop`'s existing 1-second reconnect backoff. Swallowing it would leave a follower silently ack-less on a half-dead connection, which is precisely the "unknown reads as healthy" failure this whole chain exists to prevent.
- **The apply loop's existing body must survive the restructure verbatim** — the generation re-check, the `aof.map(|a| a.lock_all_shards())` order guard, the `dispatch` call, the error logging, the `last_apply` stamp, and plan 03's offset advance. Task 2 moves that body into a `select!` arm; it does not rewrite it.
- **Timing tests are bounded polls or bounded timeouts with an explicit deadline**, never a bare `sleep` + assert. Copy the shape of `wait_for` at `crates/server/tests/replication.rs:42-60`. The ack interval is 1 second, so give any test that waits for a *second* ack at least 5 seconds of headroom.
- **The three CI gates must be clean before every commit:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
  Clippy is strict (`-D warnings`) and lints test code too; an unused private const fails CI as dead code, which is why `ACK_INTERVAL` is introduced in Task 2, the task that first uses it, and not in Task 1.
- **Known flaky test:** `ttls_set_before_the_kill_come_back_as_absolute_deadlines_not_restarted_countdowns` in `crates/server/tests/kill_and_recover.rs` is timing-sensitive and pre-existing. If it fails, re-run, or confirm with `cargo test --workspace -- --test-threads=1`. Do not "fix" it here.
- **Comment style:** short, easy, full sentences ending in a punctuation mark. No emojis.

---

### Task 1: ack the snapshot offset as soon as the snapshot is loaded

**Files:**
- Modify: `crates/server/src/replication.rs` (add `replconf_ack_frame` immediately above `async fn sync_once`, whose doc comment starts at `:599`; add the send in `sync_once` right after `status.link_up.store(true, Ordering::Relaxed);` at `:711` and the `Framed::from_parts` at `:716`)
- Test: `crates/server/src/replication.rs` (existing `#[cfg(test)] mod tests`, after `sync_once_loads_the_snapshot_then_applies_streamed_frames`, which ends at `:939`)

**Interfaces:**
- Consumes:
  - `FollowerStatus<'a>.slave_offset: &'a AtomicU64` (plan 02, Task 3), already seeded from the snapshot header by the time this send runs.
  - `Engine::load_snapshot(&self, bytes: &[u8]) -> Result<u64, SnapshotError>` — plan 02 stamps the leader's `master_repl_offset` into that header.
  - `futures_util::SinkExt::send` on `tokio_util::codec::Framed<S, RespCodec>` (already imported at `replication.rs:3`).
  - The leader-side parser `parse_replconf_ack` (plan 05, Task 2) — not called here, but it is what makes this frame meaningful.
- Produces:
  - `fn replconf_ack_frame(offset: u64) -> protocol::Frame` (private to `replication.rs`) — consumed by Task 2.
  - The invariant that a freshly-synced follower has acked its snapshot's offset before it applies a single streamed frame.

- [ ] **Step 1: Verify the plumbing this plan writes against actually exists**

Run: `rg -n "struct FollowerStatus" -A 6 crates/server/src/replication.rs && rg -n "fn slave_repl_offset|fn advance_slave_repl_offset|fn set_slave_repl_offset" crates/server/src/replication.rs`

Expected: `FollowerStatus<'a>` has three fields, the third being `slave_offset: &'a AtomicU64`, and all three `ReplicationHandle` offset accessors exist. If `slave_offset` is missing or differently named, **stop and report** — plans 02/03 did not land as the design contract describes, and improvising a name here would fork the chain.

- [ ] **Step 2: Write the failing test**

Append to `crates/server/src/replication.rs`'s test module, after `sync_once_loads_the_snapshot_then_applies_streamed_frames`:

```rust
    /// A follower that has just loaded a snapshot already knows exactly where it sits in the
    /// replication stream -- plan 02 stamped that position into the blob's 8-byte header. Saying
    /// so straight away, rather than waiting out a whole ack interval, is what stops a
    /// freshly-attached replica spending its first second reported as `offset=0,lag=-1` on a
    /// leader that is in fact fully caught up with it.
    #[tokio::test]
    async fn sync_once_acks_the_snapshot_offset_as_soon_as_it_has_loaded_it() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // `*1\r\n$5\r\nPSYNC\r\n` is exactly 15 bytes.
            let mut psync_bytes = [0u8; 15];
            socket.read_exact(&mut psync_bytes).await.unwrap();

            // 4096 is this leader's replication offset at hand-off time, carried in the
            // snapshot header exactly as `serve_replica` does it.
            let blob = engine::Engine::new().snapshot(4096);
            socket
                .write_all(&(blob.len() as u64).to_le_bytes())
                .await
                .unwrap();
            socket.write_all(&blob).await.unwrap();

            // Past the blob, the follower speaks plain RESP back up this same socket.
            let mut framed =
                tokio_util::codec::Framed::new(socket, protocol::codec::RespCodec::default());
            tokio::time::timeout(std::time::Duration::from_secs(5), framed.next())
                .await
                .expect("the follower sent no REPLCONF ACK within 5s of loading the snapshot")
                .expect("the connection ended before any ack arrived")
                .unwrap()
        });

        let engine = Arc::new(engine::Engine::new());
        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
        let slave_offset = Arc::new(AtomicU64::new(0));
        let sync_task = {
            let engine = Arc::clone(&engine);
            let generation = Arc::clone(&generation);
            let slave_offset = Arc::clone(&slave_offset);
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
                        slave_offset: &slave_offset,
                    },
                    &FollowerIdentity::default(),
                )
                .await
            })
        };

        let ack = fake_leader.await.unwrap();
        sync_task.abort();

        assert_eq!(
            ack,
            protocol::Frame::Array(vec![
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"REPLCONF")),
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"ACK")),
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"4096")),
            ]),
            "the follower must ack the offset its snapshot was stamped with"
        );
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p rocket-mem replication::tests::sync_once_acks_the_snapshot_offset_as_soon_as_it_has_loaded_it`

Expected: FAIL with the panic `the follower sent no REPLCONF ACK within 5s of loading the snapshot: Elapsed(())`. That is the correct failure — the follower currently sends nothing after `PSYNC`.

(If it instead fails on the *value*, reporting `ACK 0` rather than `ACK 4096`, the implementation below is not the problem: plan 02's snapshot-header seeding did not land. Stop and report.)

- [ ] **Step 4: Add the ack frame builder**

Insert into `crates/server/src/replication.rs`, immediately above `async fn sync_once`'s doc comment (`:599`):

```rust
/// Builds one `REPLCONF ACK <offset>` frame: a plain RESP array on the follower's existing
/// replication socket, with the offset as decimal ASCII. This is the shape the failover-safety
/// design contract's §2.3 fixes, and it is deliberately ordinary -- a leader that has never
/// heard of it decodes a well-formed frame it has no handler for, logs it at `debug`, and keeps
/// streaming, rather than erroring or dropping the connection.
fn replconf_ack_frame(offset: u64) -> protocol::Frame {
    protocol::Frame::Array(vec![
        protocol::Frame::Bulk(bytes::Bytes::from_static(b"REPLCONF")),
        protocol::Frame::Bulk(bytes::Bytes::from_static(b"ACK")),
        protocol::Frame::Bulk(bytes::Bytes::from(offset.to_string())),
    ])
}
```

- [ ] **Step 5: Send the first ack right after the snapshot lands**

In `sync_once`, replace the block that rebuilds the `Framed` (`:713-716` — the three comment lines plus `let mut framed = ...`) with:

```rust
    // From here on the leader sends plain RESP frames, byte-for-byte what its own AOF
    // received — rebuild a Framed over the same socket (whose read position is exactly past
    // the blob) to resume decoding normally. It is a Sink as well as a Stream, which is what
    // lets the acks below go back up this same socket with no second connection.
    let mut framed = tokio_util::codec::Framed::from_parts(parts);

    // Ack immediately, before the periodic timer's first tick. The snapshot header already told
    // this follower where it sits, so there is nothing to wait for -- and without this, a
    // freshly-attached replica reads as `offset=0,lag=-1` on the leader for a whole interval
    // even though both ends agree it is caught up. A send failure here is the connection dying;
    // returning the error puts `replication_client_loop` into its normal reconnect backoff.
    framed
        .send(replconf_ack_frame(
            status.slave_offset.load(Ordering::Relaxed),
        ))
        .await?;
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p rocket-mem replication::tests::sync_once_acks_the_snapshot_offset_as_soon_as_it_has_loaded_it`

Expected: PASS.

- [ ] **Step 7: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

Expected: all clean/green. The other `sync_once` tests use fake leaders that never read what the follower sends; a ~37-byte ack sitting unread in a socket buffer changes nothing for them.

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "Ack the snapshot offset as soon as a follower loads it"
```

---

### Task 2: keep acking on a timer while applying frames

**Files:**
- Modify: `crates/server/src/replication.rs` (add the `ACK_INTERVAL` const after `unix_now_secs`, which ends at `:16`; replace `sync_once`'s apply loop, `:717-742`)
- Test: `crates/server/src/replication.rs` (existing `#[cfg(test)] mod tests`, after the test Task 1 added)

**Interfaces:**
- Consumes: `replconf_ack_frame(offset: u64) -> protocol::Frame` (Task 1); `FollowerStatus<'a>.slave_offset: &'a AtomicU64`, advanced per applied frame by plan 03; `crate::aof::encode_frame(&Frame) -> std::io::Result<Vec<u8>>` (test only).
- Produces:
  - `const ACK_INTERVAL: std::time::Duration` (private to `replication.rs`).
  - The invariant that a linked follower reports its position at least once per `ACK_INTERVAL` for as long as the connection lives. Consumed by Task 3's end-to-end assertion and by `07-fencing-config.md`, whose `good_replicas` window is meaningless without a steady ack cadence.

- [ ] **Step 1: Print the current apply loop, so the restructure below preserves it exactly**

Run: `sed -n '/let mut framed = tokio_util::codec::Framed::from_parts/,/^    Ok(())/p' crates/server/src/replication.rs`

Expected: the `Framed::from_parts` line, Task 1's immediate ack, then the `while let Some(result) = framed.next().await { ... }` loop and the trailing `Ok(())`. **Keep the printed loop body byte-for-byte** when you move it in Step 4 — plan 03 added an offset advance inside it that this plan must not drop.

- [ ] **Step 2: Write the failing test**

Append to `crates/server/src/replication.rs`'s test module, after `sync_once_acks_the_snapshot_offset_as_soon_as_it_has_loaded_it`:

```rust
    /// One ack at sync time is not enough: the leader's `lag` field goes stale the moment the
    /// follower stops talking, and `min-replicas-to-write` fencing would fence a perfectly
    /// healthy replica within its own max-lag window. This pins the steady cadence -- a second
    /// ack, carrying an offset advanced by exactly the frame that was applied in between.
    #[tokio::test]
    async fn sync_once_keeps_acking_on_a_timer_as_it_applies_frames() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let set_frame = protocol::Frame::Array(vec![
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"SET")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"k")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"v")),
        ]);
        // The leader counts encoded bytes, and the follower re-encodes each applied frame to
        // learn the same number -- see the design contract's §2.1. Computing it here rather
        // than hardcoding 27 keeps this test honest if the encoding ever changes.
        let streamed_len = crate::aof::encode_frame(&set_frame).unwrap().len() as u64;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = {
            let set_frame = set_frame.clone();
            tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut psync_bytes = [0u8; 15];
                socket.read_exact(&mut psync_bytes).await.unwrap();

                let blob = engine::Engine::new().snapshot(0);
                socket
                    .write_all(&(blob.len() as u64).to_le_bytes())
                    .await
                    .unwrap();
                socket.write_all(&blob).await.unwrap();

                let mut framed =
                    tokio_util::codec::Framed::new(socket, protocol::codec::RespCodec::default());
                let first = tokio::time::timeout(std::time::Duration::from_secs(5), framed.next())
                    .await
                    .expect("no ack after the snapshot")
                    .expect("connection ended before the first ack")
                    .unwrap();

                // Stream one write, then wait for the *next* ack. The interval is one second,
                // so five is generous headroom on a loaded CI box.
                framed.send(set_frame).await.unwrap();
                let second = tokio::time::timeout(std::time::Duration::from_secs(5), framed.next())
                    .await
                    .expect("the follower stopped acking after its first ack")
                    .expect("connection ended before the second ack")
                    .unwrap();

                (first, second)
            })
        };

        let engine = Arc::new(engine::Engine::new());
        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
        let slave_offset = Arc::new(AtomicU64::new(0));
        let sync_task = {
            let engine = Arc::clone(&engine);
            let generation = Arc::clone(&generation);
            let slave_offset = Arc::clone(&slave_offset);
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
                        slave_offset: &slave_offset,
                    },
                    &FollowerIdentity::default(),
                )
                .await
            })
        };

        let (first, second) = fake_leader.await.unwrap();
        sync_task.abort();

        assert_eq!(
            first,
            protocol::Frame::Array(vec![
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"REPLCONF")),
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"ACK")),
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"0")),
            ])
        );
        assert_eq!(
            second,
            protocol::Frame::Array(vec![
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"REPLCONF")),
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"ACK")),
                protocol::Frame::Bulk(bytes::Bytes::from(streamed_len.to_string())),
            ]),
            "the second ack must carry the offset the applied frame advanced it to"
        );
        // The frame really was applied, so the ack is reporting work, not just ticking.
        assert_eq!(
            engine.get(b"k"),
            Some(engine::Value::String(bytes::Bytes::from_static(b"v")))
        );
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p rocket-mem replication::tests::sync_once_keeps_acking_on_a_timer_as_it_applies_frames`

Expected: FAIL with the panic `the follower stopped acking after its first ack: Elapsed(())`. That is the correct failure — Task 1 sends exactly one ack and then parks in the apply loop with no timer.

(If it instead fails on the second ack's *value*, showing `ACK 0`, plan 03's per-frame offset advance did not land. Stop and report.)

- [ ] **Step 4: Add the interval constant**

Insert into `crates/server/src/replication.rs`, immediately after `unix_now_secs` (which ends at `:16`):

```rust
/// How often a linked follower tells its leader where it is in the replication stream. One
/// second matches real Redis's `REPLCONF ACK` cadence. Per-frame acks were considered and
/// rejected: they would double the replication stream's packet count for a number nothing reads
/// more often than this, and the leader's `lag` field only has one-second resolution anyway.
const ACK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
```

- [ ] **Step 5: Turn the apply loop into a select between inbound frames and ack ticks**

Replace `sync_once`'s apply loop — the `while let Some(result) = framed.next().await { ... }` block at `:717-742`, up to but not including the trailing `Ok(())` — with the following. **The contents of the `incoming` arm below are today's loop body; if Step 1's output differs (plan 03 adds an offset advance inside it), keep what the tree has and only change the loop scaffolding around it.**

```rust
    // `interval_at`, not `interval`: `interval`'s first tick completes immediately, which would
    // duplicate the ack Task 1 already sent above. The first tick belongs one interval out.
    let mut acks = tokio::time::interval_at(
        tokio::time::Instant::now() + ACK_INTERVAL,
        ACK_INTERVAL,
    );
    // A tick missed because the apply loop was busy must not become a burst of catch-up acks
    // the instant it frees up -- one ack per interval is the whole point of having an interval.
    acks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            incoming = framed.next() => {
                let Some(result) = incoming else {
                    return Ok(()); // the leader closed the stream
                };
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
            _ = acks.tick() => {
                // `framed` is a Sink as well as a Stream, so the ack goes back up the same
                // socket the frames arrive on. Both branch futures are cancel-safe, and this
                // send runs only after the other one has been dropped, which is what lets a
                // single `&mut framed` serve both arms.
                //
                // A send failure is the connection dying. Returning the error hands control to
                // `replication_client_loop`'s existing reconnect backoff, rather than leaving
                // this follower silently ack-less on a half-dead socket -- which would read on
                // the leader as a healthy replica that has simply gone quiet.
                framed
                    .send(replconf_ack_frame(
                        status.slave_offset.load(Ordering::Relaxed),
                    ))
                    .await?;
            }
        }
    }
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p rocket-mem replication::tests::sync_once_keeps_acking_on_a_timer_as_it_applies_frames`

Expected: PASS.

- [ ] **Step 7: Run the whole replication module and the replication integration suite**

Run: `cargo test -p rocket-mem replication:: && cargo test -p rocket-mem --test replication`

Expected: all PASS. `a_save_racing_the_apply_loop_never_observes_a_half_applied_multi_key_write` is the one to watch: it proves the order guard still wraps the `dispatch` call and nothing else after the move into the `select!` arm.

- [ ] **Step 8: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

Expected: all clean/green.

- [ ] **Step 9: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "Send a periodic REPLCONF ACK from the follower's apply loop"
```

---

### Task 3: end-to-end — a leader's `INFO` shows its follower's offset advancing

**Files:**
- Test: `crates/server/tests/replication.rs` (add the two helpers after `wait_for`, which ends at `:60`; add the test at the end of the file)

**Interfaces:**
- Consumes: everything plans 01–06 built — the leader's `master_repl_offset` (plan 01), the snapshot handoff (plan 02), the follower's per-frame advance (plan 03), the bidirectional connection (plan 04), the ack parsing and the extended `INFO` slave line (plan 05), and the periodic ack (Tasks 1–2). Plus the `spawn_node` and `wait_for` helpers already in this file.
- Produces:
  - `async fn info_replication(addr: &str) -> String` and `fn slave0_offset_and_lag(info: &str) -> Option<(u64, i64)>` — the integration-level `INFO` helpers this repo does not have yet (the design contract's §1.9 notes their absence). Available to chain C's alerting-probe work.
  - The end-to-end proof that the whole chain reports a real, advancing offset with a small lag. This is the deliverable the spec's step 1 was asking for.

- [ ] **Step 1: Write the failing test**

Add the helpers to `crates/server/tests/replication.rs` immediately after `wait_for`:

```rust
/// One `INFO replication` over raw RESP, returning the bulk body. Raw rather than the `redis`
/// crate so the exact `slaveN:` field spelling is what gets asserted -- there is no
/// integration-level INFO helper in this repo, and this is the shape `tests/cluster.rs`'s own
/// `send` helper already uses for the same reason.
async fn info_replication(addr: &str) -> String {
    let mut framed = tokio_util::codec::Framed::new(
        tokio::net::TcpStream::connect(addr).await.unwrap(),
        protocol::codec::RespCodec::default(),
    );
    framed
        .send(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"INFO")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"replication")),
        ]))
        .await
        .unwrap();
    match framed.next().await.unwrap().unwrap() {
        protocol::Frame::Bulk(body) => String::from_utf8_lossy(&body).into_owned(),
        other => panic!("INFO replied with {other:?}"),
    }
}

/// Pulls `offset` and `lag` out of an `INFO replication` body's `slave0:` line. `None` when
/// there is no such line yet (the replica has not attached), or when either field is missing --
/// which is itself a failure worth surfacing rather than defaulting away.
fn slave0_offset_and_lag(info: &str) -> Option<(u64, i64)> {
    let line = info.lines().find(|l| l.starts_with("slave0:"))?;
    let mut offset = None;
    let mut lag = None;
    for field in line.trim_start_matches("slave0:").split(',') {
        match field.split_once('=') {
            Some(("offset", v)) => offset = v.parse().ok(),
            Some(("lag", v)) => lag = v.parse().ok(),
            _ => {}
        }
    }
    Some((offset?, lag?))
}
```

Then append the test at the end of the file:

```rust
/// The whole chain, end to end, over real sockets. A write on the leader advances its
/// `master_repl_offset`, is streamed to the follower, is applied there and advances the
/// follower's `slave_repl_offset`, is acked back up the same connection, is recorded on the
/// leader's registry entry, and finally shows up in the leader's own `INFO REPLICATION` as a
/// non-zero `offset` with a small `lag`.
///
/// Before this chain, that line read `slave0:...,state=online` with nothing behind it: the
/// leader could not answer "how caught up is this replica" at all, which is exactly why the
/// spec refused to build failover on top of it.
#[tokio::test]
async fn a_leader_reports_its_followers_advancing_offset_and_small_lag_in_info() {
    let (_leader_dir, _leader_engine, _leader_aof, _leader_replication, leader_addr) =
        spawn_node().await;
    let (_f_dir, f_engine, _f_aof, f_replication, _f_addr) = spawn_node().await;

    f_replication.start_replicating(leader_addr.clone());
    let link_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !f_replication.link_up() {
        assert!(
            tokio::time::Instant::now() < link_deadline,
            "the follower never linked up"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let client = redis::Client::open(format!("redis://{leader_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = con.set("k", "v").await.unwrap();
    wait_for(&f_engine, b"k", b"v").await;

    // Bounded poll, not a sleep: the ack cadence is one second, so the offset-carrying ack can
    // legitimately take that long to arrive. Anything past five seconds is a real failure.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let info = info_replication(&leader_addr).await;
        if let Some((offset, lag)) = slave0_offset_and_lag(&info) {
            if offset > 0 {
                assert!(
                    (0..=2).contains(&lag),
                    "a replica acking every second must report a lag of about 0s, got {lag}\n{info}"
                );
                // The follower's own view must agree with what the leader is reporting about it.
                assert_eq!(
                    f_replication.slave_repl_offset(),
                    offset,
                    "the leader's recorded ack must match the follower's own position\n{info}"
                );
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the leader never saw its follower's offset advance past 0:\n{}",
            info_replication(&leader_addr).await
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p rocket-mem --test replication a_leader_reports_its_followers_advancing_offset_and_small_lag_in_info`

Expected: PASS. This is the chain's acceptance test, not a red-then-green driver — Tasks 1 and 2 landed the last missing piece, and this is what proves the six plans compose. If it fails, read which assertion:
- no `slave0:` line at all → the follower never attached; check `PSYNC`, not this plan.
- `offset` stuck at `0` → the ack is not reaching the leader (plans 04/05) or the follower's offset is not advancing (plan 03).
- `lag` reported as `-1` → the leader has never recorded an ack for this replica; plan 05's parser or plan 04's inbound arm is the place to look.
- the leader/follower offsets disagree → the re-encoding invariant in the design contract's §2.1 is broken.

Do not weaken any assertion to make it pass.

- [ ] **Step 3: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

Expected: all clean/green.

- [ ] **Step 4: Commit**

```bash
git add crates/server/tests/replication.rs
git commit -m "Cover the leader reporting a follower's live replication offset"
```

---

## Next plan

[`07-fencing-config.md`](07-fencing-config.md) — with real acks flowing, `min-replicas-to-write` becomes meaningful: a leader with too few recently-acked replicas refuses writes with `NOREPLICAS`, which is the single change that makes split-brain bounded instead of unbounded.
