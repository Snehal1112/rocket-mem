// crates/server/tests/startup_logging.rs
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// How long to wait for the child to emit `expected_listeners` `listener bound` lines before
/// giving up and asserting on whatever arrived. Generous on purpose: a loaded machine running the
/// whole workspace suite in parallel can take a while to get a freshly spawned binary through
/// recovery, and the cost of this ceiling is only paid when the expectation is already going to
/// fail.
const CAPTURE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// The event these tests wait on. `listener bound` is the *last* thing `main` logs before it
/// blocks forever in its accept loop, and the config summary is logged well before the first one,
/// so counting occurrences of this one message is enough to know the whole startup log has
/// arrived -- without any test needing to know how many lines that is in total. A fixed total-line
/// count would silently truncate the wait the moment any future `info` line is added earlier in
/// startup, turning an unrelated change into a confusing failure here.
const LISTENER_EVENT: &str = "listener bound";

/// Spawns the real compiled binary with every port bound to `127.0.0.1:0` (OS-assigned) and
/// returns its stderr -- where the `tracing` subscriber writes, per `main.rs`'s
/// `.with_writer(std::io::stderr)`. Mirrors `kill_and_recover.rs`'s `spawn_server`, but reads
/// the log stream instead of the plain `println!` startup banner on stdout.
///
/// Every address is `:0`, so this can never collide with a rocket-mem already running on this
/// machine. `RUST_LOG` is cleared from the inherited environment (a caller wanting one passes it
/// through `extra_env`) so a developer's shell setting cannot filter the very lines under test
/// out of the child's output.
///
/// With no `--config` in `extra_args` the binary stays on `config.rs`'s default *relative*
/// `rocket-mem.toml` path, which does not exist in this package's directory (cargo runs an
/// integration test with its cwd at the package root) -- so the repo-root `rocket-mem.toml`,
/// which is a live deployment's config, is never loaded. The `cluster_mode=false` assertion
/// below doubles as the canary for that: if that file ever did leak in, it fails loudly instead
/// of the test silently starting a second cluster node. A caller passing `--config` must point it
/// at a file it wrote itself, never at the repo-root one.
///
/// A reader thread, rather than reading the pipe inline, is what makes this terminate: the
/// server logs a fixed handful of lines and then blocks forever in its accept loop, so an
/// inline `read_line` past the last one would hang the test rather than fail it. The thread
/// drains until EOF while the caller waits for `expected_listeners` (or `CAPTURE_DEADLINE`), then
/// the child is killed and reaped here -- before any assertion runs -- so a panicking test
/// cannot leak a server process.
fn spawn_and_capture_stderr(
    dir: &std::path::Path,
    extra_env: &[(&str, &str)],
    extra_args: &[&str],
    expected_listeners: usize,
) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rocket-mem"));
    cmd.env_remove("RUST_LOG")
        .env("ROCKET_MEM_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_AOF_PATH", dir.join("startup-log-test.aof"))
        .env(
            "ROCKET_MEM_SNAPSHOT_PATH",
            dir.join("startup-log-test.snapshot"),
        )
        .args(extra_args)
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
        let seen = captured
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains(LISTENER_EVENT))
            .count();
        if seen >= expected_listeners {
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

    let stderr = spawn_and_capture_stderr(dir.path(), &[], &[], 3);

    assert!(
        stderr.contains("resolved config summary"),
        "expected a 'resolved config summary' info line, got:\n{stderr}"
    );
    // `cluster_mode` is also the live-config canary described on `spawn_and_capture_stderr`.
    assert!(stderr.contains("cluster_mode=false"), "got:\n{stderr}");
    assert!(stderr.contains("acl_enabled=false"), "got:\n{stderr}");
    assert!(stderr.contains("acl_user_count=0"), "got:\n{stderr}");
    assert!(stderr.contains("tls_enabled=false"), "got:\n{stderr}");
    assert!(
        stderr.contains("tls_replication_enabled=false"),
        "got:\n{stderr}"
    );
    // The two config fields that govern this series' own output. An operator staring at a
    // truncated trace log needs `log_value_max_bytes` in the log to explain it.
    assert!(stderr.contains("log_value_max_bytes=128"), "got:\n{stderr}");
    assert!(
        stderr.contains("slowlog_threshold_micros=10000"),
        "got:\n{stderr}"
    );
    assert!(
        !stderr.to_lowercase().contains("password"),
        "config summary must never log credential material, got:\n{stderr}"
    );
}

/// The summary must report the filter that is *in force*, not `config.log_level`. `RUST_LOG`
/// beats the config field (`config::resolve_log_filter_directive`), so logging the field would
/// have the summary claim `info` while the very same process emits `debug` lines -- exactly the
/// question an operator reads this event to answer.
#[test]
fn resolved_config_summary_reports_the_effective_log_filter_not_the_configured_one() {
    let dir = tempfile::tempdir().unwrap();

    let stderr = spawn_and_capture_stderr(dir.path(), &[("RUST_LOG", "debug")], &[], 3);

    assert!(
        stderr.contains("log_filter=debug"),
        "expected the RUST_LOG-resolved directive, not config.log_level's 'info', got:\n{stderr}"
    );
}

/// The regression guard for the summary's redaction rule. Every other test in this file runs a
/// config with no ACL users and no TLS material, so none of them can notice a change that renders
/// `config` (or `config.acl`) wholesale -- a single `full = ?config` field would put the ACL
/// username and the TLS cert/key paths on stderr at `info` and every other assertion here would
/// still pass.
///
/// Verified by mutation: adding `full = ?config` to the event makes this test fail on the
/// `zzuser` assertion, and only this test. The `zzsecret`, `zzkeypattern` and
/// `zzleaderpassword` assertions all survive that mutation, because `config::AclUserConfig` and
/// `config::Config` both have hand-written redacting `Debug` impls -- this test and those impls
/// are complementary, not redundant: the impls make the *credential* leak impossible from any
/// call site, while this test still guards the residue the summary must enumerate by hand (the
/// ACL usernames and the TLS paths, which those impls deliberately still render).
#[test]
fn secret_bearing_config_is_never_rendered_into_the_summary() {
    let dir = tempfile::tempdir().unwrap();
    // Real cert and key, not fake paths: the config below sets `tls_resp_addr`/`tls_rmp_addr`, so
    // the TLS listeners actually bind and the summary's `tls_enabled=true` branch is exercised
    // against a fully-configured deployment rather than a half-configured one. Their absolute
    // paths still name private key material on disk, so they are asserted absent below alongside
    // the ACL fields.
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let cert = fixtures.join("test-cert.pem");
    let key = fixtures.join("test-key.pem");

    // Written into the tempdir, never the repo-root `rocket-mem.toml` -- that file is a live
    // deployment's credential-bearing config and no test may load it.
    let config_path = dir.path().join("redaction-test.toml");
    // `replicaof_auth_password` without `replicaof`: `validate_replicaof` only requires the
    // username and password to be set together, and `start_replicating_from_config` is a no-op
    // with no target -- so the plaintext leader credential is genuinely present in the `Config`
    // this process logs a summary of, without the child spawning a doomed reconnect loop that
    // would fill this capture with unrelated warnings.
    let config_toml = format!(
        r#"
tls_resp_addr = "127.0.0.1:0"
tls_rmp_addr = "127.0.0.1:0"
tls_cert_path = "{}"
tls_key_path = "{}"
replicaof_auth_username = "zzleaderuser"
replicaof_auth_password = "zzleaderpassword"

[[acl.users]]
username = "zzuser"
password = "zzsecret"
enabled = true
rules = ["allcommands", "~zzkeypattern*"]
"#,
        cert.display(),
        key.display()
    );
    std::fs::write(&config_path, config_toml).unwrap();

    let stderr = spawn_and_capture_stderr(
        dir.path(),
        &[],
        &["--config", config_path.to_str().unwrap()],
        5,
    );

    // The `true` branches nothing else in this file reaches.
    assert!(stderr.contains("acl_enabled=true"), "got:\n{stderr}");
    assert!(stderr.contains("acl_user_count=1"), "got:\n{stderr}");
    assert!(stderr.contains("tls_enabled=true"), "got:\n{stderr}");

    for secret in [
        "zzuser",
        "zzsecret",
        "zzkeypattern",
        // The plaintext leader credential. `Config`'s hand-written `Debug` now redacts it, so
        // even a `full = ?config` mutation cannot put it here -- but the summary must not
        // enumerate it by hand either, and only this assertion says so.
        "zzleaderpassword",
        cert.to_str().unwrap(),
        key.to_str().unwrap(),
    ] {
        assert!(
            !stderr.contains(secret),
            "startup logs must never contain credential material, but found {secret:?} in:\n{stderr}"
        );
    }
}

#[test]
fn listener_bound_is_logged_for_the_always_on_listeners() {
    let dir = tempfile::tempdir().unwrap();

    let stderr = spawn_and_capture_stderr(dir.path(), &[], &[], 3);

    // `addr=` terminates each match so `protocol=RESP` cannot be satisfied by the
    // `protocol=RESP+TLS` line, which shares its prefix.
    for protocol in ["metrics", "RMP", "RESP"] {
        assert!(
            stderr.contains(&format!("protocol={protocol} addr=")),
            "expected a 'listener bound' line for protocol={protocol}, got:\n{stderr}"
        );
    }
    assert_eq!(
        stderr.matches(LISTENER_EVENT).count(),
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
        &[],
        5,
    );

    for protocol in ["metrics", "RMP", "RESP+TLS", "RMP+TLS", "RESP"] {
        assert!(
            stderr.contains(&format!("protocol={protocol} addr=")),
            "expected a 'listener bound' line for protocol={protocol}, got:\n{stderr}"
        );
    }
    assert_eq!(
        stderr.matches(LISTENER_EVENT).count(),
        5,
        "expected exactly 5 listener-bound lines with TLS configured, got:\n{stderr}"
    );
}

/// How long to wait for the child to exit on its own before declaring the test failed. Only paid
/// when the expectation is already going to fail -- a config the binary correctly rejects exits
/// in milliseconds.
const EXIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawns the real binary and waits for it to **exit**, returning whether it exited successfully
/// and everything it wrote to stderr. The counterpart to `spawn_and_capture_stderr` above, for
/// the configs that must abort startup rather than reach the accept loop.
///
/// `try_wait` in a bounded poll loop, never a bare `wait()` or `Command::output()`: the failure
/// mode this test exists to catch is a binary that does NOT reject the config, and such a binary
/// blocks forever in `rocket_mem::serve`. An unbounded wait would hang the suite instead of
/// failing the test. The child is killed and reaped before any assertion runs, so a panicking
/// test cannot leak a server process. Same `:0` addressing and same `RUST_LOG` scrub as
/// `spawn_and_capture_stderr`, and likewise no `--config`, so the repo-root `rocket-mem.toml` is
/// never loaded.
fn spawn_and_wait_for_exit(dir: &std::path::Path, extra_env: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rocket-mem"));
    cmd.env_remove("RUST_LOG")
        .env("ROCKET_MEM_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_AOF_PATH", dir.join("startup-exit-test.aof"))
        .env(
            "ROCKET_MEM_SNAPSHOT_PATH",
            dir.join("startup-exit-test.snapshot"),
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

    let deadline = std::time::Instant::now() + EXIT_DEADLINE;
    let status = loop {
        match child.try_wait().expect("failed to poll the child") {
            Some(status) => break Some(status),
            None if std::time::Instant::now() >= deadline => break None,
            None => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    };

    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    let output = captured.lock().unwrap().concat();

    let Some(status) = status else {
        panic!(
            "the binary was still running after {EXIT_DEADLINE:?}; it must have rejected the \
             config and exited. stderr so far:\n{output}"
        );
    };
    (status.success(), output)
}

/// `validate_replica_announce_addr` is only useful if `main.rs` actually calls it, and calls it
/// early. This asserts both: a non-zero exit, and no `listener bound` line -- the validator sits
/// with `validate_replicaof`/`validate_tls`, above every `TcpListener::bind` in `main`, so a
/// rejected config must never have opened a port.
#[test]
fn a_malformed_replica_announce_addr_aborts_startup_before_any_listener_binds() {
    let dir = tempfile::tempdir().unwrap();

    let (success, stderr) = spawn_and_wait_for_exit(
        dir.path(),
        &[("ROCKET_MEM_REPLICA_ANNOUNCE_ADDR", "numericlabs.lxd")],
    );

    assert!(
        !success,
        "a replica_announce_addr with no port must fail startup, got a clean exit and:\n{stderr}"
    );
    assert!(
        stderr.contains("replica_announce_addr"),
        "the error must name the field so an operator has something to grep for, got:\n{stderr}"
    );
    assert!(
        stderr.contains("numericlabs.lxd"),
        "the error must echo the offending value, got:\n{stderr}"
    );
    assert!(
        !stderr.contains(LISTENER_EVENT),
        "validation must run before anything binds, got:\n{stderr}"
    );
}

/// The other half: a well-shaped value must not be rejected. Without this, deleting the
/// `Ok(())` arm and rejecting everything would still pass the test above.
#[test]
fn a_well_shaped_replica_announce_addr_starts_normally() {
    let dir = tempfile::tempdir().unwrap();

    let stderr = spawn_and_capture_stderr(
        dir.path(),
        &[("ROCKET_MEM_REPLICA_ANNOUNCE_ADDR", "numericlabs.lxd:16479")],
        &[],
        3,
    );

    assert_eq!(
        stderr.matches(LISTENER_EVENT).count(),
        3,
        "a valid announce address must not stop the three always-on listeners, got:\n{stderr}"
    );
}

/// Same reasoning as the announce-address pair above: `validate_min_replicas` is only worth having
/// if `main.rs` actually calls it, and calls it before anything binds. A nonzero
/// `min_replicas_to_write` with a zero lag window can never be satisfied by any replica, so it
/// would refuse every client write forever -- a permanent outage that must abort startup rather
/// than come up looking healthy and fail the first write.
#[test]
fn fencing_with_a_zero_lag_window_aborts_startup_before_any_listener_binds() {
    let dir = tempfile::tempdir().unwrap();

    let (success, stderr) = spawn_and_wait_for_exit(
        dir.path(),
        &[
            ("ROCKET_MEM_MIN_REPLICAS_TO_WRITE", "1"),
            ("ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS", "0"),
        ],
    );

    assert!(
        !success,
        "fencing with a zero lag window must fail startup, got a clean exit and:\n{stderr}"
    );
    assert!(
        stderr.contains("min_replicas_to_write") && stderr.contains("min_replicas_max_lag_secs"),
        "the error must name both fields so an operator knows which pairing is wrong, got:\n{stderr}"
    );
    assert!(
        !stderr.contains(LISTENER_EVENT),
        "validation must run before anything binds, got:\n{stderr}"
    );
}

/// The other half, for the same reason the announce-address pair needs one: without this, making
/// `validate_min_replicas` reject every config would still pass the test above. Fencing switched
/// on with a sane lag window is a supported deployment and must start normally.
#[test]
fn fencing_enabled_with_a_nonzero_lag_window_starts_normally() {
    let dir = tempfile::tempdir().unwrap();

    let stderr = spawn_and_capture_stderr(
        dir.path(),
        &[
            ("ROCKET_MEM_MIN_REPLICAS_TO_WRITE", "1"),
            ("ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS", "10"),
        ],
        &[],
        3,
    );

    assert_eq!(
        stderr.matches(LISTENER_EVENT).count(),
        3,
        "enabling fencing must not stop the three always-on listeners, got:\n{stderr}"
    );
}

/// `validate_cluster_health` is only worth having if `main.rs` actually calls it, and calls it
/// before `spawn_peer_prober` would build a `tokio::time::interval` (which panics outright on a
/// zero period) or anything binds. Checked unconditionally, not only in cluster mode -- see the
/// validator's own doc comment -- so a standalone config with no cluster fields set is enough to
/// exercise it here.
#[test]
fn a_zero_cluster_probe_interval_aborts_startup_before_any_listener_binds() {
    let dir = tempfile::tempdir().unwrap();

    let (success, stderr) = spawn_and_wait_for_exit(
        dir.path(),
        &[("ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS", "0")],
    );

    assert!(
        !success,
        "a zero probe interval must fail startup, got a clean exit and:\n{stderr}"
    );
    assert!(
        stderr.contains("cluster_probe_interval_secs"),
        "the error must name the field, got:\n{stderr}"
    );
    assert!(
        !stderr.contains(LISTENER_EVENT),
        "validation must run before anything binds, got:\n{stderr}"
    );
}

/// The other half: a sane, nonzero pair of cluster-health timers must not stop startup, even
/// outside cluster mode -- these fields are validated unconditionally but only ever read by a
/// prober that a standalone node never spawns.
#[test]
fn nonzero_cluster_health_timers_start_normally_even_outside_cluster_mode() {
    let dir = tempfile::tempdir().unwrap();

    let stderr = spawn_and_capture_stderr(
        dir.path(),
        &[
            ("ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS", "2"),
            ("ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS", "20"),
        ],
        &[],
        3,
    );

    assert_eq!(
        stderr.matches(LISTENER_EVENT).count(),
        3,
        "sane cluster-health timers must not stop the three always-on listeners, got:\n{stderr}"
    );
}

/// A TLS follower that never says where a peer should reach it announces its *plaintext* address,
/// which is the misconfiguration the announce-address spec exists to surface. It must be visible
/// at startup, at `warn`, exactly once -- not discovered later by reading a leader's
/// `INFO REPLICATION` and noticing the port is the wrong one.
#[test]
fn a_tls_follower_with_no_announce_addr_is_warned_about_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let cert = fixtures.join("test-cert.pem");
    let key = fixtures.join("test-key.pem");

    // `replicaof` points at a port nothing listens on: the warn is a pure function of the config
    // and fires before any connection is attempted, so a doomed reconnect loop is harmless here.
    // Its own retry warnings carry a different message and cannot satisfy the assertions below.
    let stderr = spawn_and_capture_stderr(
        dir.path(),
        &[
            ("ROCKET_MEM_REPLICAOF", "127.0.0.1:1"),
            ("ROCKET_MEM_TLS_RESP_ADDR", "127.0.0.1:0"),
            ("ROCKET_MEM_TLS_CERT_PATH", cert.to_str().unwrap()),
            ("ROCKET_MEM_TLS_KEY_PATH", key.to_str().unwrap()),
        ],
        &[],
        4, // metrics, RMP, RESP+TLS, RESP
    );

    let line = stderr
        .lines()
        .find(|l| l.contains("replica_announce_addr is unset"))
        .unwrap_or_else(|| {
            panic!("expected a plaintext-announce warning at startup, got:\n{stderr}")
        });

    assert!(
        line.contains("WARN"),
        "this is an operator-actionable misconfiguration, so it must be warn, not info: {line}"
    );
    // `spawn_and_capture_stderr` sets ROCKET_MEM_ADDR=127.0.0.1:0, and the warning reports the
    // configured `addr` -- the value that would be announced -- not the OS-assigned bound port.
    // Unquoted, because the field is rendered with `%` (Display); a `?` sigil would produce
    // `announced="127.0.0.1:0"` and this assertion would fail.
    assert!(
        line.contains("announced=127.0.0.1:0"),
        "the warning must name the plaintext address being announced, unquoted: {line}"
    );
    assert_eq!(
        stderr.matches("replica_announce_addr is unset").count(),
        1,
        "one line at startup, never a repeat, got:\n{stderr}"
    );
}

/// The fix must actually silence the warning -- otherwise an operator who sets the field keeps
/// seeing it and learns to ignore it. Identical config to the test above except for the one
/// field, so a pass here can only mean that field is what turned it off.
#[test]
fn setting_replica_announce_addr_silences_the_startup_warning() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let cert = fixtures.join("test-cert.pem");
    let key = fixtures.join("test-key.pem");

    let stderr = spawn_and_capture_stderr(
        dir.path(),
        &[
            ("ROCKET_MEM_REPLICAOF", "127.0.0.1:1"),
            ("ROCKET_MEM_TLS_RESP_ADDR", "127.0.0.1:0"),
            ("ROCKET_MEM_TLS_CERT_PATH", cert.to_str().unwrap()),
            ("ROCKET_MEM_TLS_KEY_PATH", key.to_str().unwrap()),
            ("ROCKET_MEM_REPLICA_ANNOUNCE_ADDR", "numericlabs.lxd:16479"),
        ],
        &[],
        4,
    );

    assert!(
        !stderr.contains("replica_announce_addr is unset"),
        "a node that announces an explicit address must not be warned, got:\n{stderr}"
    );
    // The canary for the assertion above: it must be looking at a real startup log, not at an
    // empty capture that would make any absence-assertion pass vacuously.
    assert_eq!(
        stderr.matches(LISTENER_EVENT).count(),
        4,
        "the capture must contain a real startup log, got:\n{stderr}"
    );
}
