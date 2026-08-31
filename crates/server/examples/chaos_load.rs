// crates/server/examples/chaos_load.rs
//! Chaos-test load generator: writes sequential keys against a live server over real TCP,
//! reconnecting on every write so a server that dies mid-connection (the whole point of the
//! chaos loop in scripts/chaos.sh) never wedges this loop. Logs one line per *confirmed*
//! successful write -- this log is the independent "what should be there" record chaos.sh
//! verifies the post-chaos keyspace against. See
//! ../../docs/superpowers/plans/2026-08-31-sprint-8-plans/11-chaos-test.md.
//!
//! Throughput is not the point here -- correctness of the confirmed-write log is. The write
//! loop is rate-limited to roughly 10 writes/second (a 100ms delay after every attempt, success
//! or failure) so that scripts/chaos.sh's exhaustive per-key verification step (one `redis-cli
//! GET` round trip per key per node, ~3ms each, by design not sampled) stays tractable at real
//! chaos-run durations: a full 3600s run produces on the order of 36,000 keys instead of
//! millions, keeping verification a matter of minutes rather than hours.

use redis::Commands;
use std::io::Write;

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .expect("usage: chaos_load <redis-url> <log-path> <duration-secs>");
    let log_path = args
        .next()
        .expect("usage: chaos_load <redis-url> <log-path> <duration-secs>");
    let duration_secs: u64 = args
        .next()
        .expect("usage: chaos_load <redis-url> <log-path> <duration-secs>")
        .parse()
        .expect("duration-secs must be a number");

    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("failed to open the write log");

    let start = std::time::Instant::now();
    let mut written: u64 = 0;
    let mut i: u64 = 0;
    while start.elapsed().as_secs() < duration_secs {
        let key = format!("chaos:{i}");
        let value = format!("v{i}");
        i += 1;

        let outcome = redis::Client::open(url.as_str())
            .and_then(|client| client.get_connection())
            .and_then(|mut con| con.set::<_, _, ()>(&key, &value));

        match outcome {
            Ok(()) => {
                writeln!(log, "{key} {value}").expect("failed to append to the write log");
                log.flush().expect("failed to flush the write log");
                written += 1;
                // Rate-limit to ~10 writes/sec -- see the module doc comment: this keeps a
                // full-length chaos run's keyspace small enough for chaos.sh's exhaustive
                // per-key verification to finish in minutes instead of hours.
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            // A connection error (the target was just kill -9'd, or hasn't restarted yet) is
            // expected and swallowed -- the loop just tries again on the next key. A write
            // whose connection died before its reply arrived is NOT logged, so it is correctly
            // absent from the expected-state record even if it landed on the server before the
            // reply was lost -- this makes the log a conservative (never over-claiming) record.
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    println!("chaos_load: confirmed {written} writes over {duration_secs}s");
}
