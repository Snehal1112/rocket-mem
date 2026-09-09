# Verbose Logging Plan 18: Cluster Events

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Log the cluster topology once, when it loads, and log a `-MOVED` redirect's key, slot, and target node — the two events the spec's Event catalogue names for `server/cluster.rs` and `server/dispatcher.rs`.

**Architecture:** Two independent, unrelated insertion points, so this plan has two tasks rather than three. Topology-loaded is a one-time, startup-only `info!` inside `ClusterConfig::load`. The MOVED redirect is different in kind: `dispatcher.rs`'s `cluster_redirect` runs on **every** dispatched command once a node is in cluster mode (it is checked "before everything else" per its own call-site comment at `dispatcher.rs:3058`), so the log call there must sit only inside the branch that actually redirects — never on the `owns(first) => None` fast path every correctly-routed command takes. This plan proves that placement with a dedicated negative test, the same way plan 19 proves the slowlog log call sits only inside its own recording branch.

**Tech Stack:** Rust 2021, `tracing 0.1`, `tracing-subscriber 0.3` (test-only capture, already a `crates/server` dependency).

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the Event catalogue's "Cluster" row.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting. The load-bearing one for Task 2 specifically: this event sits on the dispatch hot path in cluster mode, so its fields must stay lazy (`%`/`?` sigils) and the call itself must be provably confined to the redirect branch, not the common owned-key path.

**Note on test infrastructure:** the shared `CapturedLogs` log-capture helper already exists from plan 17's first task, defined once in `crates/server/src/logging.rs` under `#[cfg(test)] pub(crate) mod test_support` (a `tracing_subscriber::fmt` writer over a shared buffer, using only the already-present `tracing-subscriber` dependency). Every task below imports it with `use crate::logging::test_support::CapturedLogs;` rather than redefining it. It is distinct from plan 09's `capture_at` helper in `crates/server/tests/logging.rs`: files under `tests/` compile as separate crates, so a helper there cannot be shared with `src/` unit tests.

---

### Task 1: Log the cluster topology once it loads

**Files:**
- Modify: `crates/server/src/cluster.rs` — `ClusterConfig::load` (lines 244–247)
- Modify: `crates/server/src/cluster.rs` — `#[cfg(test)] mod tests` (starts line 251)

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing consumed by a later plan — a leaf startup log line.

- [ ] **Step 1: Write the failing test**

Add to the existing `mod tests` in `crates/server/src/cluster.rs` (after the last existing test, before the closing `}`):

```rust
    use crate::logging::test_support::CapturedLogs;

    #[test]
    fn load_logs_the_topology_at_info_with_node_id_slot_range_and_node_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cluster.conf");
        std::fs::write(&path, THREE_SHARDS).unwrap();

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let config = ClusterConfig::load(&path, "shard-b").unwrap();
        drop(_guard);

        let text = captured.text();
        assert!(
            text.contains("shard-b"),
            "expected this node's own id in the topology-loaded log:\n{text}"
        );
        assert!(
            text.contains("5461") && text.contains("10922"),
            "expected this node's own slot range in the topology-loaded log:\n{text}"
        );
        assert!(
            text.contains("cluster topology loaded"),
            "expected the topology-loaded message:\n{text}"
        );
        assert_eq!(config.myself().id, "shard-b"); // load's own return value is unaffected
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p rocket-mem cluster::tests::load_logs_the_topology_at_info
```

Expected: FAIL — `ClusterConfig::load` emits no log line yet, so all three text assertions fail against an empty captured buffer.

- [ ] **Step 3: Implement the log line**

In `crates/server/src/cluster.rs`, replace `load` (lines 244–247):

```rust
    /// Reads the topology file at `path` and delegates to `parse`. A missing file surfaces as
    /// the underlying `NotFound` io::Error rather than being treated as "cluster mode off":
    /// `ROCKET_MEM_CLUSTER_CONFIG` being *set* to a path that doesn't exist is an operator
    /// mistake, and starting up silently in standalone mode would hide it until keys started
    /// landing on the wrong node.
    pub fn load(path: &std::path::Path, node_id: &str) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let config = Self::parse(&text, node_id)?;
        let me = config.myself();
        tracing::info!(
            node_id = %me.id,
            first_slot = me.first_slot,
            last_slot = me.last_slot,
            node_count = config.nodes.len(),
            "cluster topology loaded"
        );
        Ok(config)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem cluster::tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: the new test PASSes alongside every pre-existing `cluster.rs` test (including `load_reads_a_config_from_disk` and `load_surfaces_a_missing_file_as_an_io_error`, both unaffected since this change only adds a log call on the success path), fmt clean, clippy clean, full workspace suite green.

- [ ] **Step 5: Manual check**

Per `.claude/manual-testing.md`'s "Cluster mode" section, start the three-shard cluster with `RUST_LOG=info`. Confirm each node's startup log includes a `cluster topology loaded` line naming its own `node_id`, `first_slot`/`last_slot`, and a `node_count` of 3.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/cluster.rs
git commit -m "feat(logging): log the cluster topology when it loads"
```

---

### Task 2: Log a MOVED redirect's key, slot, and target node

**Files:**
- Modify: `crates/server/src/dispatcher.rs` — `cluster_redirect` (lines 1474–1496)
- Modify: `crates/server/src/dispatcher.rs` — `#[cfg(test)] mod tests` (existing cluster-redirect tests at lines 9940–10067, using the `cluster_handle` helper at lines 9565–9577, the `cmd` helper at lines 3400–3407, and `test_aof` at lines 7862–7867)

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing consumed by a later plan — a leaf log line on the redirect path only.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `crates/server/src/dispatcher.rs`, directly after the existing `a_key_this_node_does_not_own_is_redirected_with_moved` test (around line 9970):

```rust
    use crate::logging::test_support::CapturedLogs;

    #[test]
    fn a_moved_redirect_logs_the_key_slot_and_target_node_at_debug() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        // "foo" hashes to slot 12182, which shard-c owns -- same fixture as
        // `a_key_this_node_does_not_own_is_redirected_with_moved` above.
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"GET", b"foo"]),
            &Session::new(),
            1,
        );
        drop(_guard);

        assert_eq!(reply, Frame::Error("MOVED 12182 127.0.0.1:7003".into()));
        let text = captured.text();
        assert!(text.contains("foo"), "expected the key in the MOVED debug log:\n{text}");
        assert!(text.contains("12182"), "expected the slot in the MOVED debug log:\n{text}");
        assert!(
            text.contains("127.0.0.1:7003"),
            "expected the target node in the MOVED debug log:\n{text}"
        );
    }

    #[test]
    fn a_key_this_node_owns_produces_no_cluster_redirect_log() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        // "hello" hashes to slot 866, which shard-a (this node) owns -- the fast, non-redirect
        // path every correctly-routed command takes. This is the hot path the Global Constraints
        // flag: the log call in `cluster_redirect` must never fire here.
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"SET", b"hello", b"1"]),
            &Session::new(),
            1,
        );
        drop(_guard);

        assert_eq!(reply, Frame::Simple("OK".into()));
        assert!(
            captured.text().is_empty(),
            "the owned-key fast path must not log anything from cluster_redirect, got:\n{}",
            captured.text()
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem dispatcher::tests::a_moved_redirect_logs_the_key_slot_and_target_node_at_debug
cargo test -p rocket-mem dispatcher::tests::a_key_this_node_owns_produces_no_cluster_redirect_log
```

Expected: the first FAILs — `cluster_redirect` logs nothing yet, so all three substring assertions fail against an empty buffer. The second test PASSes vacuously against the unmodified code (nothing logs anywhere yet), which is expected and not the one driving this task — it exists to catch a regression in Step 3, not to currently be red.

- [ ] **Step 3: Implement the log line, confined to the redirect branch**

In `crates/server/src/dispatcher.rs`, replace `cluster_redirect` (lines 1474–1496):

```rust
fn cluster_redirect(
    frame: &Frame,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let cluster = replication.cluster()?;
    let keys = command_keys(frame);
    // Captured before `keys` is consumed by `into_iter()` below -- `first_key` borrows from
    // `frame`, which outlives this function, so it stays valid after the `Vec<&Bytes>`
    // container itself is dropped.
    let first_key: Option<&Bytes> = keys.first().copied();
    let mut slots = keys.into_iter().map(|k| crate::cluster::key_slot(k));
    let first = slots.next()?; // no keys => nothing to route
    if !slots.all(|s| s == first) {
        // Without this, `MSET a 1 b 2` across two slots would be accepted by whichever node owns
        // `a` and would then write `b` onto a node that does not own it -- a silent, permanent
        // violation of the routing invariant, undetectable by any client. Hash tags are how a
        // client legitimately keeps multi-key commands working under this rule.
        return Some(Frame::Error(
            "CROSSSLOT Keys in request don't hash to the same slot".into(),
        ));
    }
    if cluster.owns(first) {
        return None;
    }
    let owner = cluster.owner_of(first);
    // Only reached on an actual redirect, never on the `owns(first)` fast path above -- that is
    // what keeps this log call off the hot path every correctly-routed command in cluster mode
    // takes. `key` uses `%` over a lossy UTF-8 render rather than `?` (Debug) on the raw bytes,
    // per the hot-path guardrail against byte-by-byte Bytes rendering.
    if let Some(key) = first_key {
        tracing::debug!(
            key = %String::from_utf8_lossy(key),
            slot = first,
            target = %owner.addr,
            "cluster redirect"
        );
    }
    Some(Frame::Error(format!("MOVED {first} {}", owner.addr)))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem dispatcher::tests::a_moved_redirect_logs_the_key_slot_and_target_node_at_debug
cargo test -p rocket-mem dispatcher::tests::a_key_this_node_owns_produces_no_cluster_redirect_log
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: both new tests PASS, fmt clean, clippy clean, full workspace suite green — including every pre-existing cluster-redirect test in this module (`a_key_this_node_owns_is_served_normally`, `a_redirected_write_never_reaches_the_engine`, `keys_spanning_two_slots_are_rejected_with_crossslot`, `a_hash_tag_keeps_a_multi_key_command_on_one_slot`, `keyless_commands_are_never_redirected`, `nothing_is_redirected_when_cluster_mode_is_off`, `moved_takes_precedence_over_readonly_on_a_node_that_is_both`), none of which assert on log output and all of which keep passing since no return value changed.

- [ ] **Step 5: Benchmark gate**

`cluster_redirect` only runs when a node is started in cluster mode; `scripts/benchmark.sh`'s standalone-mode baseline from plan 01 is unaffected by this change (cluster mode is off in that harness), so no re-run is required for the ≤2% gate. The hot-path guardrail is instead verified directly by Step 4's `a_key_this_node_owns_produces_no_cluster_redirect_log` test, which proves the log call is unreachable on the fast path a benchmark would actually exercise if it *were* run in cluster mode.

- [ ] **Step 6: Manual check**

Per `.claude/manual-testing.md`'s "Cluster mode" section, bring up the three-shard cluster with `RUST_LOG=debug` on `shard-a`. Run `redis-cli -p 7001 set foo bar` (wrong node) and confirm `shard-a`'s log shows a `cluster redirect` line naming `key="foo"`, `slot=12182`, `target="127.0.0.1:7003"`, immediately before the `MOVED` reply reaches the client. Then run a command whose key `shard-a` does own and confirm no such line appears.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(logging): log a MOVED redirect's key, slot, and target node"
```

---

## Next plan

[`19-slowlog-and-metrics-events.md`](19-slowlog-and-metrics-events.md) — warns when a command actually crosses the slow-log threshold, and traces a served `/metrics` scrape.
