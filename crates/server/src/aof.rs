use protocol::Frame;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;
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
/// fire-and-forget (`EverySecond`/`Never`: `append()` must not block on I/O at all).
/// `AppendAndFsync` and `Flush` both carry an ack channel so their caller can block until
/// the writer thread confirms the data is durable -- used for `FsyncPolicy::Always` and for
/// the explicit `fsync()` method, respectively.
enum AofMsg {
    Append(Vec<u8>),
    AppendAndFsync(Vec<u8>, mpsc::SyncSender<()>),
    Flush(mpsc::SyncSender<()>),
}

pub struct AofWriter {
    tx: mpsc::Sender<AofMsg>,
    policy: FsyncPolicy,
    /// Held by `dispatcher::dispatch_and_log` across "mutate the engine, then log it" for
    /// write commands, so concurrent writers' appends always land in the AOF in the same
    /// relative order their mutations committed in. See
    /// ../../docs/superpowers/specs/2026-08-30-tech-debt-cleanup-spec.md Item 2.
    order: Mutex<()>,
}

impl AofWriter {
    pub fn open(path: &Path, policy: FsyncPolicy) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut writer = BufWriter::new(file);
        let (tx, rx) = mpsc::channel::<AofMsg>();

        thread::Builder::new()
            .name("aof-writer".into())
            .spawn(move || {
                for msg in rx {
                    match msg {
                        AofMsg::Append(bytes) => {
                            if let Err(e) = writer.write_all(&bytes) {
                                eprintln!("aof append failed: {e}");
                            }
                        }
                        AofMsg::AppendAndFsync(bytes, ack) => {
                            if let Err(e) = writer
                                .write_all(&bytes)
                                .and_then(|_| writer.flush())
                                .and_then(|_| writer.get_ref().sync_data())
                            {
                                eprintln!("aof append failed: {e}");
                            }
                            let _ = ack.send(());
                        }
                        AofMsg::Flush(ack) => {
                            if let Err(e) =
                                writer.flush().and_then(|_| writer.get_ref().sync_data())
                            {
                                eprintln!("aof fsync failed: {e}");
                            }
                            let _ = ack.send(());
                        }
                    }
                }
            })
            .expect("failed to spawn aof writer thread");

        Ok(Self {
            tx,
            policy,
            order: Mutex::new(()),
        })
    }

    /// Encodes `frame` in RESP wire format and sends it to the dedicated writer thread.
    /// Under `FsyncPolicy::Always` this blocks until the write is fsynced -- matching the
    /// durability contract the caller relies on (the client's reply must not precede
    /// durability). Under `EverySecond`/`Never` it returns as soon as the message is
    /// enqueued, with no blocking I/O on the calling thread.
    pub fn append(&self, frame: Frame) -> std::io::Result<()> {
        let mut buf = bytes::BytesMut::new();
        protocol::codec::RespCodec::default().encode(frame, &mut buf)?;
        let bytes = buf.to_vec();
        if self.policy == FsyncPolicy::Always {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            self.send(AofMsg::AppendAndFsync(bytes, ack_tx))?;
            ack_rx.recv().map_err(writer_gone)
        } else {
            self.send(AofMsg::Append(bytes))
        }
    }

    /// Flushes the buffer and fsyncs the underlying file, blocking until the writer thread
    /// confirms it's done. Called directly by tests, and on a timer by
    /// `FsyncPolicy::EverySecond`'s periodic loop in `connection.rs`.
    pub fn fsync(&self) -> std::io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        self.send(AofMsg::Flush(ack_tx))?;
        ack_rx.recv().map_err(writer_gone)
    }

    /// The fsync policy this writer was opened with. Never changes after `open`.
    pub fn policy(&self) -> FsyncPolicy {
        self.policy
    }

    /// Acquired by `dispatcher::dispatch_and_log` around "mutate, then log" for write
    /// commands -- see the `order` field's doc comment above.
    pub fn lock_for_ordering(&self) -> std::sync::MutexGuard<'_, ()> {
        self.order.lock().unwrap()
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
/// replayed. A missing file is a no-op (nothing to recover on first run). A corrupt or
/// incomplete final frame stops replay at the last fully-decoded frame and truncates the
/// file on disk to that exact byte offset — see this plan's Global Constraints for why an
/// in-memory-only skip isn't sufficient.
pub fn replay(path: &Path, engine: &engine::Engine) -> std::io::Result<()> {
    use tokio_util::codec::Decoder;

    let raw = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let mut buf = bytes::BytesMut::from(&raw[..]);
    let mut codec = protocol::codec::RespCodec::default();
    let mut valid_len = 0usize;
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
        replay(&path, &engine).unwrap();
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
        replay(&path, &engine).unwrap();
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
        replay(&path, &engine).unwrap(); // must not panic
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
        replay(&path, &engine).unwrap();

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
        replay(&path, &engine2).unwrap();
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
        replay(&path, &engine).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), valid);
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
}
