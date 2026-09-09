// crates/server/tests/startup_logging.rs
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// How long to wait for the child to emit `expected_lines` before giving up and asserting on
/// whatever arrived. Generous on purpose: a loaded machine running the whole workspace suite in
/// parallel can take a while to get a freshly spawned binary through recovery, and the cost of
/// this ceiling is only paid when the expectation is already going to fail.
const CAPTURE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawns the real compiled binary with every port bound to `127.0.0.1:0` (OS-assigned) and
/// returns its stderr -- where the `tracing` subscriber writes, per `main.rs`'s
/// `.with_writer(std::io::stderr)`. Mirrors `kill_and_recover.rs`'s `spawn_server`, but reads
/// the log stream instead of the plain `println!` startup banner on stdout.
///
/// Every address is `:0` and no `--config` is passed, so this can never collide with a
/// rocket-mem already running on this machine. That also keeps the binary on `config.rs`'s
/// default *relative* `rocket-mem.toml` path, which does not exist in this package's directory
/// (cargo runs an integration test with its cwd at the package root) -- so the repo-root
/// `rocket-mem.toml`, which is a live deployment's config, is never loaded. The
/// `cluster_mode=false`/`acl_enabled=false` assertions below double as the canary for that: if
/// that file ever did leak in, they fail loudly instead of the test silently starting a second
/// cluster node.
///
/// A reader thread, rather than reading the pipe inline, is what makes this terminate: the
/// server logs a fixed handful of lines and then blocks forever in its accept loop, so an
/// inline `read_line` past the last one would hang the test rather than fail it. The thread
/// drains until EOF while the caller waits for `expected_lines` (or `CAPTURE_DEADLINE`), then
/// the child is killed and reaped here -- before any assertion runs -- so a panicking test
/// cannot leak a server process.
fn spawn_and_capture_stderr(
    dir: &std::path::Path,
    extra_env: &[(&str, &str)],
    expected_lines: usize,
) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rocket-mem"));
    cmd.env("ROCKET_MEM_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_AOF_PATH", dir.join("startup-log-test.aof"))
        .env(
            "ROCKET_MEM_SNAPSHOT_PATH",
            dir.join("startup-log-test.snapshot"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("failed to spawn the rocket-mem binary");

    let stderr = child.stderr.take().expect("child stderr was not piped");
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&captured);
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break, // EOF, or the pipe died with the child
                Ok(_) => sink.lock().unwrap().push(line),
            }
        }
    });

    let deadline = std::time::Instant::now() + CAPTURE_DEADLINE;
    while std::time::Instant::now() < deadline {
        if captured.lock().unwrap().len() >= expected_lines {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    let lines = captured.lock().unwrap();
    lines.concat()
}

#[test]
fn resolved_config_summary_is_logged_at_startup() {
    let dir = tempfile::tempdir().unwrap();

    // "rocket-mem starting", the config summary, and the AOF recovery summary.
    let stderr = spawn_and_capture_stderr(dir.path(), &[], 3);

    assert!(
        stderr.contains("resolved config summary"),
        "expected a 'resolved config summary' info line, got:\n{stderr}"
    );
    // These two are also the live-config canary described on `spawn_and_capture_stderr`.
    assert!(stderr.contains("cluster_mode=false"), "got:\n{stderr}");
    assert!(stderr.contains("acl_enabled=false"), "got:\n{stderr}");
    assert!(stderr.contains("tls_enabled=false"), "got:\n{stderr}");
    assert!(
        !stderr.to_lowercase().contains("password"),
        "config summary must never log credential material, got:\n{stderr}"
    );
}

#[test]
fn listener_bound_is_logged_for_the_always_on_listeners() {
    let dir = tempfile::tempdir().unwrap();

    // The three lines above plus one `listener bound` per always-on listener.
    let stderr = spawn_and_capture_stderr(dir.path(), &[], 6);

    // `addr=` terminates each match so `protocol=RESP` cannot be satisfied by the
    // `protocol=RESP+TLS` line, which shares its prefix.
    for protocol in ["metrics", "RMP", "RESP"] {
        assert!(
            stderr.contains(&format!("protocol={protocol} addr=")),
            "expected a 'listener bound' line for protocol={protocol}, got:\n{stderr}"
        );
    }
    assert_eq!(
        stderr.matches("listener bound").count(),
        3,
        "expected exactly 3 listener-bound lines with no TLS configured, got:\n{stderr}"
    );
}

#[test]
fn listener_bound_is_logged_for_tls_listeners_when_configured() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let cert = fixtures.join("test-cert.pem");
    let key = fixtures.join("test-key.pem");

    let stderr = spawn_and_capture_stderr(
        dir.path(),
        &[
            ("ROCKET_MEM_TLS_RESP_ADDR", "127.0.0.1:0"),
            ("ROCKET_MEM_TLS_RMP_ADDR", "127.0.0.1:0"),
            ("ROCKET_MEM_TLS_CERT_PATH", cert.to_str().unwrap()),
            ("ROCKET_MEM_TLS_KEY_PATH", key.to_str().unwrap()),
        ],
        8,
    );

    for protocol in ["metrics", "RMP", "RESP+TLS", "RMP+TLS", "RESP"] {
        assert!(
            stderr.contains(&format!("protocol={protocol} addr=")),
            "expected a 'listener bound' line for protocol={protocol}, got:\n{stderr}"
        );
    }
    assert_eq!(
        stderr.matches("listener bound").count(),
        5,
        "expected exactly 5 listener-bound lines with TLS configured, got:\n{stderr}"
    );
}
