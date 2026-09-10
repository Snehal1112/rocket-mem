# Leader Replication Offset Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The leader counts the replication stream it produces. A monotonic `master_repl_offset` on `ReplicationHandle` advances by the encoded byte length of every frame handed to `ReplicaRegistry::broadcast`, is surfaced as `INFO REPLICATION`'s `master_repl_offset:<n>`, and is exported as the `rocket_mem_master_repl_offset` gauge. This is the prerequisite every later plan in this folder depends on — without it, "which replica is most caught up" and "did we lose acknowledged writes" are unanswerable questions.

**Architecture:** One `Arc<AtomicU64>` field on `ReplicationHandle`, advanced at exactly one place: the fan-out loop at the bottom of `dispatch_and_log_inner`, which already holds the AOF per-shard ordering guard `_order_guard` and already has `encoded.len()` in hand for free. Advancing there — under the same guard, in the same loop iteration as the `broadcast` call — is what makes the offset order match the broadcast order. Nothing else in the codebase writes this counter. Reading it is a plain relaxed atomic load from `info_text` and `metrics::refresh_sampled_gauges`.

**Tech Stack:** `std::sync::atomic` only. No new dependencies.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md) — "Decision: v1 is not failover — it's the safety primitives failover needs", step 1 ("Replication offsets"), is authoritative for why this exists at all.

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) in full before writing any code.** It is normative: every name, type, and semantic decision below is fixed there. Where this plan and the contract disagree, the contract wins and the disagreement is a bug worth reporting before you write code. §2.1 ("The offset counts bytes, not frames") and §2.4 ("Names, fixed") govern this plan directly.
- **The offset counts bytes, not commands.** `master_repl_offset` is the summed `encoded.len()` of every frame broadcast. A frame count under a byte-count name would make `INFO` lie in exactly the way this spec exists to stop.
- **The offset advances with zero replicas connected.** It measures the write stream this leader produced, not what anyone received. `broadcast` walking an empty registry is not a reason to skip the advance.
- **The advance happens under the same `_order_guard` as the broadcast**, in the same loop. Dropping the guard first would let two concurrent writers assign offsets in the opposite order from the one their frames were fanned out in.
- **The offset is process-local and resets to 0 on restart.** Do not add cross-restart persistence — that would imply a partial-resync capability this project does not have. See contract §2.1.
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

### Task 1: the counter on `ReplicationHandle`

**Files:**
- Modify: `crates/server/src/replication.rs` (add a field to `pub struct ReplicationHandle` — the struct opens at `:91`, insert after the `link_up` field at `:187`; add the initializer to `new`'s struct literal at `:208-230`; add the two methods to the `impl ReplicationHandle` block, after `link_up_slot` at `:444-446`; add the unit test to the existing `mod tests` at `:745`)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `pub fn ReplicationHandle::master_repl_offset(&self) -> u64`
  - `pub fn ReplicationHandle::advance_master_repl_offset(&self, bytes: u64) -> u64`

  Both consumed by Task 2 and Task 3, and by `02-snapshot-offset-handoff.md`.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/replication.rs — add to the existing `mod tests`, after
// `last_save_unix_is_zero_until_a_save_records_one` (:844-853)
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem replication::tests::master_repl_offset_starts_at_zero`
Expected: FAIL to compile — `error[E0599]: no method named 'master_repl_offset' found for struct 'ReplicationHandle'`

- [ ] **Step 3: Add the field**

```rust
// crates/server/src/replication.rs — add as a field of `pub struct ReplicationHandle`,
// directly after the `link_up` field (:187) and before the `slowlog` doc comment (:188)
    /// Leader side: how many bytes of replication stream this node has produced since it
    /// started. Advanced by `dispatch_and_log_inner`'s fan-out loop by the encoded length of
    /// every frame it hands to `ReplicaRegistry::broadcast`, under the same AOF ordering guard
    /// the broadcast itself is under, so offsets are assigned in exactly fan-out order. It
    /// counts the write stream this leader produced, not what any replica received, so it
    /// advances even when no replica is connected. Process-local: it resets to 0 on restart,
    /// which is safe only because every reconnect is a full resync that re-seeds the follower
    /// from the snapshot header, so a follower can never carry a stale offset across a leader
    /// restart. An `Arc` for symmetry with the follower-side counter added in
    /// `03-follower-replication-offset.md`, whose spawned task is `'static`.
    master_repl_offset: Arc<AtomicU64>,
```

- [ ] **Step 4: Add the initializer**

```rust
// crates/server/src/replication.rs — add to `new`'s struct literal (:208-230), directly after
// the `link_up: Arc::new(AtomicBool::new(false)),` line (:226)
            master_repl_offset: Arc::new(AtomicU64::new(0)),
```

- [ ] **Step 5: Add the two methods**

```rust
// crates/server/src/replication.rs — add to the existing `impl ReplicationHandle` block,
// directly after `link_up_slot` (:444-446) and before the block's closing brace (:447)
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
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p rocket-mem replication::tests::master_repl_offset_starts_at_zero`
Expected: PASS

- [ ] **Step 7: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "Add a leader replication-offset counter to ReplicationHandle"
```

---

### Task 2: advance the offset at the broadcast site

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (the fan-out loop in `dispatch_and_log_inner` at `:3245-3253`; add the unit test to the existing `mod tests` at `:3392`, after `info_lists_each_connected_slaves_advertised_address` ends at `:5149`)

**Interfaces:**
- Consumes: `ReplicationHandle::{master_repl_offset, advance_master_repl_offset}` from Task 1; `crate::aof::encode_frame(&Frame) -> std::io::Result<Vec<u8>>` (`crates/server/src/aof.rs:48`).
- Produces: the invariant that `master_repl_offset` equals the summed encoded length of every frame this leader has broadcast. Consumed by Task 3, by `02-snapshot-offset-handoff.md`, and by `03-follower-replication-offset.md`'s convergence test.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/dispatcher.rs — add to the existing `mod tests`, directly after
// `info_lists_each_connected_slaves_advertised_address` (ends at :5149)
    /// The offset is a byte count of the replication stream this leader produced, so it must
    /// advance for a write even with no replica attached, must not advance for a read, and must
    /// accumulate both frames of a multi-frame write (`SET ... EX n` logs a flagless `SET` plus
    /// an absolute `PEXPIREAT`).
    #[test]
    fn writes_advance_the_master_replication_offset_with_no_replicas_attached() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        assert!(replication.registry.is_empty());
        assert_eq!(replication.master_repl_offset(), 0);

        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &Session::new(),
            1,
        );
        let one_set = crate::aof::encode_frame(&cmd(&[b"SET", b"k", b"v"]))
            .unwrap()
            .len() as u64;
        assert_eq!(
            replication.master_repl_offset(),
            one_set,
            "a write must advance the offset by exactly the bytes it broadcast"
        );

        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"GET", b"k"]),
            &Session::new(),
            1,
        );
        assert_eq!(
            replication.master_repl_offset(),
            one_set,
            "a read produces no replication stream and must not move the offset"
        );

        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"t", b"v", b"EX", b"10"]),
            &Session::new(),
            1,
        );
        // Two frames, so strictly more than one flagless SET's worth. The PEXPIREAT's exact
        // length depends on a wall-clock millisecond timestamp, so this asserts the accumulation
        // rather than a brittle exact total.
        assert!(
            replication.master_repl_offset() > one_set * 2,
            "SET with a TTL broadcasts two frames and must advance by both, got {}",
            replication.master_repl_offset()
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem dispatcher::tests::writes_advance_the_master_replication_offset`
Expected: FAIL — `assertion \`left == right\` failed: a write must advance the offset by exactly the bytes it broadcast`, with `left: 0`. The counter exists but nothing advances it yet.

- [ ] **Step 3: Advance the offset in the fan-out loop**

```rust
// crates/server/src/dispatcher.rs — replace the fan-out loop (:3245-3253) with this. The
// comment above the loop is extended, not replaced.
    // Broadcast while still holding the ordering guard: two writers to the same key must
    // broadcast in the same relative order their appends landed in, or followers permanently
    // diverge from the leader (see the comment above `to_broadcast`). Broadcasting is a mutex
    // acquisition plus N unbounded-channel sends -- it never blocks on I/O -- so this costs
    // nothing worth trading correctness for. Only now, with every append and every broadcast
    // for this command done, is the guard's work finished.
    for encoded in to_broadcast {
        // The replication offset advances here, under the same guard and in the same iteration
        // as the broadcast, so offset order matches fan-out order exactly. It counts the bytes
        // this leader produced, not the bytes anyone received, so it advances even when the
        // registry is empty -- that is what makes it comparable across a leader and a follower
        // that attached later. `encoded.len()` is read before the move into `broadcast`.
        replication.advance_master_repl_offset(encoded.len() as u64);
        replication.registry.broadcast(encoded);
    }
    drop(_order_guard);
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem dispatcher::tests::writes_advance_the_master_replication_offset`
Expected: PASS

- [ ] **Step 5: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "Advance the leader replication offset at the broadcast site"
```

---

### Task 3: surface it in `INFO REPLICATION` and Prometheus

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (`info_text`'s `role:master` branch at `:1916-1932`; the existing test `info_reports_connected_slaves_on_a_master` at `:5115-5125`; add one new test after `info_lists_each_connected_slaves_advertised_address`, which ends at `:5149`)
- Modify: `crates/server/src/metrics.rs` (`refresh_sampled_gauges` at `:50-62`; the existing test `the_metrics_endpoint_serves_the_rendered_registry_and_404s_everything_else` at `:151-212`)

**Interfaces:**
- Consumes: `ReplicationHandle::master_repl_offset()` from Task 1, kept live by Task 2.
- Produces: `INFO REPLICATION`'s `master_repl_offset:<n>` line on a leader, and the `rocket_mem_master_repl_offset` Prometheus gauge.

**Note on existing tests:** the three `contains`-based `info_text` assertions near `:5089`, `:5110` and `:5142` are *not* broken by adding a new line to the master branch — `slave{i}:...state=online\r\n` keeps its exact current spelling in this plan (it is `05-replica-ack-tracking.md` that extends it). Step 3 below still strengthens `info_reports_connected_slaves_on_a_master` with a positive assertion for the new key, in the same commit as the format change, so the new line is pinned rather than merely tolerated. Do not weaken any existing assertion.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — replace the existing `info_reports_connected_slaves_on_a_master`
// (:5115-5125) with this strengthened version
    #[test]
    fn info_reports_connected_slaves_on_a_master() {
        let engine = Engine::new();
        let replication = ReplicationHandle::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        replication.registry.register(None, tx);
        let text = info_text_for(&replication, &engine, &[b"replication"]);
        assert!(text.contains("role:master\r\n"), "{text}");
        assert!(text.contains("connected_slaves:1\r\n"), "{text}");
        assert!(text.contains("master_repl_offset:0\r\n"), "{text}");
        assert!(!text.contains("master_host:"), "{text}");
    }
```

```rust
// crates/server/src/dispatcher.rs — add to the existing `mod tests`, directly after
// `info_lists_each_connected_slaves_advertised_address` (ends at :5149)
    /// The reported offset is the live counter, not a placeholder -- and it is non-zero on a
    /// leader that has never had a replica attached, because it counts the write stream itself.
    #[test]
    fn info_reports_the_live_master_replication_offset() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();

        let before = info_text_for_writer(&replication, &engine, &aof, &[b"replication"]);
        assert!(before.contains("master_repl_offset:0\r\n"), "{before}");

        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &Session::new(),
            1,
        );

        let expected = crate::aof::encode_frame(&cmd(&[b"SET", b"k", b"v"]))
            .unwrap()
            .len();
        let after = info_text_for_writer(&replication, &engine, &aof, &[b"replication"]);
        assert!(
            after.contains(&format!("master_repl_offset:{expected}\r\n")),
            "{after}"
        );
    }
```

```rust
// crates/server/src/metrics.rs — add to the existing test
// `the_metrics_endpoint_serves_the_rendered_registry_and_404s_everything_else`, directly after
// the `assert!(body.contains("rocket_mem_memory_used_bytes"), "{body}");` line (:191)
        assert!(body.contains("rocket_mem_master_repl_offset"), "{body}");
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::info_reports_connected_slaves_on_a_master dispatcher::tests::info_reports_the_live_master_replication_offset metrics::tests::the_metrics_endpoint_serves`
Expected: all three FAIL — the two `info` tests on `assertion failed: text.contains("master_repl_offset:...")` because `info_text` emits no such line, and the metrics test on `assertion failed: body.contains("rocket_mem_master_repl_offset")` because no such gauge is registered.

- [ ] **Step 3: Emit the `INFO` line**

```rust
// crates/server/src/dispatcher.rs — replace the `else` (role:master) branch of `info_text`'s
// replication section (:1916-1932) with this. Only the trailing `master_repl_offset` push is
// new; everything above it is unchanged.
        } else {
            out.push_str("role:master\r\n");
            let addrs = replication.registry.addrs();
            out.push_str(&format!("connected_slaves:{}\r\n", addrs.len()));
            // One `slaveN:` line per connected replica, real Redis's format -- `ip`/`port` come
            // from the address the replica advertised in its own `PSYNC` (see
            // `ReplicationHandle::own_addr`'s doc comment), not this connection's ephemeral
            // source port. `ip=?,port=0` for a replica that advertised none (a bare `PSYNC`,
            // from an old client or a test) rather than silently omitting the line.
            for (i, addr) in addrs.iter().enumerate() {
                let (ip, port) = match addr {
                    Some(a) => split_addr(a),
                    None => ("?", 0),
                };
                out.push_str(&format!("slave{i}:ip={ip},port={port},state=online\r\n"));
            }
            // After the per-replica lines, matching real Redis's own field order. This is a
            // count of replication-stream bytes this leader has produced, so it is non-zero on
            // a leader that has taken writes even if no replica has ever connected.
            out.push_str(&format!(
                "master_repl_offset:{}\r\n",
                replication.master_repl_offset()
            ));
        }
```

- [ ] **Step 4: Add the Prometheus gauge**

```rust
// crates/server/src/metrics.rs — add to `refresh_sampled_gauges`, directly after the
// `rocket_mem_replication_last_apply_timestamp_seconds` gauge (:57-58)
    ::metrics::gauge!("rocket_mem_master_repl_offset").set(replication.master_repl_offset() as f64);
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests::info_reports_connected_slaves_on_a_master dispatcher::tests::info_reports_the_live_master_replication_offset metrics::tests::the_metrics_endpoint_serves`
Expected: PASS, all three

- [ ] **Step 6: Run the full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/dispatcher.rs crates/server/src/metrics.rs
git commit -m "Report the leader replication offset in INFO and Prometheus"
```

---

## Next plan

[`02-snapshot-offset-handoff.md`](02-snapshot-offset-handoff.md) — carry this leader offset to a newly-attaching follower through the snapshot's existing 8-byte header, with no wire-format change.
