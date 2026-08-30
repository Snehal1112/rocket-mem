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
                eprintln!(
                    "metrics: a global recorder was already installed; metrics may be incomplete"
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
}
