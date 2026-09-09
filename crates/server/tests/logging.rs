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
use protocol::Frame;
use std::io;
use std::sync::{Arc, Mutex};
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
