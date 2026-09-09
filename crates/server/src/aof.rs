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
    /// Compares the open fd's identity against a fresh `stat` of `path` -- see
    /// `is_file_intact`'s doc comment for why this exists. Carries `path` rather than reading
    /// `AofWriter::path` itself because the writer thread has no access to `self`, only to
    /// whatever each message hands it (the same reason `Rotate` carries `new_path`).
    CheckIntact(PathBuf, mpsc::SyncSender<std::io::Result<bool>>),
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

/// Runs `f`, freeing the current worker thread for other tasks while it blocks, if called from
/// inside a multi-threaded Tokio runtime. `AofWriter`'s ack-channel `recv()` and `send()` calls
/// block the calling OS thread for real I/O (a disk fsync, or backpressure from a full writer
/// queue) -- called directly from an async task, that freezes the whole worker thread, starving
/// every other task queued on it for the wait's duration. `tokio::task::block_in_place` is the
/// fix: it hands the runtime a replacement worker so other tasks keep running.
///
/// Wrap only the sections that genuinely block, never a whole method: the handoff costs real
/// time (measured at roughly half a microsecond a call), so it pays for itself against a disk
/// fsync but not against a queue push that almost always succeeds outright. That is why `send`
/// tries first and only wraps the full-queue fallback, while the ack-channel `recv()`s -- an
/// unavoidable wait on the writer thread -- are wrapped at each call site.
///
/// `block_in_place` requires an actual multi-threaded Tokio runtime and panics anywhere else --
/// both with no runtime at all and on a current-thread one. Both cases are real here: every
/// synchronous unit test in this file calls `fsync`/`append_encoded`/`rotate_to` with no runtime
/// present, and several `#[tokio::test]` tests elsewhere in the crate (which are current-thread
/// by default) drive real AOF writes. So the flavor, not just the presence, of a runtime gates
/// the call: anything but a multi-threaded runtime runs `f` directly, exactly as before this fix.
/// The only production runtime is `#[tokio::main]` in `main.rs`, which is multi-threaded, so
/// production always takes the `block_in_place` path. (`rmp_connection`'s
/// `spawn_isolated_test_server` builds a multi-threaded runtime too, but it is a `#[cfg(test)]`
/// helper, not a server runtime.)
///
/// One panicking case this gate cannot detect: inside a `LocalSet` on a multi-threaded runtime,
/// `runtime_flavor()` still reports `MultiThread`, yet `block_in_place` panics anyway because it
/// is not allowed within a `LocalSet`. No `LocalSet` exists anywhere in this codebase today, so
/// this is a latent gap rather than a live one -- but adding one near an AOF write path would
/// slip past this check.
fn run_blocking<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let on_multi_thread = tokio::runtime::Handle::try_current().is_ok_and(|handle| {
        handle.runtime_flavor() != tokio::runtime::RuntimeFlavor::CurrentThread
    });
    if on_multi_thread {
        tokio::task::block_in_place(f)
    } else {
        f()
    }
}

pub struct AofWriter {
    /// Bounded at `AOF_QUEUE_CAPACITY`; see that constant for why.
    tx: mpsc::SyncSender<AofMsg>,
    policy: FsyncPolicy,
    /// One ordering guard per engine shard, held by `dispatcher::dispatch_and_log` across
    /// "mutate the engine, then log it" for write commands, so concurrent writers' appends
    /// always land in the AOF in the same relative order their mutations committed in. The same
    /// guard also covers the replica fan-out that follows the append: broadcasting after the
    /// guard is dropped would let two writers to the same key broadcast out of commit order,
    /// permanently reordering that key on every follower even though the AOF itself stayed
    /// correct.
    ///
    /// Per shard rather than one global guard because replay only needs ordering *per key*:
    /// two commands touching disjoint keys may be appended in either order and replay
    /// identically. A single global guard serialised every write in the process, which made
    /// the engine's 16 independently-locked shards worthless for writes -- pipelined `SET` ran
    /// ~3x slower than with no guard at all. See
    /// ../../docs/superpowers/specs/2026-09-08-per-shard-aof-ordering-spec.md, and
    /// ../../docs/superpowers/specs/2026-08-30-tech-debt-cleanup-spec.md Item 2 for the
    /// original ordering requirement.
    ///
    /// Indexed by `engine::Engine::shard_index`; never acquired except through `lock_shards`
    /// or `lock_all_shards`, which impose the ascending-index order that prevents deadlock.
    order: Vec<Mutex<()>>,
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
        // The writer thread processes `AofMsg`s strictly one at a time, so this plain `u64` —
        // not an atomic — is the whole cost of tracking the running offset for the trace log
        // below. No cross-thread synchronization, and `append_encoded`/`fsync` on the calling
        // side never touch it.
        let mut offset = file.metadata().map(|m| m.len()).unwrap_or(0);
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
                            let len = bytes.len() as u64;
                            match writer.write_all(&bytes) {
                                Ok(()) => {
                                    tracing::trace!(offset, bytes = len, "aof append");
                                    offset += len;
                                }
                                Err(e) => tracing::error!(error = %e, "aof append failed"),
                            }
                        }
                        // The acked variants hand the real I/O result back to the waiting
                        // caller instead of printing it, so a full disk surfaces where the
                        // write was requested. A failed send just means the caller gave up
                        // waiting; dropping the result is the only sensible response.
                        AofMsg::AppendAndFsync(bytes, ack) => {
                            let len = bytes.len() as u64;
                            let result = writer
                                .write_all(&bytes)
                                .and_then(|_| writer.flush())
                                .and_then(|_| writer.get_ref().sync_data());
                            if result.is_ok() {
                                tracing::trace!(offset, bytes = len, "aof append");
                                offset += len;
                                tracing::debug!(offset, "aof fsync");
                            }
                            let _ = ack.send(result);
                        }
                        AofMsg::Flush(ack) => {
                            let result = writer.flush().and_then(|_| writer.get_ref().sync_data());
                            if result.is_ok() {
                                tracing::debug!(offset, "aof fsync");
                            }
                            let _ = ack.send(result);
                        }
                        AofMsg::CheckIntact(path, ack) => {
                            let result = writer.get_ref().metadata().map(|fd_meta| {
                                std::fs::metadata(&path)
                                    .map(|path_meta| same_file(&fd_meta, &path_meta))
                                    .unwrap_or(false)
                            });
                            let _ = ack.send(result);
                        }
                        AofMsg::Rotate(new_path, ack) => {
                            let result = writer.flush().and_then(|_| writer.get_ref().sync_data());
                            let result = result.and_then(|_| {
                                OpenOptions::new().create(true).append(true).open(&new_path)
                            });
                            let result = match result {
                                Ok(file) => {
                                    // Not always 0: `rotate_to`'s own doc comment notes the new
                                    // path may already have content from an interrupted previous
                                    // rewrite, in which case appends resume after it, not at 0.
                                    offset = file.metadata().map(|m| m.len()).unwrap_or(0);
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
            order: (0..engine::SHARD_COUNT).map(|_| Mutex::new(())).collect(),
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
            run_blocking(|| ack_rx.recv().map_err(writer_gone))?
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
        run_blocking(|| ack_rx.recv().map_err(writer_gone))?
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

    /// Whether the file this writer is appending to still has a directory entry at its
    /// configured path. POSIX lets writes and fsyncs against a deleted (or otherwise
    /// disconnected) file keep succeeding for as long as this process holds the fd open --
    /// nothing about `append`/`fsync` fails, and every client keeps getting `OK`. The bytes
    /// only vanish, silently and all at once, when the fd finally closes (a restart) and the
    /// kernel frees the orphaned inode with them. This is the check `periodic_fsync_loop` polls
    /// once a second specifically to turn that silent failure into a loud one before it costs
    /// hours of writes.
    pub fn is_file_intact(&self) -> std::io::Result<bool> {
        let path = self.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        self.send(AofMsg::CheckIntact(path, ack_tx))?;
        run_blocking(|| ack_rx.recv().map_err(writer_gone))?
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
        run_blocking(|| ack_rx.recv().map_err(writer_gone))??;
        *self.path.lock().unwrap_or_else(|e| e.into_inner()) = new_path.to_path_buf();
        Ok(())
    }

    /// Acquires the ordering guards for `shards`, around "mutate, then log" for a write command
    /// -- see the `order` field's doc comment above.
    ///
    /// Sorts and deduplicates before acquiring, so guards are always taken in ascending shard
    /// index. That single rule is what makes deadlock impossible between two multi-key commands
    /// whose key sets overlap in different orders, and it is why callers must come through here
    /// rather than indexing `order` themselves.
    ///
    /// At most one acquisition from this writer may be held live at a time. `let a =
    /// aof.lock_shards(&[9]); let b = aof.lock_shards(&[3]);` compiles today and would deadlock
    /// against a concurrent caller that locks the same two shards in the reverse order --
    /// ascending order only prevents deadlock within a single acquisition's own shard list.
    #[must_use = "the returned guards must be bound and held across the whole mutate-then-log \
                  section; dropping them immediately releases the locks and loses the AOF \
                  ordering guarantee entirely"]
    pub fn lock_shards(&self, shards: &[usize]) -> Vec<std::sync::MutexGuard<'_, ()>> {
        let mut idx: Vec<usize> = shards.to_vec();
        idx.sort_unstable();
        idx.dedup();
        idx.into_iter()
            .map(|i| {
                // Recover from poison rather than propagate it: these mutexes are held across
                // arbitrary command dispatch (dispatcher::dispatch_and_log), so a panicking
                // command handler must not turn into a permanent write outage. The guarded data
                // is `()` -- there is no invariant a panicking holder could have left broken.
                self.order[i].lock().unwrap_or_else(|e| e.into_inner())
            })
            .collect()
    }

    /// Every ordering guard, ascending. Required by the paths that need a consistent view of the
    /// *whole* keyspace rather than of particular keys: `SAVE`, `BGREWRITEAOF`, `serve_replica`'s
    /// snapshot-then-register, and the follower apply loop (which must exclude a concurrent
    /// `SAVE` from observing a multi-key command half-applied across shards). Those are all rare
    /// next to a snapshot walk, so taking 16 locks instead of 1 costs nothing that matters.
    #[must_use = "the returned guards must be bound and held across the section they protect"]
    pub fn lock_all_shards(&self) -> Vec<std::sync::MutexGuard<'_, ()>> {
        self.lock_shards(&(0..self.order.len()).collect::<Vec<_>>())
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

    /// Enqueues `msg` for the writer thread, blocking only if the bounded queue is actually
    /// full. The try-first shape matters: `block_in_place` is not free (it hands the worker's
    /// core to another thread and steals it back), and every write command funnels through
    /// here, so paying that on each one to guard against a queue that has room would tax the
    /// hot path for nothing. A full queue is the rare case, and only there does the send
    /// genuinely block -- the intended backpressure, now taken without freezing a worker.
    fn send(&self, msg: AofMsg) -> std::io::Result<()> {
        match self.tx.try_send(msg) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(writer_gone_err()),
            Err(mpsc::TrySendError::Full(msg)) => {
                run_blocking(|| self.tx.send(msg).map_err(|_| writer_gone_err()))
            }
        }
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

/// Summary of one `replay_with_stats` call: how many commands were replayed, how many bytes of
/// the file that consumed, and how long it took. `recover` logs this as the operator-facing line
/// that answers "why did startup take this long" — see the design spec's AOF event catalogue row
/// for the recovery replay summary.
pub struct ReplayStats {
    pub commands: u64,
    pub bytes: u64,
    pub elapsed: std::time::Duration,
}

/// Replays every command in the AOF at `path` against `engine`, via the plain (non-logging)
/// `dispatcher::dispatch` — never `dispatch_and_log`, which would re-append what's being
/// replayed. A missing file is a no-op (nothing to recover on first run). `start_at` is
/// clamped to the file's actual length rather than trusted blindly, so a caller passing a
/// stale or wrong offset degrades to "replay nothing" instead of panicking on an
/// out-of-range slice; `aof::recover` (below) is what decides *whether* a mismatched offset
/// should reach this function at all. A corrupt or incomplete final frame stops replay at the
/// last fully-decoded frame and truncates the file on disk to that exact byte offset.
///
/// Kept as a thin wrapper over `replay_with_stats` so its own signature and behavior never
/// change — see that function for the counting logic `recover`'s log line needs.
pub fn replay(path: &Path, engine: &engine::Engine, start_at: u64) -> std::io::Result<()> {
    replay_with_stats(path, engine, start_at).map(|_| ())
}

/// Does the same work as `replay`, additionally returning how many commands and bytes were
/// replayed and how long it took. Split out from `replay` rather than changing `replay` itself,
/// so `replay`'s eleven existing test call sites in this module need no changes at all.
pub fn replay_with_stats(
    path: &Path,
    engine: &engine::Engine,
    start_at: u64,
) -> std::io::Result<ReplayStats> {
    use tokio_util::codec::Decoder;

    let started = std::time::Instant::now();

    let raw = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReplayStats {
                commands: 0,
                bytes: 0,
                elapsed: started.elapsed(),
            });
        }
        Err(e) => {
            // Not a missing file (handled above) but an unreadable one, which aborts startup --
            // `recover`'s only caller is `main`, which propagates this into a process exit. Logged
            // here rather than at the two `recover` call sites so it is reported exactly once.
            tracing::error!(
                aof_path = %path.display(),
                error = %e,
                "aof recovery failed: aof file unreadable"
            );
            return Err(e);
        }
    };

    let start = (start_at as usize).min(raw.len());
    let mut buf = bytes::BytesMut::from(&raw[start..]);
    let mut codec = protocol::codec::RespCodec::default();
    let mut valid_len = start;
    let mut commands: u64 = 0;
    // Which of the two tolerated tail conditions ended the loop, for the warning below. Both are
    // handled identically -- this only names the cause. The decode error itself is deliberately
    // not carried: `RespCodec::decode` already logs it (see `codec.rs`'s `protocol error decoding
    // frame`), so repeating it here would only duplicate a line an operator already has.
    let mut tail_reason = "incomplete";
    loop {
        let before = buf.len();
        match codec.decode(&mut buf) {
            Ok(Some(frame)) => {
                valid_len += before - buf.len();
                commands += 1;
                let mut protocol = protocol::codec::Protocol::default();
                crate::dispatcher::dispatch(engine, frame, &mut protocol, 0);
            }
            // Incomplete or corrupt tail — stop here, keep what decoded.
            Ok(None) => break,
            Err(_) => {
                tail_reason = "corrupt";
                break;
            }
        }
    }

    if valid_len < raw.len() {
        // `warn`, not `error`: a half-written trailing record is what a crash mid-append leaves,
        // and this function deliberately recovers from it by keeping everything before it. The
        // offset and the two lengths are the whole payload -- the discarded bytes are client data
        // and never reach the log.
        //
        // The message is present-tense on purpose: this fires *before* the open/`set_len` below,
        // so a past-tense "truncated" would assert an outcome that has not happened yet and may
        // still fail. When it does fail, the two `error!`s below say so.
        tracing::warn!(
            aof_path = %path.display(),
            offset = valid_len,
            aof_len = raw.len(),
            reason = tail_reason,
            "aof tail discarded; truncating the file to the last complete record"
        );
        let file = OpenOptions::new().write(true).open(path).inspect_err(|e| {
            tracing::error!(
                aof_path = %path.display(),
                error = %e,
                "aof recovery failed: cannot open the aof to truncate its discarded tail"
            );
        })?;
        file.set_len(valid_len as u64).inspect_err(|e| {
            tracing::error!(
                aof_path = %path.display(),
                offset = valid_len,
                error = %e,
                "aof recovery failed: cannot truncate the aof to its last complete record"
            );
        })?;
    }
    Ok(ReplayStats {
        commands,
        bytes: (valid_len - start) as u64,
        elapsed: started.elapsed(),
    })
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
/// Whether `fd_meta` (from the writer thread's open file descriptor) and `path_meta` (a fresh
/// `stat` of the configured path) name the same on-disk file. Inode numbers are only unique
/// within a device, so both must match -- comparing `ino` alone would false-positive if the AOF
/// were deleted and a new, unrelated file happened to land on a recycled inode on a *different*
/// filesystem mount. Unix-only for the same reason as `fsync_parent_dir`: Windows has no stable
/// inode-equivalent exposed through `std::fs::Metadata`, and this project's release matrix builds
/// there, so the check degrades to "trust the fd" rather than failing every call.
#[cfg(unix)]
fn same_file(fd_meta: &std::fs::Metadata, path_meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    fd_meta.dev() == path_meta.dev() && fd_meta.ino() == path_meta.ino()
}

#[cfg(not(unix))]
fn same_file(_fd_meta: &std::fs::Metadata, _path_meta: &std::fs::Metadata) -> bool {
    true
}

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
    // Every failure below is logged at `error` and then propagated unchanged: `main` is
    // `recover`'s only production caller and turns an `Err` here into a process exit, so without
    // a log line the operator sees a dead server and nothing naming the file that killed it.
    // The tolerated conditions -- an unreadable snapshot, an offset past the AOF's end, a
    // truncated tail -- keep their `warn`, because startup continues through all three.
    let gen = read_generation(snapshot_path).inspect_err(|e| {
        tracing::error!(
            snapshot_path = %snapshot_path.display(),
            error = %e,
            "aof recovery failed: generation manifest unreadable"
        );
    })?;
    let aof_path = &generation_path(aof_path, gen);
    let snapshot_path = &generation_path(snapshot_path, gen);

    let engine = engine::Engine::new();
    let start_at = match std::fs::read(snapshot_path) {
        Ok(bytes) => {
            let load_started = std::time::Instant::now();
            match engine.load_snapshot(&bytes) {
                Ok(offset) => {
                    // `snapshot_path`, matching the four other events in this same function and
                    // the config key of the same name. It used to be `path` alone, which meant
                    // `grep snapshot_path` missed the single most important snapshot event on
                    // the recovery path -- the one saying the snapshot was actually loaded.
                    tracing::info!(
                        snapshot_path = %snapshot_path.display(),
                        bytes = bytes.len(),
                        elapsed_us = load_started.elapsed().as_micros() as u64,
                        "snapshot loaded"
                    );
                    // A missing AOF is distinct from a zero-length one: the former means the
                    // snapshot alone is the recovered state (per the spec's hybrid-recovery
                    // decision), the latter means the offset genuinely overshoots and the
                    // snapshot/AOF pair has diverged.
                    let aof_len = match std::fs::metadata(aof_path) {
                        Ok(m) => Some(m.len()),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                        Err(e) => {
                            tracing::error!(
                                aof_path = %aof_path.display(),
                                error = %e,
                                "aof recovery failed: aof file metadata unreadable"
                            );
                            return Err(e);
                        }
                    };
                    match aof_len {
                        None => {
                            // No commands/bytes/elapsed_us fields here on purpose: those would
                            // look identical to the "AOF present but zero-length" case below
                            // (which legitimately replays an empty file and reports
                            // commands=0 bytes=0), and this path never touches the AOF at all --
                            // the snapshot alone is the entire recovered state. Same message
                            // prefix and level as that summary so one grep for "aof recovery
                            // replay complete" still surfaces every recovery outcome.
                            tracing::info!(
                                aof_path = %aof_path.display(),
                                "aof recovery replay complete (no aof file; snapshot is the entire recovered state)"
                            );
                            return Ok(engine);
                        }
                        Some(len) if offset > len => {
                            tracing::warn!(
                                snapshot_path = %snapshot_path.display(),
                                offset,
                                aof_len = len,
                                "snapshot offset past end of AOF; discarding snapshot and replaying full AOF"
                            );
                            let fresh = engine::Engine::new();
                            let stats = replay_with_stats(aof_path, &fresh, 0)?;
                            tracing::info!(
                                commands = stats.commands,
                                bytes = stats.bytes,
                                elapsed_us = stats.elapsed.as_micros() as u64,
                                "aof recovery replay complete"
                            );
                            return Ok(fresh);
                        }
                        Some(_) => offset,
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        snapshot_path = %snapshot_path.display(),
                        error = %e,
                        "snapshot unreadable; falling back to full AOF replay"
                    );
                    0
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        // Distinct from the `snapshot unreadable` warning above: that one is a snapshot whose
        // *bytes* would not decode, which recovery survives by replaying the whole AOF instead.
        // This is a snapshot file that could not be read at all, which it does not survive.
        Err(e) => {
            tracing::error!(
                snapshot_path = %snapshot_path.display(),
                error = %e,
                "aof recovery failed: snapshot file unreadable"
            );
            return Err(e);
        }
    };
    let stats = replay_with_stats(aof_path, &engine, start_at)?;
    tracing::info!(
        commands = stats.commands,
        bytes = stats.bytes,
        elapsed_us = stats.elapsed.as_micros() as u64,
        "aof recovery replay complete"
    );
    Ok(engine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use engine::{Engine, Value};
    use protocol::Frame;
    use std::io::Read;

    /// How long the blocking task waits for the other task's ping before giving up. Only ever
    /// waited out in full when the fix is absent, so it trades a slow failure for a fast pass.
    const STARVATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn run_blocking_does_not_starve_other_tasks_while_it_blocks() {
        // A single-worker-thread runtime, deliberately: with exactly one worker thread, a
        // blocking call that doesn't free that thread makes every other task provably unable
        // to run until it returns -- no timing luck needed to observe the difference. Note the
        // blocking call has to happen inside a *spawned* task: this test body runs on
        // `block_on`'s own thread, which is not a worker thread, so blocking here would starve
        // nothing and prove nothing.
        let (ping_tx, ping_rx) = mpsc::channel::<()>();
        let (blocking_started_tx, blocking_started_rx) = mpsc::channel::<()>();

        // Occupies the one worker thread, then blocks until the other task pings it.
        let blocker = tokio::spawn(async move {
            blocking_started_tx.send(()).unwrap();
            run_blocking(|| ping_rx.recv_timeout(STARVATION_TIMEOUT).is_ok())
        });

        // Blocking this thread is safe -- it is not a worker thread. Waiting here first means
        // the other task is only spawned once the worker thread is genuinely taken.
        blocking_started_rx.recv().unwrap();

        let other_task = tokio::spawn(async move {
            ping_tx.send(()).ok();
        });

        // The ping can only arrive if the other task ran *during* the blocking call, which can
        // only happen if run_blocking handed the runtime a replacement worker thread.
        assert!(
            blocker.await.unwrap(),
            "other_task never ran while run_blocking was blocking the only worker thread; \
             run_blocking starved the runtime instead of freeing it via block_in_place"
        );
        other_task.await.unwrap();
    }

    #[test]
    fn run_blocking_falls_back_to_a_direct_call_with_no_runtime() {
        // `block_in_place` panics outside a multi-threaded runtime, and every other test in
        // this module calls the AOF's blocking methods with no runtime at all.
        assert_eq!(run_blocking(|| 7), 7);
    }

    #[tokio::test]
    async fn run_blocking_falls_back_to_a_direct_call_on_a_current_thread_runtime() {
        // `#[tokio::test]` is current-thread by default, and several such tests elsewhere in
        // the crate drive real AOF writes -- `block_in_place` would panic for all of them.
        assert_eq!(run_blocking(|| 7), 7);
    }

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
    fn replay_with_stats_counts_every_command_and_byte_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let raw: &[u8] =
            b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n";
        write_raw(&path, raw);
        let engine = Engine::new();
        let stats = replay_with_stats(&path, &engine, 0).unwrap();
        assert_eq!(stats.commands, 2);
        assert_eq!(stats.bytes, raw.len() as u64);
    }

    #[test]
    fn replay_with_stats_excludes_a_corrupt_tail_from_both_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let valid = b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n";
        write_raw(&path, valid);
        write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$3\r\ngar"); // truncated mid-bulk-body
        let engine = Engine::new();
        let stats = replay_with_stats(&path, &engine, 0).unwrap();
        assert_eq!(stats.commands, 1);
        assert_eq!(stats.bytes, valid.len() as u64);
    }

    #[test]
    fn replay_with_stats_on_a_missing_file_reports_zero_commands_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.aof");
        let engine = Engine::new();
        let stats = replay_with_stats(&path, &engine, 0).unwrap();
        assert_eq!(stats.commands, 0);
        assert_eq!(stats.bytes, 0);
    }

    #[test]
    fn replay_still_reports_no_stats_and_behaves_exactly_as_before() {
        // `replay` is now a thin wrapper over `replay_with_stats`; this test pins its public
        // signature and behavior so a future change to `replay_with_stats` cannot silently
        // change what `replay`'s many existing callers observe.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        write_raw(&path, b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n");
        let engine = Engine::new();
        let result: std::io::Result<()> = replay(&path, &engine, 0);
        assert!(result.is_ok());
        assert_eq!(
            engine.get(b"a"),
            Some(Value::String(bytes::Bytes::from_static(b"1")))
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

    /// The guard for one shard must still serialise everything that touches that shard --
    /// per-shard guards are only sound if each individual guard is as strict as the single
    /// global one used to be.
    #[test]
    fn one_shards_guard_still_serializes_concurrent_holders_of_that_shard() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("same-shard.aof");
        let writer = std::sync::Arc::new(AofWriter::open(&path, FsyncPolicy::Never).unwrap());
        let log: std::sync::Arc<Mutex<Vec<(usize, bool)>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for id in 0..4 {
            let writer = std::sync::Arc::clone(&writer);
            let log = std::sync::Arc::clone(&log);
            handles.push(std::thread::spawn(move || {
                for _ in 0..50 {
                    let _guard = writer.lock_shards(&[7]); // all four contend for shard 7
                    log.lock().unwrap().push((id, true));
                    std::thread::yield_now();
                    log.lock().unwrap().push((id, false));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let log = log.lock().unwrap();
        let mut i = 0;
        while i < log.len() {
            let (id, entering) = log[i];
            assert!(entering, "expected an entry at position {i}");
            assert_eq!(
                log[i + 1],
                (id, false),
                "another thread interleaved into shard 7"
            );
            i += 2;
        }
    }

    /// The point of the whole change: writes to keys in *different* shards must not block each
    /// other. Without this test a future refactor could quietly reintroduce a global guard and
    /// cost ~3x on pipelined writes while every correctness test still passed.
    #[test]
    fn guards_for_different_shards_do_not_block_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disjoint.aof");
        let writer = std::sync::Arc::new(AofWriter::open(&path, FsyncPolicy::Never).unwrap());

        let held = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writer2 = std::sync::Arc::clone(&writer);
        let held2 = std::sync::Arc::clone(&held);
        let other = std::thread::spawn(move || {
            let _guard = writer2.lock_shards(&[1]);
            // Rendezvous *while still holding* shard 1's guard. If the main thread could not
            // take shard 2 concurrently, neither side would reach the barrier and the test
            // would hang rather than fail -- which is why it runs under a joined thread.
            held2.wait();
        });

        let _guard = writer.lock_shards(&[2]);
        held.wait(); // reached only because shard 1 and shard 2 are independent
        other.join().unwrap();
    }

    /// Two multi-key commands whose key sets overlap in opposite orders. `lock_shards` sorts
    /// before acquiring, so this cannot deadlock; the test is what proves the rule is actually
    /// applied rather than merely documented.
    #[test]
    fn overlapping_multi_key_acquisitions_do_not_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deadlock.aof");
        let writer = std::sync::Arc::new(AofWriter::open(&path, FsyncPolicy::Never).unwrap());

        let mut handles = Vec::new();
        for order in [vec![3usize, 9], vec![9usize, 3]] {
            let writer = std::sync::Arc::clone(&writer);
            handles.push(std::thread::spawn(move || {
                for _ in 0..200 {
                    let _guards = writer.lock_shards(&order);
                    std::thread::yield_now();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }

    /// Job 1 of the ordering guard, end to end: the AOF's append order must match the order
    /// mutations committed in, so a replay reproduces the value that was actually committed.
    /// One key means one shard, so every writer here contends -- that is the point.
    ///
    /// Uses APPEND, not SET, on purpose. SET replaces the value, so only a reordering that
    /// lands on the very last write of a round is visible -- every earlier reordering gets
    /// silently overwritten by a later, correctly-ordered write and leaves no trace. APPEND
    /// accumulates instead, so every write's position in the final string is observable: any
    /// reordering, anywhere in the round, changes the accumulated result. That turns roughly one
    /// observable event per round into roughly one per write, which is what makes this test
    /// actually able to catch the defect it guards against.
    #[test]
    fn concurrent_writes_to_one_key_replay_to_the_committed_value() {
        use std::sync::Arc;

        // A reordering only shows up in this assertion if it lands on the very last write of a
        // burst -- roughly one chance per burst, no matter how many writes the burst contains.
        // Many short rounds, each with their own log, turn the same total work into one
        // independent chance per round instead of one chance per run.
        const ROUNDS: usize = 200;
        const WRITERS: usize = 4;
        const WRITES_PER_WRITER: usize = 25;

        for round in 0..ROUNDS {
            let dir = tempfile::tempdir().unwrap();
            let aof_path = dir.path().join("ordering.aof");
            let aof = Arc::new(AofWriter::open(&aof_path, FsyncPolicy::Never).unwrap());
            let engine = Arc::new(engine::Engine::new());
            let replication = Arc::new(crate::replication::ReplicationHandle::new(
                Arc::clone(&engine),
                dir.path().join("ordering.snapshot"),
            ));

            let mut writers = Vec::new();
            for w in 0..WRITERS {
                let engine = Arc::clone(&engine);
                let aof = Arc::clone(&aof);
                let replication = Arc::clone(&replication);
                writers.push(std::thread::spawn(move || {
                    for i in 0..WRITES_PER_WRITER {
                        let value = format!("w{w}-{i}.");
                        let frame = protocol::Frame::Array(vec![
                            protocol::Frame::Bulk(bytes::Bytes::from_static(b"APPEND")),
                            protocol::Frame::Bulk(bytes::Bytes::from_static(b"hot")),
                            protocol::Frame::Bulk(bytes::Bytes::from(value)),
                        ]);
                        crate::dispatcher::dispatch_and_log(
                            &engine,
                            &aof,
                            &replication,
                            frame,
                            &crate::dispatcher::Session::new(),
                            1,
                        );
                    }
                }));
            }
            for w in writers {
                w.join().unwrap();
            }
            aof.fsync().unwrap();

            // Replaying the log must land on whatever the engine actually holds. Because APPEND
            // accumulates, any AOF line that ever overtook the mutation it logged changes the
            // accumulated string's order, so replay and the engine would diverge.
            let replayed = recover(&aof_path, &dir.path().join("absent.snapshot")).unwrap();
            let committed = engine.get(b"hot");
            assert!(
                committed.is_some(),
                "no writes landed in round {round} -- dispatch_and_log may have silently \
                 stopped mutating"
            );
            assert_eq!(
                replayed.get(b"hot"),
                committed,
                "AOF replay diverged from committed state in round {round}"
            );
        }
    }

    /// Job 2: `SAVE` must produce a point-in-time cut of (offset, keyspace). If the offset is read
    /// at one instant and the walk happens at another, recovery replays commands the snapshot
    /// already contains -- which double-counts every non-idempotent command. INCR is used
    /// deliberately: with SET this bug is invisible.
    #[test]
    fn a_save_racing_writes_produces_a_replayable_point_in_time_cut() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("cut.aof");
        let snapshot_path = dir.path().join("cut.snapshot");
        let aof = Arc::new(AofWriter::open(&aof_path, FsyncPolicy::Never).unwrap());
        let engine = Arc::new(engine::Engine::new());
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            snapshot_path.clone(),
        ));

        let stop = Arc::new(AtomicBool::new(false));
        let mut writers = Vec::new();
        for w in 0..4 {
            let engine = Arc::clone(&engine);
            let aof = Arc::clone(&aof);
            let replication = Arc::clone(&replication);
            let stop = Arc::clone(&stop);
            writers.push(std::thread::spawn(move || {
                let key = format!("counter{w}");
                let mut issued = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let frame = protocol::Frame::Array(vec![
                        protocol::Frame::Bulk(bytes::Bytes::from_static(b"INCR")),
                        protocol::Frame::Bulk(bytes::Bytes::from(key.clone())),
                    ]);
                    crate::dispatcher::dispatch_and_log(
                        &engine,
                        &aof,
                        &replication,
                        frame,
                        &crate::dispatcher::Session::new(),
                        1,
                    );
                    issued += 1;
                }
                issued
            }));
        }

        // Fire SAVEs while the counters are climbing, so at least one cut is taken mid-flight.
        for _ in 0..20 {
            crate::dispatcher::dispatch_and_log(
                &engine,
                &aof,
                &replication,
                protocol::Frame::Array(vec![protocol::Frame::Bulk(bytes::Bytes::from_static(
                    b"SAVE",
                ))]),
                &crate::dispatcher::Session::new(),
                1,
            );
        }

        stop.store(true, Ordering::Relaxed);
        let mut total_issued = 0u64;
        for w in writers {
            total_issued += w.join().unwrap();
        }
        assert!(
            total_issued > 0,
            "no writer issued any INCR -- dispatch_and_log may have silently stopped mutating"
        );
        aof.fsync().unwrap();

        // The AOF alone is the reference: every INCR, replayed once. Snapshot-plus-tail must land
        // on exactly the same counters. A non-atomic cut double-counts and these diverge.
        let from_aof_only = recover(&aof_path, &dir.path().join("absent.snapshot")).unwrap();
        let from_snapshot_and_tail = recover(&aof_path, &snapshot_path).unwrap();
        for w in 0..4 {
            let key = format!("counter{w}");
            assert_eq!(
                from_snapshot_and_tail.get(key.as_bytes()),
                from_aof_only.get(key.as_bytes()),
                "snapshot+tail diverged from full replay at {key}"
            );
        }
    }

    /// Job 3: `Store::snapshot_entries` walks shard by shard, so a multi-key write spanning
    /// shards must be atomic with respect to that walk. Both halves of each MSET carry the same
    /// generation number, so a half-applied snapshot is directly observable. This is a distinct
    /// failure mode from append-order inversion (covered elsewhere) or offset-cut double
    /// counting: here a single write is caught mid-flight, split across two shards, by the
    /// snapshot walk itself.
    ///
    /// `handle_save` takes `lock_all_shards()` -- every one of the 16 order guards -- before it
    /// reads a single shard. That means a writer holding even one of those guards for its whole
    /// mutate-then-log section already blocks `SAVE` at that index for the entire operation, both
    /// keys included. So a writer that locks only a *subset* of its keys' shards (say, just the
    /// first key's) is not observable here: `SAVE` still stalls on whichever guard the writer
    /// does hold, and by the time it is released both keys are already written. The only mutation
    /// this test can actually catch is a writer that holds *no* guard for a multi-key write at
    /// all -- confirmed by hand against `aof.lock_shards(&[])` at dispatcher.rs, which fails
    /// within the first save. Locking all of a command's keys' shards is what write-write
    /// ordering (a separate job) needs; snapshot atomicity here only needs one of them held, so
    /// this test cannot distinguish "all shards locked" from "some shards locked" -- only from
    /// "no shards locked."
    #[test]
    fn an_mset_spanning_shards_is_never_snapshotted_half_applied() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("mset.aof");
        let snapshot_path = dir.path().join("mset.snapshot");
        let aof = Arc::new(AofWriter::open(&aof_path, FsyncPolicy::Never).unwrap());
        let engine = Arc::new(engine::Engine::new());
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            snapshot_path.clone(),
        ));

        // Pick a pair that actually straddles two shards. Asserting it rather than assuming it
        // keeps this test meaningful if the hash or the shard count ever changes.
        let (left, right) = (0..1000)
            .map(|i| (format!("pair-a-{i}"), format!("pair-b-{i}")))
            .find(|(a, b)| engine.shard_index(a.as_bytes()) != engine.shard_index(b.as_bytes()))
            .expect("no key pair landed on different shards");

        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let engine = Arc::clone(&engine);
            let aof = Arc::clone(&aof);
            let replication = Arc::clone(&replication);
            let stop = Arc::clone(&stop);
            let (left, right) = (left.clone(), right.clone());
            std::thread::spawn(move || {
                let mut generation = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    generation += 1;
                    let value = bytes::Bytes::from(generation.to_string());
                    let frame = protocol::Frame::Array(vec![
                        protocol::Frame::Bulk(bytes::Bytes::from_static(b"MSET")),
                        protocol::Frame::Bulk(bytes::Bytes::from(left.clone())),
                        protocol::Frame::Bulk(value.clone()),
                        protocol::Frame::Bulk(bytes::Bytes::from(right.clone())),
                        protocol::Frame::Bulk(value),
                    ]);
                    crate::dispatcher::dispatch_and_log(
                        &engine,
                        &aof,
                        &replication,
                        frame,
                        &crate::dispatcher::Session::new(),
                        1,
                    );
                }
            })
        };

        for i in 0..50 {
            let reply = crate::dispatcher::dispatch_and_log(
                &engine,
                &aof,
                &replication,
                protocol::Frame::Array(vec![protocol::Frame::Bulk(bytes::Bytes::from_static(
                    b"SAVE",
                ))]),
                &crate::dispatcher::Session::new(),
                1,
            );
            assert_eq!(
                reply,
                protocol::Frame::Simple("OK".into()),
                "SAVE #{i} failed instead of writing a snapshot: {reply:?}"
            );

            // Read back the snapshot SAVE just wrote and check the pair agrees. Loading it into a
            // fresh engine deliberately skips the AOF tail: the snapshot alone must be coherent.
            let gen = read_generation(&snapshot_path).unwrap();
            let written = generation_path(&snapshot_path, gen);
            if let Ok(raw) = std::fs::read(&written) {
                let restored = engine::Engine::new();
                restored.load_snapshot(&raw).unwrap();
                let (l, r) = (
                    restored.get(left.as_bytes()),
                    restored.get(right.as_bytes()),
                );
                // Both absent is fine -- the snapshot predates the first MSET.
                if l.is_some() || r.is_some() {
                    assert_eq!(l, r, "snapshot caught an MSET half-applied across shards");
                }
            }
        }

        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }

    /// Duplicate indices must not self-deadlock: `MSET k1 v1 k1 v2` and any command whose keys
    /// collide onto one shard reach `lock_shards` with repeats.
    #[test]
    fn repeated_shard_indices_are_deduplicated_rather_than_locked_twice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedup.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        let guards = writer.lock_shards(&[5, 5, 5, 2, 2]);
        assert_eq!(guards.len(), 2, "expected one guard per distinct shard");
    }

    /// The common case: nothing has touched the file since `open`, so the fd and the path still
    /// name the same inode.
    #[test]
    fn is_file_intact_is_true_for_a_freshly_opened_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        assert!(writer.is_file_intact().unwrap());
    }

    /// The incident this guards against: something deletes the AOF out from under a running
    /// process. POSIX keeps the fd fully writable -- `append`/`fsync` keep succeeding -- but the
    /// path no longer resolves to that inode, which is exactly the mismatch this method exists
    /// to catch before the process restarts and silently discards everything written since.
    #[test]
    fn is_file_intact_is_false_once_the_path_is_unlinked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        writer
            .append(protocol::Frame::Bulk(bytes::Bytes::from_static(b"x")))
            .unwrap();
        writer.fsync().unwrap();
        assert!(writer.is_file_intact().unwrap());

        std::fs::remove_file(&path).unwrap();
        assert!(!writer.is_file_intact().unwrap());

        // The orphaned fd keeps accepting writes -- that's the whole danger this test documents.
        writer
            .append(protocol::Frame::Bulk(bytes::Bytes::from_static(b"y")))
            .unwrap();
        writer.fsync().unwrap();
    }

    /// A path that now names a *different* file (e.g. something recreated it after a delete) is
    /// exactly as dangerous as a missing one and must also be reported as not intact.
    #[test]
    fn is_file_intact_is_false_once_the_path_is_replaced_by_a_different_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        assert!(writer.is_file_intact().unwrap());

        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"unrelated content").unwrap();
        assert!(!writer.is_file_intact().unwrap());
    }

    #[test]
    fn lock_all_shards_serializes_concurrent_holders() {
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
                    let _guard = writer.lock_all_shards();
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

    /// The matching assertion on this path's *log* line lives in
    /// `crates/server/tests/logging.rs`'s `snapshot_only_recovery_logs_a_distinguishable_summary_at_info`
    /// -- do not re-add it here. Every recovery test in this module calls `recover` with no
    /// subscriber installed, and `tracing` caches a callsite's `Interest` process-globally on
    /// first reach, so a capture assertion in this binary can be decided `never` before it runs.
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
