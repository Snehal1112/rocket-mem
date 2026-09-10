use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::OnceLock;

/// Latency buckets, in seconds. Explicit buckets matter: without them the exporter renders
/// histograms as *summaries with quantiles*, which cannot be aggregated across instances and are
/// the wrong shape for "latency histograms per command". The ladder starts at 50µs because a
/// local in-memory GET lands in the tens of microseconds -- a ladder starting at 5ms would put
/// every command in the first bucket and measure nothing.
const LATENCY_BUCKETS: [f64; 14] = [
    0.000_05, 0.000_1, 0.000_25, 0.000_5, 0.001, 0.002_5, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5,
    1.0,
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
                tracing::warn!(
                    "global metrics recorder already installed; metrics may be incomplete"
                );
            }
            handle
        })
        .clone()
}

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
    ::metrics::gauge!("rocket_mem_master_repl_offset").set(replication.master_repl_offset() as f64);
    // Zero on a node that has never been a follower. `INFO` hides this behind `role:slave`;
    // a gauge cannot, so it simply reads 0 there, which is the honest value.
    ::metrics::gauge!("rocket_mem_slave_repl_offset").set(replication.slave_repl_offset() as f64);
    // Reported unconditionally, whether or not fencing is enabled -- "how many replicas are
    // currently good" is useful information on its own, and it's the exact input the
    // NOREPLICAS gate in dispatch_and_log_inner compares against min_replicas_to_write.
    ::metrics::gauge!("rocket_mem_good_replicas").set(
        replication
            .registry
            .good_replicas(replication.min_replicas_max_lag()) as f64,
    );
    // The furthest-behind connected replica's acked offset -- paired with
    // rocket_mem_master_repl_offset above, this makes replication lag in bytes computable
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
    // Exact-match the route: prefix matching would incorrectly 200 unrelated paths like /metricsx or /metrics-admin.
    let response = if path == "/metrics" || path.starts_with("/metrics?") {
        refresh_sampled_gauges(&engine, &replication);
        let body = handle.render();
        // `trace`, not `debug`: this endpoint is scraped on a fixed interval (Prometheus
        // defaults to 15s) for as long as the process runs, so anything louder would be
        // constant background noise at a level operators are told is safe to leave on. No
        // enclosing span carries this -- `serve_one_scrape` is its own accept-loop task, wired
        // up independently of `serve()`'s per-connection spans -- so the event stands alone.
        tracing::trace!(bytes = body.len(), "metrics scrape served");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every sampled gauge (`rocket_mem_keys`, `rocket_mem_good_replicas`,
    /// `rocket_mem_replica_min_ack_offset`, ...) lives on the one process-wide recorder
    /// `recorder_handle` installs, and `refresh_sampled_gauges` overwrites *all* of them in one
    /// call -- not just whichever one a given test cares about. `cargo test` runs this file's
    /// tests concurrently by default, so any two tests that call `refresh_sampled_gauges` (or
    /// trigger it indirectly, as `the_metrics_endpoint_...` does through `serve_metrics` on
    /// every scrape) race: one test's call can land between another's set and its own read of
    /// the same gauge, so a test asserting an exact value can observe a value a sibling test's
    /// unrelated `Engine`/`ReplicationHandle` wrote. `tokio::sync::Mutex`, not `std::sync`,
    /// because `the_metrics_endpoint_...` is a `#[tokio::test]` that must hold this across an
    /// `.await` (the GET that triggers the scrape); plain `#[test]`s use `blocking_lock()`.
    /// Held only across each test's own set/scrape-then-read window, so this serializes just
    /// the tests that touch sampled gauges, not the whole suite.
    static GAUGE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

    #[test]
    fn refresh_sampled_gauges_reports_good_replicas() {
        let _guard = GAUGE_TEST_LOCK.blocking_lock();
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
        let _guard = GAUGE_TEST_LOCK.blocking_lock();
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
        let _guard = GAUGE_TEST_LOCK.blocking_lock();
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
        let _guard = GAUGE_TEST_LOCK.blocking_lock();
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

    #[tokio::test]
    async fn the_metrics_endpoint_serves_the_rendered_registry_and_404s_everything_else() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let handle = recorder_handle();
        let engine = std::sync::Arc::new(engine::Engine::new());
        engine.set(
            bytes::Bytes::from_static(b"k"),
            engine::Value::String(bytes::Bytes::from_static(b"v")),
        );
        let replication = std::sync::Arc::new(crate::replication::ReplicationHandle::default());
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

        replication.advance_master_repl_offset(42);
        // A different number from the leader offset above, on purpose: identical values would
        // let a gauge wired to the wrong accessor pass this test.
        replication.set_slave_repl_offset(4134);
        // Held across this one scrape only: it's the only request below whose assertions pin
        // exact sampled-gauge values, so it's the only one that can race against the other
        // gauge tests in this module -- see GAUGE_TEST_LOCK's doc comment.
        let body = {
            let _guard = GAUGE_TEST_LOCK.lock().await;
            get(addr, "/metrics").await
        };
        assert!(body.starts_with("HTTP/1.1 200 OK\r\n"), "{body}");
        assert!(
            body.contains("Content-Type: text/plain; version=0.0.4"),
            "{body}"
        );
        assert!(body.contains("rocket_mem_keys 1"), "{body}");
        assert!(body.contains("rocket_mem_connected_clients 1"), "{body}");
        assert!(body.contains("rocket_mem_memory_used_bytes"), "{body}");
        assert!(body.contains("rocket_mem_master_repl_offset 42"), "{body}");
        assert!(body.contains("rocket_mem_slave_repl_offset 4134"), "{body}");

        let missing = get(addr, "/nope").await;
        assert!(
            missing.starts_with("HTTP/1.1 404 Not Found\r\n"),
            "{missing}"
        );

        // Verify exact-match: /metricsx should 404, not 200 (prefix-match bug).
        let metricsx = get(addr, "/metricsx").await;
        assert!(
            metricsx.starts_with("HTTP/1.1 404 Not Found\r\n"),
            "expected 404 for /metricsx, got: {metricsx}"
        );

        // Verify query strings still work: /metrics?foo=bar should 200.
        let with_query = get(addr, "/metrics?format=openmetrics").await;
        assert!(
            with_query.starts_with("HTTP/1.1 200 OK\r\n"),
            "expected 200 for /metrics?format=openmetrics, got: {with_query}"
        );
    }

    // A capture assertion on `serve_one_scrape`'s `trace!` output does not live here. `tracing`
    // caches callsite `Interest` per callsite, process-globally, the first time that callsite
    // is reached (see `test(logging): stop asserting on captured logs from unit-test binaries`
    // for the two tests this bit for real). The test above scrapes `/metrics` three times with
    // no subscriber installed, which would reach the new `trace!` callsite first in the
    // ~575-test unit binary this file compiles into and could permanently decide it's
    // uninteresting before a later test's own capture subscriber gets a turn.
    // `a_metrics_scrape_is_traced_and_a_404_is_not` lives in `crates/server/tests/logging.rs`
    // instead, a separate, much smaller integration binary where this callsite is touched by
    // nothing else. Do not re-add a capture assertion here.
}
