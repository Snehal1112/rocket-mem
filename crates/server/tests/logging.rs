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
        let replication = rocket_mem::replication::ReplicationHandle::default();
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
}
