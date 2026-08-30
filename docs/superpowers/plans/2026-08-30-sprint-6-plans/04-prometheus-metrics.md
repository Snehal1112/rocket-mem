# Prometheus Metrics & `/metrics` Endpoint Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** every client command is counted and timed, server-level gauges are sampled at scrape time, and a Prometheus scraper can read them all from `http://<ROCKET_MEM_METRICS_ADDR>/metrics`.

**Architecture:** a new `crates/server/src/metrics.rs` installs a process-wide `metrics-exporter-prometheus` recorder behind a `OnceLock` and serves one route over its own `tokio` listener (~50 lines of HTTP/1.1, no `hyper`). `dispatch_and_log`'s existing body is renamed `dispatch_and_log_inner` and `dispatch_and_log` becomes a thin wrapper with the identical public signature — the single place per-command metrics are recorded, so none of its seven early returns can be missed. `dispatch` itself is deliberately left uninstrumented: it is what AOF replay and the follower apply loop call, and counting a boot-time replay as client traffic would make every dashboard lie.

**Tech Stack:** `metrics = "0.24"`, `metrics-exporter-prometheus = { version = "0.18", default-features = false }` (default features pull in the whole `hyper`/`rustls` stack for an HTTP listener we do not use), plus the `tokio` already in the workspace.

**Spec:** ../../specs/2026-08-30-sprint-6-spec.md — "Prometheus metrics are recorded in one wrapper around `dispatch_and_log`, and served from our own listener" is authoritative for this plan, including the exact metric names. Sequenced after `02-cluster-commands-and-moved.md` because both edit `dispatch_and_log`.

## Global Constraints

- `default-features = false` on `metrics-exporter-prometheus` is load-bearing, not cosmetic: its default `http-listener` + `push-gateway` features pull `hyper`, `hyper-util`, `hyper-rustls`, `rustls`, and `ipnet` into a workspace that currently has none of them.
- `serve()` must **not** bind the metrics port. Every integration test in the workspace calls `serve()`, and a fixed port would make them collide with each other and with a developer's running server. Only `main.rs` (and the metrics test, on `127.0.0.1:0`) binds it.
- `::metrics::set_global_recorder` succeeds at most once per process; `recorder_handle()` is therefore idempotent and never panics on a second call.
- Label cardinality is bounded by `KNOWN_COMMANDS` (from `02-cluster-commands-and-moved.md`): anything not in that list becomes the literal `other`, so a hostile client cannot create unbounded Prometheus series.
- The endpoint is unauthenticated, and defaults to loopback for exactly that reason. Auth arrives project-wide in Sprint 8.

---

### Task 1: dependencies and the recorder

**Files:**
- Modify: `Cargo.toml` (workspace root, `[workspace.dependencies]` at `:9-20`)
- Modify: `crates/server/Cargo.toml` (`[dependencies]` at `:14-21`)
- Create: `crates/server/src/metrics.rs`
- Modify: `crates/server/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn recorder_handle() -> PrometheusHandle` in `crate::metrics`, consumed by Tasks 3 and 5.

- [ ] **Step 1: Add the dependencies**

```toml
# Cargo.toml (workspace root) — append to [workspace.dependencies]
metrics = "0.24"
metrics-exporter-prometheus = { version = "0.18", default-features = false }
```

```toml
# crates/server/Cargo.toml — append to [dependencies]
metrics.workspace = true
metrics-exporter-prometheus.workspace = true
```

```rust
// crates/server/src/lib.rs — add the module, keeping the list alphabetical
pub mod aof;
pub mod cluster;
pub mod connection;
pub mod dispatcher;
pub mod metrics;
pub mod replication;
pub use connection::serve;
```

- [ ] **Step 2: Write the failing test**

```rust
// crates/server/src/metrics.rs — the whole file, for now just the test
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_handle_is_idempotent_and_renders_what_was_recorded() {
        let first = recorder_handle();
        let second = recorder_handle(); // must not panic on the second install attempt
        ::metrics::counter!("rocket_mem_test_counter").increment(3);
        let rendered = second.render();
        assert!(
            rendered.contains("rocket_mem_test_counter 3"),
            "counter missing from render:\n{rendered}"
        );
        assert!(first.render().contains("rocket_mem_test_counter"));
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p rocket-mem metrics::tests`
Expected: FAIL to compile with "cannot find function `recorder_handle`"

- [ ] **Step 4: Implement the recorder**

```rust
// crates/server/src/metrics.rs — add above the tests module
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::OnceLock;

/// Latency buckets, in seconds. Explicit buckets matter: without them the exporter renders
/// histograms as *summaries with quantiles*, which cannot be aggregated across instances and are
/// the wrong shape for "latency histograms per command". The ladder starts at 50µs because a
/// local in-memory GET lands in the tens of microseconds -- a ladder starting at 5ms would put
/// every command in the first bucket and measure nothing.
const LATENCY_BUCKETS: [f64; 14] = [
    0.000_05, 0.000_1, 0.000_25, 0.000_5, 0.001, 0.002_5, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25,
    0.5, 1.0,
];

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Installs the process-wide Prometheus recorder exactly once and returns a handle to it.
/// `::metrics::set_global_recorder` may only succeed once per process, and a test binary runs many
/// servers in one process, so this is behind a `OnceLock`: the first caller installs, every later
/// caller gets a clone of the same handle.
pub fn recorder_handle() -> PrometheusHandle {
    HANDLE
        .get_or_init(|| {
            let recorder = PrometheusBuilder::new()
                .set_buckets(&LATENCY_BUCKETS)
                .expect("LATENCY_BUCKETS is a non-empty ascending slice of finite values")
                .build_recorder();
            let handle = recorder.handle();
            // A failed install means something else already installed a recorder in this
            // process. That is not fatal: our handle still renders whatever reaches our
            // recorder, and the alternative -- panicking -- would take down a server over an
            // observability detail.
            if ::metrics::set_global_recorder(recorder).is_err() {
                eprintln!(
                    "metrics: a global recorder was already installed; metrics may be incomplete"
                );
            }
            handle
        })
        .clone()
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p rocket-mem metrics::tests`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/server/Cargo.toml crates/server/src/lib.rs crates/server/src/metrics.rs
git commit -m "feat(server): install a Prometheus recorder with explicit latency buckets"
```

---

### Task 2: the counters metrics read from

**Files:**
- Modify: `crates/engine/src/shard.rs` (add beside `keys()`, `:101-111`)
- Modify: `crates/engine/src/store.rs` (add beside `keys()`, `:39-41`)
- Modify: `crates/engine/src/engine.rs` (add beside `memory_used()`, `:104-106`)
- Modify: `crates/server/src/replication.rs` (`ReplicaRegistry` at `:12-37`, `ReplicationHandle` struct at `:42`, `new` at `:93`)

**Interfaces:**
- Consumes: nothing.
- Produces: `Shard::counts() -> (usize, usize)`, `Store::key_counts() -> (usize, usize)`, `Engine::key_counts() -> (usize, usize)`, `ReplicaRegistry::{len, is_empty}`, and on `ReplicationHandle`: `connection_opened`, `connection_closed`, `connected_clients`, `total_connections`, `command_executed`, `total_commands`, `record_expired`, `expired_keys`, `last_apply_unix`, `last_apply_slot`. Consumed by Tasks 3–4 and by `05-info-and-hello-overhaul.md`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/engine.rs — add to the existing tests module
    #[test]
    fn key_counts_reports_live_keys_and_how_many_have_an_expiry() {
        let engine = Engine::new();
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
        assert_eq!(engine.key_counts(), (2, 1));
    }

    #[test]
    fn key_counts_ignores_already_expired_keys() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"gone"),
            Value::String(Bytes::from_static(b"1")),
        );
        engine.expire_at(
            b"gone",
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );
        assert_eq!(engine.key_counts(), (0, 0));
    }
```

```rust
// crates/server/src/replication.rs — add to the existing tests module
    #[test]
    fn registry_len_tracks_registered_replicas() {
        let registry = ReplicaRegistry::default();
        assert!(registry.is_empty());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register(tx);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn connection_counters_move_with_open_and_close() {
        let h = ReplicationHandle::default();
        assert_eq!(h.connected_clients(), 0);
        assert_eq!(h.total_connections(), 0);
        h.connection_opened();
        h.connection_opened();
        assert_eq!(h.connected_clients(), 2);
        assert_eq!(h.total_connections(), 2);
        h.connection_closed();
        assert_eq!(h.connected_clients(), 1);
        assert_eq!(h.total_connections(), 2); // total never goes down
    }

    #[test]
    fn command_and_expiry_counters_accumulate() {
        let h = ReplicationHandle::default();
        h.command_executed();
        h.command_executed();
        assert_eq!(h.total_commands(), 2);
        h.record_expired(5);
        h.record_expired(0);
        h.record_expired(2);
        assert_eq!(h.expired_keys(), 7);
    }

    #[test]
    fn last_apply_unix_starts_at_zero_and_follows_the_shared_slot() {
        let h = ReplicationHandle::default();
        assert_eq!(h.last_apply_unix(), 0);
        h.last_apply_slot()
            .store(1_756_512_000, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(h.last_apply_unix(), 1_756_512_000);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p engine engine::tests::key_counts && cargo test -p rocket-mem replication::tests::registry_len replication::tests::connection_counters replication::tests::command_and_expiry replication::tests::last_apply_unix`
Expected: FAIL to compile — none of these methods exist yet

- [ ] **Step 3: Implement the engine-side counts**

```rust
// crates/engine/src/shard.rs — add directly below `keys()` (:111)
    /// `(live entries, of which carry an expiry)` in one read lock and one pass. Skips entries
    /// that are already expired but not yet reaped, matching what `keys()` and `entries()`
    /// report, so `INFO`'s keyspace line and `KEYS *` can never disagree.
    pub fn counts(&self) -> (usize, usize) {
        let guard = self.map.read();
        let mut total = 0;
        let mut with_expiry = 0;
        for entry in guard.values() {
            if entry.is_expired() {
                continue;
            }
            total += 1;
            if entry.expires_at.is_some() {
                with_expiry += 1;
            }
        }
        (total, with_expiry)
    }
```

```rust
// crates/engine/src/store.rs — add directly below `keys()` (:41)
    /// Sums `Shard::counts` across all 16 shards. Each shard is locked and released in turn, so
    /// this is a sampling read, not a point-in-time view of the whole store -- which is exactly
    /// right for a metrics gauge and for `INFO`.
    pub fn key_counts(&self) -> (usize, usize) {
        self.shards.iter().fold((0, 0), |(t, e), shard| {
            let (st, se) = shard.counts();
            (t + st, e + se)
        })
    }
```

```rust
// crates/engine/src/engine.rs — add directly below `memory_used()` (:106)
    /// `(live keys, of which carry an expiry)`. A thin facade over `Store`, matching `Engine`'s
    /// established role. Feeds the `rocket_mem_keys` gauges and `INFO`'s keyspace section.
    pub fn key_counts(&self) -> (usize, usize) {
        self.store.key_counts()
    }
```

- [ ] **Step 4: Implement the server-side counters**

```rust
// crates/server/src/replication.rs — add to the existing `impl ReplicaRegistry` block (:17-37)
    /// How many replicas are currently registered. Note this counts senders, which are pruned
    /// lazily by `broadcast`, so a replica that died since the last write may still be counted
    /// until the next one -- an acceptable lag for a gauge, and cheaper than probing sockets.
    pub fn len(&self) -> usize {
        self.senders.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Required by `clippy::len_without_is_empty`, which `-D warnings` makes a hard error.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
```

```rust
// crates/server/src/replication.rs — add as fields of `pub struct ReplicationHandle`
    /// Live client connections, kept by a Drop guard in `handle_connection` so it is decremented
    /// on every one of that function's early returns, including the `serve_replica` path.
    connected_clients: AtomicUsize,
    /// Every connection ever accepted; never decremented.
    total_connections: AtomicU64,
    /// Every command that reached `dispatch_and_log`. Replicated commands applied through plain
    /// `dispatch` are deliberately *not* counted -- they are not client traffic.
    total_commands: AtomicU64,
    /// Keys removed by the active expiry sweep. Passively expired keys (a read finding a key
    /// already dead) are not counted: that would mean a counter on the hottest read path in the
    /// project, inside `Shard`, for a statistic nothing gates on.
    expired_keys: AtomicU64,
    /// Unix seconds at which this node last applied a replicated frame; 0 if it never has. An
    /// `Arc` because the spawned follower task is `'static` and needs its own handle.
    last_apply_unix: Arc<AtomicI64>,
```

```rust
// crates/server/src/replication.rs — add to `new`'s struct literal (:93)
            connected_clients: AtomicUsize::new(0),
            total_connections: AtomicU64::new(0),
            total_commands: AtomicU64::new(0),
            expired_keys: AtomicU64::new(0),
            last_apply_unix: Arc::new(AtomicI64::new(0)),
```

```rust
// crates/server/src/replication.rs — the imports at the top of the file gain the two new types
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
```

```rust
// crates/server/src/replication.rs — add to the existing `impl ReplicationHandle` block
    /// Called once per accepted client connection. Bumps both the live gauge and the lifetime
    /// total; `connection_closed` is its Drop-guarded pair.
    pub fn connection_opened(&self) {
        self.connected_clients.fetch_add(1, Ordering::Relaxed);
        self.total_connections.fetch_add(1, Ordering::Relaxed);
    }
    pub fn connection_closed(&self) {
        self.connected_clients.fetch_sub(1, Ordering::Relaxed);
    }
    pub fn connected_clients(&self) -> usize {
        self.connected_clients.load(Ordering::Relaxed)
    }
    pub fn total_connections(&self) -> u64 {
        self.total_connections.load(Ordering::Relaxed)
    }
    pub fn command_executed(&self) {
        self.total_commands.fetch_add(1, Ordering::Relaxed);
    }
    pub fn total_commands(&self) -> u64 {
        self.total_commands.load(Ordering::Relaxed)
    }
    pub fn record_expired(&self, removed: usize) {
        if removed > 0 {
            self.expired_keys.fetch_add(removed as u64, Ordering::Relaxed);
        }
    }
    pub fn expired_keys(&self) -> u64 {
        self.expired_keys.load(Ordering::Relaxed)
    }
    pub fn last_apply_unix(&self) -> i64 {
        self.last_apply_unix.load(Ordering::Relaxed)
    }
    /// The shared slot itself, for the spawned follower task to write into.
    pub fn last_apply_slot(&self) -> Arc<AtomicI64> {
        Arc::clone(&self.last_apply_unix)
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p engine engine::tests::key_counts && cargo test -p rocket-mem replication::tests`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/shard.rs crates/engine/src/store.rs crates/engine/src/engine.rs crates/server/src/replication.rs
git commit -m "feat: add key counts and server-level counters for metrics"
```

---

### Task 3: the `/metrics` endpoint

**Files:**
- Modify: `crates/server/src/metrics.rs`

**Interfaces:**
- Consumes: `recorder_handle` (Task 1), the counters from Task 2.
- Produces: `pub fn refresh_sampled_gauges(engine: &Engine, replication: &ReplicationHandle)` and `pub async fn serve_metrics(listener: TcpListener, handle: PrometheusHandle, engine: Arc<Engine>, replication: Arc<ReplicationHandle>)`, consumed by Task 5 and by the integration test in Task 6.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/metrics.rs — add to the existing tests module
    #[tokio::test]
    async fn the_metrics_endpoint_serves_the_rendered_registry_and_404s_everything_else() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let handle = recorder_handle();
        let engine = std::sync::Arc::new(engine::Engine::new());
        engine.set(
            bytes::Bytes::from_static(b"k"),
            engine::Value::String(bytes::Bytes::from_static(b"v")),
        );
        let replication =
            std::sync::Arc::new(crate::replication::ReplicationHandle::default());
        replication.connection_opened();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve_metrics(
            listener,
            handle,
            std::sync::Arc::clone(&engine),
            std::sync::Arc::clone(&replication),
        ));

        async fn get(addr: std::net::SocketAddr, path: &str) -> String {
            let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
            socket
                .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).await.unwrap();
            response
        }

        let body = get(addr, "/metrics").await;
        assert!(body.starts_with("HTTP/1.1 200 OK\r\n"), "{body}");
        assert!(
            body.contains("Content-Type: text/plain; version=0.0.4"),
            "{body}"
        );
        assert!(body.contains("rocket_mem_keys 1"), "{body}");
        assert!(body.contains("rocket_mem_connected_clients 1"), "{body}");
        assert!(body.contains("rocket_mem_memory_used_bytes"), "{body}");

        let missing = get(addr, "/nope").await;
        assert!(missing.starts_with("HTTP/1.1 404 Not Found\r\n"), "{missing}");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem metrics::tests::the_metrics_endpoint`
Expected: FAIL to compile with "cannot find function `serve_metrics`"

- [ ] **Step 3: Implement the endpoint**

```rust
// crates/server/src/metrics.rs — add above the tests module
use crate::replication::ReplicationHandle;
use engine::Engine;
use std::sync::Arc;

/// Refreshes the metrics that are *sampled* rather than incremented as they happen. Called
/// immediately before each render, so a scrape reflects the moment it was taken rather than the
/// last write. Counters use `.absolute()` because their authoritative value already lives in an
/// atomic elsewhere -- incrementing a second copy would be one more thing to keep in sync.
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

/// Serves `GET /metrics` (404 for anything else) over `listener` forever, and runs the
/// exporter's periodic upkeep. A hand-rolled HTTP/1.1 responder rather than a `hyper`
/// dependency: one route, no keep-alive, no body parsing.
///
/// Deliberately *not* started by `serve()` -- every integration test in the workspace calls
/// `serve()`, and a fixed metrics port would make them collide with each other and with a
/// developer's running server. `main.rs` binds it; this test binds `127.0.0.1:0`.
pub async fn serve_metrics(
    listener: tokio::net::TcpListener,
    handle: PrometheusHandle,
    engine: Arc<Engine>,
    replication: Arc<ReplicationHandle>,
) {
    tokio::spawn(upkeep_loop(handle.clone()));
    loop {
        let Ok((socket, _addr)) = listener.accept().await else {
            continue; // a failed accept shouldn't take the metrics listener down
        };
        tokio::spawn(serve_one_scrape(
            socket,
            handle.clone(),
            Arc::clone(&engine),
            Arc::clone(&replication),
        ));
    }
}

/// The exporter accumulates per-bucket histogram state that `run_upkeep` drains; skipping it is
/// a slow leak in a long-running process.
async fn upkeep_loop(handle: PrometheusHandle) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        interval.tick().await;
        handle.run_upkeep();
    }
}

async fn serve_one_scrape(
    mut socket: tokio::net::TcpStream,
    handle: PrometheusHandle,
    engine: Arc<Engine>,
    replication: Arc<ReplicationHandle>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // One read is enough: a scrape is a bare GET with a few headers, and this endpoint has no
    // request body to parse. A request larger than this is not one we would answer differently.
    let mut buf = [0u8; 1024];
    let read = match socket.read(&mut buf).await {
        Ok(0) | Err(_) => return,
        Ok(n) => n,
    };
    let request = String::from_utf8_lossy(&buf[..read]);
    let path = request.split_whitespace().nth(1).unwrap_or("");
    let response = if path.starts_with("/metrics") {
        refresh_sampled_gauges(&engine, &replication);
        let body = handle.render();
        format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/plain; version=0.0.4; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
    } else {
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
    };
    let _ = socket.write_all(response.as_bytes()).await;
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem metrics::tests`
Expected: PASS, both tests

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/metrics.rs
git commit -m "feat(server): serve Prometheus metrics from a dedicated listener"
```

---

### Task 4: instrument `dispatch_and_log`, connections, and expiry

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (`dispatch_and_log` at `:1042-1167`)
- Modify: `crates/server/src/connection.rs` (`active_expire_loop` at `:44-52`, `handle_connection` at `:73-125`)
- Modify: `crates/server/src/replication.rs` (`start_replicating` at `:136`, `replication_client_loop` at `:185`, `sync_once` at `:218`)

**Interfaces:**
- Consumes: `KNOWN_COMMANDS` (`02-cluster-commands-and-moved.md`), the counters from Task 2.
- Produces: `fn command_name_upper(frame: &Frame) -> String` and `fn metric_label(name: &str) -> String` in `dispatcher.rs`, plus `dispatch_and_log_inner` with `dispatch_and_log`'s original signature — all three consumed by `06-slowlog.md`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
    #[test]
    fn metric_label_lowercases_known_commands_and_collapses_the_rest() {
        assert_eq!(metric_label("GET"), "get");
        assert_eq!(metric_label("ZINCRBY"), "zincrby");
        assert_eq!(metric_label("CLUSTER"), "cluster");
        // an unknown name must never become its own Prometheus series
        assert_eq!(metric_label("DEFINITELYNOTACOMMAND"), "other");
        assert_eq!(metric_label(""), "other");
    }

    #[test]
    fn command_name_upper_reads_the_command_name_from_any_frame_shape() {
        assert_eq!(command_name_upper(&cmd(&[b"get", b"k"])), "GET");
        assert_eq!(command_name_upper(&cmd(&[b"SeT", b"k", b"v"])), "SET");
        assert_eq!(command_name_upper(&Frame::Simple("nope".into())), "");
        assert_eq!(command_name_upper(&Frame::Array(vec![])), "");
    }

    #[test]
    fn dispatch_and_log_counts_every_command_it_handles() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        for command in [
            cmd(&[b"SET", b"k", b"v"]),
            cmd(&[b"GET", b"k"]),
            cmd(&[b"PING"]),
        ] {
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                command,
                &mut Protocol::default(),
                1,
            );
        }
        assert_eq!(replication.total_commands(), 3);
    }

    #[test]
    fn dispatch_and_log_still_behaves_identically_after_the_wrapper_split() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"SET", b"k", b"v"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Simple("OK".into())
        );
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"GET", b"k"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"v"))
        );
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"NOPE"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR unknown command 'NOPE'".into())
        );
    }
```

```rust
// crates/server/src/connection.rs — add to the existing tests module
    #[tokio::test]
    async fn serve_tracks_connected_clients_and_drops_the_count_on_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::default());
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let mut framed = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        framed
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))]))
            .await
            .unwrap();
        framed.next().await.unwrap().unwrap();
        assert_eq!(replication.connected_clients(), 1);
        assert_eq!(replication.total_connections(), 1);

        drop(framed);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(replication.connected_clients(), 0);
        assert_eq!(replication.total_connections(), 1); // the lifetime total never drops
    }

    #[tokio::test]
    async fn the_active_expiry_sweep_counts_the_keys_it_removes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let engine = Arc::new(Engine::new());
        engine.set(
            Bytes::from_static(b"k"),
            engine::Value::String(Bytes::from_static(b"v")),
        );
        engine.expire_at(
            b"k",
            std::time::Instant::now() + std::time::Duration::from_millis(20),
        );
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::default());
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        // one shard per 100ms tick, 16 shards -- 2s covers a full rotation with headroom, the
        // same bound `serve_actively_expires_a_key_even_without_any_read_touching_it` uses.
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
        assert_eq!(replication.expired_keys(), 1);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::metric_label dispatcher::tests::command_name_upper dispatcher::tests::dispatch_and_log_counts connection::tests::serve_tracks_connected connection::tests::the_active_expiry_sweep`
Expected: FAIL to compile with "cannot find function `metric_label`"/"cannot find function `command_name_upper`"; the two `connection` tests fail on `total_commands`/`expired_keys` being 0 once they compile

- [ ] **Step 3: Split `dispatch_and_log` and instrument the wrapper**

Rename the existing `pub fn dispatch_and_log` (`:1042`) to `fn dispatch_and_log_inner`, keeping its body and parameters **exactly** as they are, and add the new wrapper above it:

```rust
// crates/server/src/dispatcher.rs
/// The uppercased command name, or `""` for a frame that isn't a command array. Cheap enough to
/// call once per command; `07-benchmark-and-flamegraph.md` is where this allocation goes away.
fn command_name_upper(frame: &Frame) -> String {
    let Frame::Array(items) = frame else {
        return String::new();
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return String::new();
    };
    String::from_utf8_lossy(name).to_ascii_uppercase()
}

/// The `cmd` label value for a command name: its lowercase form if we know the command, the
/// literal `other` otherwise. The `other` fallback is what bounds Prometheus label cardinality --
/// without it, a client sending random command names could create unbounded series.
fn metric_label(name: &str) -> String {
    if KNOWN_COMMANDS.binary_search(&name).is_ok() {
        name.to_ascii_lowercase()
    } else {
        "other".to_string()
    }
}

/// Times and counts every client command, then delegates to `dispatch_and_log_inner`, which
/// holds all the actual behavior. The split exists because the inner function has seven early
/// returns (-MOVED, -CROSSSLOT, -READONLY, SAVE, REPLICAOF, CLUSTER, and the unknown-command
/// fall-through) and instrumenting each one would guarantee a future eighth is missed.
///
/// `dispatch` itself is deliberately *not* instrumented: it is what `aof::replay` and the
/// follower apply loop call, and counting a 5,000-frame boot-time replay as 5,000 client
/// commands would make every dashboard lie about traffic.
///
/// The signature is byte-for-byte the one Sprint 5 left, so none of the ~36 call sites change.
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
    let name = command_name_upper(&frame); // read before `frame` is moved into the inner call
    let label = metric_label(&name);
    let started = std::time::Instant::now();

    let reply = dispatch_and_log_inner(engine, aof, replication, frame, protocol, client_id);

    let elapsed = started.elapsed();
    replication.command_executed();
    ::metrics::counter!("rocket_mem_commands_total", "cmd" => label.clone()).increment(1);
    ::metrics::histogram!("rocket_mem_command_duration_seconds", "cmd" => label.clone())
        .record(elapsed.as_secs_f64());
    if matches!(reply, Frame::Error(_)) {
        ::metrics::counter!("rocket_mem_command_errors_total", "cmd" => label).increment(1);
    }
    reply
}
```

- [ ] **Step 4: Instrument connections and the expiry sweep**

```rust
// crates/server/src/connection.rs — add above `handle_connection` (:73)
/// Decrements the live-connection count on drop, so every one of `handle_connection`'s early
/// returns -- and the `serve_replica` path, which never returns normally -- is covered without
/// each of them having to remember.
struct ClientGuard(Arc<ReplicationHandle>);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.connection_closed();
    }
}
```

```rust
// crates/server/src/connection.rs — the first two statements inside `handle_connection`'s body
    replication.connection_opened();
    let _client_guard = ClientGuard(Arc::clone(&replication));
```

```rust
// crates/server/src/connection.rs — `active_expire_loop` gains the handle so it can count
async fn active_expire_loop(engine: Arc<Engine>, replication: Arc<ReplicationHandle>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut shard_idx: usize = 0;
    loop {
        interval.tick().await;
        replication.record_expired(engine.active_expire_cycle(shard_idx));
        shard_idx = shard_idx.wrapping_add(1);
    }
}
```

```rust
// crates/server/src/connection.rs — `serve`'s spawn of that loop (:17) gains the argument
    tokio::spawn(active_expire_loop(
        Arc::clone(&engine),
        Arc::clone(&replication),
    ));
```

```rust
// crates/server/src/replication.rs — `sync_once` records when it last applied a frame.
// Its signature gains a trailing `last_apply: &AtomicI64`; `replication_client_loop` gains a
// trailing `last_apply: Arc<AtomicI64>` and passes `&last_apply` down; `start_replicating`
// passes `self.last_apply_slot()`. The two existing direct callers of `sync_once` in this
// file's tests (`sync_once_loads_the_snapshot_then_applies_streamed_frames` at :357 and
// `sync_once_does_not_load_the_snapshot_when_its_generation_is_already_stale` at :414) each
// gain a trailing `&AtomicI64::new(0)` argument.
//
// Inside sync_once's `while let Some(result) = framed.next().await` loop, directly after the
// existing `if let protocol::Frame::Error(e) = reply { ... }` block:
        last_apply.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            Ordering::Relaxed,
        );
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem`
Expected: PASS, every test in the crate — the wrapper split changes no behavior, so every pre-existing `dispatch_and_log` test must still pass unchanged

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs crates/server/src/connection.rs crates/server/src/replication.rs
git commit -m "feat(server): record per-command metrics in a dispatch_and_log wrapper"
```

---

### Task 5: `main.rs` wiring for `ROCKET_MEM_METRICS_ADDR`

**Files:**
- Modify: `crates/server/src/main.rs`

**Interfaces:**
- Consumes: `metrics::recorder_handle`, `metrics::serve_metrics` (Tasks 1 and 3).
- Produces: a running `/metrics` endpoint in the shipped binary; verified by Task 6's integration test and by plan 07's benchmark run.

- [ ] **Step 1: Install the recorder first and bind the endpoint last**

```rust
// crates/server/src/main.rs — the FIRST statement of `main`, before any env var is read: the
// recorder must be installed before anything can record, or early metrics are dropped.
    let metrics_handle = rocket_mem::metrics::recorder_handle();
```

```rust
// crates/server/src/main.rs — after `replication` is built and before `serve` is awaited
    let metrics_addr = std::env::var("ROCKET_MEM_METRICS_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:9121".to_string());
    let metrics_listener = tokio::net::TcpListener::bind(&metrics_addr).await?;
    println!(
        "Metrics on http://{}/metrics",
        metrics_listener.local_addr()?
    );
    tokio::spawn(rocket_mem::metrics::serve_metrics(
        metrics_listener,
        metrics_handle,
        Arc::clone(&engine),
        Arc::clone(&replication),
    ));
```

- [ ] **Step 2: Verify by hand**

```bash
cargo build --workspace
ROCKET_MEM_ADDR=127.0.0.1:7999 \
  ROCKET_MEM_AOF_PATH=/tmp/rm-metrics.aof \
  ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-metrics.snapshot \
  ROCKET_MEM_METRICS_ADDR=127.0.0.1:9199 \
  ./target/debug/rocket-mem &
sleep 1
redis-cli -p 7999 set foo bar
redis-cli -p 7999 get foo
curl -s http://127.0.0.1:9199/metrics | grep -E 'rocket_mem_(commands_total|command_duration_seconds_bucket|keys|connected_clients)' | head -20
kill %1
```

Expected: `rocket_mem_commands_total{cmd="set"} 1`, `rocket_mem_commands_total{cmd="get"} 1`, several `rocket_mem_command_duration_seconds_bucket{cmd="set",le="..."}` lines, `rocket_mem_keys 1`, and a non-zero `rocket_mem_connected_clients`. (If `redis-cli` isn't installed, drive the two commands with the `tests/cluster.rs` harness style instead — Task 6's automated test covers the same ground.)

- [ ] **Step 3: Commit**

```bash
git add crates/server/src/main.rs
git commit -m "feat(server): expose /metrics from ROCKET_MEM_METRICS_ADDR"
```

---

### Task 6: end-to-end scrape test

**Files:**
- Create: `crates/server/tests/metrics.rs`

**Interfaces:**
- Consumes: `rocket_mem::metrics::{recorder_handle, serve_metrics}`, `rocket_mem::serve`.
- Produces: the sprint's third DoD item ("Prometheus metrics visible and scraping correctly"), evidenced.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/tests/metrics.rs
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use protocol::codec::RespCodec;
use protocol::Frame;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

async fn scrape(addr: std::net::SocketAddr) -> String {
    let mut socket = TcpStream::connect(addr).await.unwrap();
    socket
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn command_counts_and_latencies_appear_in_the_prometheus_output() {
    let handle = rocket_mem::metrics::recorder_handle();
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("node.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&engine),
        dir.path().join("node.snapshot"),
    ));

    let resp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let resp_addr = resp_listener.local_addr().unwrap();
    tokio::spawn(rocket_mem::serve(
        resp_listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));

    let metrics_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let metrics_addr = metrics_listener.local_addr().unwrap();
    tokio::spawn(rocket_mem::metrics::serve_metrics(
        metrics_listener,
        handle,
        Arc::clone(&engine),
        Arc::clone(&replication),
    ));

    let mut client = Framed::new(
        TcpStream::connect(resp_addr).await.unwrap(),
        RespCodec::default(),
    );
    for command in [
        vec![b"SET".to_vec(), b"k".to_vec(), b"v".to_vec()],
        vec![b"GET".to_vec(), b"k".to_vec()],
        vec![b"GET".to_vec(), b"k".to_vec()],
        vec![b"NOSUCHCOMMAND".to_vec()],
    ] {
        client
            .send(Frame::Array(
                command.into_iter().map(|p| Frame::Bulk(Bytes::from(p))).collect(),
            ))
            .await
            .unwrap();
        client.next().await.unwrap().unwrap();
    }

    let body = scrape(metrics_addr).await;
    assert!(body.contains(r#"rocket_mem_commands_total{cmd="set"} 1"#), "{body}");
    assert!(body.contains(r#"rocket_mem_commands_total{cmd="get"} 2"#), "{body}");
    // an unknown command is counted, but collapsed into the bounded `other` label
    assert!(body.contains(r#"rocket_mem_commands_total{cmd="other"} 1"#), "{body}");
    assert!(
        body.contains(r#"rocket_mem_command_errors_total{cmd="other"} 1"#),
        "{body}"
    );
    assert!(
        body.contains(r#"rocket_mem_command_duration_seconds_bucket{cmd="set","#),
        "{body}"
    );
    assert!(body.contains("rocket_mem_keys 1"), "{body}");
    assert!(body.contains("rocket_mem_connected_clients 1"), "{body}");
    assert!(body.contains("rocket_mem_connected_replicas 0"), "{body}");
    assert!(body.contains("rocket_mem_connections_total 1"), "{body}");
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p rocket-mem --test metrics`
Expected: PASS. If a bucket assertion fails, print `body` and check whether the exporter rendered a `_sum`/`_count` summary instead of `_bucket` lines — that means `set_buckets` was dropped from `recorder_handle` in Task 1.

- [ ] **Step 3: Run the full workspace verification**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 4: Commit**

```bash
git add crates/server/tests/metrics.rs
git commit -m "test(server): scrape /metrics end-to-end and assert command series"
```
