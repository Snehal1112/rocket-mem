# Replica-Fencing Observability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** an operator can see fencing working without reading source: a `rocket_mem_good_replicas` gauge always reports the live count (whether or not fencing is enabled), a `rocket_mem_replica_min_ack_offset` gauge exposes the furthest-behind connected replica's acked offset (which, paired with plan 01's `rocket_mem_master_repl_offset`, makes replication lag in bytes answerable from Prometheus instead of by parsing `INFO` text), a `rocket_mem_writes_rejected_no_replicas_total` counter tracks how many writes fencing has refused, a log line fires on each transition into and out of the fenced state (never one per rejected write — that would flood the log under a sustained outage), and `.claude/manual-testing.md` shows how to exercise all of it by hand.

**Architecture:** both gauges are sampled metrics, following `metrics::refresh_sampled_gauges`'s existing pattern (`.set()` from a live read, refreshed at every `/metrics` scrape). `rocket_mem_good_replicas` reports `registry.good_replicas(replication.min_replicas_max_lag())`; `rocket_mem_replica_min_ack_offset` reports the minimum `ack_offset` across `registry.states()`, defaulting to `0` when the vector is empty (no replicas connected). Both are reported unconditionally, since "how many replicas are currently good" and "how far behind is the furthest replica" are useful even with fencing off. The counter and the transition log line, by contrast, are event-driven and belong at the point the event actually happens: inside `08-fencing-enforcement.md`'s gate in `dispatch_and_log_inner`, not in the scrape-driven gauge refresh — tying a log line's timing to whether anyone happens to be scraping `/metrics` would make the log silent for a deployment with no Prometheus at all. A new `fenced: AtomicBool` field on `ReplicationHandle`, flipped via `swap` inside the gate, gives an exact one-log-per-edge guarantee and a testable `is_fenced()` accessor without needing to capture `tracing` output in a test (this codebase has no log-capture test infrastructure today).

**Tech Stack:** nothing new — `::metrics::gauge!`/`::metrics::counter!` and `tracing::warn!`/`tracing::info!` are both already used throughout `crates/server/src`.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md)

## Global Constraints

- [`00-design-contract.md`](00-design-contract.md) is normative. §2.4's metric table fixes `rocket_mem_good_replicas`, `rocket_mem_replica_min_ack_offset` (both gauges), and `rocket_mem_writes_rejected_no_replicas_total` (counter, `_total` suffix) exactly, and states **no per-replica metric labels** — all three are aggregate values only.
- `rocket_mem_replica_min_ack_offset` semantics, pinned by the coordinator and not to be "improved" later: with no replicas connected it reports `0`, never `master_repl_offset` or another sentinel — a `0` alongside `rocket_mem_connected_replicas == 0` is unambiguous, and any nonzero value here would wrongly imply a replica exists. A connected replica that has never acked has `ack_offset == 0` and must **not** be filtered out of the minimum — an un-acked replica IS maximally behind as far as the leader can prove, and filtering it out would report healthy lag while a silent replica falls arbitrarily far behind, exactly the blind spot this metric exists to close.
- This plan relies on `ReplicaRegistry::good_replicas(&self, max_lag: std::time::Duration) -> usize` and `ReplicaRegistry::states(&self) -> Vec<ReplicaState>` (landed by an earlier plan in this chain), and on `ReplicationHandle::min_replicas_to_write()`/`min_replicas_max_lag()` and the `NOREPLICAS` gate (`08-fencing-enforcement.md`). This plan does not re-implement or re-verify any of those — it only adds observability on top.
- **One log line per transition, never per rejected write.** A sustained outage where every write is refused must produce exactly one `entering fenced state` line, not one per request. Verify this by asserting the underlying `is_fenced()` flip, not by counting log lines (no log-capture infrastructure exists in this codebase to do the latter).
- Metric conventions, from `00-design-contract.md` §1.9: `::metrics::gauge!("rocket_mem_x").set(v as f64)`, `::metrics::counter!("rocket_mem_x_total").increment(1)`. Sampled gauges are refreshed in `metrics::refresh_sampled_gauges`.
- Comment style: short, easy, full sentences ending in punctuation. No emojis.
- The three CI gates must be clean before every commit:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```

---

### Task 1: `rocket_mem_good_replicas` and `rocket_mem_replica_min_ack_offset` gauges

**Files:**
- Modify: `crates/server/src/metrics.rs`

**Interfaces:**
- Consumes: `ReplicaRegistry::good_replicas(&self, max_lag: std::time::Duration) -> usize` and `ReplicaRegistry::states(&self) -> Vec<ReplicaState>` (`ReplicaState` fields `addr`, `ack_offset`, `last_ack_unix`) (both landed by an earlier plan in this chain); `ReplicationHandle::min_replicas_max_lag(&self) -> std::time::Duration` (`08-fencing-enforcement.md`).
- Produces: the `rocket_mem_good_replicas` and `rocket_mem_replica_min_ack_offset` gauges, both refreshed at every scrape. No other plan consumes either directly — they're read by whatever scrapes `/metrics`. Paired with plan 01's `rocket_mem_master_repl_offset`, `rocket_mem_replica_min_ack_offset` makes replication lag in bytes computable from Prometheus alone.

- [ ] **Step 1: Write the failing tests**

Add these tests to `crates/server/src/metrics.rs`'s `mod tests`, after `recorder_handle_is_idempotent_and_renders_what_was_recorded`. Each constructs its own `ReplicationHandle` and calls `refresh_sampled_gauges` directly (not through `serve_metrics`'s HTTP endpoint) — the same reason the pre-existing `recorder_handle_is_idempotent_and_renders_what_was_recorded` test above it does: the process-wide recorder is shared across the whole test binary, so asserting an exact value is only safe for a metric this test itself just set, read back immediately, with nothing else in between that could interleave a render.

```rust
#[test]
fn refresh_sampled_gauges_reports_good_replicas() {
    let handle = recorder_handle();
    let engine = std::sync::Arc::new(engine::Engine::new());
    let replication = std::sync::Arc::new(
        crate::replication::ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            "/tmp/unused.snapshot".into(),
        )
        .with_min_replicas(1, std::time::Duration::from_secs(10)),
    );

    refresh_sampled_gauges(&engine, &replication);
    let rendered = handle.render();
    assert!(
        rendered.contains("rocket_mem_good_replicas 0"),
        "expected 0 good replicas with none connected:\n{rendered}"
    );
}

#[test]
fn refresh_sampled_gauges_reports_replica_min_ack_offset_as_zero_with_no_replicas() {
    let handle = recorder_handle();
    let engine = std::sync::Arc::new(engine::Engine::new());
    let replication = std::sync::Arc::new(crate::replication::ReplicationHandle::new(
        std::sync::Arc::clone(&engine),
        "/tmp/unused.snapshot".into(),
    ));

    refresh_sampled_gauges(&engine, &replication);
    let rendered = handle.render();
    assert!(
        rendered.contains("rocket_mem_replica_min_ack_offset 0"),
        "expected 0 with no replicas connected, not master_repl_offset or a sentinel:\n{rendered}"
    );
}

#[test]
fn refresh_sampled_gauges_reports_replica_min_ack_offset_for_one_acked_replica() {
    let handle = recorder_handle();
    let engine = std::sync::Arc::new(engine::Engine::new());
    let replication = std::sync::Arc::new(crate::replication::ReplicationHandle::new(
        std::sync::Arc::clone(&engine),
        "/tmp/unused.snapshot".into(),
    ));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
    let entry = replication
        .registry
        .register(Some("127.0.0.1:1".to_string()), tx);
    entry
        .ack_offset
        .store(150, std::sync::atomic::Ordering::Relaxed);

    refresh_sampled_gauges(&engine, &replication);
    let rendered = handle.render();
    assert!(
        rendered.contains("rocket_mem_replica_min_ack_offset 150"),
        "expected the single replica's own ack_offset:\n{rendered}"
    );
}

#[test]
fn refresh_sampled_gauges_reports_the_lower_of_two_replicas_ack_offsets() {
    let handle = recorder_handle();
    let engine = std::sync::Arc::new(engine::Engine::new());
    let replication = std::sync::Arc::new(crate::replication::ReplicationHandle::new(
        std::sync::Arc::clone(&engine),
        "/tmp/unused.snapshot".into(),
    ));
    let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
    let entry1 = replication
        .registry
        .register(Some("127.0.0.1:1".to_string()), tx1);
    entry1
        .ack_offset
        .store(500, std::sync::atomic::Ordering::Relaxed);
    let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
    let entry2 = replication
        .registry
        .register(Some("127.0.0.1:2".to_string()), tx2);
    entry2
        .ack_offset
        .store(200, std::sync::atomic::Ordering::Relaxed);

    refresh_sampled_gauges(&engine, &replication);
    let rendered = handle.render();
    assert!(
        rendered.contains("rocket_mem_replica_min_ack_offset 200"),
        "expected the lower of the two replicas' ack_offsets to win:\n{rendered}"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib metrics::tests::refresh_sampled_gauges_reports_good_replicas metrics::tests::refresh_sampled_gauges_reports_replica_min_ack_offset -- --nocapture`
Expected: FAIL — the assertions fail because neither `rocket_mem_good_replicas` nor `rocket_mem_replica_min_ack_offset` appears in the rendered output (neither metric exists yet). If `08-fencing-enforcement.md`'s `with_min_replicas` isn't merged yet, this instead FAILs to compile.

- [ ] **Step 3: Add both gauges**

In `crates/server/src/metrics.rs`, `refresh_sampled_gauges` currently reads:

```rust
pub fn refresh_sampled_gauges(engine: &Engine, replication: &ReplicationHandle) {
    let (keys, with_expiry) = engine.key_counts();
    ::metrics::gauge!("rocket_mem_keys").set(keys as f64);
    ::metrics::gauge!("rocket_mem_keys_with_expiry").set(with_expiry as f64);
    ::metrics::gauge!("rocket_mem_memory_used_bytes").set(engine.memory_used() as f64);
    ::metrics::gauge!("rocket_mem_connected_clients").set(replication.connected_clients() as f64);
    ::metrics::gauge!("rocket_mem_connected_replicas").set(replication.registry.len() as f64);
    ::metrics::gauge!("rocket_mem_replication_last_apply_timestamp_seconds")
        .set(replication.last_apply_unix() as f64);
    ::metrics::counter!("rocket_mem_evicted_keys_total").absolute(engine.eviction_count() as u64);
    ::metrics::counter!("rocket_mem_expired_keys_total").absolute(replication.expired_keys());
    ::metrics::counter!("rocket_mem_connections_total").absolute(replication.total_connections());
}
```

Change it to add the two new gauges (order doesn't matter; placed next to the other replication-related gauges):

```rust
pub fn refresh_sampled_gauges(engine: &Engine, replication: &ReplicationHandle) {
    let (keys, with_expiry) = engine.key_counts();
    ::metrics::gauge!("rocket_mem_keys").set(keys as f64);
    ::metrics::gauge!("rocket_mem_keys_with_expiry").set(with_expiry as f64);
    ::metrics::gauge!("rocket_mem_memory_used_bytes").set(engine.memory_used() as f64);
    ::metrics::gauge!("rocket_mem_connected_clients").set(replication.connected_clients() as f64);
    ::metrics::gauge!("rocket_mem_connected_replicas").set(replication.registry.len() as f64);
    ::metrics::gauge!("rocket_mem_replication_last_apply_timestamp_seconds")
        .set(replication.last_apply_unix() as f64);
    // Reported unconditionally, whether or not fencing is enabled -- "how many replicas are
    // currently good" is useful information on its own, and it's the exact input the
    // NOREPLICAS gate in dispatch_and_log_inner compares against min_replicas_to_write.
    ::metrics::gauge!("rocket_mem_good_replicas").set(
        replication
            .registry
            .good_replicas(replication.min_replicas_max_lag()) as f64,
    );
    // The furthest-behind connected replica's acked offset -- paired with
    // rocket_mem_master_repl_offset (plan 01), this makes replication lag in bytes computable
    // without parsing INFO text. Reported unconditionally, exactly like rocket_mem_good_replicas
    // above: observability must not depend on whether the operator opted into fencing. Defaults
    // to 0 with no replicas connected -- never master_repl_offset or another sentinel, since a 0
    // here alongside rocket_mem_connected_replicas == 0 is unambiguous, and any nonzero value
    // would wrongly imply a replica exists. A replica that has never acked has ack_offset == 0,
    // which correctly drags this minimum to 0 and must NOT be filtered out: an un-acked replica
    // IS maximally behind as far as the leader can prove, and filtering it out would report
    // healthy lag while a silent replica falls arbitrarily far behind -- exactly the blind spot
    // this metric exists to close.
    let min_ack_offset = replication
        .registry
        .states()
        .iter()
        .map(|s| s.ack_offset)
        .min()
        .unwrap_or(0);
    ::metrics::gauge!("rocket_mem_replica_min_ack_offset").set(min_ack_offset as f64);
    ::metrics::counter!("rocket_mem_evicted_keys_total").absolute(engine.eviction_count() as u64);
    ::metrics::counter!("rocket_mem_expired_keys_total").absolute(replication.expired_keys());
    ::metrics::counter!("rocket_mem_connections_total").absolute(replication.total_connections());
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib metrics:: -- --nocapture`
Expected: all PASS, including `the_metrics_endpoint_serves_the_rendered_registry_and_404s_everything_else` and the other pre-existing test.

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --lib metrics::`
Expected: all green.

```bash
git add crates/server/src/metrics.rs
git commit -m "$(cat <<'EOF'
Add rocket_mem_good_replicas and rocket_mem_replica_min_ack_offset

Both reported unconditionally at every /metrics scrape, whether or
not fencing is enabled. rocket_mem_good_replicas is the same value
the NOREPLICAS gate compares against min_replicas_to_write, so an
operator can watch it approach the threshold before a write is ever
actually refused. rocket_mem_replica_min_ack_offset is the furthest-
behind connected replica's acked offset -- paired with the existing
rocket_mem_master_repl_offset, it makes replication lag in bytes
computable from Prometheus alone. A replica that has never acked
counts as offset 0 and is deliberately not filtered out of the
minimum: it is maximally behind as far as the leader can prove.
EOF
)"
```

---

### Task 2: `rocket_mem_writes_rejected_no_replicas_total` and fenced-state transition logging

**Files:**
- Modify: `crates/server/src/replication.rs`
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: the `NOREPLICAS` gate in `dispatch_and_log_inner` (`08-fencing-enforcement.md`); `ReplicaRegistry::register(&self, addr: Option<String>, sender: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>) -> Arc<ReplicaEntry>` and `ReplicaEntry`'s public `ack_offset: AtomicU64`/`last_ack_unix: AtomicI64` fields (landed by an earlier plan in this chain) — used only by this task's own test to simulate a replica's ack.
- Produces: `ReplicationHandle::is_fenced(&self) -> bool`, the `rocket_mem_writes_rejected_no_replicas_total` counter, and the transition log lines. No later plan in this folder consumes these directly.

- [ ] **Step 1: Write the failing tests**

Add these tests to `crates/server/src/dispatcher.rs`'s `mod tests`, immediately after `a_write_command_is_rejected_with_noreplicas_when_fencing_is_enabled_and_no_replica_has_acked`:

```rust
#[test]
fn a_rejected_write_flips_is_fenced_to_true() {
    let engine = std::sync::Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = ReplicationHandle::new(
        std::sync::Arc::clone(&engine),
        "/tmp/unused.snapshot".into(),
    )
    .with_min_replicas(1, std::time::Duration::from_secs(10));

    assert!(!replication.is_fenced(), "must start out not fenced");

    dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"SET", b"k", b"v"]),
        &Session::new(),
        1,
    );
    assert!(replication.is_fenced());
}

#[test]
fn leaving_the_fenced_state_after_a_replica_starts_acking_flips_is_fenced_back_to_false() {
    let engine = std::sync::Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = ReplicationHandle::new(
        std::sync::Arc::clone(&engine),
        "/tmp/unused.snapshot".into(),
    )
    .with_min_replicas(1, std::time::Duration::from_secs(10));

    // No replica connected yet -- this write enters the fenced state.
    let rejected = dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"SET", b"k", b"v"]),
        &Session::new(),
        1,
    );
    assert_eq!(
        rejected,
        Frame::Error("NOREPLICAS Not enough good replicas to write.".into())
    );
    assert!(replication.is_fenced());

    // A replica registers and acks recently enough to count as "good".
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let entry = replication
        .registry
        .register(Some("127.0.0.1:1".to_string()), tx);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    entry
        .last_ack_unix
        .store(now, std::sync::atomic::Ordering::Relaxed);
    entry.ack_offset.store(100, std::sync::atomic::Ordering::Relaxed);

    let accepted = dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"SET", b"k", b"v2"]),
        &Session::new(),
        1,
    );
    assert_eq!(accepted, Frame::Simple("OK".into()));
    assert!(
        !replication.is_fenced(),
        "must leave the fenced state once a replica is good"
    );
}

#[test]
fn a_rejected_write_increments_the_writes_rejected_no_replicas_counter() {
    let engine = std::sync::Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = ReplicationHandle::new(
        std::sync::Arc::clone(&engine),
        "/tmp/unused.snapshot".into(),
    )
    .with_min_replicas(1, std::time::Duration::from_secs(10));

    let handle = crate::metrics::recorder_handle();
    dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"SET", b"k", b"v"]),
        &Session::new(),
        1,
    );
    let rendered = handle.render();
    assert!(
        rendered.contains("rocket_mem_writes_rejected_no_replicas_total"),
        "counter missing from render:\n{rendered}"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::a_rejected_write_flips_is_fenced_to_true dispatcher::tests::leaving_the_fenced_state_after_a_replica_starts_acking_flips_is_fenced_back_to_false dispatcher::tests::a_rejected_write_increments_the_writes_rejected_no_replicas_counter -- --nocapture`
Expected: FAIL to compile — `no method named \`is_fenced\` found for struct \`ReplicationHandle\``.

- [ ] **Step 3: Add the `fenced` field and `is_fenced` accessor**

In `crates/server/src/replication.rs`, add one field to the `ReplicationHandle` struct definition (after `min_replicas_max_lag: std::time::Duration,`, added in `08-fencing-enforcement.md`):

```rust
    /// Whether this node is currently refusing writes with `NOREPLICAS`. Flipped by
    /// `dispatch_and_log_inner`'s fencing gate on every write attempt while `min_replicas_to_write`
    /// is nonzero, so a transition can be logged exactly once per edge instead of once per
    /// rejected write. `false` -- the default -- for every handle with fencing disabled. `pub`,
    /// not accessed only through `is_fenced()` below, matching `is_replica`'s existing pattern
    /// just above it in this struct: `dispatch_and_log_inner` needs to `swap` it directly (an
    /// atomic read-modify-write, not a separate load-then-store), and it lives in a sibling
    /// module (`dispatcher.rs`), so a plain private field would not be visible there at all --
    /// Rust's field privacy is scoped to the defining module and its descendants, not the whole
    /// crate. A plain field, not `Arc<AtomicBool>`, for the same reason `is_replica` isn't: the
    /// whole handle is already behind one `Arc` wherever it's shared.
    pub fenced: std::sync::atomic::AtomicBool,
```

Add it to `ReplicationHandle::new`'s constructor body (after `min_replicas_max_lag: std::time::Duration::from_secs(10),`):

```rust
            fenced: std::sync::atomic::AtomicBool::new(false),
```

Add the accessor after `min_replicas_max_lag`'s accessor:

```rust
    /// Whether this node is currently in the fenced state (refusing writes with `NOREPLICAS`).
    pub fn is_fenced(&self) -> bool {
        self.fenced.load(std::sync::atomic::Ordering::Relaxed)
    }
```

- [ ] **Step 4: Add the counter and transition logging to the gate**

In `crates/server/src/dispatcher.rs`, the fencing gate added in `08-fencing-enforcement.md` currently reads:

```rust
    // `min-replicas-to-write` self-fencing (design contract §2.5). Checked immediately after the
    // READONLY gate above -- a replica must still answer READONLY first, which it already has by
    // the time execution reaches here, so there is no need to re-check `is_replica` -- and before
    // every other interception and the write path's AOF ordering lock further down: a rejected
    // write must never touch that lock, since it never gets far enough to log or broadcast
    // anything. `min_replicas_to_write() == 0` short-circuits the common case (every deployment
    // before this feature existed, and every deployment that hasn't opted in) without touching
    // `registry.good_replicas`, which takes the registry's mutex.
    if replication.min_replicas_to_write() > 0
        && extract_write_command_name(&frame).is_some()
        && (replication
            .registry
            .good_replicas(replication.min_replicas_max_lag()) as u64)
            < replication.min_replicas_to_write()
    {
        return Frame::Error("NOREPLICAS Not enough good replicas to write.".into());
    }
```

Change it to track the fenced-state transition and increment the counter on rejection:

```rust
    // `min-replicas-to-write` self-fencing (design contract §2.5). Checked immediately after the
    // READONLY gate above -- a replica must still answer READONLY first, which it already has by
    // the time execution reaches here, so there is no need to re-check `is_replica` -- and before
    // every other interception and the write path's AOF ordering lock further down: a rejected
    // write must never touch that lock, since it never gets far enough to log or broadcast
    // anything. `min_replicas_to_write() == 0` short-circuits the common case (every deployment
    // before this feature existed, and every deployment that hasn't opted in) without touching
    // `registry.good_replicas`, which takes the registry's mutex.
    if replication.min_replicas_to_write() > 0 && extract_write_command_name(&frame).is_some() {
        let good = replication
            .registry
            .good_replicas(replication.min_replicas_max_lag());
        let is_fenced = (good as u64) < replication.min_replicas_to_write();
        // `swap`, not a separate load-then-store: this runs on every write attempt while fencing
        // is enabled, potentially from many concurrent connections, and a torn read-then-write
        // here could double-log a single transition under concurrency.
        let was_fenced = replication
            .fenced
            .swap(is_fenced, std::sync::atomic::Ordering::Relaxed);
        if is_fenced && !was_fenced {
            tracing::warn!(
                good_replicas = good,
                min_replicas_to_write = replication.min_replicas_to_write(),
                "entering fenced state: not enough good replicas, refusing writes with NOREPLICAS"
            );
        } else if !is_fenced && was_fenced {
            tracing::info!(
                good_replicas = good,
                min_replicas_to_write = replication.min_replicas_to_write(),
                "leaving fenced state: enough good replicas acked, accepting writes again"
            );
        }
        if is_fenced {
            ::metrics::counter!("rocket_mem_writes_rejected_no_replicas_total").increment(1);
            return Frame::Error("NOREPLICAS Not enough good replicas to write.".into());
        }
    }
```

This compiles only because Step 3 declared `fenced` as `pub`: `dispatcher.rs` is a sibling module to `replication.rs`, not a descendant of it, and Rust's field-privacy is scoped to the defining module and its descendants -- a plain private field would not be visible here at all, regardless of both modules living in the same crate. This mirrors how the pre-existing `READONLY` gate reads `replication.is_replica` directly a few lines above, which is `pub` for the exact same reason.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher:: replication:: -- --nocapture`
Expected: all PASS, including every pre-existing test in both modules.

- [ ] **Step 6: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --lib`
Expected: all green.

```bash
git add crates/server/src/replication.rs crates/server/src/dispatcher.rs
git commit -m "$(cat <<'EOF'
Log fenced-state transitions and count rejected writes

Adds ReplicationHandle::is_fenced (backed by a new AtomicBool,
flipped via swap on every write attempt while fencing is enabled) so
the NOREPLICAS gate logs exactly one line per transition into or out
of the fenced state, never one per rejected write -- which would
flood the log under a sustained outage. Also adds the
rocket_mem_writes_rejected_no_replicas_total counter, incremented
alongside each rejection.
EOF
)"
```

---

### Task 3: manual-testing guide

**Files:**
- Modify: `.claude/manual-testing.md`

**Interfaces:**
- Consumes: `min_replicas_to_write`/`min_replicas_max_lag_secs` config fields (`07-fencing-config.md`); the `NOREPLICAS` error, `rocket_mem_good_replicas`/`rocket_mem_writes_rejected_no_replicas_total` metrics, and the transition log lines (Tasks 1-2 of this plan).
- Produces: nothing — pure documentation, the last task in this plan.

- [ ] **Step 1: Add a "Replica fencing" section**

In `.claude/manual-testing.md`, insert a new section after `## Replication (\`REPLICAOF\`)` and before `## Cluster mode`:

```markdown
## Replica fencing (`min-replicas-to-write`)

`min_replicas_to_write`/`min_replicas_max_lag_secs` make a leader refuse client writes with a
`NOREPLICAS` error unless enough replicas have acked recently enough. Disabled by default
(`min_replicas_to_write=0`) -- every command below except the last one only changes what happens
once you opt in.

```bash
# leader, fencing enabled: needs at least 1 replica acked within the last 10s to accept writes
ROCKET_MEM_ADDR=127.0.0.1:6400 ROCKET_MEM_AOF_PATH=/tmp/rm-leader.aof \
ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-leader.snap ROCKET_MEM_METRICS_ADDR=127.0.0.1:9200 \
ROCKET_MEM_RMP_ADDR=127.0.0.1:6480 \
ROCKET_MEM_MIN_REPLICAS_TO_WRITE=1 ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS=10 \
  ./target/release/rocket-mem &

redis-cli -p 6400 set k v                # -> (error) NOREPLICAS Not enough good replicas to write.

# follower
ROCKET_MEM_ADDR=127.0.0.1:6401 ROCKET_MEM_AOF_PATH=/tmp/rm-follower.aof \
ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-follower.snap ROCKET_MEM_METRICS_ADDR=127.0.0.1:9201 \
ROCKET_MEM_RMP_ADDR=127.0.0.1:6481 \
  ./target/release/rocket-mem &

redis-cli -p 6401 replicaof 127.0.0.1 6400
sleep 2                                   # let it connect and send its first REPLCONF ACK

redis-cli -p 6400 set k v                # -> OK, now that a replica has acked within the lag window
redis-cli -p 6400 info replication       # slave0:...,offset=<n>,lag=<secs>

curl -s http://127.0.0.1:9200/metrics | grep -E 'rocket_mem_good_replicas|rocket_mem_writes_rejected_no_replicas_total'

kill %1 %2
```

The leader's stderr shows the fenced-state transitions: a `WARN ... entering fenced state` line
right after startup (before the follower has acked), and an `INFO ... leaving fenced state` line
once the first ack arrives -- exactly one of each, not one per rejected `SET`. Set
`RUST_LOG=rocket_mem=info` (or `debug`) when starting the leader if you don't see them at the
default filter.

The zero-lag startup guard from `07-fencing-config.md` rejects a config that could never satisfy
itself:

```bash
ROCKET_MEM_MIN_REPLICAS_TO_WRITE=1 ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS=0 ./target/release/rocket-mem
# -> config error: min_replicas_to_write is set but min_replicas_max_lag_secs is 0 -- no replica
#    could ever qualify, so every write would be refused forever
```
```

- [ ] **Step 2: Verify**

Run: `grep -n "min-replicas-to-write\|NOREPLICAS\|rocket_mem_good_replicas" .claude/manual-testing.md`
Expected: multiple matches, all inside the new "Replica fencing" section.

- [ ] **Step 3: Commit**

```bash
git add .claude/manual-testing.md
git commit -m "$(cat <<'EOF'
Document how to exercise replica fencing by hand

Adds a "Replica fencing" section to the manual-testing guide: a
leader/follower pair showing the NOREPLICAS error before a replica
acks, OK after, the good-replicas/rejected-writes metrics, the
fenced-state transition log lines, and the zero-lag startup guard.
EOF
)"
```

---

## Next plan

[`10-replication-health-probe.md`](10-replication-health-probe.md) — chain C (spec step 3): a manual promotion runbook plus a health-probe/alerting script, using the offsets from chain A (plans 01-06) and the fencing behavior from this chain (07-09) to help an operator pick the right replica during a manual failover, without any of it acting automatically.
