//! Level separation for the per-command dispatch logging.
//!
//! The spec's worst failure mode: a one-character change from `debug_span!` to `info_span!`, or
//! `debug!` to `info!`, in `dispatcher.rs` turns every production node into a per-request log
//! emitter -- and at `trace`, into one writing plaintext user data to disk. Review does not
//! reliably catch it and no other test does, so this file asserts on the bytes a subscriber
//! actually produces at each level.
//!
//! See ../../../docs/superpowers/specs/2026-09-09-verbose-logging-design.md.

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use protocol::Frame;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::EnvFilter;

/// A `MakeWriter` that appends everything the subscriber writes into a shared buffer, so a test
/// can assert on the exact bytes an operator would see on stderr.
///
/// `Arc<Mutex<Vec<u8>>>` rather than a plain `Vec`: `MakeWriter::make_writer` hands out a fresh
/// writer per event and takes `&self`, so the buffer has to be shared and interior-mutable. It
/// also has to be `Send + Sync + 'static` for `Dispatch` to accept the subscriber.
#[derive(Clone)]
struct BufferWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for BufferWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for BufferWriter {
    type Writer = BufferWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Dispatches one `SET level-key level-value` with a subscriber filtered to `level`, and returns
/// everything that subscriber wrote.
///
/// Every dispatch happens inside the `with_default` closure, on the calling thread, and that is
/// mandatory rather than stylistic: `tracing::subscriber::with_default` installs the subscriber
/// for the *current thread only*, and `cargo test` runs tests on parallel threads. Work moved
/// off this thread -- a `tokio::spawn`, a `std::thread::spawn`, or an `.await` on a multi-thread
/// runtime -- would log to the global (empty) subscriber and this function would return "".
/// These are plain `#[test]` functions for exactly that reason; `dispatch_and_log` is
/// synchronous and needs no runtime. The flip side is the useful one: per-thread scoping means
/// these tests need no serialization and cannot capture each other's output.
fn capture_at(level: &str) -> String {
    capture_frames_at(level, vec![set_frame()])
}

/// `capture_at` with the frames spelled out, for the tests that need a command other than `SET`.
fn capture_frames_at(level: &str, frames: Vec<Frame>) -> String {
    capture_frames_with_at(
        level,
        rocket_mem::replication::ReplicationHandle::default(),
        frames,
    )
}

/// `capture_frames_at` with the `ReplicationHandle` supplied, for the tests that need one built
/// differently -- currently only the slow-log threshold, which `ReplicationHandle::default()`
/// leaves at the 10ms production default no test command would ever cross.
fn capture_frames_with_at(
    level: &str,
    replication: rocket_mem::replication::ReplicationHandle,
    frames: Vec<Frame>,
) -> String {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_env_filter(EnvFilter::new(level))
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine::Engine::new();
        let aof = rocket_mem::aof::AofWriter::open(
            &dir.path().join("logging-test.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .expect("open aof");
        let session = rocket_mem::dispatcher::Session::new();

        for frame in frames {
            rocket_mem::dispatcher::dispatch_and_log(
                &engine,
                &aof,
                &replication,
                frame,
                &session,
                1,
            );
        }
    });

    let bytes = buffer.lock().unwrap_or_else(|e| e.into_inner()).clone();
    String::from_utf8(bytes).expect("subscriber output is utf-8")
}

/// `SET level-key level-value`. A write command on purpose: it is the shape that exercises the
/// most of `dispatch_and_log_inner` (AOF append, replica fan-out) while still returning a
/// plain `+OK`.
fn set_frame() -> Frame {
    Frame::Array(vec![
        Frame::Bulk(Bytes::from_static(b"SET")),
        Frame::Bulk(Bytes::from_static(b"level-key")),
        Frame::Bulk(Bytes::from_static(b"level-value")),
    ])
}

/// Runs `f` under a subscriber filtered to `level` and returns its result together with
/// everything that subscriber wrote. The recovery tests at the bottom of this file drive
/// `aof::recover` rather than the dispatcher, so `capture_frames_at` above does not fit them.
fn capture_during<T>(level: &str, f: impl FnOnce() -> T) -> (T, String) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_env_filter(EnvFilter::new(level))
        .finish();

    let value = tracing::subscriber::with_default(subscriber, f);

    let bytes = buffer.lock().unwrap_or_else(|e| e.into_inner()).clone();
    (
        value,
        String::from_utf8(bytes).expect("subscriber output is utf-8"),
    )
}

/// A command frame from its parts, so a test listing several commands stays readable.
fn cmd(parts: &[&[u8]]) -> Frame {
    Frame::Array(
        parts
            .iter()
            .map(|p| Frame::Bulk(Bytes::copy_from_slice(p)))
            .collect(),
    )
}

#[test]
fn info_emits_no_per_command_lines() {
    let output = capture_at("info");
    assert!(
        !output.contains("command dispatched"),
        "the per-command debug line escaped to the production default level; output was:\n{output}"
    );
    assert!(
        !output.contains("elapsed_us"),
        "the per-command debug line's fields escaped to `info`; output was:\n{output}"
    );
    assert!(
        !output.contains("level-key"),
        "a key reached the log at `info`; output was:\n{output}"
    );
}

#[test]
fn debug_emits_the_per_command_line() {
    let output = capture_at("debug");
    assert!(
        output.contains("command dispatched"),
        "the per-command debug line is missing at `debug`; output was:\n{output}"
    );
    assert!(
        output.contains("elapsed_us"),
        "the per-command line lost its elapsed_us field; output was:\n{output}"
    );
    assert!(
        // `reply_kind` returns a `&str`, recorded with no `%`/`?` sigil, so
        // `tracing-subscriber`'s default formatter debug-quotes it -- unlike `elapsed_us`
        // (a number, recorded via `record_u64`/`record_i64`, which is never quoted) or the
        // `cmd`/`key` span fields (recorded with `%`, i.e. Display, which is also unquoted).
        output.contains("reply=\"ok\""),
        "the per-command line lost its reply field; output was:\n{output}"
    );
}

#[test]
fn debug_logs_the_key_but_not_the_value() {
    // The level taxonomy in one assertion: `debug` is "what happened" (the key), `trace` is
    // "what the bytes were" (the value). A value leaking into `debug` would make the level an
    // operator is told is safe to leave on in production a data-exposure decision instead.
    let output = capture_at("debug");
    assert!(
        output.contains("level-key"),
        "the key is missing from the debug line; output was:\n{output}"
    );
    assert!(
        !output.contains("level-value"),
        "a stored value reached the log at `debug`; output was:\n{output}"
    );
    assert!(
        !output.contains("command arguments"),
        "the trace argument line escaped to `debug`; output was:\n{output}"
    );
}

#[test]
fn trace_renders_the_full_argument_list() {
    let output = capture_at("trace");
    assert!(
        output.contains("command arguments"),
        "the trace argument line is missing at `trace`; output was:\n{output}"
    );
    assert!(
        output.contains("args=level-key level-value"),
        "the argument list did not render as text; output was:\n{output}"
    );
}

#[test]
fn trace_never_renders_a_credential() {
    let output = capture_frames_at(
        "trace",
        vec![
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"AUTH")),
                Frame::Bulk(Bytes::from_static(b"alice")),
                Frame::Bulk(Bytes::from_static(b"hunter2")),
            ]),
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"HELLO")),
                Frame::Bulk(Bytes::from_static(b"3")),
                Frame::Bulk(Bytes::from_static(b"AUTH")),
                Frame::Bulk(Bytes::from_static(b"alice")),
                Frame::Bulk(Bytes::from_static(b"hunter2")),
            ]),
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"ACL")),
                Frame::Bulk(Bytes::from_static(b"SETUSER")),
                Frame::Bulk(Bytes::from_static(b"alice")),
                Frame::Bulk(Bytes::from_static(b">hunter2")),
            ]),
        ],
    );
    assert!(
        output.contains("<redacted>"),
        "a credential-carrying command was not redacted at all; output was:\n{output}"
    );
    // The whole point, asserted at the highest-volume level, on the real dispatch path.
    assert!(
        !output.contains("hunter2"),
        "a password reached the log at `trace`; output was:\n{output}"
    );
}

#[test]
fn the_span_renders_the_key_as_text_not_as_debug_bytes() {
    let output = capture_at("debug");
    assert!(
        output.contains("key=level-key"),
        "the cmd span's key field is missing or not rendered as text; output was:\n{output}"
    );
    // The two shapes the mistake produces. `key = ?first_key` on an `Option<Bytes>` renders as
    // `Some(b"level-key")`; `key = ?key` on a bare `Bytes` renders as `b"level-key"`. Both are
    // ungreppable, and both cost O(len) of formatting on the hottest path in the project.
    assert!(
        !output.contains("Some(b\""),
        "the key was logged through Debug; output was:\n{output}"
    );
    assert!(
        !output.contains("key=b\""),
        "the key was logged through Debug; output was:\n{output}"
    );
}

#[test]
fn the_span_carries_the_command_name_and_arity() {
    // The field vocabulary the spec fixes (`cmd`, `key`, `argc`) is what makes one grep follow
    // an activity end to end -- a renamed field breaks every runbook written against it.
    let output = capture_at("debug");
    assert!(
        output.contains("cmd=SET"),
        "the cmd span lost its command name; output was:\n{output}"
    );
    assert!(
        output.contains("argc=2"),
        "the cmd span lost its arity; output was:\n{output}"
    );
}

/// The `key` field names the command's *key*, picked by the same `key_spec` table the cluster
/// router routes on -- not "whatever argument came first". Two defects motivated this and the
/// assertions below cover both: `MEMORY USAGE <key>` / `OBJECT ENCODING <key>` used to report
/// the subcommand (`USAGE`/`ENCODING`) as the key, and every keyless command reported its first
/// argument, which for `ECHO <payload>` / `PING <message>` is a client-supplied *value* with no
/// length cap -- exactly what the spec says never reaches a log line.
#[test]
fn the_span_key_is_key_spec_aware_and_never_renders_a_value() {
    let output = capture_frames_at(
        "debug",
        vec![
            cmd(&[b"MEMORY", b"USAGE", b"memory-key"]),
            cmd(&[b"OBJECT", b"ENCODING", b"object-key"]),
            cmd(&[b"ECHO", b"echo-payload"]),
            cmd(&[b"PING", b"ping-payload"]),
            cmd(&[b"GET", b"plain-key"]),
            cmd(&[b"AUTH", b"auth-secret"]),
        ],
    );
    assert!(
        output.contains("key=memory-key"),
        "MEMORY USAGE must log the key, not the USAGE subcommand; output was:\n{output}"
    );
    assert!(
        output.contains("key=object-key"),
        "OBJECT ENCODING must log the key, not the ENCODING subcommand; output was:\n{output}"
    );
    assert!(
        output.contains("cmd=ECHO key= argc=1"),
        "a keyless command must log an empty key; output was:\n{output}"
    );
    assert!(
        !output.contains("echo-payload"),
        "ECHO's client-supplied value reached the log; output was:\n{output}"
    );
    assert!(
        !output.contains("ping-payload"),
        "PING's client-supplied value reached the log; output was:\n{output}"
    );
    assert!(
        output.contains("key=plain-key"),
        "an ordinary first-argument key must still be logged; output was:\n{output}"
    );
    assert!(
        !output.contains("auth-secret"),
        "AUTH's password reached the log; output was:\n{output}"
    );
}

/// The same key-spec-aware rendering on the slow log's `warn!` -- which, unlike the `cmd` span,
/// is emitted at this project's production default level, so a wrong or value-carrying `key`
/// there reaches a real operator's log file rather than only an opt-in `debug` one.
///
/// A 1ns threshold makes every command "slow", which is the only way to reach the warn branch
/// deterministically; `Duration::ZERO` would disable the slow log entirely.
#[test]
fn the_slowlog_warning_key_is_key_spec_aware_and_never_renders_a_value() {
    let output = capture_frames_with_at(
        "warn",
        rocket_mem::replication::ReplicationHandle::default()
            .with_slowlog_threshold(Duration::from_nanos(1)),
        vec![
            cmd(&[b"MEMORY", b"USAGE", b"memory-key"]),
            cmd(&[b"ECHO", b"echo-payload"]),
        ],
    );
    assert!(
        output.contains("slow command recorded"),
        "expected the slow-log warning at `warn`; output was:\n{output}"
    );
    assert!(
        output.contains("key=memory-key"),
        "the slow-log warning must name the key, not the USAGE subcommand; output was:\n{output}"
    );
    assert!(
        !output.contains("echo-payload"),
        "ECHO's client-supplied value reached a `warn`-level log; output was:\n{output}"
    );
}

// The two tests below were moved here from `crates/server/src/{replication,connection}.rs`,
// where they flaked under parallel `cargo test --workspace` (roughly one run in ten). Root
// cause: `tracing` caches callsite `Interest` per callsite, process-globally, the first time
// that callsite is reached. Whichever test in a binary happens to touch a given `trace!`/
// `debug!`/`info!` call site first -- often with no subscriber installed at all, or one that
// doesn't want that level -- permanently decides whether that callsite is live for every other
// test in the *same process*, including one installing a subscriber that very much wants it.
// The unit-test binary those two tests used to live in runs ~575 tests whose subscribers
// install and drop constantly, so the odds of an unrelated test poisoning the callsite first
// were high enough to flake regularly. This file compiles to its own, separate binary with far
// fewer tests and callsites; every capture test in it has run green across the life of this
// project. Both scenarios below are driven through `rocket_mem`'s public API (`ReplicationHandle`
// and `serve`) rather than the crate-private functions the original tests called directly, and
// neither dropped any behavioural coverage: both originals asserted on captured log text only,
// and the equivalent behavioural checks already exist elsewhere (see each test's comment).

/// Moved from `crates/server/src/replication.rs`'s
/// `sync_once_logs_stream_offset_and_the_applied_command_name`. That test asserted only on
/// captured log text -- no engine-state assertions -- so nothing behavioural was left behind;
/// `sync_once_loads_the_snapshot_then_applies_streamed_frames` (still in `replication.rs`)
/// covers the snapshot-load/frame-apply behaviour. Drives `sync_once` indirectly through the
/// public `ReplicationHandle::start_replicating`, since `sync_once` itself is crate-private.
#[tokio::test]
async fn sync_once_logs_stream_offset_and_the_applied_command_name() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let fake_leader = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut psync_bytes = [0u8; 15];
        socket.read_exact(&mut psync_bytes).await.unwrap();

        let snapshot_engine = engine::Engine::new();
        let blob = snapshot_engine.snapshot(0);
        socket
            .write_all(&(blob.len() as u64).to_le_bytes())
            .await
            .unwrap();
        socket.write_all(&blob).await.unwrap();

        socket
            .write_all(b"*3\r\n$3\r\nSET\r\n$11\r\nfrom-stream\r\n$1\r\nv\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_env_filter(EnvFilter::new("trace"))
        .finish();

    let engine = Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&engine),
        dir.path().join("sync-once-logging-test.snapshot"),
    ));

    let _guard = tracing::subscriber::set_default(subscriber);
    replication.start_replicating(addr.to_string());
    tokio::time::sleep(Duration::from_millis(150)).await;
    replication.stop_replicating();
    drop(_guard);
    fake_leader.abort();

    let bytes_out = buffer.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let text = String::from_utf8(bytes_out).expect("subscriber output is utf-8");
    assert!(
        text.contains("replication stream advanced") && text.contains("offset"),
        "expected an offset-progress trace line:\n{text}"
    );
    assert!(
        text.contains("applied replicated command") && text.contains("SET"),
        "expected a per-command apply debug line naming SET:\n{text}"
    );
}

/// Cluster mode's startup event: `ClusterConfig::load` logs this node's own id, slot range, and
/// the topology's node count once, on the success path, at `info`.
///
/// Lives here rather than as a `#[cfg(test)]` capture assertion inside `cluster.rs` itself, per
/// this project's established rule (see the comment block above): `tracing` caches callsite
/// `Interest` per callsite, process-globally, and the `rocket-mem` unit-test binary runs ~575
/// tests whose subscribers install and drop constantly, so a capture assertion living there would
/// flake under `cargo test --workspace`. `ClusterConfig::load` is `pub fn` on a `pub mod`, so it
/// is reachable here through the same public-API route the two tests above already use.
#[test]
fn cluster_config_load_logs_the_topology_at_info() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cluster.conf");
    std::fs::write(
        &path,
        "shard-a 127.0.0.1:7001 0 5460\n\
         shard-b 127.0.0.1:7002 5461 10922\n\
         shard-c 127.0.0.1:7003 10923 16383\n",
    )
    .expect("write cluster config");

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_env_filter(EnvFilter::new("info"))
        .finish();

    let config = tracing::subscriber::with_default(subscriber, || {
        rocket_mem::cluster::ClusterConfig::load(&path, "shard-b").expect("load cluster config")
    });

    let bytes_out = buffer.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let text = String::from_utf8(bytes_out).expect("subscriber output is utf-8");
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

/// A three-shard topology whose ranges are the even thirds of the slot space, matching the
/// fixture `crates/server/src/dispatcher.rs`'s own `cluster_handle` test helper uses -- same
/// slot math, so the same reference keys land on the same nodes.
const THREE_SHARDS: &str = "shard-a 127.0.0.1:7001 0 5460\n\
     shard-b 127.0.0.1:7002 5461 10922\n\
     shard-c 127.0.0.1:7003 10923 16383\n";

/// Dispatches one frame against a cluster-mode node (`node_id`'s slot range per `THREE_SHARDS`)
/// under a subscriber filtered to `level`, and returns the reply plus everything that subscriber
/// wrote. `dispatch_and_log`'s cluster-redirect gate is crate-private to invoke directly, so this
/// drives it the only way an external caller can: through the same public `dispatch_and_log` +
/// `ReplicationHandle::with_cluster` route `main.rs` itself uses to put a node into cluster mode.
fn capture_cluster_dispatch_at(level: &str, node_id: &str, frame: Frame) -> (Frame, String) {
    let config = rocket_mem::cluster::ClusterConfig::parse(THREE_SHARDS, node_id)
        .expect("parse cluster config");
    let replication = rocket_mem::replication::ReplicationHandle::default()
        .with_cluster(std::sync::Arc::new(config));

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_env_filter(EnvFilter::new(level))
        .finish();

    let reply = tracing::subscriber::with_default(subscriber, || {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine::Engine::new();
        let aof = rocket_mem::aof::AofWriter::open(
            &dir.path().join("cluster-logging-test.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .expect("open aof");
        let session = rocket_mem::dispatcher::Session::new();
        rocket_mem::dispatcher::dispatch_and_log(&engine, &aof, &replication, frame, &session, 1)
    });

    let bytes_out = buffer.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let text = String::from_utf8(bytes_out).expect("subscriber output is utf-8");
    (reply, text)
}

/// Cluster mode's MOVED-redirect event: `cluster_redirect` logs the key, slot, and target node
/// at `debug`, on the redirect path only. Lives here rather than as a `#[cfg(test)]` capture
/// assertion in `dispatcher.rs` itself, for the same process-global-callsite-caching reason as
/// `cluster_config_load_logs_the_topology_at_info` above.
#[test]
fn a_moved_redirect_logs_the_key_slot_and_target_node_at_debug() {
    // "foo" hashes to slot 12182, which shard-c owns -- same fixture as
    // `dispatcher.rs`'s own `a_key_this_node_does_not_own_is_redirected_with_moved`.
    let (reply, text) = capture_cluster_dispatch_at(
        "debug",
        "shard-a",
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"GET")),
            Frame::Bulk(Bytes::from_static(b"foo")),
        ]),
    );
    assert_eq!(reply, Frame::Error("MOVED 12182 127.0.0.1:7003".into()));
    assert!(
        text.contains("cluster redirect"),
        "expected the redirect event's message:\n{text}"
    );
    assert!(
        text.contains("foo"),
        "expected the key in the MOVED debug log:\n{text}"
    );
    assert!(
        text.contains("12182"),
        "expected the slot in the MOVED debug log:\n{text}"
    );
    assert!(
        text.contains("127.0.0.1:7003"),
        "expected the target node in the MOVED debug log:\n{text}"
    );
}

/// The hot-path guardrail: a key this node owns must never reach the redirect log call, even at
/// `trace` -- the fast path every correctly-routed command in cluster mode takes.
#[test]
fn a_key_this_node_owns_produces_no_cluster_redirect_log() {
    // "hello" hashes to slot 866, which shard-a (this node) owns.
    let (reply, text) = capture_cluster_dispatch_at(
        "trace",
        "shard-a",
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Bulk(Bytes::from_static(b"hello")),
            Frame::Bulk(Bytes::from_static(b"1")),
        ]),
    );
    assert_eq!(reply, Frame::Simple("OK".into()));
    assert!(
        !text.contains("cluster redirect"),
        "the owned-key fast path must not log a cluster redirect, got:\n{text}"
    );
}

/// Moved from `crates/server/src/connection.rs`'s
/// `a_replica_registering_and_being_pruned_are_both_logged_at_info`. That test asserted only on
/// captured log text -- no behavioural assertions -- so nothing behavioural was left behind;
/// `psync_with_an_advertised_address_registers_it_on_the_leader` and
/// `a_registered_replica_is_pruned_after_its_connection_drops` (still in `connection.rs`) cover
/// the registration and pruning behaviour respectively.
#[tokio::test]
async fn a_replica_registering_and_being_pruned_are_both_logged_at_info() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("replica-register-prune.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .expect("open aof"),
    );
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&engine),
        dir.path().join("replica-register-prune.snapshot"),
    ));
    tokio::spawn(rocket_mem::serve(
        listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_env_filter(EnvFilter::new("info"))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut framed = Framed::new(
        TcpStream::connect(addr).await.unwrap(),
        protocol::codec::RespCodec::default(),
    );
    framed
        .send(Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"PSYNC")),
            Frame::Bulk(Bytes::from_static(b"127.0.0.1:6480")),
        ]))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await; // let serve_replica register
    drop(framed); // disconnect the replica

    // Two broadcasts: the first send after a drop can still succeed on some platforms before the
    // OS notices the close, so pruning is only guaranteed observable after a second attempt.
    let mut client = Framed::new(
        TcpStream::connect(addr).await.unwrap(),
        protocol::codec::RespCodec::default(),
    );
    for _ in 0..2 {
        client
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"k")),
                Frame::Bulk(Bytes::from_static(b"v")),
            ]))
            .await
            .unwrap();
        client.next().await.unwrap().unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    drop(_guard);
    let bytes_out = buffer.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let text = String::from_utf8(bytes_out).expect("subscriber output is utf-8");
    assert!(
        text.contains("replica registered") && text.contains("127.0.0.1:6480"),
        "expected a registration log naming the advertised address:\n{text}"
    );
    assert!(
        text.contains("replica pruned") && text.contains("127.0.0.1:6480"),
        "expected a prune log naming the same address:\n{text}"
    );
    // The connection span's `protocol` field, unquoted and uppercase, matching the same field on
    // `main.rs`'s "listener bound" events. It used to render `protocol="resp"` -- a bare `&str`
    // records through `Debug` -- so neither `grep protocol=RESP` nor `grep 'protocol="resp"'`
    // found both, defeating the fixed field vocabulary's whole purpose.
    assert!(
        text.contains("protocol=RESP"),
        "expected the connection span's protocol field unquoted and uppercase:\n{text}"
    );
}

/// `SlowLog::maybe_record`'s `warn!` -- lives here rather than as a `#[cfg(test)]` capture
/// assertion inside `crates/server/src/slowlog.rs` itself, per this file's established rule
/// (see the comment block above): every other test in that file's own `mod tests` calls
/// `maybe_record` at or over its threshold with no subscriber installed, which would reach the
/// new `warn!` callsite first in the ~575-test unit binary that file compiles into and could
/// permanently decide it's uninteresting before this test's own capture subscriber gets a turn.
/// This file has nothing else that ever reaches that callsite.
#[test]
fn maybe_record_only_warns_when_the_command_actually_gets_recorded() {
    let log = rocket_mem::slowlog::SlowLog::with_threshold(Duration::from_millis(10));

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_max_level(tracing::Level::WARN)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // Under the threshold: the hot path every command takes. Must not log anything at all --
    // getting this backwards would put a `warn!` on the hottest path in the project.
    log.maybe_record(
        "GET",
        Some(Bytes::from_static(b"fast")),
        1,
        Duration::from_micros(50),
        Some(&Bytes::from_static(b"fast")),
    );
    let after_fast_command =
        String::from_utf8(buffer.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .expect("subscriber output is utf-8");
    assert!(
        after_fast_command.is_empty(),
        "a command under the threshold must not log a warning, got:\n{after_fast_command}"
    );

    // At/over the threshold: must log, with the command, key, and elapsed microseconds.
    // `GET`/`LRANGE` are `KeySpec::First`, so the stored key and the logged key are the same
    // `Bytes` here -- `dispatch_and_log` is where the two can differ (`MEMORY USAGE <key>`), and
    // `the_slowlog_warning_key_is_key_spec_aware_and_never_renders_a_value` covers that.
    log.maybe_record(
        "LRANGE",
        Some(Bytes::from_static(b"mylist")),
        3,
        Duration::from_millis(25),
        Some(&Bytes::from_static(b"mylist")),
    );
    drop(_guard);
    let text = String::from_utf8(buffer.lock().unwrap_or_else(|e| e.into_inner()).clone())
        .expect("subscriber output is utf-8");
    assert!(
        text.contains("LRANGE"),
        "expected the command name in the slowlog warning:\n{text}"
    );
    assert!(
        text.contains("mylist"),
        "expected the key in the slowlog warning:\n{text}"
    );
    assert!(
        text.contains("25000"),
        "expected elapsed_us (25000) in the slowlog warning:\n{text}"
    );
}

/// A served `/metrics` scrape traces its byte count; a 404 does not. Lives here rather than as
/// a `#[cfg(test)]` capture assertion inside `crates/server/src/metrics.rs` itself, per this
/// file's established rule (see the comment block above `maybe_record_only_warns_...` and the
/// one further up): `crates/server/src/metrics.rs`'s own
/// `the_metrics_endpoint_serves_the_rendered_registry_and_404s_everything_else` scrapes
/// `/metrics` three times with no subscriber installed, in the same ~575-test unit binary,
/// which could permanently decide the new `trace!` callsite is uninteresting before a capture
/// test's own subscriber ever got a turn.
#[tokio::test]
async fn a_metrics_scrape_is_traced_and_a_404_is_not() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let handle = rocket_mem::metrics::recorder_handle();
    let engine = Arc::new(engine::Engine::new());
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::default());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(rocket_mem::metrics::serve_metrics(
        listener,
        handle,
        Arc::clone(&engine),
        Arc::clone(&replication),
    ));

    async fn get(addr: std::net::SocketAddr, path: &str) -> String {
        let mut socket = TcpStream::connect(addr).await.unwrap();
        socket
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).await.unwrap();
        response
    }

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let missing = get(addr, "/nope").await;
    assert!(
        missing.starts_with("HTTP/1.1 404 Not Found\r\n"),
        "{missing}"
    );
    let after_404 = String::from_utf8(buffer.lock().unwrap_or_else(|e| e.into_inner()).clone())
        .expect("subscriber output is utf-8");
    assert!(
        after_404.is_empty(),
        "a 404 must not trigger the scrape-served trace, got:\n{after_404}"
    );

    let body = get(addr, "/metrics").await;
    assert!(body.starts_with("HTTP/1.1 200 OK\r\n"), "{body}");

    drop(_guard);
    let text = String::from_utf8(buffer.lock().unwrap_or_else(|e| e.into_inner()).clone())
        .expect("subscriber output is utf-8");
    assert!(
        text.contains("metrics scrape served") && text.contains("bytes"),
        "expected a trace-level scrape event carrying a byte count:\n{text}"
    );
}

/// A complete `SET a 1` command, the prefix every AOF fixture below starts from.
const ONE_VALID_COMMAND: &[u8] = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";

/// A half-written trailing record -- the state a crash mid-append leaves -- is a condition
/// `replay_with_stats` deliberately tolerates: it stops at the last complete record and truncates
/// the file to that offset. So it is a `warn`, not an `error`; promoting it would make an ordinary
/// crash restart look like a fault. What it must carry is the offset it stopped at and the length
/// it stopped short of, and what it must never carry is the discarded bytes themselves -- those
/// are client data, and this event fires at a level an operator leaves on in production.
///
/// Lives here rather than in `aof.rs`'s own `mod tests`, per this file's established rule (see
/// the comment block further up): every recovery test in that module calls `recover` with no
/// subscriber installed, in the same ~575-test unit binary, so the callsite's `Interest` could be
/// cached `never` before a capture assertion there ever got a turn.
#[test]
fn a_truncated_aof_tail_warns_with_an_offset_and_never_the_discarded_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let aof_path = dir.path().join("truncated.aof");
    let mut contents = ONE_VALID_COMMAND.to_vec();
    // A bulk header promising 20 bytes followed by only 15: incomplete, never decodable.
    contents.extend_from_slice(b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$20\r\ntop-secret-tail");
    std::fs::write(&aof_path, &contents).expect("write aof");
    let snapshot_path = dir.path().join("absent.snapshot");

    let (engine, text) = capture_during("warn", || {
        rocket_mem::aof::recover(&aof_path, &snapshot_path).expect("recover")
    });

    assert!(
        engine.get(b"a").is_some(),
        "the complete record before the truncated tail must still have been replayed"
    );
    assert!(
        text.contains("aof tail discarded"),
        "expected a warn naming the discarded tail:\n{text}"
    );
    assert!(
        text.contains(&format!("offset={}", ONE_VALID_COMMAND.len())),
        "expected the offset the good data ends at:\n{text}"
    );
    assert!(
        text.contains(&format!("aof_len={}", contents.len())),
        "expected the file's full length alongside the offset:\n{text}"
    );
    assert!(
        !text.contains("top-secret-tail"),
        "the discarded bytes themselves reached the log:\n{text}"
    );
}

/// The four ways `recover` gives up and returns an `Err` to `main`, which turns them into a
/// process exit. Every one of them used to be completely silent -- the operator saw a dead
/// process and nothing else -- so each now logs at `error`: unlike the truncated tail above,
/// none of these is recovered from, and startup does not continue.
///
/// One test covering all four rather than four tests, deliberately: they share a subscriber and a
/// tempdir, and this file's capture-test count is under watch (plan 21's Task 2).
#[test]
fn every_aborting_recovery_failure_is_logged_at_error() {
    let dir = tempfile::tempdir().expect("tempdir");

    // The failing fixtures. Each names a path that exists but cannot serve its role: a directory
    // where a file is expected reads back as an error that is *not* `NotFound`, which is the one
    // distinction `recover`'s own matches turn on. No permissions games, so this behaves the same
    // for any user running the suite.
    let unreadable_dir = dir.path().join("a-directory");
    std::fs::create_dir(&unreadable_dir).expect("create dir");
    let a_file = dir.path().join("a-file");
    std::fs::write(&a_file, b"not a directory").expect("write file");

    let bad_manifest_snapshot = dir.path().join("bad-manifest.snapshot");
    std::fs::write(
        dir.path().join("bad-manifest.snapshot.manifest"),
        b"not-a-number",
    )
    .expect("write manifest");

    // A snapshot that loads cleanly, so recovery gets far enough to stat the AOF.
    let good_snapshot = dir.path().join("good.snapshot");
    std::fs::write(&good_snapshot, engine::Engine::new().snapshot(0)).expect("write snapshot");

    let (results, text) = capture_during("error", || {
        [
            // The generation manifest is present but not a number.
            rocket_mem::aof::recover(&dir.path().join("absent.aof"), &bad_manifest_snapshot),
            // The snapshot path cannot be read at all (and is not merely absent).
            rocket_mem::aof::recover(&dir.path().join("absent.aof"), &unreadable_dir),
            // The AOF path cannot be read at all (and is not merely absent).
            rocket_mem::aof::recover(&unreadable_dir, &dir.path().join("absent.snapshot")),
            // The AOF cannot even be stat-ed: a path under a plain file is `ENOTDIR`, not
            // `NotFound`, so it is not the "snapshot alone is the whole state" case.
            rocket_mem::aof::recover(&a_file.join("under-a-file.aof"), &good_snapshot),
        ]
    });

    for (i, result) in results.iter().enumerate() {
        assert!(result.is_err(), "fixture {i} was expected to fail recovery");
    }
    for expected in [
        "aof recovery failed: generation manifest unreadable",
        "aof recovery failed: snapshot file unreadable",
        "aof recovery failed: aof file unreadable",
        "aof recovery failed: aof file metadata unreadable",
    ] {
        assert!(
            text.contains(expected),
            "expected an error log for `{expected}`:\n{text}"
        );
    }
    assert_eq!(
        text.matches("ERROR").count(),
        4,
        "expected exactly one error line per failure, no duplicates:\n{text}"
    );
}
