# `INFO` & `HELLO` Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `INFO` returns the eight real-Redis sections tooling parses, filled only with state this server actually tracks, and both `INFO` and `HELLO` report `role:slave` on a follower — closing the two limitations Sprint 5 knowingly shipped.

**Architecture:** `INFO` and `HELLO` are *moved* out of `dispatch`'s match into `handle_info`/`handle_hello` interceptions inside `dispatch_and_log_inner`, joining `SAVE`/`REPLICAOF`/`CLUSTER`. This establishes the rule that makes both fixes possible at all: **`dispatch` answers questions about the keyspace; `dispatch_and_log` answers questions about the server**, because only the latter has the `AofWriter` and `ReplicationHandle` these two commands must read. A handful of new `ReplicationHandle` fields and two new `Engine` getters supply the fields that were previously unavailable.

**Tech Stack:** `std` only.

**Spec:** ../../specs/2026-08-30-sprint-6-spec.md — "`INFO` and `HELLO` move out of `dispatch` into interceptions, and report real state" is authoritative, including the exact field list. Depends on `01-hash-slots-and-cluster-config.md` (`cluster_enabled`) and `04-prometheus-metrics.md` (the connection/command/expiry counters and `Engine::key_counts`, and the `dispatch_and_log_inner` split this plan's interceptions are inserted into).

## Global Constraints

- **Nothing is invented.** Every emitted field is backed by state this codebase tracks. Fields real Redis has and this server cannot compute — `keyspace_hits`/`keyspace_misses`, `tcp_port`, `rdb_changes_since_last_save` — are **omitted**, not faked, and the omissions are recorded in the README by `08-sprint-6-close.md`.
- **`role:slave`, not `role:replica`.** Real Redis still emits `slave` for backward compatibility and every client library parses for it. Matching the wire is the point; the codebase's own prose keeps saying "follower".
- `expired_keys` counts only *actively* expired keys (the `active_expire_loop` sweep). Passive expiry — a read finding a key already dead, `crates/engine/src/shard.rs:37-58` — removes keys without counting them, and adding a counter there would touch the hottest read path in the project for a statistic nothing gates on.
- Moving `HELLO` out of `dispatch` means plain `dispatch` answers `HELLO` with its unknown-command error. That is correct: `dispatch`'s only direct callers are `aof::replay` and the follower apply loop, neither of which can ever see a `HELLO`. `handle_hello` still takes `&mut Protocol`, so `connection.rs:105`'s `framed.codec_mut().protocol = protocol` keeps observing `HELLO 3`'s switch exactly as before.

---

### Task 1: the remaining server-state fields

**Files:**
- Modify: `crates/server/src/replication.rs` (struct at `:42`, `new` at `:93`, `start_replicating` at `:136`, `stop_replicating` at `:158`, `replication_client_loop` at `:185`, `sync_once` at `:218`)
- Modify: `crates/engine/src/engine.rs` (`maxmemory` field at `:15`; add the getter beside `memory_used()` at `:104`)
- Modify: `crates/server/src/dispatcher.rs` (`handle_save` at `:999`)

**Interfaces:**
- Consumes: `ReplicationHandle` as extended by `04-prometheus-metrics.md`.
- Produces: `ReplicationHandle::{uptime_secs, last_save_unix, record_save, master_addr, link_up, link_up_slot}` and `Engine::maxmemory() -> Option<usize>`, all consumed by Task 2.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/engine.rs — add to the existing tests module
    #[test]
    fn maxmemory_reports_the_configured_ceiling_or_none() {
        assert_eq!(Engine::new().maxmemory(), None);
        assert_eq!(Engine::with_maxmemory(4_096).maxmemory(), Some(4_096));
    }
```

```rust
// crates/server/src/replication.rs — add to the existing tests module
    #[test]
    fn uptime_starts_at_zero_and_never_goes_backwards() {
        let h = ReplicationHandle::default();
        let first = h.uptime_secs();
        assert!(first < 2, "a just-built handle should report ~0s, got {first}");
        assert!(h.uptime_secs() >= first);
    }

    #[test]
    fn last_save_unix_is_zero_until_a_save_records_one() {
        let h = ReplicationHandle::default();
        assert_eq!(h.last_save_unix(), 0);
        h.record_save();
        assert!(
            h.last_save_unix() > 1_700_000_000,
            "record_save should store a real unix timestamp, got {}",
            h.last_save_unix()
        );
    }

    #[tokio::test]
    async fn master_addr_and_link_up_follow_the_replica_role() {
        let h = ReplicationHandle::default();
        assert_eq!(h.master_addr(), None);
        assert!(!h.link_up());

        h.start_replicating("127.0.0.1:1".to_string()); // nothing is listening; that's fine
        assert_eq!(h.master_addr(), Some("127.0.0.1:1".to_string()));

        h.stop_replicating();
        assert_eq!(h.master_addr(), None);
        assert!(!h.link_up());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p engine engine::tests::maxmemory_reports && cargo test -p rocket-mem replication::tests::uptime replication::tests::last_save_unix replication::tests::master_addr_and_link_up`
Expected: FAIL to compile — none of these methods exist yet

- [ ] **Step 3: Add `Engine::maxmemory`**

```rust
// crates/engine/src/engine.rs — add directly below `memory_used()` (:106)
    /// The configured `MAXMEMORY` ceiling, if any. `INFO`'s memory section reports it; note the
    /// shipped binary always answers `None`, because `main.rs` builds its `Engine` through
    /// `aof::recover`, which calls `Engine::new()`. Wiring a `ROCKET_MEM_MAXMEMORY` env var is
    /// deliberately out of this sprint's scope; the gap is recorded in the README.
    pub fn maxmemory(&self) -> Option<usize> {
        self.maxmemory
    }
```

- [ ] **Step 4: Add the `ReplicationHandle` fields**

```rust
// crates/server/src/replication.rs — add as fields of `pub struct ReplicationHandle`
    /// When this handle was built, which for `main.rs`'s single handle is process start. Feeds
    /// `INFO`'s `uptime_in_seconds`.
    started_at: std::time::Instant,
    /// Unix seconds of the last successful `SAVE`; 0 if none has run in this process. This is
    /// per-process state, not read back from the snapshot file: the file has no timestamp field
    /// (Sprint 5 deliberately gave it no header beyond the AOF offset), so reporting anything
    /// else would be a guess.
    last_save_unix: AtomicI64,
    /// `Some(host:port)` while this node is a follower. Set by `start_replicating`, cleared by
    /// `stop_replicating`; feeds `INFO`'s `master_host`/`master_port`.
    master_addr: Mutex<Option<String>>,
    /// Whether the follower's link to its leader is currently up -- set true once a sync has
    /// loaded a snapshot, false when that connection ends or fails. An `Arc` because the spawned
    /// follower task is `'static`. This is the honest counterpart to real Redis's
    /// `master_link_status`: it tracks the connection, not a byte offset, because this project
    /// has no replication offsets at all.
    link_up: Arc<AtomicBool>,
```

```rust
// crates/server/src/replication.rs — add to `new`'s struct literal (:93)
            started_at: std::time::Instant::now(),
            last_save_unix: AtomicI64::new(0),
            master_addr: Mutex::new(None),
            link_up: Arc::new(AtomicBool::new(false)),
```

```rust
// crates/server/src/replication.rs — add to the existing `impl ReplicationHandle` block
    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
    pub fn last_save_unix(&self) -> i64 {
        self.last_save_unix.load(Ordering::Relaxed)
    }
    /// Called by `handle_save` after a snapshot has landed on disk.
    pub fn record_save(&self) {
        self.last_save_unix.store(unix_now_secs(), Ordering::Relaxed);
    }
    pub fn master_addr(&self) -> Option<String> {
        self.master_addr
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn link_up(&self) -> bool {
        self.link_up.load(Ordering::Relaxed)
    }
    /// The shared flag itself, for the spawned follower task to write into.
    pub fn link_up_slot(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.link_up)
    }
```

```rust
// crates/server/src/replication.rs — a free helper near the top of the file, used by
// `record_save` and by `sync_once`'s last-apply stamp
/// Unix seconds now, or 0 if the system clock is somehow before the epoch. Never panics: a
/// bogus clock must not take down a server over a metrics field. `04-prometheus-metrics.md`
/// inlined this same `SystemTime` expression into `sync_once`'s last-apply stamp; replace that
/// inline copy with a call to this function so there is exactly one implementation.
fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 5: Maintain `master_addr` and `link_up`**

```rust
// crates/server/src/replication.rs — inside `start_replicating` (:136), after the existing
// `let my_generation = ...` line and before the spawn
        *self
            .master_addr
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(host_port.clone());
        let link_up = self.link_up_slot();
```

```rust
// crates/server/src/replication.rs — `start_replicating`'s spawn gains the two new arguments
        *task = Some(tokio::spawn(replication_client_loop(
            host_port,
            engine,
            generation,
            my_generation,
            aof,
            self.last_apply_slot(),
            link_up,
        )));
```

Lock ordering: `master_addr`'s mutex is only ever taken *inside* `follower_task`'s, in both `start_replicating` and `stop_replicating`, and never the reverse — so the pair cannot invert.

```rust
// crates/server/src/replication.rs — inside `stop_replicating` (:158), after the abort
        *self
            .master_addr
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.link_up.store(false, Ordering::Relaxed);
```

```rust
// crates/server/src/replication.rs — `replication_client_loop` (:185) gains a trailing
// `link_up: Arc<AtomicBool>` parameter (its 7th, still under clippy's too-many-arguments
// threshold), passes `&link_up` into `sync_once`, and clears it after every returned sync:
        link_up.store(false, Ordering::Relaxed);
// ...placed immediately after the `match sync_once(...).await { ... }` block and before the
// 1-second backoff sleep, so a dropped connection is visible in INFO within one iteration.
```

```rust
// crates/server/src/replication.rs — `sync_once` (:218) gains a trailing
// `link_up: &AtomicBool` parameter and sets it true immediately after `engine.load_snapshot`
// succeeds -- the first moment this follower genuinely holds its leader's state:
        link_up.store(true, Ordering::Relaxed);
// The two existing direct callers in this file's tests
// (`sync_once_loads_the_snapshot_then_applies_streamed_frames` at :357 and
// `sync_once_does_not_load_the_snapshot_when_its_generation_is_already_stale` at :414) each
// gain a trailing `&AtomicBool::new(false)` argument, alongside the `&AtomicI64::new(0)` that
// 04-prometheus-metrics.md added.
```

```rust
// crates/server/src/dispatcher.rs — inside `handle_save` (:999), in the success arm of the
// existing `match write_snapshot_atomically(...)`:
        Ok(()) => {
            replication.record_save();
            Frame::Simple("OK".into())
        }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p engine && cargo test -p rocket-mem replication::tests`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/engine/src/engine.rs crates/server/src/replication.rs crates/server/src/dispatcher.rs
git commit -m "feat(server): track uptime, last-save, master address and link status"
```

---

### Task 2: `INFO` as an interception with real sections

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (remove the `"INFO"` arm at `:829-832`; add helpers below `handle_cluster`; wire into `dispatch_and_log_inner`; migrate the test at `:1609`)

**Interfaces:**
- Consumes: everything from Task 1, `Engine::key_counts`/`memory_used`/`eviction_count`, `AofWriter::policy`, `ReplicationHandle`'s counters, `split_addr` (from `02-cluster-commands-and-moved.md`), `cluster_info_text`'s `cluster_enabled` idea (re-derived here from `replication.cluster()`).
- Produces: `fn handle_info(...) -> Option<Frame>`; nothing later depends on it.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
    fn info_text_for(replication: &ReplicationHandle, engine: &Engine, args: &[&[u8]]) -> String {
        let (_dir, aof) = test_aof();
        let mut command = vec![&b"INFO"[..]];
        command.extend_from_slice(args);
        let Frame::Bulk(text) = dispatch_and_log(
            engine,
            &aof,
            replication,
            cmd(&command),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("INFO should reply with a Bulk string")
        };
        String::from_utf8(text.to_vec()).unwrap()
    }

    #[test]
    fn info_emits_every_section_by_default() {
        let engine = Engine::new();
        let text = info_text_for(&ReplicationHandle::default(), &engine, &[]);
        for header in [
            "# Server",
            "# Clients",
            "# Memory",
            "# Persistence",
            "# Stats",
            "# Replication",
            "# Cluster",
        ] {
            assert!(text.contains(header), "missing {header} in:\n{text}");
        }
        assert!(text.contains("redis_version:rocket-mem-"), "{text}");
        assert!(text.contains("redis_mode:standalone\r\n"), "{text}");
        assert!(text.contains("maxmemory_policy:allkeys-lru\r\n"), "{text}");
        assert!(text.contains("aof_enabled:1\r\n"), "{text}");
        assert!(text.contains("rdb_bgsave_in_progress:0\r\n"), "{text}");
        assert!(text.contains("aof_fsync_policy:no\r\n"), "{text}"); // test_aof uses Never
    }

    #[test]
    fn info_with_a_section_argument_returns_only_that_section() {
        let engine = Engine::new();
        let text = info_text_for(&ReplicationHandle::default(), &engine, &[b"replication"]);
        assert!(text.contains("# Replication"), "{text}");
        assert!(text.contains("role:master\r\n"), "{text}");
        assert!(!text.contains("# Memory"), "{text}");
        // the section name is case-insensitive, like real Redis
        let upper = info_text_for(&ReplicationHandle::default(), &engine, &[b"REPLICATION"]);
        assert!(upper.contains("# Replication"), "{upper}");
        // `all` and `default` both mean everything
        let all = info_text_for(&ReplicationHandle::default(), &engine, &[b"all"]);
        assert!(all.contains("# Memory") && all.contains("# Replication"), "{all}");
    }

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
        assert!(!text.contains("connected_slaves:"), "{text}");
    }

    #[test]
    fn info_reports_connected_slaves_on_a_master() {
        let engine = Engine::new();
        let replication = ReplicationHandle::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        replication.registry.register(tx);
        let text = info_text_for(&replication, &engine, &[b"replication"]);
        assert!(text.contains("role:master\r\n"), "{text}");
        assert!(text.contains("connected_slaves:1\r\n"), "{text}");
        assert!(!text.contains("master_host:"), "{text}");
    }

    #[test]
    fn info_keyspace_line_appears_only_when_there_are_keys() {
        let engine = Engine::new();
        let empty = info_text_for(&ReplicationHandle::default(), &engine, &[b"keyspace"]);
        assert!(!empty.contains("db0:"), "{empty}");

        engine.set(
            Bytes::from_static(b"a"),
            Value::String(Bytes::from_static(b"1")),
        );
        engine.set(
            Bytes::from_static(b"b"),
            Value::String(Bytes::from_static(b"2")),
        );
        engine.expire_at(
            b"b",
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        let filled = info_text_for(&ReplicationHandle::default(), &engine, &[b"keyspace"]);
        assert!(filled.contains("db0:keys=2,expires=1,avg_ttl=0\r\n"), "{filled}");
    }

    #[test]
    fn info_reports_cluster_mode_from_the_loaded_config() {
        let engine = Engine::new();
        let off = info_text_for(&ReplicationHandle::default(), &engine, &[b"cluster"]);
        assert!(off.contains("cluster_enabled:0\r\n"), "{off}");
        let on = info_text_for(&cluster_handle("shard-a"), &engine, &[b"cluster"]);
        assert!(on.contains("cluster_enabled:1\r\n"), "{on}");
        let server = info_text_for(&cluster_handle("shard-a"), &engine, &[b"server"]);
        assert!(server.contains("redis_mode:cluster\r\n"), "{server}");
    }

    #[test]
    fn info_stats_counts_the_commands_that_ran_before_it() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        replication.connection_opened();
        for _ in 0..3 {
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"PING"]),
                &mut Protocol::default(),
                1,
            );
        }
        let text = info_text_for(&replication, &engine, &[b"stats"]);
        // 3 PINGs; the INFO itself is counted by the wrapper only *after* the body ran
        assert!(text.contains("total_commands_processed:3\r\n"), "{text}");
        assert!(text.contains("total_connections_received:1\r\n"), "{text}");
        assert!(text.contains("expired_keys:0\r\n"), "{text}");
        assert!(text.contains("evicted_keys:0\r\n"), "{text}");
        let clients = info_text_for(&replication, &engine, &[b"clients"]);
        assert!(clients.contains("connected_clients:1\r\n"), "{clients}");
    }

    #[test]
    fn info_memory_reports_a_configured_maxmemory() {
        let engine = Engine::with_maxmemory(4_096);
        let text = info_text_for(&ReplicationHandle::default(), &engine, &[b"memory"]);
        assert!(text.contains("maxmemory:4096\r\n"), "{text}");
        assert!(text.contains("used_memory:"), "{text}");
        assert!(text.contains("used_memory_human:"), "{text}");
    }

    #[tokio::test]
    async fn info_reports_the_master_address_while_replicating() {
        let engine = Engine::new();
        let replication = ReplicationHandle::default();
        replication.start_replicating("127.0.0.1:1".to_string()); // nothing listening; fine
        let text = info_text_for(&replication, &engine, &[b"replication"]);
        assert!(text.contains("master_host:127.0.0.1\r\n"), "{text}");
        assert!(text.contains("master_port:1\r\n"), "{text}");
        replication.stop_replicating();
    }
```

Also migrate the existing `info_replies_a_non_empty_bulk_string` (`:1609`): keep the test name and its `assert!(!info.is_empty())`, and change its two body lines to

```rust
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(info) = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"INFO"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Bulk")
        };
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::info_`
Expected: FAIL — `INFO` still returns only the two-line `# Server` section from `dispatch`'s arm, so every section assertion fails

- [ ] **Step 3: Implement the sections**

```rust
// crates/server/src/dispatcher.rs — add below `handle_cluster`
/// Real Redis's human-readable byte format, e.g. `80.00K`. Purely cosmetic -- `used_memory` is
/// the machine-readable field; tooling that graphs memory reads that one.
fn human_bytes(bytes: usize) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b >= K * K * K {
        format!("{:.2}G", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.2}M", b / (K * K))
    } else if b >= K {
        format!("{:.2}K", b / K)
    } else {
        format!("{bytes}B")
    }
}

/// The `INFO persistence` name for an fsync policy, matching real Redis's spelling
/// (`always`/`everysec`/`no`) rather than this codebase's enum names.
fn fsync_policy_name(policy: crate::aof::FsyncPolicy) -> &'static str {
    match policy {
        crate::aof::FsyncPolicy::Always => "always",
        crate::aof::FsyncPolicy::EverySecond => "everysec",
        crate::aof::FsyncPolicy::Never => "no",
    }
}

/// Builds `INFO`'s body. `section` is `None` for "every section" (no argument, or `all`/
/// `default`/`everything`), otherwise a lowercase section name.
///
/// Every field here is backed by state this server actually tracks. Fields real Redis has that
/// this one cannot compute -- `keyspace_hits`/`keyspace_misses` (nothing counts them),
/// `tcp_port` (the dispatcher never learns the listen address), `rdb_changes_since_last_save`
/// -- are omitted rather than faked.
fn info_text(
    section: Option<&str>,
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> String {
    let wanted = |name: &str| section.is_none() || section == Some(name);
    let mut out = String::new();

    if wanted("server") {
        let uptime = replication.uptime_secs();
        out.push_str(&format!(
            "# Server\r\n\
             redis_version:rocket-mem-{version}\r\n\
             rocket_mem_version:{version}\r\n\
             redis_mode:{mode}\r\n\
             os:{os}\r\n\
             arch_bits:{bits}\r\n\
             process_id:{pid}\r\n\
             uptime_in_seconds:{uptime}\r\n\
             uptime_in_days:{days}\r\n\r\n",
            version = env!("CARGO_PKG_VERSION"),
            mode = if replication.cluster().is_some() {
                "cluster"
            } else {
                "standalone"
            },
            os = std::env::consts::OS,
            bits = usize::BITS,
            pid = std::process::id(),
            days = uptime / 86_400,
        ));
    }

    if wanted("clients") {
        out.push_str(&format!(
            "# Clients\r\nconnected_clients:{}\r\n\r\n",
            replication.connected_clients()
        ));
    }

    if wanted("memory") {
        let used = engine.memory_used();
        out.push_str(&format!(
            "# Memory\r\n\
             used_memory:{used}\r\n\
             used_memory_human:{}\r\n\
             maxmemory:{}\r\n\
             maxmemory_policy:allkeys-lru\r\n\r\n",
            human_bytes(used),
            engine.maxmemory().unwrap_or(0),
        ));
    }

    if wanted("persistence") {
        out.push_str(&format!(
            "# Persistence\r\n\
             aof_enabled:1\r\n\
             aof_fsync_policy:{}\r\n\
             rdb_last_save_time:{}\r\n\
             rdb_bgsave_in_progress:0\r\n\r\n",
            fsync_policy_name(aof.policy()),
            replication.last_save_unix(),
        ));
    }

    if wanted("stats") {
        out.push_str(&format!(
            "# Stats\r\n\
             total_connections_received:{}\r\n\
             total_commands_processed:{}\r\n\
             expired_keys:{}\r\n\
             evicted_keys:{}\r\n\r\n",
            replication.total_connections(),
            replication.total_commands(),
            replication.expired_keys(),
            engine.eviction_count(),
        ));
    }

    if wanted("replication") {
        let is_replica = replication
            .is_replica
            .load(std::sync::atomic::Ordering::Relaxed);
        out.push_str("# Replication\r\n");
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
        } else {
            out.push_str("role:master\r\n");
            out.push_str(&format!(
                "connected_slaves:{}\r\n",
                replication.registry.len()
            ));
        }
        out.push_str("\r\n");
    }

    if wanted("cluster") {
        out.push_str(&format!(
            "# Cluster\r\ncluster_enabled:{}\r\n\r\n",
            i32::from(replication.cluster().is_some())
        ));
    }

    if wanted("keyspace") {
        out.push_str("# Keyspace\r\n");
        let (keys, expires) = engine.key_counts();
        if keys > 0 {
            // Omitted entirely on an empty keyspace, exactly as real Redis does -- tooling
            // treats the absence of a `db0:` line as "this database is empty".
            out.push_str(&format!("db0:keys={keys},expires={expires},avg_ttl=0\r\n"));
        }
        out.push_str("\r\n");
    }

    out
}

/// Returns `Some(reply)` if `frame` was `INFO`. Lives here rather than in `dispatch` because it
/// reads the `AofWriter` and the `ReplicationHandle`, which plain `dispatch` has no parameter
/// for -- the same reason `SAVE`, `REPLICAOF`, and `CLUSTER` are intercepted.
fn handle_info(
    frame: &Frame,
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"INFO") {
        return None;
    }
    let section = match items.get(1) {
        Some(Frame::Bulk(raw)) => {
            let requested = String::from_utf8_lossy(raw).to_ascii_lowercase();
            match requested.as_str() {
                "all" | "default" | "everything" => None,
                _ => Some(requested),
            }
        }
        _ => None,
    };
    Some(Frame::Bulk(Bytes::from(info_text(
        section.as_deref(),
        engine,
        aof,
        replication,
    ))))
}
```

- [ ] **Step 4: Remove `dispatch`'s `INFO` arm and wire the interception**

```rust
// crates/server/src/dispatcher.rs — DELETE these lines from dispatch's match (:829-832)
        "INFO" => Frame::Bulk(Bytes::from(format!(
            "# Server\r\nredis_version:rocket-mem-{}\r\n",
            env!("CARGO_PKG_VERSION")
        ))),
```

```rust
// crates/server/src/dispatcher.rs — inside dispatch_and_log_inner, directly after the
// `handle_cluster` interception added by 02-cluster-commands-and-moved.md
    if let Some(reply) = handle_info(&frame, engine, aof, replication) {
        return reply;
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests::info_`
Expected: PASS, all 10 tests (9 new plus the migrated `info_replies_a_non_empty_bulk_string`)

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): flesh out INFO with real server, memory, stats and replication data"
```

---

### Task 3: `HELLO` as an interception with a real role

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (remove the `"HELLO"` arm at `:863-882`; `hello_reply` at `:887-921`; migrate the five tests at `:2087`, `:2122`, `:2134`, `:2169`, `:3431`)

**Interfaces:**
- Consumes: `ReplicationHandle::is_replica`.
- Produces: `fn handle_hello(frame: &Frame, protocol: &mut Protocol, client_id: u64, replication: &ReplicationHandle) -> Option<Frame>` and `fn hello_reply(protocol: Protocol, client_id: u64, role: &'static str, mode: &'static str) -> Frame`.

- [ ] **Step 1: Write the failing test and migrate the five existing ones**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
    #[test]
    fn hello_reports_role_slave_on_a_replica_and_master_otherwise() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();

        let master = ReplicationHandle::default();
        let Frame::Map(fields) = dispatch_and_log(
            &engine,
            &aof,
            &master,
            cmd(&[b"HELLO"]),
            &mut Protocol::default(),
            7,
        ) else {
            panic!("expected Map")
        };
        assert!(fields.contains(&(
            Frame::Bulk(Bytes::from_static(b"role")),
            Frame::Bulk(Bytes::from_static(b"master"))
        )));

        let replica = ReplicationHandle::default();
        replica
            .is_replica
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let Frame::Map(fields) = dispatch_and_log(
            &engine,
            &aof,
            &replica,
            cmd(&[b"HELLO"]),
            &mut Protocol::default(),
            7,
        ) else {
            panic!("expected Map")
        };
        assert!(
            fields.contains(&(
                Frame::Bulk(Bytes::from_static(b"role")),
                Frame::Bulk(Bytes::from_static(b"slave"))
            )),
            "{fields:?}"
        );
    }
```

Migrate the five existing `HELLO` tests — `hello_with_no_args_reports_current_protocol_without_switching` (`:2087`), `hello_2_switches_protocol_to_resp2` (`:2122`), `hello_3_switches_protocol_to_resp3` (`:2134`), `hello_with_unsupported_protover_returns_noproto_and_leaves_protocol_unchanged` (`:2169`), and `hello_with_extra_args_after_protover_is_a_syntax_error` (`:3431`) — with the same mechanical edit in each, changing nothing else about them:

1. add `let (_dir, aof) = test_aof();` directly after the test's existing `let engine = Engine::new();` line, and
2. replace the call `dispatch(&engine, <command>, <protocol>, <client_id>)` with
   `dispatch_and_log(&engine, &aof, &ReplicationHandle::default(), <command>, <protocol>, <client_id>)`.

Every assertion in those five tests stays exactly as it is: the reply shape, the protocol switching, the `NOPROTO`/syntax errors, and the `role` field's `master` value (`ReplicationHandle::default()` is not a replica) are all unchanged by the move.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::hello`
Expected: FAIL — `hello_reports_role_slave_on_a_replica_and_master_otherwise` sees `master` for both handles, since `hello_reply` still hardcodes it

- [ ] **Step 3: Move `HELLO` and make the role real**

```rust
// crates/server/src/dispatcher.rs — DELETE the whole "HELLO" arm from dispatch's match
// (:863-882), the block beginning `"HELLO" => match rest.first() {`
```

```rust
// crates/server/src/dispatcher.rs — `hello_reply` (:887-921) in full, with the role parameter.
// Only the signature and the `role` pair differ from what is there today; the `version` field
// keeps its existing hardcoded `rocket-mem-0.1.0` string so this move changes nothing a client
// can observe beyond the role.
fn hello_reply(protocol: Protocol, client_id: u64, role: &'static str, mode: &'static str) -> Frame {
    Frame::Map(vec![
        (
            Frame::Bulk(Bytes::from_static(b"server")),
            Frame::Bulk(Bytes::from_static(b"redis")),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"version")),
            Frame::Bulk(Bytes::from_static(b"rocket-mem-0.1.0")),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"proto")),
            Frame::Integer(match protocol {
                Protocol::Resp2 => 2,
                Protocol::Resp3 => 3,
            }),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"id")),
            Frame::Integer(client_id as i64),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"mode")),
            Frame::Bulk(Bytes::from(mode)),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"role")),
            Frame::Bulk(Bytes::from(role)),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"modules")),
            Frame::Array(vec![]),
        ),
    ])
}
```

```rust
// crates/server/src/dispatcher.rs — add below `handle_info`
/// Returns `Some(reply)` if `frame` was `HELLO`. Moved out of `dispatch` this sprint for one
/// reason: the reply's `role` field must reflect whether this node is a follower, and only
/// `dispatch_and_log` has the `ReplicationHandle` that knows. The protocol-switching behavior is
/// identical to the arm it replaces, and it still mutates the caller's `&mut Protocol`, so
/// `connection.rs`'s `framed.codec_mut().protocol = protocol` keeps working unchanged.
///
/// `dispatch` therefore answers `HELLO` with its unknown-command error, which is correct: its
/// only direct callers are `aof::replay` and the follower apply loop, neither of which can ever
/// see a `HELLO`.
fn handle_hello(
    frame: &Frame,
    protocol: &mut Protocol,
    client_id: u64,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"HELLO") {
        return None;
    }
    let role = if replication
        .is_replica
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        "slave"
    } else {
        "master"
    };
    // Kept consistent with `INFO server`'s `redis_mode`, which reports the same thing.
    let mode = if replication.cluster().is_some() {
        "cluster"
    } else {
        "standalone"
    };
    let args = &items[1..];
    Some(match args.first() {
        None => hello_reply(*protocol, client_id, role, mode),
        Some(Frame::Bulk(arg)) => match arg.as_ref() {
            b"2" => {
                if args.len() > 1 {
                    return Some(Frame::Error("ERR syntax error".into()));
                }
                *protocol = Protocol::Resp2;
                hello_reply(*protocol, client_id, role, mode)
            }
            b"3" => {
                if args.len() > 1 {
                    return Some(Frame::Error("ERR syntax error".into()));
                }
                *protocol = Protocol::Resp3;
                hello_reply(*protocol, client_id, role, mode)
            }
            _ => Frame::Error("NOPROTO unsupported protocol version".into()),
        },
        // A non-Bulk argument was previously caught by `dispatch`'s `frame_to_args`; keep that
        // exact error so the move changes no observable behavior.
        Some(_) => Frame::Error("ERR invalid request, expected array of bulk strings".into()),
    })
}
```

```rust
// crates/server/src/dispatcher.rs — inside dispatch_and_log_inner, directly after the
// `handle_info` interception
    if let Some(reply) = handle_hello(&frame, protocol, client_id, replication) {
        return reply;
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem`
Expected: PASS, every test in the crate — including `crates/server/tests/integration.rs`, whose `redis`-crate clients must still complete their handshakes

- [ ] **Step 5: Run the full workspace verification**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): report the real replica role in HELLO"
```
