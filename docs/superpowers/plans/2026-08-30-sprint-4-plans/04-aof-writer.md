# AOF Writer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** an `AofWriter` that appends a command (as a RESP array of bulk strings) to a file, with a configurable `fsync` policy — the mechanism `05-aof-dispatch-wiring.md` calls after every successful write command.

**Architecture:** new `crates/server/src/aof.rs`. `AofWriter` wraps a `std::sync::Mutex<BufWriter<File>>` for interior mutability across concurrent connection tasks (per `../../specs/2026-08-30-sprint-4-spec.md`'s decision to keep the dispatcher synchronous rather than threading `tokio::fs` through it). Encoding reuses `protocol::codec::RespCodec`'s existing `Encoder<Frame>` impl — no new wire-format code.

**Tech Stack:** `std::fs`/`std::io` (already available), `protocol::{Frame, codec::RespCodec}` (existing crate, already a `server` dependency).

**Spec:** `../../specs/2026-08-30-sprint-4-spec.md` — the synchronous-`std::fs`-over-`tokio::fs` decision and the `WRITE_COMMANDS` allowlist are authoritative.

**Depends on:** nothing this sprint. `05-aof-dispatch-wiring.md` and `06-aof-replay-and-corrupt-recovery.md` both depend on this plan.

---

### Task 1: `FsyncPolicy`, `AofWriter`, and `WRITE_COMMANDS`

**Files:**
- Create: `crates/server/src/aof.rs`
- Modify: `crates/server/src/lib.rs`

**Interfaces:**
- Consumes: `protocol::{Frame, codec::RespCodec}` (existing), `tokio_util::codec::Encoder` (existing workspace dependency, already used by `RespCodec`).
- Produces: `pub enum FsyncPolicy { Always, EverySecond, Never }`; `pub struct AofWriter { .. }` with `pub fn open(path: &std::path::Path, policy: FsyncPolicy) -> std::io::Result<Self>`, `pub fn append(&self, frame: protocol::Frame) -> std::io::Result<()>`, `pub fn fsync(&self) -> std::io::Result<()>`; `pub const WRITE_COMMANDS: &[&str]`. `05-aof-dispatch-wiring.md` consumes all of these; `06-aof-replay-and-corrupt-recovery.md` consumes none of `AofWriter` (replay reads the file directly) but lives in this same module.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/aof.rs
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
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

    #[test]
    fn append_writes_the_frame_in_resp_wire_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        writer.append(frame(&[b"SET", b"k", b"v"])).unwrap();
        writer.fsync().unwrap();

        let mut contents = String::new();
        std::fs::File::open(&path).unwrap().read_to_string(&mut contents).unwrap();
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
        std::fs::File::open(&path).unwrap().read_to_string(&mut contents).unwrap();
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
        std::fs::File::open(&path).unwrap().read_to_string(&mut contents).unwrap();
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
        std::fs::File::open(&path).unwrap().read_to_string(&mut contents).unwrap();
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
}
```

- [ ] **Step 2: Add the `tempfile` dev-dependency**

```toml
# crates/server/Cargo.toml — add to [dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p rocket-mem aof::tests`
Expected: FAIL — the `aof` module doesn't exist yet

- [ ] **Step 4: Write the implementation**

```rust
// crates/server/src/aof.rs (above the tests module)
use protocol::Frame;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use tokio_util::codec::Encoder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsyncPolicy {
    Always,
    EverySecond,
    Never,
}

pub struct AofWriter {
    file: Mutex<BufWriter<File>>,
    policy: FsyncPolicy,
}

impl AofWriter {
    pub fn open(path: &Path, policy: FsyncPolicy) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(BufWriter::new(file)),
            policy,
        })
    }

    /// Encodes `frame` in RESP wire format and appends it to the file. Buffered — call
    /// `fsync` (or rely on the `Always`/`EverySecond` policy) to guarantee durability.
    pub fn append(&self, frame: Frame) -> std::io::Result<()> {
        let mut buf = bytes::BytesMut::new();
        // RespCodec's Encoder::Error is already std::io::Error, so `?` needs no mapping.
        protocol::codec::RespCodec::default().encode(frame, &mut buf)?;
        let mut guard = self.file.lock().unwrap();
        guard.write_all(&buf)?;
        if self.policy == FsyncPolicy::Always {
            guard.flush()?;
            guard.get_ref().sync_data()?;
        }
        Ok(())
    }

    /// Flushes the buffer and fsyncs the underlying file. Called directly by tests, and on a
    /// timer by `FsyncPolicy::EverySecond`'s periodic loop, which lives in
    /// `05-aof-dispatch-wiring.md`'s `connection.rs`. (`06-aof-replay-and-corrupt-recovery.md`
    /// does *not* call it — replay reads the file directly and runs before any `AofWriter`
    /// handle is opened.)
    pub fn fsync(&self) -> std::io::Result<()> {
        let mut guard = self.file.lock().unwrap();
        guard.flush()?;
        guard.get_ref().sync_data()
    }
}

/// Commands whose successful execution mutates the keyspace and must be replayed on
/// recovery. See ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md for why this is a
/// static allowlist rather than inferred from the reply shape, and why `SPOP` and the
/// `EXPIRE` family are rewritten (not logged verbatim) despite appearing here.
pub const WRITE_COMMANDS: &[&str] = &[
    "SET", "APPEND", "SETRANGE", "GETSET", "MSET", "MSETNX", "INCR", "DECR", "INCRBY", "DEL",
    "EXPIRE", "PEXPIRE", "EXPIREAT", "PEXPIREAT", "PERSIST", "RENAME", "RENAMENX", "HSET",
    "HDEL", "HINCRBY", "HSETNX", "RPUSH", "LPUSH", "RPOP", "LPOP", "LSET", "LTRIM", "LREM",
    "LINSERT", "SADD", "SREM", "SPOP", "SINTERSTORE", "SUNIONSTORE", "SDIFFSTORE", "ZADD",
    "ZREM", "ZINCRBY",
];
```

```rust
// crates/server/src/lib.rs
pub mod aof;
pub mod connection;
pub mod dispatcher;
pub use connection::serve;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rocket-mem aof::tests`
Expected: PASS, all tests including the 6 new ones

- [ ] **Step 6: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 7: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/aof.rs`, `crates/server/src/lib.rs`, `crates/server/Cargo.toml`, and
`Cargo.lock` — do not compose the commit message freeform. Suggested subject:
`feat(server): add AofWriter with configurable fsync policy`.
