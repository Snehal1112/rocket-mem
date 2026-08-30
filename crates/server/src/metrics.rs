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
