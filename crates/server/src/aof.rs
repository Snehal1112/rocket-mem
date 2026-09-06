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
    /// Flushes and fsyncs the current file, opens `new_path` (create, append — so a rotation
    /// onto a file an interrupted previous rewrite already partially wrote appends after its
    /// content rather than clobbering it), and swaps the writer thread's target to it.
    Rotate(PathBuf, mpsc::SyncSender<std::io::Result<()>>),
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
    /// Serializes whole rewrites against each other and against `SAVE` -- see
    /// `lock_for_rewrite`. Distinct from `order`, which is far too narrow for this: it is
    /// released the moment `start_rewrite` returns, leaving the manifest read, the snapshot
    /// write and the commit rename unprotected. Always acquired *before* `order`, never the
    /// other way round, so the two can never deadlock.
    rewrite: Mutex<()>,
    /// The file currently being appended to. `Mutex`-wrapped (not a plain `PathBuf`) so
    /// `rotate_to` can repoint it after a successful rotation; read by `current_offset`.
    path: Mutex<PathBuf>,
    /// The original path `open` was given. Never changes, even across `rotate_to` calls --
    /// generation-path math (plan 03) must always start from this, never from `path`, which
    /// already reflects whatever generation is currently active.
    base_path: PathBuf,
}

impl AofWriter {
    pub fn open(path: &Path, policy: FsyncPolicy) -> std::io::Result<Self> {
        Self::open_with_base(path, path, policy)
    }

    /// Opens the AOF at generation `gen`'s file (`generation_path(base_path, gen)`) while keeping
    /// `base_path()` equal to the *unresolved* `base_path` it was handed. That split is the whole
    /// point of this constructor: `open(generation_path(base, gen), policy)` would look equivalent
    /// but sets `base_path` to the already-suffixed path, so the next rewrite's `rotate_to` would
    /// compute `<base>.1.2` instead of `<base>.2`. Startup (`main.rs`) must use this, not `open`,
    /// or every write made after a restart lands in a file the manifest no longer names and is
    /// lost at the following restart.
    pub fn open_at_generation(
        base_path: &Path,
        gen: u64,
        policy: FsyncPolicy,
    ) -> std::io::Result<Self> {
        Self::open_with_base(&generation_path(base_path, gen), base_path, policy)
    }

    /// The shared body of `open`/`open_at_generation`: `path` is the file actually opened and
    /// appended to, `base_path` is what `base_path()` reports and all generation-path math starts
    /// from. They differ only when opening at a non-zero generation.
    fn open_with_base(path: &Path, base_path: &Path, policy: FsyncPolicy) -> std::io::Result<Self> {
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
                        AofMsg::Rotate(new_path, ack) => {
                            let result = writer.flush().and_then(|_| writer.get_ref().sync_data());
                            let result = result.and_then(|_| {
                                OpenOptions::new().create(true).append(true).open(&new_path)
                            });
                            let result = match result {
                                Ok(file) => {
                                    writer = BufWriter::new(file);
                                    Ok(())
                                }
                                Err(e) => Err(e),
                            };
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
            rewrite: Mutex::new(()),
            path: Mutex::new(path.to_path_buf()),
            base_path: base_path.to_path_buf(),
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
        let path = self.path.lock().unwrap_or_else(|e| e.into_inner());
        Ok(std::fs::metadata(&*path)?.len())
    }

    /// The fsync policy this writer was opened with. Never changes after `open`.
    pub fn policy(&self) -> FsyncPolicy {
        self.policy
    }

    /// The file currently being appended to.
    pub fn path(&self) -> PathBuf {
        self.path.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The original path `open` was given — stable across any number of `rotate_to` calls. See
    /// the `base_path` field's doc comment for why generation-path math must use this, not
    /// `path()`.
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Flushes and fsyncs the current file, then redirects subsequent appends to `new_path`
    /// (created fresh, or appended-to if it already exists). Must be called while the caller
    /// already holds `lock_for_ordering()` — see this plan's Global Constraints for why no new
    /// lock is needed here. Acked like `fsync()`, so the caller knows the rotation completed
    /// (and `path()`/`current_offset()` reflect the new file) before proceeding.
    pub fn rotate_to(&self, new_path: &Path) -> std::io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        self.send(AofMsg::Rotate(new_path.to_path_buf(), ack_tx))?;
        ack_rx.recv().map_err(writer_gone)??;
        *self.path.lock().unwrap_or_else(|e| e.into_inner()) = new_path.to_path_buf();
        Ok(())
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

    /// Held by `dispatcher::handle_bgrewriteaof` across its *entire* sequence -- generation read,
    /// rotation, snapshot write, manifest commit, cleanup -- and by `handle_save` across its own
    /// generation-read-then-write. Two rewrites that interleave here do not merely race: both read
    /// generation `G`, both target `G + 1`, and their `<snapshot>.<G+1>.tmp` renames collide, so
    /// one fails outright while the other's AOF rotation has already been overwritten.
    /// `lock_for_ordering` cannot serve this purpose -- it is deliberately released before the
    /// snapshot write so no client write blocks on the disk I/O.
    ///
    /// Poison is recovered from rather than propagated, exactly as in `lock_for_ordering`: the
    /// guarded data is `()`, and a panicking rewrite must not permanently break `SAVE`.
    #[must_use = "the returned guard must be held across the whole rewrite; dropping it \
                  immediately reintroduces the interleaved-generation race"]
    pub fn lock_for_rewrite(&self) -> std::sync::MutexGuard<'_, ()> {
        self.rewrite.lock().unwrap_or_else(|e| e.into_inner())
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

/// The manifest path derived from `snapshot_path` — always `<snapshot_path>.manifest`, never
/// separately configured. See the design spec's "no new configuration surface" scope note.
fn manifest_path(snapshot_path: &Path) -> PathBuf {
    let mut os = snapshot_path.as_os_str().to_owned();
    os.push(".manifest");
    PathBuf::from(os)
}

/// `base` unchanged for generation 0 — so every pre-compaction deployment's files keep their
/// exact names forever, no migration needed — or `<base>.<gen>` for generation 1 and up.
pub fn generation_path(base: &Path, gen: u64) -> PathBuf {
    if gen == 0 {
        return base.to_path_buf();
    }
    let mut os = base.as_os_str().to_owned();
    os.push(format!(".{gen}"));
    PathBuf::from(os)
}

/// The current generation, read from `<snapshot_path>.manifest`. A missing manifest means
/// generation 0 — every deployment that has never run `BGREWRITEAOF` — so `snapshot_path`/
/// `aof_path` are used exactly as configured. See the design spec's "generations + a manifest"
/// decision.
pub fn read_generation(snapshot_path: &Path) -> std::io::Result<u64> {
    match std::fs::read_to_string(manifest_path(snapshot_path)) {
        Ok(contents) => contents
            .trim()
            .parse::<u64>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

/// Atomically (tmp + fsync + rename — the same primitive `dispatcher::write_snapshot_atomically`
/// uses, duplicated here rather than shared so `aof.rs` doesn't gain a dependency on
/// `dispatcher.rs`, reversing this crate's existing one-way dependency direction) writes `gen`
/// as the new current generation. This rename is the single commit point of a rewrite: before
/// it, generation `gen - 1`'s files are authoritative; after it, generation `gen`'s are. See the
/// design spec's "Decision: `BGREWRITEAOF` command", step 3.
/// fsyncs the directory holding `path`, making a rename *into* that directory durable. Without
/// it, `tmp + fsync + rename` only guarantees the tmp file's *contents* survive a crash: the
/// directory entry the rename creates can still be lost, leaving the manifest reading stale
/// while a rewrite's best-effort cleanup has already deleted the old generation's files -- the
/// one ordering in which `recover()` finds nothing at all.
///
/// A path with no directory component (a bare relative filename) has an empty `parent()`, which
/// is not openable; the process's own working directory is the implicit parent, so `.` is synced
/// instead. Unix-only: Windows cannot open a directory as a file, and this project's release
/// matrix builds there, so the sync degrades to a no-op rather than failing every commit.
#[cfg(unix)]
pub(crate) fn fsync_parent_dir(path: &Path) -> std::io::Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn fsync_parent_dir(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub fn write_generation_atomically(snapshot_path: &Path, gen: u64) -> std::io::Result<()> {
    use std::io::Write;
    let path = manifest_path(snapshot_path);
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp_path = PathBuf::from(tmp_os);
    {
        let file = std::fs::File::create(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        write!(writer, "{gen}")?;
        writer.flush()?;
        writer.get_ref().sync_data()?;
    }
    std::fs::rename(&tmp_path, &path)?;
    // The rename is this rewrite's commit point, and `handle_bgrewriteaof` deletes the previous
    // generation's files immediately after it returns -- so the entry has to be durable before
    // the only other copy of that state goes away.
    fsync_parent_dir(&path)?;
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
    let gen = read_generation(snapshot_path)?;
    let aof_path = &generation_path(aof_path, gen);
    let snapshot_path = &generation_path(snapshot_path, gen);

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

    #[test]
    fn recover_ignores_a_new_generation_whose_manifest_was_never_committed() {
        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("test.aof");
        let snapshot_path = dir.path().join("test.snapshot");

        // Generation 0: committed history, exactly as if no rewrite had ever been attempted.
        write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");

        // Simulate a rewrite that got as far as writing generation 1's files but crashed before
        // the manifest commit -- this is the exact state `start_rewrite` + a snapshot write leave
        // on disk one line before `write_generation_atomically` runs.
        let gen1_engine = Engine::new();
        gen1_engine.set(
            Bytes::from_static(b"b"),
            Value::String(Bytes::from_static(b"orphaned")),
        );
        std::fs::write(generation_path(&snapshot_path, 1), gen1_engine.snapshot(0)).unwrap();
        std::fs::write(generation_path(&aof_path, 1), b"").unwrap();
        // No manifest written -- this is the frozen crash point.

        let engine = recover(&aof_path, &snapshot_path).unwrap();
        assert_eq!(
            engine.get(b"a"),
            Some(Value::String(Bytes::from_static(b"1")))
        ); // from generation 0's AOF, completely untouched by the abandoned attempt
        assert_eq!(engine.get(b"b"), None); // generation 1 must be entirely ignored
    }

    #[test]
    fn recover_reads_generation_1_once_the_manifest_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("test.aof");
        let snapshot_path = dir.path().join("test.snapshot");

        // Generation 0 exists but must be ignored once the manifest points past it.
        write_raw(&aof_path, b"*3\r\n$3\r\nSET\r\n$1\r\nold\r\n$1\r\n1\r\n");

        // Generation 1 is complete and committed.
        let gen1_engine = Engine::new();
        gen1_engine.set(
            Bytes::from_static(b"new"),
            Value::String(Bytes::from_static(b"2")),
        );
        std::fs::write(generation_path(&snapshot_path, 1), gen1_engine.snapshot(0)).unwrap();
        std::fs::write(generation_path(&aof_path, 1), b"").unwrap();
        write_generation_atomically(&snapshot_path, 1).unwrap();

        let engine = recover(&aof_path, &snapshot_path).unwrap();
        assert_eq!(
            engine.get(b"new"),
            Some(Value::String(Bytes::from_static(b"2")))
        ); // from generation 1
        assert_eq!(engine.get(b"old"), None); // generation 0 must be ignored once superseded

        // Generation 0's AOF is still on disk here -- the exact state `handle_bgrewriteaof`
        // leaves one line before its best-effort `remove_file` calls run. Being merely
        // unreferenced rather than gone must not change the outcome above.
        assert!(aof_path.exists());
    }

    #[test]
    fn generation_path_for_generation_zero_is_the_bare_path_unchanged() {
        let base = std::path::Path::new("/tmp/dump.snapshot");
        assert_eq!(generation_path(base, 0), base);
    }

    #[test]
    fn generation_path_for_a_later_generation_appends_dot_gen() {
        let base = std::path::Path::new("/tmp/dump.snapshot");
        assert_eq!(
            generation_path(base, 1),
            std::path::PathBuf::from("/tmp/dump.snapshot.1")
        );
        assert_eq!(
            generation_path(base, 42),
            std::path::PathBuf::from("/tmp/dump.snapshot.42")
        );
    }

    #[test]
    fn read_generation_with_no_manifest_on_disk_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("dump.snapshot"); // never created
        assert_eq!(read_generation(&snapshot_path).unwrap(), 0);
    }

    #[test]
    fn read_generation_reads_back_a_hand_written_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("dump.snapshot");
        std::fs::write(dir.path().join("dump.snapshot.manifest"), "7").unwrap();
        assert_eq!(read_generation(&snapshot_path).unwrap(), 7);
    }

    #[test]
    fn read_generation_tolerates_a_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("dump.snapshot");
        std::fs::write(dir.path().join("dump.snapshot.manifest"), "3\n").unwrap();
        assert_eq!(read_generation(&snapshot_path).unwrap(), 3);
    }

    #[test]
    fn read_generation_on_a_corrupt_manifest_is_an_error_not_a_silent_zero() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("dump.snapshot");
        std::fs::write(dir.path().join("dump.snapshot.manifest"), "not-a-number").unwrap();
        assert!(read_generation(&snapshot_path).is_err());
    }

    #[test]
    fn write_generation_atomically_is_read_back_by_read_generation() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("dump.snapshot");
        write_generation_atomically(&snapshot_path, 5).unwrap();
        assert_eq!(read_generation(&snapshot_path).unwrap(), 5);
    }

    #[test]
    fn write_generation_atomically_overwrites_a_previous_generation() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("dump.snapshot");
        write_generation_atomically(&snapshot_path, 1).unwrap();
        write_generation_atomically(&snapshot_path, 2).unwrap();
        assert_eq!(read_generation(&snapshot_path).unwrap(), 2);
    }

    #[test]
    fn fsync_parent_dir_succeeds_for_a_path_inside_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        // The file itself need not exist -- it is the *directory entry* being made durable.
        fsync_parent_dir(&dir.path().join("dump.snapshot.manifest")).unwrap();
    }

    #[test]
    fn fsync_parent_dir_on_a_bare_relative_filename_is_not_an_error() {
        // `Path::parent()` yields an empty path here, which is not openable. A manifest commit
        // configured with a bare relative path must still succeed, not fail on the dir sync.
        fsync_parent_dir(std::path::Path::new("dump.snapshot.manifest")).unwrap();
    }

    #[test]
    fn write_generation_atomically_does_not_leave_a_tmp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("dump.snapshot");
        write_generation_atomically(&snapshot_path, 1).unwrap();
        assert!(!dir.path().join("dump.snapshot.manifest.tmp").exists());
    }

    #[test]
    fn path_reports_the_file_aof_writer_was_opened_with() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        assert_eq!(writer.path(), path);
    }

    #[test]
    fn rotate_to_freezes_the_old_file_and_directs_new_appends_to_the_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let old_path = dir.path().join("old.aof");
        let new_path = dir.path().join("new.aof");
        let writer = AofWriter::open(&old_path, FsyncPolicy::Never).unwrap();

        writer.append(frame(&[b"SET", b"a", b"1"])).unwrap();
        writer.fsync().unwrap();

        writer.rotate_to(&new_path).unwrap();

        writer.append(frame(&[b"SET", b"b", b"2"])).unwrap();
        writer.fsync().unwrap();

        assert_eq!(
            std::fs::read(&old_path).unwrap(),
            b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n"
        ); // frozen at rotation, never touched again
        assert_eq!(
            std::fs::read(&new_path).unwrap(),
            b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n"
        ); // starts fresh at byte 0
    }

    #[test]
    fn rotate_to_updates_path_and_current_offset() {
        let dir = tempfile::tempdir().unwrap();
        let old_path = dir.path().join("old.aof");
        let new_path = dir.path().join("new.aof");
        let writer = AofWriter::open(&old_path, FsyncPolicy::Never).unwrap();
        writer.append(frame(&[b"SET", b"a", b"1"])).unwrap();
        writer.fsync().unwrap();

        writer.rotate_to(&new_path).unwrap();

        assert_eq!(writer.path(), new_path);
        assert_eq!(writer.current_offset().unwrap(), 0); // the new file starts empty
    }

    #[test]
    fn rotate_to_an_already_existing_file_appends_after_its_current_content() {
        // Mirrors `open_on_an_existing_file_appends_rather_than_truncating` — rotation must not
        // clobber a new-generation file a previous, interrupted rewrite already partially wrote.
        let dir = tempfile::tempdir().unwrap();
        let old_path = dir.path().join("old.aof");
        let new_path = dir.path().join("new.aof");
        write_raw(&new_path, b"*3\r\n$3\r\nSET\r\n$1\r\nx\r\n$1\r\n0\r\n");

        let writer = AofWriter::open(&old_path, FsyncPolicy::Never).unwrap();
        writer.rotate_to(&new_path).unwrap();
        writer.append(frame(&[b"SET", b"y", b"1"])).unwrap();
        writer.fsync().unwrap();

        assert_eq!(
            std::fs::read(&new_path).unwrap(),
            b"*3\r\n$3\r\nSET\r\n$1\r\nx\r\n$1\r\n0\r\n*3\r\n$3\r\nSET\r\n$1\r\ny\r\n$1\r\n1\r\n"
        );
    }

    #[test]
    fn open_at_generation_writes_to_the_generation_file_but_reports_the_bare_base_path() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("test.aof");
        let writer = AofWriter::open_at_generation(&base, 1, FsyncPolicy::Never).unwrap();

        writer.append(frame(&[b"SET", b"a", b"1"])).unwrap();
        writer.fsync().unwrap();

        assert_eq!(writer.path(), generation_path(&base, 1)); // appends land in generation 1's file
        assert!(!base.exists()); // the bare generation-0 file is never touched
        assert_eq!(writer.base_path(), base); // unresolved, so the next rotation targets `.2`
    }

    #[test]
    fn open_at_generation_zero_is_identical_to_open() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("test.aof");
        let writer = AofWriter::open_at_generation(&base, 0, FsyncPolicy::Never).unwrap();
        assert_eq!(writer.path(), base);
        assert_eq!(writer.base_path(), base);
    }

    #[test]
    fn a_rotation_after_open_at_generation_does_not_double_suffix_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("test.aof");
        let writer = AofWriter::open_at_generation(&base, 1, FsyncPolicy::Never).unwrap();

        writer
            .rotate_to(&generation_path(writer.base_path(), 2))
            .unwrap();

        assert_eq!(writer.path(), dir.path().join("test.aof.2")); // not `test.aof.1.2`
    }

    #[test]
    fn base_path_never_changes_across_a_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let original_path = dir.path().join("original.aof");
        let writer = AofWriter::open(&original_path, FsyncPolicy::Never).unwrap();

        writer.rotate_to(&dir.path().join("gen1.aof")).unwrap();
        writer.rotate_to(&dir.path().join("gen2.aof")).unwrap();

        assert_eq!(writer.base_path(), original_path); // unchanged despite two rotations
        assert_eq!(writer.path(), dir.path().join("gen2.aof")); // this one does change
    }
}
