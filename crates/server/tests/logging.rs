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

/// A `ReplicationHandle` pointed at a snapshot path inside `dir`, for the role-transition tests
/// below. They never reach a leader -- the transition and its log line happen synchronously,
/// before the spawned client loop's first connect attempt -- so the address they follow is
/// deliberately one nothing listens on.
fn transition_handle(dir: &std::path::Path) -> rocket_mem::replication::ReplicationHandle {
    rocket_mem::replication::ReplicationHandle::new(
        Arc::new(engine::Engine::new()),
        dir.join("replicaof-transition-test.snapshot"),
    )
}

/// An address nothing listens on. Port 1 needs root to bind, so the spawned reconnect loop fails
/// fast and forever rather than ever finding a real leader.
const DEAD_LEADER: &str = "127.0.0.1:1";

/// Promotion and demotion driven by a client's `REPLICAOF` command, at the production default
/// level -- the state change an operator asks about first during an incident, and which was
/// silent at every level before this.
///
/// **The password in the fixture is load-bearing.** An absence assertion against a fixture with no
/// secret in it passes forever and proves nothing; the spec records that as a real defect already
/// found once in this series. `zzcommandpassword` is genuinely parsed by `handle_replicaof` into
/// the AUTH tuple this transition carries, and mutation-checked: replacing `auth = auth_configured`
/// in `start_replicating_inner` with the credential itself makes this test fail on that assertion.
#[tokio::test]
async fn replicaof_command_transition_is_logged_at_info_without_the_password() {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = engine::Engine::new();
    let aof = rocket_mem::aof::AofWriter::open(
        &dir.path().join("replicaof-command.aof"),
        rocket_mem::aof::FsyncPolicy::Never,
    )
    .expect("open aof");
    let replication = transition_handle(dir.path());
    let session = rocket_mem::dispatcher::Session::new();

    let (_, text) = capture_during("info", || {
        // The full six-token credential-carrying form, straight through the real dispatcher.
        rocket_mem::dispatcher::dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[
                b"REPLICAOF",
                b"127.0.0.1",
                b"1",
                b"AUTH",
                b"app",
                b"zzcommandpassword",
            ]),
            &session,
            1,
        );
        replication.stop_replicating();
    });

    assert!(
        text.contains("replication started, node is now a follower"),
        "a client REPLICAOF must log the promotion at `info`; output was:\n{text}"
    );
    assert!(
        text.contains("source=command"),
        "the transition must say a client command caused it; output was:\n{text}"
    );
    assert!(
        text.contains(&format!("host_port={DEAD_LEADER}")),
        "the transition must name the leader; output was:\n{text}"
    );
    assert!(
        text.contains("auth=true"),
        "the transition must report that auth is configured; output was:\n{text}"
    );
    assert!(
        text.contains("replication stopped, node promoted to leader"),
        "REPLICAOF NO ONE's demotion must log at `info`; output was:\n{text}"
    );
    // The whole point of the fixture above.
    assert!(
        !text.contains("zzcommandpassword"),
        "the leader password reached the log; output was:\n{text}"
    );
}

/// The same transition reached from the config file instead of from a client, which is the path
/// the `replicaof` startup auto-connect takes (`main.rs` calls
/// `ReplicationHandle::start_replicating_from_config`). It must be distinguishable from the
/// command-driven one: "this node was told to follow" and "this node started up already
/// following" have different causes and different fixes.
///
/// Same mutation check, same reason: `zzconfigpassword` really is in the `Config` this drives,
/// reaching `start_replicating_inner` through `config::replicaof_auth`.
#[tokio::test]
async fn replicaof_config_transition_is_logged_at_info_without_the_password() {
    let dir = tempfile::tempdir().expect("tempdir");
    let replication = transition_handle(dir.path());
    let config = rocket_mem::config::Config {
        replicaof: Some(DEAD_LEADER.to_string()),
        replicaof_auth_username: Some("app".to_string()),
        replicaof_auth_password: Some("zzconfigpassword".to_string()),
        ..rocket_mem::config::Config::default()
    };

    let (_, text) = capture_during("info", || {
        replication.start_replicating_from_config(&config);
        replication.stop_replicating();
    });

    assert!(
        text.contains("replication started, node is now a follower"),
        "the startup auto-connect must log the same transition; output was:\n{text}"
    );
    assert!(
        text.contains("source=config"),
        "the config-driven transition must be distinguishable from the command-driven one; \
         output was:\n{text}"
    );
    assert!(
        !text.contains("source=command"),
        "the config path must not claim a client command caused it; output was:\n{text}"
    );
    assert!(
        text.contains(&format!("host_port={DEAD_LEADER}")) && text.contains("auth=true"),
        "the transition lost its leader or auth field; output was:\n{text}"
    );
    assert!(
        !text.contains("zzconfigpassword"),
        "the configured leader password reached the log; output was:\n{text}"
    );
}

/// `REPLICAOF NO ONE` against a node that was never a follower is a no-op, and must not claim a
/// promotion that did not happen -- an operator reading "promoted to leader" during a failover
/// has to be able to trust it.
#[tokio::test]
async fn stopping_replication_on_a_node_that_was_never_a_follower_claims_no_promotion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let replication = transition_handle(dir.path());

    let (_, text) = capture_during("info", || replication.stop_replicating());
    assert!(
        !text.contains("promoted to leader"),
        "a no-op stop must not report a promotion; output was:\n{text}"
    );

    let (_, debug_text) = capture_during("debug", || replication.stop_replicating());
    assert!(
        debug_text.contains("replication stop requested, but node was not a follower"),
        "the no-op is still worth seeing at `debug`; output was:\n{debug_text}"
    );
}

/// The moment a follower becomes in-sync (`link_up` goes true), at the production default level.
/// It used to be `debug!("snapshot loaded")`, which meant that at `info` a synced follower and one
/// still stuck in the reconnect loop emitted exactly the same output: nothing.
///
/// The message must also not collide with `aof.rs`'s own `info`-level `snapshot loaded`, which is
/// the *recovery* path reading this node's own snapshot off disk. Both are `info`, so a shared
/// string would make one grep return two events meaning opposite things -- hence the last
/// assertion, which is about the vocabulary, not about this code path.
#[tokio::test]
async fn a_follower_reaching_sync_is_logged_at_info() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let fake_leader = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut psync_bytes = [0u8; 15];
        socket.read_exact(&mut psync_bytes).await.unwrap();
        let blob = engine::Engine::new().snapshot(0);
        socket
            .write_all(&(blob.len() as u64).to_le_bytes())
            .await
            .unwrap();
        socket.write_all(&blob).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_env_filter(EnvFilter::new("info"))
        .finish();

    let dir = tempfile::tempdir().expect("tempdir");
    let replication = Arc::new(transition_handle(dir.path()));

    let _guard = tracing::subscriber::set_default(subscriber);
    replication.start_replicating(addr.to_string());
    tokio::time::sleep(Duration::from_millis(150)).await;
    replication.stop_replicating();
    drop(_guard);
    fake_leader.abort();

    let bytes_out = buffer.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let text = String::from_utf8(bytes_out).expect("subscriber output is utf-8");
    assert!(
        text.contains("follower in sync with leader"),
        "the sync-complete milestone is missing at `info`; output was:\n{text}"
    );
    assert!(
        !text.contains("snapshot loaded"),
        "this event must not reuse aof.rs's recovery-path message; output was:\n{text}"
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

/// The snapshot-only recovery path's own summary event. `aof.rs`'s
/// `recover_with_a_snapshot_and_no_aof_keeps_the_snapshot_state` covers the behaviour -- that a
/// missing AOF must not discard the snapshot -- but asserted nothing about the log, so the event
/// could have been deleted with every test still green. This is the log half, and it lives here
/// rather than in that test for this file's usual callsite-`Interest` reason.
///
/// The absence assertion is the point of the event: this path deliberately omits `commands`,
/// `bytes` and `elapsed_us`, because reporting `commands=0 bytes=0` would look identical to the
/// genuinely-empty-AOF case, which does replay a file. It shares the other path's message prefix
/// on purpose, so one grep still finds every recovery outcome.
#[test]
fn snapshot_only_recovery_logs_a_distinguishable_summary_at_info() {
    let dir = tempfile::tempdir().expect("tempdir");
    let aof_path = dir.path().join("absent.aof"); // deliberately never created
    let snapshot_path = dir.path().join("snapshot-only.snapshot");

    let snapshotted = engine::Engine::new();
    snapshotted.set(
        Bytes::from_static(b"k"),
        engine::Value::String(Bytes::from_static(b"snapshot-value")),
    );
    // A nonzero embedded offset, as a real snapshot taken after some AOF writes would have.
    std::fs::write(&snapshot_path, snapshotted.snapshot(31)).expect("write snapshot");

    let (recovered, text) = capture_during("info", || {
        rocket_mem::aof::recover(&aof_path, &snapshot_path).expect("recover")
    });

    assert!(
        recovered.get(b"k").is_some(),
        "the snapshot alone is the recovered state here"
    );
    assert!(
        text.contains("aof recovery replay complete (no aof file"),
        "expected the snapshot-only recovery summary at `info`:\n{text}"
    );
    assert!(
        !text.contains("commands="),
        "the no-aof summary must not report replay counts it never measured:\n{text}"
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

// ---------------------------------------------------------------------------------------------
// Log injection: client-controlled text must never reach a log line unescaped or uncapped.
//
// This is a log-*integrity* guard, not a confidentiality one -- no secret or stored value
// escapes through these fields. What escapes is the operator's ability to trust their own audit
// trail: `tracing`'s `%` (Display) fields reach the writer verbatim, so a key or username
// carrying `\n` writes a second, indistinguishable record, and one carrying an ANSI escape
// repaints the terminal reading it. Both are reachable at the production default of `info`, and
// the `user` field is reachable *before* the client has authenticated.
//
// The escaping itself is unit-tested in `common::log_escape` and `server::logging`. These tests
// exist because those cannot see what a real subscriber actually writes -- and the whole class
// of bug here is "the helper is correct but one call site does not use it".
// ---------------------------------------------------------------------------------------------

/// A forged log record, spelled exactly the way `tracing_subscriber::fmt`'s default format
/// spells a real one. If this ever reaches the output at the start of a line, neither an
/// operator reading the file nor a log shipper parsing it can tell it from a genuine record --
/// and it claims a successful authentication that never happened.
const FORGED_RECORD: &str =
    "2000-01-01T00:00:00.000000Z  INFO rocket_mem::dispatcher: auth success user=attacker";

/// The one assertion that distinguishes escaped from unescaped output.
///
/// It deliberately does *not* assert the forged text is absent: the payload is legitimately
/// still there, as data, inside the field it was supplied in. What must be absent is a *line*
/// beginning with it, because that -- not its presence -- is what makes it a record.
fn assert_no_forged_record_line(output: &str) {
    assert!(
        !output.lines().any(|line| line.starts_with("2000-01-01T")),
        "client-supplied text forged a log record of its own:\n{output}"
    );
}

/// A `ReplicationHandle` whose slow log fires on every command, so the `warn!` in
/// `SlowLog::maybe_record` -- emitted at this project's production default level, with the slow
/// log on by default -- is reached deterministically. `Duration::ZERO` would disable the slow
/// log entirely, hence 1ns.
fn always_slow() -> rocket_mem::replication::ReplicationHandle {
    rocket_mem::replication::ReplicationHandle::default()
        .with_slowlog_threshold(Duration::from_nanos(1))
}

/// A `ReplicationHandle` with one ACL user, so `AUTH` is answered (and logged) rather than
/// refused outright by `try_authenticate`'s "no password is set" guard.
fn with_one_acl_user() -> rocket_mem::replication::ReplicationHandle {
    let replication = rocket_mem::replication::ReplicationHandle::default();
    replication
        .acl
        .set_user(
            "alice",
            &[Bytes::from_static(b"on"), Bytes::from_static(b">pw")],
        )
        .expect("configure an ACL");
    replication
}

#[test]
fn a_key_carrying_a_newline_cannot_forge_a_log_record() {
    let key = format!("realkey\n{FORGED_RECORD}");
    let output =
        capture_frames_with_at("warn", always_slow(), vec![cmd(&[b"GET", key.as_bytes()])]);

    assert!(
        output.contains("slow command recorded"),
        "the slow-log warning did not fire, so this test proved nothing:\n{output}"
    );
    assert_no_forged_record_line(&output);
    assert!(
        output.contains("key=realkey\\x0a"),
        "expected the newline rendered as an escape inside the key field:\n{output}"
    );
}

#[test]
fn a_key_carrying_an_ansi_escape_cannot_repaint_an_operators_terminal() {
    // `\x1b[2J` clears the screen and `\x1b[31m` recolours everything after it. Neither is
    // something a stored key gets to do to the terminal an operator reads the log in.
    let output = capture_frames_with_at(
        "warn",
        always_slow(),
        vec![cmd(&[b"GET", b"realkey\x1b[2J\x1b[31mBOOM"])],
    );

    assert!(
        output.contains("slow command recorded"),
        "the slow-log warning did not fire, so this test proved nothing:\n{output}"
    );
    assert!(
        !output.contains('\x1b'),
        "a raw ESC byte reached the log line:\n{output:?}"
    );
    assert!(
        output.contains("key=realkey\\x1b[2J\\x1b[31mBOOM"),
        "expected each ESC rendered as an escape inside the key field:\n{output}"
    );
}

#[test]
fn an_unbounded_key_is_capped_before_it_reaches_the_log() {
    // Without a cap, one client writes one arbitrarily long record per slow command, at `warn`.
    let key = "k".repeat(4096);
    let output =
        capture_frames_with_at("warn", always_slow(), vec![cmd(&[b"GET", key.as_bytes()])]);

    assert!(
        output.contains("…(3840 more)"),
        "expected the key truncated at the identifier cap with a dropped-byte marker:\n{output}"
    );
    assert!(
        output.len() < 1024,
        "the log line is still unbounded in the key's length; it came to {} bytes",
        output.len()
    );
}

/// The `user` field, the worst of the set: `warn`, so on at the production default, and built
/// from a raw client bulk *before* the client has authenticated. Any remote party that can
/// reach an ACL-configured server can append records to its audit trail, unboundedly, with
/// `AUTH "<newline><forged record>" x`.
#[test]
fn a_username_carrying_a_newline_cannot_forge_a_log_record() {
    let username = format!("alice\n{FORGED_RECORD}");
    let output = capture_frames_with_at(
        "warn",
        with_one_acl_user(),
        vec![cmd(&[b"AUTH", username.as_bytes(), b"pw"])],
    );

    assert!(
        output.contains("auth failure"),
        "the pre-auth warning did not fire, so this test proved nothing:\n{output}"
    );
    assert_no_forged_record_line(&output);
    assert!(
        output.contains("user=alice\\x0a"),
        "expected the newline rendered as an escape inside the user field:\n{output}"
    );
}

#[test]
fn a_username_carrying_an_ansi_escape_cannot_repaint_an_operators_terminal() {
    let output = capture_frames_with_at(
        "warn",
        with_one_acl_user(),
        vec![cmd(&[b"AUTH", b"alice\x1b[2J\x1b[31mBOOM", b"pw"])],
    );

    assert!(
        output.contains("auth failure"),
        "the pre-auth warning did not fire, so this test proved nothing:\n{output}"
    );
    assert!(
        !output.contains('\x1b'),
        "a raw ESC byte reached the log line:\n{output:?}"
    );
}

#[test]
fn an_unbounded_username_is_capped_before_it_reaches_the_log() {
    let username = "u".repeat(4096);
    let output = capture_frames_with_at(
        "warn",
        with_one_acl_user(),
        vec![cmd(&[b"AUTH", username.as_bytes(), b"pw"])],
    );

    assert!(
        output.contains("…(3840 more)"),
        "expected the username truncated at the identifier cap with a marker:\n{output}"
    );
    assert!(
        output.len() < 1024,
        "the log line is still unbounded in the username's length; it came to {} bytes",
        output.len()
    );
}

/// `maybe_evict` runs after **every** mutation, so once the store sits at `maxmemory` every
/// subsequent write evicts something. A `warn!` on each such cycle therefore means one `warn`
/// line per write, at the production default level, for as long as the pressure lasts -- exactly
/// the firehose the level taxonomy exists to prevent, and for a `maxmemory` deployment the
/// loudest line in the log.
///
/// Both halves of the fix are asserted here, in one capture at `debug` so the file's capture-test
/// count does not grow twice: eviction becoming active is still reported at `warn` (it is a
/// genuine operational milestone -- silence would be the opposite mistake), and the per-cycle
/// detail an operator opts into is still there, per cycle, at `debug`.
///
/// Lives here rather than beside the other eviction tests in `crates/engine/src/engine.rs`, per
/// this file's established rule (see the comment block further up): those tests drive
/// `maybe_evict` with no subscriber installed, in the same unit-test binary, so the new
/// callsites' `Interest` could be cached before a capture assertion there ever got a turn.
#[test]
fn sustained_eviction_reports_its_onset_but_never_one_warn_per_write() {
    // 100-byte values against a 2_000-byte ceiling: the first handful of writes fill it, and
    // every write after that evicts, which is the steady state under test.
    let engine = engine::Engine::with_maxmemory(2_000);

    let (evictions, text) = capture_during("debug", || {
        for i in 0..300 {
            engine.set(
                Bytes::from(format!("evict-{i}")),
                engine::Value::String(Bytes::from(vec![b'x'; 100])),
            );
        }
        engine.eviction_count()
    });

    assert!(
        evictions > 200,
        "the ceiling did not force sustained eviction, so this test proved nothing: \
         {evictions} evictions"
    );
    assert_eq!(
        text.matches("WARN").count(),
        1,
        "sustained eviction emitted {} warn lines across 300 writes and {evictions} evictions; \
         the warn roll-up must not scale with the write rate:\n{text}",
        text.matches("WARN").count()
    );
    assert!(
        text.contains("maxmemory eviction active"),
        "eviction became active and the operator was never told at the default level:\n{text}"
    );
    // The opt-in detail: still one summary per cycle, and still one line per evicted key.
    assert!(
        text.matches("maxmemory eviction cycle").count() > 1,
        "the per-cycle summary must survive at `debug`, once per cycle:\n{text}"
    );
    assert!(
        text.contains("evicted key"),
        "the per-key eviction line must survive at `debug`:\n{text}"
    );
}

// ---------------------------------------------------------------------------------------------
// Span *names*, as they render.
//
// The spec fixes three span names -- `conn`, `cmd`, `repl` -- and the whole point of fixing them
// is that an operator greps for them. Every assertion in this file up to here checked span
// *fields* instead: `a_replica_registering_and_being_pruned_are_both_logged_at_info`'s
// `protocol=RESP`, `the_span_carries_the_command_name_and_arity`'s `cmd=`/`argc=`. A field
// assertion passes identically no matter what the span is called, which is exactly how the
// connection span went ~80 commits rendering as `handle_connection` -- `#[instrument]` defaults
// the name to the function's -- before anyone noticed.
//
// Each assertion below pins one name *together with its first field* (`conn{conn_id=`, not just
// `conn`), so it matches the rendered span header rather than the same letters appearing
// anywhere in a message or a target path.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_per_command_span_renders_under_the_name_cmd() {
    let output = capture_at("debug");
    assert!(
        output.contains("cmd{cmd="),
        "the per-command span is not rendering as `cmd`; output was:\n{output}"
    );
}

/// `conn` and `repl` in one capture because one PSYNC produces both: `serve_replica`'s `repl`
/// span opens inside the connection task's already-open `conn` span, so an event from within it
/// renders the full `conn{…}:repl{…}:` prefix an operator would grep.
#[tokio::test]
async fn the_connection_and_replication_spans_render_under_the_names_conn_and_repl() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("span-names.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .expect("open aof"),
    );
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&engine),
        dir.path().join("span-names.snapshot"),
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
            Frame::Bulk(Bytes::from_static(b"127.0.0.1:6481")),
        ]))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await; // let serve_replica register

    drop(_guard);
    let bytes_out = buffer.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let text = String::from_utf8(bytes_out).expect("subscriber output is utf-8");

    assert!(
        text.contains("conn{conn_id="),
        "the connection span is not rendering as `conn`; output was:\n{text}"
    );
    assert!(
        text.contains("repl{host_port="),
        "the leader-side replication span is not rendering as `repl`; output was:\n{text}"
    );
}

/// The `conn` span must parent RMP command dispatch, exactly as it already does on RESP.
///
/// RESP dispatches inline in the connection task, so its `cmd` span nests under `conn` for free.
/// RMP dispatches each request in its own `tokio::spawn` (deliberately -- that is what allows
/// several in-flight requests per connection), and `tracing`'s current-span context is
/// **task-local**: a bare `tokio::spawn` starts with no current span, so everything nested under
/// RMP dispatch used to emit with no `conn` parent at all -- no `conn_id`, no `peer`, no
/// `protocol`, no `tls`. That silently made the spec's central promise -- three spans carry
/// correlation and everything nested inherits it -- true for one protocol and false for the
/// other, so an operator could not attribute an RMP `permission denied` to a peer.
///
/// Asserting on the `cmd` span's own event is the strongest available check: it is the innermost
/// thing dispatch emits, so if *it* renders the `conn{…}:cmd{…}:` prefix, everything else the
/// spawned task emits (auth events, the slow-log warning, the AOF errors) inherits the same
/// parent.
#[tokio::test]
async fn an_rmp_command_dispatched_in_its_own_task_still_logs_under_the_conn_span() {
    use protocol::rmp::{MsgType, RmpCodec, RmpMessage};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("rmp-conn-span.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .expect("open aof"),
    );
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::default());
    tokio::spawn(rocket_mem::rmp_connection::serve(
        listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .with_env_filter(EnvFilter::new("debug"))
        .finish();
    // `set_default` scopes the subscriber to this thread, and `#[tokio::test]` builds a
    // current-thread runtime -- so the server's connection task and the per-request task it
    // spawns both run on this very thread and see this subscriber. A multi-thread runtime here
    // would capture nothing.
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut con = Framed::new(TcpStream::connect(addr).await.unwrap(), RmpCodec);
    con.send(RmpMessage {
        request_id: 1,
        msg_type: MsgType::Request,
        frame: cmd(&[b"SET", b"rmp-span-key", b"rmp-span-value"]),
    })
    .await
    .unwrap();
    let reply = con.next().await.unwrap().unwrap();
    assert_eq!(reply.frame, Frame::Simple("OK".into()));

    drop(_guard);
    let bytes_out = buffer.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let text = String::from_utf8(bytes_out).expect("subscriber output is utf-8");

    let dispatched: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("command dispatched"))
        .collect();
    assert!(
        !dispatched.is_empty(),
        "no per-command line was captured at all, so this test proved nothing:\n{text}"
    );
    for line in dispatched {
        assert!(
            line.contains("conn{conn_id="),
            "an RMP command logged with no `conn` parent span:\n{line}"
        );
        assert!(
            line.contains("protocol=RMP"),
            "the inherited `conn` span lost its protocol field:\n{line}"
        );
        assert!(
            line.contains("cmd{cmd=SET"),
            "the `cmd` span is missing from the per-command line:\n{line}"
        );
    }
}

#[test]
fn multi_and_discard_log_at_debug_with_counts_only_never_command_arguments() {
    let (_, output) = capture_during("debug", || {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine::Engine::new();
        let aof = rocket_mem::aof::AofWriter::open(
            &dir.path().join("tx-logging.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .expect("open aof");
        let replication = rocket_mem::replication::ReplicationHandle::default();
        let session = rocket_mem::dispatcher::Session::new();

        rocket_mem::dispatcher::dispatch_and_log(
            &engine,
            &aof,
            &replication,
            Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"MULTI"))]),
            &session,
            1,
        );
        rocket_mem::dispatcher::dispatch_and_log(
            &engine,
            &aof,
            &replication,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"queued-key")),
                Frame::Bulk(Bytes::from_static(b"super-secret-value")),
            ]),
            &session,
            1,
        );
        rocket_mem::dispatcher::dispatch_and_log(
            &engine,
            &aof,
            &replication,
            Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"DISCARD"))]),
            &session,
            1,
        );
    });

    assert!(output.contains("transaction started"), "got: {output}");
    assert!(
        output.contains("transaction discarded") && output.contains("queued_count=1"),
        "got: {output}"
    );
    // The queued SET's *key* legitimately reaches the log: `dispatch_and_log`'s pre-existing
    // `cmd` span logs every command's key name at `debug` regardless of whether a transaction
    // is queuing it, per this file's `debug_logs_the_key_but_not_the_value` test and CLAUDE.md's
    // redaction policy (key names and byte lengths, never value contents). Only the *value* is
    // the thing this test must prove never leaks.
    assert!(
        !output.contains("super-secret-value"),
        "a queued command's value must never reach the log at debug, got: {output}"
    );
}

#[test]
fn exec_logs_queued_count_shard_count_and_elapsed_at_debug() {
    let (_, output) = capture_during("debug", || {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine::Engine::new();
        let aof = rocket_mem::aof::AofWriter::open(
            &dir.path().join("tx-exec-logging.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .expect("open aof");
        let replication = rocket_mem::replication::ReplicationHandle::default();
        let session = rocket_mem::dispatcher::Session::new();

        for frame in [vec!["MULTI"], vec!["SET", "k", "v"], vec!["EXEC"]] {
            let frame = Frame::Array(
                frame
                    .into_iter()
                    .map(|s| Frame::Bulk(Bytes::from(s.to_string())))
                    .collect(),
            );
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

    assert!(output.contains("transaction executed"), "got: {output}");
    assert!(output.contains("queued_count=1"), "got: {output}");
    assert!(output.contains("shard_count=1"), "got: {output}");
    assert!(output.contains("elapsed_us="), "got: {output}");
}
