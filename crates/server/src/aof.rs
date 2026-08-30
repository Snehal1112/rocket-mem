use protocol::Frame;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex};
use std::thread;
use tokio_util::codec::Encoder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsyncPolicy {
    Always,
    EverySecond,
    Never,
}

/// Sent to the dedicated writer thread spawned by `AofWriter::open`. `Append` is
/// fire-and-forget (`EverySecond`/`Never`: `append()` must not block on I/O at all), so its
/// caller has already returned by the time the write happens and there is nobody left to
/// hand an error to -- a failure there can only go to stderr. `AppendAndFsync` and `Flush`
/// both carry an ack channel that carries the *result* of the I/O back, so their caller can
/// block until the writer thread confirms durability and can see a real disk error -- used
/// for `FsyncPolicy::Always` and for the explicit `fsync()` method, respectively.
enum AofMsg {
    Append(Vec<u8>),
    AppendAndFsync(Vec<u8>, mpsc::SyncSender<std::io::Result<()>>),
    Flush(mpsc::SyncSender<std::io::Result<()>>),
}

/// Bounds the writer thread's queue. Unbounded would let a stalled disk grow the queue
/// without limit, each entry a heap-allocated frame invisible to the engine's `maxmemory`
/// accounting. 1024 absorbs normal bursts without adding latency, while a sustained stall
/// makes `send` (and therefore `append`) block -- the same natural backpressure the earlier
/// `Mutex<BufWriter<File>>` design had.
const AOF_QUEUE_CAPACITY: usize = 1024;

/// Encodes `frame` in RESP wire format. A free function, not a method, so `dispatch_and_log`
/// can call it once per write and reuse the same bytes for both `append_encoded` and a
/// replica broadcast — see the sprint-5 spec's fan-out hook decision for why.
pub fn encode_frame(frame: &Frame) -> std::io::Result<Vec<u8>> {
    let mut buf = bytes::BytesMut::new();
    protocol::codec::RespCodec::default().encode(frame.clone(), &mut buf)?;
    Ok(buf.to_vec())
}

pub struct AofWriter {
    /// Bounded at `AOF_QUEUE_CAPACITY`; see that constant for why.
    tx: mpsc::SyncSender<AofMsg>,
    policy: FsyncPolicy,
    /// Held by `dispatcher::dispatch_and_log` across "mutate the engine, then log it" for
    /// write commands, so concurrent writers' appends always land in the AOF in the same
    /// relative order their mutations committed in. See
    /// ../../docs/superpowers/specs/2026-08-30-tech-debt-cleanup-spec.md Item 2.
    order: Mutex<()>,
    /// The file `open` was given. Read back by `current_offset` after an `fsync`, so it must
    /// be the same path the writer thread is appending to — never mutated after `open`.
    path: PathBuf,
}

impl AofWriter {
    pub fn open(path: &Path, policy: FsyncPolicy) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut writer = BufWriter::new(file);
        let (tx, rx) = mpsc::sync_channel::<AofMsg>(AOF_QUEUE_CAPACITY);

        thread::Builder::new()
            .name("aof-writer".into())
            .spawn(move || {
                for msg in rx {
                    match msg {
                        // Fire-and-forget: the caller already returned, so stderr is the only
                        // place an error can go.
                        AofMsg::Append(bytes) => {
                            if let Err(e) = writer.write_all(&bytes) {
                                eprintln!("aof append failed: {e}");
                            }
                        }
                        // The acked variants hand the real I/O result back to the waiting
                        // caller instead of printing it, so a full disk surfaces where the
                        // write was requested. A failed send just means the caller gave up
                        // waiting; dropping the result is the only sensible response.
                        AofMsg::AppendAndFsync(bytes, ack) => {
                            let result = writer
                                .write_all(&bytes)
                                .and_then(|_| writer.flush())
                                .and_then(|_| writer.get_ref().sync_data());
                            let _ = ack.send(result);
                        }
                        AofMsg::Flush(ack) => {
                            let result = writer.flush().and_then(|_| writer.get_ref().sync_data());
                            let _ = ack.send(result);
                        }
                    }
                }
            })
            .expect("failed to spawn aof writer thread");

        Ok(Self {
            tx,
            policy,
            order: Mutex::new(()),
            path: path.to_path_buf(),
        })
    }

    /// Sends already-encoded bytes to the writer thread -- the part of `append` that isn't
    /// encoding. Under `FsyncPolicy::Always` this blocks until the write is fsynced and
    /// returns the writer thread's actual I/O result -- matching the durability contract the
    /// caller relies on (the client's reply must not precede durability, and a failed fsync
    /// must not look like success). Under `EverySecond`/`Never` it returns as soon as the
    /// message is enqueued, with no blocking I/O on the calling thread; a later I/O failure
    /// there is only reported on stderr, since this call has already returned. Enqueueing
    /// itself can block if the writer thread is far enough behind to fill the bounded queue --
    /// that is the intended backpressure, not a stall to avoid.
    pub fn append_encoded(&self, bytes: Vec<u8>) -> std::io::Result<()> {
        if self.policy == FsyncPolicy::Always {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            self.send(AofMsg::AppendAndFsync(bytes, ack_tx))?;
            // Two failure modes, flattened into one: the writer thread vanished (recv error),
            // or it ran and the write itself failed (the inner result).
            ack_rx.recv().map_err(writer_gone)?
        } else {
            self.send(AofMsg::Append(bytes))
        }
    }

    /// Encodes `frame` and sends it to the dedicated writer thread -- a thin wrapper over
    /// `encode_frame` + `append_encoded`, kept for callers that pass an owned `Frame` rather
    /// than pre-encoded bytes. See `append_encoded`'s doc comment for the
    /// `Always`/`EverySecond`/`Never` blocking behavior, which is unchanged by this split.
    pub fn append(&self, frame: Frame) -> std::io::Result<()> {
        self.append_encoded(encode_frame(&frame)?)
    }

    /// Flushes the buffer and fsyncs the underlying file, blocking until the writer thread
    /// confirms it's done and returning that thread's actual I/O result. Called directly by
    /// tests, and on a timer by `FsyncPolicy::EverySecond`'s periodic loop in `connection.rs`.
    pub fn fsync(&self) -> std::io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        self.send(AofMsg::Flush(ack_tx))?;
        ack_rx.recv().map_err(writer_gone)?
    }

    /// Flushes and fsyncs (via the existing `Flush` message the writer thread already handles),
    /// then returns the file's length in bytes. The returned offset is guaranteed durable: every
    /// byte before it is confirmed on disk. Calling this while holding
    /// `AofWriter::lock_for_ordering()` cannot deadlock: the writer thread only ever drains its
    /// channel and touches the file, never acquiring `order` or calling back into the dispatcher —
    /// the worst case is a bounded wait for whatever's already queued ahead of the `Flush`.
    pub fn current_offset(&self) -> std::io::Result<u64> {
        self.fsync()?;
        Ok(std::fs::metadata(&self.path)?.len())
    }

    /// The fsync policy this writer was opened with. Never changes after `open`.
    pub fn policy(&self) -> FsyncPolicy {
        self.policy
    }

    /// Acquired by `dispatcher::dispatch_and_log` around "mutate, then log" for write
    /// commands -- see the `order` field's doc comment above.
    #[must_use = "the returned guard must be bound and held across the whole mutate-then-log \
                  section; dropping it immediately releases the lock and loses the AOF \
                  ordering guarantee entirely"]
    pub fn lock_for_ordering(&self) -> std::sync::MutexGuard<'_, ()> {
        // Recover from poison rather than propagate it: this mutex is held across arbitrary
        // command dispatch (dispatcher::dispatch_and_log), so a panicking command handler
        // must not turn into a permanent, server-wide write outage. The guarded data is `()`
        // -- there is no invariant a panicking holder could have left broken.
        self.order.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, msg: AofMsg) -> std::io::Result<()> {
        self.tx.send(msg).map_err(|_| writer_gone_err())
    }
}

fn writer_gone<E>(_: E) -> std::io::Error {
    writer_gone_err()
}

fn writer_gone_err() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "aof writer thread is gone")
}

/// Commands whose successful execution mutates the keyspace and must be replayed on
/// recovery. See ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md for why this is a
/// static allowlist rather than inferred from the reply shape, and why `SPOP` and the
/// `EXPIRE` family are rewritten (not logged verbatim) despite appearing here.
pub const WRITE_COMMANDS: &[&str] = &[
    "SET",
    "APPEND",
    "SETRANGE",
    "GETSET",
    "MSET",
    "MSETNX",
    "INCR",
    "DECR",
    "INCRBY",
    "DEL",
    "EXPIRE",
    "PEXPIRE",
    "EXPIREAT",
    "PEXPIREAT",
    "PERSIST",
    "RENAME",
    "RENAMENX",
    "HSET",
    "HDEL",
    "HINCRBY",
    "HSETNX",
    "RPUSH",
    "LPUSH",
    "RPOP",
    "LPOP",
    "LSET",
    "LTRIM",
    "LREM",
    "LINSERT",
    "SADD",
    "SREM",
    "SPOP",
    "SINTERSTORE",
    "SUNIONSTORE",
    "SDIFFSTORE",
    "ZADD",
    "ZREM",
    "ZINCRBY",
];

/// Replays every command in the AOF at `path` against `engine`, via the plain (non-logging)
/// `dispatcher::dispatch` — never `dispatch_and_log`, which would re-append what's being
/// replayed. A missing file is a no-op (nothing to recover on first run). `start_at` is
/// clamped to the file's actual length rather than trusted blindly, so a caller passing a
/// stale or wrong offset degrades to "replay nothing" instead of panicking on an
/// out-of-range slice; `aof::recover` (below) is what decides *whether* a mismatched offset
/// should reach this function at all. A corrupt or incomplete final frame stops replay at the
/// last fully-decoded frame and truncates the file on disk to that exact byte offset.
pub fn replay(path: &Path, engine: &engine::Engine, start_at: u64) -> std::io::Result<()> {
    use tokio_util::codec::Decoder;

    let raw = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let start = (start_at as usize).min(raw.len());
    let mut buf = bytes::BytesMut::from(&raw[start..]);
    let mut codec = protocol::codec::RespCodec::default();
    let mut valid_len = start;
    loop {
        let before = buf.len();
        match codec.decode(&mut buf) {
            Ok(Some(frame)) => {
                valid_len += before - buf.len();
                let mut protocol = protocol::codec::Protocol::default();
                crate::dispatcher::dispatch(engine, frame, &mut protocol, 0);
            }
            Ok(None) | Err(_) => break, // incomplete or corrupt tail — stop here, keep what decoded
        }
    }

    if valid_len < raw.len() {
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_len(valid_len as u64)?;
    }
    Ok(())
}

/// Orchestrates startup recovery: loads `snapshot_path` if it exists and decodes cleanly,
/// checks whether its embedded AOF offset still fits within `aof_path`'s actual length, and
/// either replays just the AOF tail after that offset (the fast path) or falls back to a full
/// replay from byte 0 on a completely fresh `Engine` (the safe path, taken when there's no
/// snapshot, the snapshot is unreadable, or its offset no longer corresponds to this AOF).
/// See `../../docs/superpowers/specs/2026-08-30-sprint-5-spec.md` for why the "no compaction"
/// constraint is what makes "byte 0 onward is always the complete history" always true, and
/// therefore why the fallback is always correct rather than merely convenient.
pub fn recover(aof_path: &Path, snapshot_path: &Path) -> std::io::Result<engine::Engine> {
    let engine = engine::Engine::new();
    let start_at = match std::fs::read(snapshot_path) {
        Ok(bytes) => match engine.load_snapshot(&bytes) {
            Ok(offset) => {
                // A missing AOF is distinct from a zero-length one: the former means the
                // snapshot alone is the recovered state (per the spec's hybrid-recovery
                // decision), the latter means the offset genuinely overshoots and the
                // snapshot/AOF pair has diverged.
                let aof_len = match std::fs::metadata(aof_path) {
                    Ok(m) => Some(m.len()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e),
                };
                match aof_len {
                    None => return Ok(engine),
                    Some(len) if offset > len => {
                        eprintln!(
                            "snapshot at {} names an AOF offset ({offset}) past the AOF's \
                             actual length ({len}) -- discarding the snapshot and replaying \
                             the full AOF from byte 0 instead",
                            snapshot_path.display()
                        );
                        let fresh = engine::Engine::new();
                        replay(aof_path, &fresh, 0)?;
                        return Ok(fresh);
                    }
                    Some(_) => offset,
                }
            }
            Err(e) => {
                eprintln!(
                    "snapshot at {} is unreadable ({e}); falling back to full AOF replay",
                    snapshot_path.display()
                );
                0
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
    replay(aof_path, &engine, start_at)?;
    Ok(engine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use engine::{Engine, Value};
    use protocol::Frame;
    use std::io::Read;

    fn frame(parts: &[&[u8]]) -> Frame {
        Frame::Array(
            parts
                .iter()
                .map(|p| Frame::Bulk(Bytes::copy_from_slice(p)))
                .collect(),
        )
    }

    fn write_raw(path: &std::path::Path, bytes: &[u8]) {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    #[test]
    fn replay_on_a_missing_file_is_a_no_op_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.aof");
        let engine = Engine::new();
        replay(&path, &engine, 0).unwrap();
        assert!(engine.keys().is_empty());
    }

    #[test]
    fn replay_reconstructs_state_from_a_well_formed_aof() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        write_raw(
            &path,
            b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n",
        );
        let engine = Engine::new();
        replay(&path, &engine, 0).unwrap();
        assert_eq!(
            engine.get(b"a"),
            Some(Value::String(bytes::Bytes::from_static(b"1")))
        );
        assert_eq!(
            engine.get(b"b"),
            Some(Value::String(bytes::Bytes::from_static(b"2")))
        );
    }

    #[test]
    fn replay_recovers_every_valid_command_before_a_corrupt_tail_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");
        write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$3\r\ngar"); // truncated mid-bulk-body
        let engine = Engine::new();
        replay(&path, &engine, 0).unwrap(); // must not panic
        assert_eq!(
            engine.get(b"a"),
            Some(Value::String(bytes::Bytes::from_static(b"1")))
        );
        assert_eq!(engine.get(b"b"), None); // the truncated command never applied
    }

    #[test]
    fn replay_truncates_the_corrupt_tail_off_the_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let valid = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
        write_raw(&path, valid);
        write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$3\r\ngar"); // truncated mid-bulk-body
        let engine = Engine::new();
        replay(&path, &engine, 0).unwrap();

        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(on_disk, valid); // corrupt bytes physically removed, not just skipped in memory

        // proves future appends land cleanly right after the last valid frame, not after garbage
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        writer
            .append(protocol::Frame::Array(vec![
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"SET")),
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"c")),
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"3")),
            ]))
            .unwrap();
        writer.fsync().unwrap();
        let engine2 = Engine::new();
        replay(&path, &engine2, 0).unwrap();
        assert_eq!(
            engine2.get(b"a"),
            Some(Value::String(bytes::Bytes::from_static(b"1")))
        );
        assert_eq!(
            engine2.get(b"c"),
            Some(Value::String(bytes::Bytes::from_static(b"3")))
        );
    }

    #[test]
    fn replay_on_a_fully_well_formed_file_does_not_truncate_anything() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let valid = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
        write_raw(&path, valid);
        let engine = Engine::new();
        replay(&path, &engine, 0).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), valid);
    }

    #[test]
    fn replay_with_a_nonzero_start_at_skips_commands_before_that_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let first = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
        write_raw(&path, first);
        write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n");

        let engine = Engine::new();
        replay(&path, &engine, first.len() as u64).unwrap();

        assert_eq!(engine.get(b"a"), None); // before start_at -- skipped
        assert_eq!(
            engine.get(b"b"),
            Some(Value::String(bytes::Bytes::from_static(b"2")))
        );
    }

    #[test]
    fn replay_with_a_start_at_past_the_end_of_the_file_replays_nothing_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");

        let engine = Engine::new();
        replay(&path, &engine, 999_999).unwrap(); // must not panic on an out-of-range slice
        assert_eq!(engine.get(b"a"), None);
    }

    #[test]
    fn replay_with_a_nonzero_start_at_still_truncates_a_corrupt_tail_from_the_true_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let first = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
        write_raw(&path, first);
        let second = b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n";
        write_raw(&path, second);
        write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\nc\r\n$3\r\ngar"); // truncated mid-bulk-body

        let engine = Engine::new();
        replay(&path, &engine, first.len() as u64).unwrap();

        let on_disk = std::fs::read(&path).unwrap();
        let mut expected = first.to_vec();
        expected.extend_from_slice(second);
        assert_eq!(on_disk, expected); // corrupt tail removed; the skipped-over prefix stays intact
    }

    #[test]
    fn encode_frame_matches_append_s_existing_wire_format() {
        let encoded = encode_frame(&frame(&[b"SET", b"k", b"v"])).unwrap();
        assert_eq!(encoded, b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
    }

    #[test]
    fn append_encoded_writes_pre_encoded_bytes_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        writer
            .append_encoded(b"raw bytes, not even valid RESP".to_vec())
            .unwrap();
        writer.fsync().unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"raw bytes, not even valid RESP"
        );
    }

    #[test]
    fn append_still_produces_the_same_output_as_before_the_split() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        writer.append(frame(&[b"SET", b"k", b"v"])).unwrap();
        writer.fsync().unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n"
        );
    }

    #[test]
    fn append_writes_the_frame_in_resp_wire_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        writer.append(frame(&[b"SET", b"k", b"v"])).unwrap();
        writer.fsync().unwrap();

        let mut contents = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
    }

    #[test]
    fn append_is_cumulative_across_multiple_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        writer.append(frame(&[b"SET", b"a", b"1"])).unwrap();
        writer.append(frame(&[b"SET", b"b", b"2"])).unwrap();
        writer.fsync().unwrap();

        let mut contents = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(
            contents,
            "*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n"
        );
    }

    #[test]
    fn open_on_an_existing_file_appends_rather_than_truncating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        {
            let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
            writer.append(frame(&[b"SET", b"a", b"1"])).unwrap();
            writer.fsync().unwrap();
        }
        {
            let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
            writer.append(frame(&[b"SET", b"b", b"2"])).unwrap();
            writer.fsync().unwrap();
        }
        let mut contents = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(
            contents,
            "*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n"
        );
    }

    #[test]
    fn append_with_always_policy_fsyncs_after_every_write() {
        // no direct way to observe an fsync syscall from a unit test; this just proves
        // Always doesn't error and the data is durably readable immediately after append,
        // without a separate explicit fsync() call
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Always).unwrap();
        writer.append(frame(&[b"SET", b"k", b"v"])).unwrap();

        let mut contents = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
    }

    // `/dev/full` accepts an open and then fails every actual write with ENOSPC, which is the
    // simplest deterministic stand-in for a full disk. It only exists on Linux, so these two
    // tests are gated; the propagation they cover is otherwise only visible by reading the
    // writer thread's ack type.
    #[cfg(target_os = "linux")]
    #[test]
    fn append_with_always_policy_propagates_a_real_io_error_from_the_writer_thread() {
        let writer = AofWriter::open(std::path::Path::new("/dev/full"), FsyncPolicy::Always)
            .expect("/dev/full opens fine; only writing to it fails");
        let err = writer
            .append(frame(&[b"SET", b"k", b"v"]))
            .expect_err("a write that cannot land must not report success");
        // Not BrokenPipe: that's the "writer thread is gone" case, which this is not.
        assert_ne!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fsync_propagates_a_real_io_error_from_the_writer_thread() {
        let writer = AofWriter::open(std::path::Path::new("/dev/full"), FsyncPolicy::Never)
            .expect("/dev/full opens fine; only writing to it fails");
        // Never buffers without touching the disk, so the failure surfaces at the fsync.
        writer.append(frame(&[b"SET", b"k", b"v"])).unwrap();
        let err = writer
            .fsync()
            .expect_err("a flush that cannot land must not report success");
        assert_ne!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn write_commands_contains_known_mutating_commands() {
        assert!(WRITE_COMMANDS.contains(&"SET"));
        assert!(WRITE_COMMANDS.contains(&"SADD"));
        assert!(WRITE_COMMANDS.contains(&"EXPIRE"));
    }

    #[test]
    fn write_commands_excludes_known_read_only_commands() {
        assert!(!WRITE_COMMANDS.contains(&"GET"));
        assert!(!WRITE_COMMANDS.contains(&"KEYS"));
        assert!(!WRITE_COMMANDS.contains(&"TTL"));
        assert!(!WRITE_COMMANDS.contains(&"PING"));
    }

    #[test]
    fn lock_for_ordering_serializes_concurrent_holders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = std::sync::Arc::new(AofWriter::open(&path, FsyncPolicy::Never).unwrap());
        let log: std::sync::Arc<Mutex<Vec<(usize, bool)>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for id in 0..4 {
            let writer = std::sync::Arc::clone(&writer);
            let log = std::sync::Arc::clone(&log);
            handles.push(std::thread::spawn(move || {
                for _ in 0..50 {
                    let _guard = writer.lock_for_ordering();
                    log.lock().unwrap().push((id, true)); // entered the critical section
                    std::thread::yield_now();
                    log.lock().unwrap().push((id, false)); // about to leave it
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Every "entered" for a given thread must be immediately followed by that same
        // thread's "leaving" -- if lock_for_ordering() didn't provide mutual exclusion,
        // another thread's "entered" could land between them.
        let log = log.lock().unwrap();
        let mut i = 0;
        while i < log.len() {
            let (id, entering) = log[i];
            assert!(entering, "expected an entry at position {i}");
            assert_eq!(
                log[i + 1],
                (id, false),
                "thread {id}'s critical section was interrupted by another holder"
            );
            i += 2;
        }
    }

    #[test]
    fn current_offset_matches_the_file_length_after_appends_land() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        writer.append(frame(&[b"SET", b"a", b"1"])).unwrap();
        let offset = writer.current_offset().unwrap();
        assert_eq!(offset, std::fs::metadata(&path).unwrap().len());
        assert!(offset > 0);
    }

    #[test]
    fn recover_with_neither_file_present_returns_an_empty_engine() {
        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("missing.aof");
        let snapshot_path = dir.path().join("missing.snapshot");
        let engine = recover(&aof_path, &snapshot_path).unwrap();
        assert!(engine.keys().is_empty());
    }

    #[test]
    fn recover_with_only_an_aof_replays_it_in_full() {
        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("test.aof");
        write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");
        let snapshot_path = dir.path().join("missing.snapshot");

        let engine = recover(&aof_path, &snapshot_path).unwrap();
        assert_eq!(
            engine.get(b"a"),
            Some(Value::String(bytes::Bytes::from_static(b"1")))
        );
    }

    #[test]
    fn recover_with_a_matching_snapshot_and_offset_loads_the_snapshot_then_only_the_aof_tail() {
        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("test.aof");
        let before_snapshot = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
        write_raw(&aof_path, before_snapshot);

        // Build the "already snapshotted" engine, snapshot it at the AOF's current length, then
        // append one more command after that point -- the AOF tail recover() must still pick up.
        let snapshotted_engine = Engine::new();
        replay(&aof_path, &snapshotted_engine, 0).unwrap();
        let snapshot_bytes = snapshotted_engine.snapshot(before_snapshot.len() as u64);
        let snapshot_path = dir.path().join("test.snapshot");
        std::fs::write(&snapshot_path, snapshot_bytes).unwrap();

        write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n");

        let engine = recover(&aof_path, &snapshot_path).unwrap();
        assert_eq!(
            engine.get(b"a"),
            Some(Value::String(bytes::Bytes::from_static(b"1")))
        ); // from the snapshot
        assert_eq!(
            engine.get(b"b"),
            Some(Value::String(bytes::Bytes::from_static(b"2")))
        ); // from the AOF tail
    }

    #[test]
    fn recover_with_an_unreadable_snapshot_falls_back_to_a_full_aof_replay() {
        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("test.aof");
        write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");
        let snapshot_path = dir.path().join("test.snapshot");
        std::fs::write(&snapshot_path, b"not a real snapshot").unwrap(); // fewer than 8 header bytes... actually more, so it'll fail bincode decode

        let engine = recover(&aof_path, &snapshot_path).unwrap();
        assert_eq!(
            engine.get(b"a"),
            Some(Value::String(bytes::Bytes::from_static(b"1")))
        );
    }

    #[test]
    fn recover_with_a_snapshot_and_no_aof_keeps_the_snapshot_state() {
        let dir = tempfile::tempdir().unwrap();
        // Deliberately never created -- reproduces "SET k value; SAVE; delete the AOF; restart".
        let aof_path = dir.path().join("missing.aof");
        let snapshot_path = dir.path().join("test.snapshot");

        let snapshotted_engine = Engine::new();
        snapshotted_engine.set(
            bytes::Bytes::from_static(b"k"),
            Value::String(bytes::Bytes::from_static(b"value")),
        );
        // A nonzero embedded offset, as a real snapshot taken after some AOF writes would have.
        let snapshot_bytes = snapshotted_engine.snapshot(31);
        std::fs::write(&snapshot_path, snapshot_bytes).unwrap();

        let engine = recover(&aof_path, &snapshot_path).unwrap();
        assert_eq!(
            engine.get(b"k"),
            Some(Value::String(bytes::Bytes::from_static(b"value")))
        ); // the snapshot alone is the recovered state -- a missing AOF must not discard it
    }

    #[test]
    fn recover_with_a_snapshot_whose_offset_overshoots_the_aof_discards_it_and_replays_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("test.aof");
        write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");

        // A snapshot claiming an AOF offset far larger than the AOF's real (small) size --
        // as if the AOF were deleted and recreated smaller after the snapshot was taken.
        let stale_engine = Engine::new();
        stale_engine.set(
            bytes::Bytes::from_static(b"stale"),
            Value::String(bytes::Bytes::from_static(b"old")),
        );
        let snapshot_bytes = stale_engine.snapshot(999_999);
        let snapshot_path = dir.path().join("test.snapshot");
        std::fs::write(&snapshot_path, snapshot_bytes).unwrap();

        let engine = recover(&aof_path, &snapshot_path).unwrap();
        assert_eq!(engine.get(b"stale"), None); // the mismatched snapshot's data must not survive
        assert_eq!(
            engine.get(b"a"),
            Some(Value::String(bytes::Bytes::from_static(b"1")))
        ); // full AOF replay instead
    }
}
