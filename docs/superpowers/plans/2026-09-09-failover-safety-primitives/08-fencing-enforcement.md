# Replica-Fencing Enforcement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a leader with `min_replicas_to_write > 0` refuses client writes with a new `NOREPLICAS` error whenever fewer than that many replicas have acked within `min_replicas_max_lag_secs` — the "self-fencing" primitive that bounds split-brain (design contract §0, §2.5). `07-fencing-config.md` made the two thresholds loadable; this plan makes them load-bearing.

**Architecture:** `dispatch_and_log_inner` receives `&ReplicationHandle`, never `&Config` (it has no way to reach the config layer at all), so the thresholds must live on the handle. `ReplicationHandle` gains a `with_min_replicas(self, to_write: u64, max_lag: std::time::Duration) -> Self` builder (design contract §2.4's fixed name) plus two private fields and accessor methods, mirroring the existing `with_slowlog_threshold`/`slowlog` pattern. The gate itself is a new `if` block in `dispatch_and_log_inner`, placed immediately after the existing `-READONLY` check (`crates/server/src/dispatcher.rs:3066-3076`) and before every other command interception — in particular before `write_name`/`_order_guard` are computed further down, so a rejected write never touches the AOF ordering lock. `main.rs` calls the new builder with `config.min_replicas_to_write`/`config.min_replicas_max_lag_secs`, converted to a `Duration`.

**Tech Stack:** nothing new — `ReplicaRegistry::good_replicas`, already landed by an earlier plan in this chain (`04`/`05`), is the only new dependency, and it's a plain method call.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md)

## Global Constraints

- [`00-design-contract.md`](00-design-contract.md) is normative. §2.4 fixes `with_min_replicas`'s exact signature. §2.5 fixes the fencing semantics precisely — reread it before writing the gate:
  > A write is refused with `NOREPLICAS` when **all** of: `min_replicas_to_write > 0`, and this node is not a replica (a replica returns `READONLY` first — the existing gate wins), and `extract_write_command_name(&frame).is_some()`, and `registry.good_replicas(max_lag) < min_replicas_to_write`.
  Note the second condition needs no explicit code: this gate sits *after* the `READONLY` gate, so any write attempt from a node with `is_replica == true` has already returned `READONLY` and never reaches this code at all — same structural argument the `READONLY` gate itself uses relative to `cluster_redirect`.
- **Gate ordering is load-bearing and must be commented in-source**: after `READONLY` (a replica must still answer `READONLY` first), before the write path takes the AOF ordering lock (a rejected write must never touch that lock — it never gets far enough to log or broadcast anything anyway).
- This plan relies on interfaces landed by earlier plans in this chain (`01`-`06`), which this plan does not implement or re-verify:
  - `ReplicaRegistry::good_replicas(&self, max_lag: std::time::Duration) -> usize` — replicas that acked within `max_lag`. A replica that has never acked is **not** good.
  - `ReplicationHandle::master_repl_offset(&self) -> u64` (not used directly by this plan, but confirms the offset machinery this fencing check assumes exists is already in place).
- **Error string, exact:** `"NOREPLICAS Not enough good replicas to write."` — matches Redis exactly, house style (`READONLY`/`WRONGPASS`): all-caps prefix, space, capitalized sentence, terminal period. Do not paraphrase it.
- `min_replicas_to_write = 0` (the default from `07-fencing-config.md`) must disable fencing entirely: every existing test, and every deployment that hasn't opted in, must see zero behavior change.
- This plan does **not** add the `rocket_mem_writes_rejected_no_replicas_total` counter or the fenced-state transition log lines — those are `09-fencing-observability.md`'s job. Keep this plan's gate minimal: check thresholds, return the error, nothing else.
- The three CI gates must be clean before every commit:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
- Comment style: short, easy, full sentences ending in punctuation. No emojis.

---

### Task 1: `ReplicationHandle::with_min_replicas` and accessors

**Files:**
- Modify: `crates/server/src/replication.rs`

**Interfaces:**
- Consumes: nothing new from earlier plans in this chain — this task only adds fields and a builder to `ReplicationHandle` itself.
- Produces: `pub fn with_min_replicas(self, to_write: u64, max_lag: std::time::Duration) -> Self` (design contract §2.4, exact name), and `pub fn min_replicas_to_write(&self) -> u64` / `pub fn min_replicas_max_lag(&self) -> std::time::Duration` accessors, following the existing `last_apply_unix()`/`connected_clients()`/`total_connections()` accessor pattern for private fields. Task 2 (the dispatcher gate) and `09-fencing-observability.md` both call these accessors.

- [ ] **Step 1: Write the failing tests**

Add these tests to `crates/server/src/replication.rs`'s `mod tests` (alongside the other `ReplicationHandle`-constructing tests):

```rust
#[test]
fn a_new_handle_has_fencing_disabled_by_default() {
    let engine = std::sync::Arc::new(Engine::new());
    let replication = ReplicationHandle::new(engine, "/tmp/unused.snapshot".into());
    assert_eq!(
        replication.min_replicas_to_write(),
        0,
        "fencing must be off for every existing ReplicationHandle::new call site"
    );
}

#[test]
fn with_min_replicas_sets_both_thresholds() {
    let engine = std::sync::Arc::new(Engine::new());
    let replication = ReplicationHandle::new(engine, "/tmp/unused.snapshot".into())
        .with_min_replicas(2, std::time::Duration::from_secs(5));
    assert_eq!(replication.min_replicas_to_write(), 2);
    assert_eq!(
        replication.min_replicas_max_lag(),
        std::time::Duration::from_secs(5)
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib replication::tests::a_new_handle_has_fencing_disabled_by_default replication::tests::with_min_replicas_sets_both_thresholds -- --nocapture`
Expected: FAIL to compile — `no method named \`min_replicas_to_write\` found for struct \`ReplicationHandle\``, `no method named \`with_min_replicas\` found for struct \`ReplicationHandle\``.

- [ ] **Step 3: Add the fields, builder, and accessors**

In `crates/server/src/replication.rs`, add two fields to the `ReplicationHandle` struct definition (after `own_addr: Option<String>,`, the last field):

```rust
    /// `min-replicas-to-write` self-fencing threshold (design contract §2.5): the minimum number
    /// of replicas that must have acked within `min_replicas_max_lag` for this node to accept a
    /// client write. `0` -- the default for `new`/`Default` -- disables fencing entirely, matching
    /// every deployment before this feature existed. Set via `with_min_replicas`; read by
    /// `dispatch_and_log_inner`'s `NOREPLICAS` gate.
    min_replicas_to_write: u64,
    /// How long a replica's last ack may be and still count as "good" for `min_replicas_to_write`.
    /// Irrelevant while `min_replicas_to_write` is `0`.
    min_replicas_max_lag: std::time::Duration,
```

Add the same two fields to `ReplicationHandle::new`'s constructor body (after `own_addr: None,`):

```rust
            min_replicas_to_write: 0,
            min_replicas_max_lag: std::time::Duration::from_secs(10),
```

Add the builder and accessors after `with_acl_bootstrap` (and before the doc comment for whichever method currently follows it):

```rust
    /// Configures `min-replicas-to-write` self-fencing thresholds, read by the `NOREPLICAS` gate
    /// in `dispatch_and_log_inner`. `to_write == 0` disables fencing entirely -- the default via
    /// `new`/`Default` -- matching every deployment before this feature existed. `main.rs` calls
    /// this with `config.min_replicas_to_write`/`config.min_replicas_max_lag_secs`; the ~25
    /// existing `ReplicationHandle::new` call sites (all tests) stay untouched by leaving this
    /// unset, which keeps fencing off for them.
    pub fn with_min_replicas(mut self, to_write: u64, max_lag: std::time::Duration) -> Self {
        self.min_replicas_to_write = to_write;
        self.min_replicas_max_lag = max_lag;
        self
    }

    /// The configured `min-replicas-to-write` threshold; `0` means fencing is off.
    pub fn min_replicas_to_write(&self) -> u64 {
        self.min_replicas_to_write
    }

    /// The configured lag window a replica's ack must fall within to count as "good".
    pub fn min_replicas_max_lag(&self) -> std::time::Duration {
        self.min_replicas_max_lag
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib replication:: -- --nocapture`
Expected: all PASS, including every pre-existing `replication::tests` test (the two new fields' initialization in `new` must not disturb any of them).

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --lib replication::`
Expected: all green.

```bash
git add crates/server/src/replication.rs
git commit -m "$(cat <<'EOF'
Add ReplicationHandle::with_min_replicas fencing thresholds

Stores min_replicas_to_write/min_replicas_max_lag on the handle,
since dispatch_and_log_inner only has access to &ReplicationHandle,
never &Config. Defaults to fencing off (to_write == 0), matching
every existing ReplicationHandle::new call site. No gate reads these
yet -- that's the next commit.
EOF
)"
```

---

### Task 2: the `NOREPLICAS` gate in `dispatch_and_log_inner`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `ReplicationHandle::min_replicas_to_write()`, `ReplicationHandle::min_replicas_max_lag()` (Task 1); `ReplicaRegistry::good_replicas(&self, max_lag: std::time::Duration) -> usize` (landed by an earlier plan in this chain, `04`/`05` — replicas that acked within `max_lag`; a replica that has never acked is not good).
- Produces: the `NOREPLICAS` write gate itself. No other plan calls it directly — it's reached only through `dispatch_and_log`/`dispatch_and_log_inner`.

- [ ] **Step 1: Write the failing tests**

Add these tests to `crates/server/src/dispatcher.rs`'s `mod tests`, immediately after `a_write_command_on_a_replica_is_rejected_with_readonly`:

```rust
#[test]
fn a_write_command_is_rejected_with_noreplicas_when_fencing_is_enabled_and_no_replica_has_acked() {
    let engine = std::sync::Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    let replication = ReplicationHandle::new(
        std::sync::Arc::clone(&engine),
        "/tmp/unused.snapshot".into(),
    )
    .with_min_replicas(1, std::time::Duration::from_secs(10));

    let reply = dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"SET", b"k", b"v"]),
        &Session::new(),
        1,
    );
    assert_eq!(
        reply,
        Frame::Error("NOREPLICAS Not enough good replicas to write.".into())
    );
    assert_eq!(
        engine.get(b"k"),
        None,
        "the write must never have reached the engine"
    );
}

#[test]
fn a_read_command_is_not_fenced_even_with_no_good_replicas() {
    let engine = std::sync::Arc::new(Engine::new());
    dispatch(
        &engine,
        cmd(&[b"SET", b"k", b"v"]),
        &mut Protocol::default(),
        1,
    );
    let (_dir, aof) = test_aof();
    let replication = ReplicationHandle::new(
        std::sync::Arc::clone(&engine),
        "/tmp/unused.snapshot".into(),
    )
    .with_min_replicas(1, std::time::Duration::from_secs(10));

    let reply = dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"GET", b"k"]),
        &Session::new(),
        1,
    );
    assert_eq!(reply, Frame::Bulk(Bytes::from_static(b"v")));
}

#[test]
fn fencing_disabled_by_default_accepts_writes_with_zero_replicas_connected() {
    let engine = std::sync::Arc::new(Engine::new());
    let (_dir, aof) = test_aof();
    // min_replicas_to_write defaults to 0 -- fencing off -- so no .with_min_replicas call here.
    let replication = ReplicationHandle::new(
        std::sync::Arc::clone(&engine),
        "/tmp/unused.snapshot".into(),
    );

    let reply = dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"SET", b"k", b"v"]),
        &Session::new(),
        1,
    );
    assert_eq!(reply, Frame::Simple("OK".into()));
    assert_eq!(
        engine.get(b"k"),
        Some(engine::Value::String(Bytes::from_static(b"v")))
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::a_write_command_is_rejected_with_noreplicas_when_fencing_is_enabled_and_no_replica_has_acked dispatcher::tests::a_read_command_is_not_fenced_even_with_no_good_replicas dispatcher::tests::fencing_disabled_by_default_accepts_writes_with_zero_replicas_connected -- --nocapture`
Expected: the first test FAILs at runtime (assertion mismatch: `dispatch_and_log` returns `Frame::Simple("OK")`, since there is no fencing gate yet, instead of the expected `Frame::Error("NOREPLICAS ...")`). The other two PASS already, since they describe behavior that already holds before this change -- they exist to pin that this plan must not break it. If `with_min_replicas` doesn't exist yet (Task 1 not merged), all three FAIL to compile instead.

- [ ] **Step 3: Add the gate**

In `crates/server/src/dispatcher.rs`, `dispatch_and_log_inner` currently reads (lines 3066-3080):

```rust
    // Checked before the SAVE/REPLICAOF interceptions below, and immediately after the
    // cluster redirect above (both interceptions are no-ops against WRITE_COMMANDS so
    // ordering relative to them doesn't matter) and extract_write_command_name's own later
    // call further down (so a rejected write never touches the AOF ordering lock).
    if replication
        .is_replica
        .load(std::sync::atomic::Ordering::Relaxed)
        && extract_write_command_name(&frame).is_some()
    {
        return Frame::Error("READONLY You can't write against a read only replica.".into());
    }

    if let Some(reply) = handle_auth(&frame, session, replication) {
        return reply;
    }
```

Change it to insert the fencing gate between the `READONLY` block and `handle_auth`:

```rust
    // Checked before the SAVE/REPLICAOF interceptions below, and immediately after the
    // cluster redirect above (both interceptions are no-ops against WRITE_COMMANDS so
    // ordering relative to them doesn't matter) and extract_write_command_name's own later
    // call further down (so a rejected write never touches the AOF ordering lock).
    if replication
        .is_replica
        .load(std::sync::atomic::Ordering::Relaxed)
        && extract_write_command_name(&frame).is_some()
    {
        return Frame::Error("READONLY You can't write against a read only replica.".into());
    }

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

    if let Some(reply) = handle_auth(&frame, session, replication) {
        return reply;
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher:: -- --nocapture`
Expected: all PASS, including every pre-existing `dispatcher::tests` test (in particular `a_write_command_on_a_replica_is_rejected_with_readonly` and `save_is_not_gated_on_a_replica` — this new gate must not change their outcome, since both use handles with fencing left at its default-disabled `0`).

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --lib dispatcher::`
Expected: all green.

```bash
git add crates/server/src/dispatcher.rs
git commit -m "$(cat <<'EOF'
Reject client writes with NOREPLICAS when fencing is unmet

Adds the min-replicas-to-write gate to dispatch_and_log_inner,
immediately after the existing READONLY check and before any command
interception or the write path's AOF ordering lock. Disabled by
default (min_replicas_to_write == 0), so no existing deployment or
test is affected until it opts in via 07-fencing-config.md's fields.
EOF
)"
```

---

### Task 3: integration test through a real TCP connection

**Files:**
- Modify: `crates/server/tests/replication.rs`

**Interfaces:**
- Consumes: `ReplicationHandle::with_min_replicas` (Task 1); the `NOREPLICAS` gate (Task 2); `spawn_node`/`wait_for` (existing helpers at `crates/server/tests/replication.rs:11-60`).
- Produces: `spawn_node_with_min_replicas`, a `spawn_node` variant used only by this file's fencing tests.

- [ ] **Step 1: Write the failing tests**

Add this helper and these two tests to `crates/server/tests/replication.rs`, immediately after the existing `spawn_node`/`wait_for` helpers (after line 60):

```rust
/// Like `spawn_node`, but applies `with_min_replicas` to the `ReplicationHandle` before serving
/// -- `spawn_node` itself has no fencing knobs, since every other test in this file needs fencing
/// off. A separate helper, not a parameter added to `spawn_node`, so every existing `spawn_node()`
/// call site in this file stays untouched.
async fn spawn_node_with_min_replicas(
    to_write: u64,
    max_lag: std::time::Duration,
) -> (
    tempfile::TempDir,
    Arc<engine::Engine>,
    Arc<rocket_mem::aof::AofWriter>,
    Arc<rocket_mem::replication::ReplicationHandle>,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("node.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let replication = Arc::new(
        rocket_mem::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            dir.path().join("node.snapshot"),
        )
        .with_min_replicas(to_write, max_lag),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(rocket_mem::serve(
        listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));
    (dir, engine, aof, replication, addr.to_string())
}

#[tokio::test]
async fn a_leader_with_fencing_enabled_and_no_acked_replicas_refuses_writes_with_noreplicas() {
    let (_dir, _engine, _aof, _replication, addr) =
        spawn_node_with_min_replicas(1, std::time::Duration::from_secs(10)).await;

    let client = redis::Client::open(format!("redis://{addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let result: Result<(), redis::RedisError> = con.set("k", "v").await;
    assert_eq!(
        result.expect_err("must refuse the write with no replicas connected").code(),
        Some("NOREPLICAS")
    );
}

#[tokio::test]
async fn a_leader_with_fencing_disabled_accepts_writes_with_zero_replicas_connected() {
    let (_dir, engine, _aof, _replication, addr) =
        spawn_node_with_min_replicas(0, std::time::Duration::from_secs(10)).await;

    let client = redis::Client::open(format!("redis://{addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = con.set("k", "v").await.unwrap();
    wait_for(&engine, b"k", b"v").await;
}
```

- [ ] **Step 2: Run the tests to verify they compile and pass**

Run: `cargo test -p rocket-mem --test replication a_leader_with_fencing -- --nocapture`
Expected: both PASS immediately — Tasks 1 and 2 already landed the gate this integration test exercises, so unlike a normal TDD step there is no red phase here. This task exists to catch a regression in the *wiring* between `main.rs`-shaped startup (a real `TcpListener` and a real `redis` client) and the unit-level gate already proven in Task 2's `dispatcher::tests` — run it and confirm the PASS explicitly rather than assuming it from those unit tests alone. Before writing Step 1's code at all, running this same command fails to compile with `cannot find function \`spawn_node_with_min_replicas\` in this scope`, which is the closest thing to a red phase this task has.

- [ ] **Step 3: Confirm and run the full suite**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --test replication`
Expected: all green, including every pre-existing test in `crates/server/tests/replication.rs`.

- [ ] **Step 4: Commit**

```bash
git add crates/server/tests/replication.rs
git commit -m "$(cat <<'EOF'
Add an end-to-end NOREPLICAS test over a real TCP connection

Proves the min-replicas-to-write gate through the same path a real
client uses (a real TcpListener and the redis crate), not just the
unit-level dispatch_and_log calls in dispatcher.rs's own test module.
Also proves the default (fencing disabled) still accepts writes with
zero replicas connected, at this same integration level.
EOF
)"
```

---

## Next plan

[`09-fencing-observability.md`](09-fencing-observability.md) — adds the `rocket_mem_good_replicas` gauge, the `rocket_mem_writes_rejected_no_replicas_total` counter, fenced-state transition log lines, and a manual-testing walkthrough for exercising fencing by hand.
