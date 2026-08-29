# RESP Frame & Parser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** define the `Frame` type and a `RespCodec` implementing `tokio_util::codec::{Decoder, Encoder}` that converts between RESP2 wire bytes and `Frame` values, with no networking involved yet — pure buffer-in, buffer-out.

**Architecture:** `crates/protocol` gains a `frame.rs` (the `Frame` enum) and a `codec.rs` (`RespCodec`, wrapping a recursive-descent parser). Both modules are `pub` at every level per `../../specs/2026-08-29-sprint-2-spec.md`'s CI gotcha note, since nothing calls this crate yet.

**Tech Stack:** `bytes::{Bytes, BytesMut, Buf}`, `tokio_util::codec::{Decoder, Encoder}`.

**Spec:** `../../specs/2026-08-29-sprint-2-spec.md` — `Frame` type and RESP2 wire format table are authoritative; don't redefine them here.

## Global Constraints

- Every `\r\n`-terminated field must actually match `\r\n`, never a bare `\n` — a lone `\n` is treated as "incomplete, need more bytes," not a valid terminator.
- `crates/protocol`'s modules are declared `pub mod` from `lib.rs`, not private `mod` (CI gotcha, see spec).
- No dependency on `engine` or `common` in this crate — protocol stays engine-agnostic (this task doesn't need `common::EngineError` at all; that mapping happens in the dispatcher, item 03).

---

### Task 1: `Frame` enum

**Files:**
- Create: `crates/protocol/src/frame.rs`
- Modify: `crates/protocol/src/lib.rs`

**Interfaces:**
- Produces: `pub enum Frame { Simple(String), Error(String), Integer(i64), Bulk(Bytes), Null, Array(Vec<Frame>) }`, `Debug + Clone + PartialEq`.

- [ ] **Step 1: Write the failing test**

```rust
// crates/protocol/src/frame.rs
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn frames_of_the_same_variant_and_content_are_equal() {
        assert_eq!(Frame::Simple("OK".into()), Frame::Simple("OK".into()));
        assert_eq!(Frame::Bulk(Bytes::from_static(b"x")), Frame::Bulk(Bytes::from_static(b"x")));
    }

    #[test]
    fn frames_of_different_variants_are_not_equal() {
        assert_ne!(Frame::Simple("OK".into()), Frame::Error("OK".into()));
    }

    #[test]
    fn array_frame_holds_nested_frames() {
        let f = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"a")), Frame::Integer(1)]);
        assert_eq!(f, Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"a")), Frame::Integer(1)]));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p protocol frame::tests`
Expected: FAIL — `Frame` is not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/protocol/src/frame.rs (above the test module)
use bytes::Bytes;

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Bytes),
    Null,
    Array(Vec<Frame>),
}
```

```rust
// crates/protocol/src/lib.rs
pub mod frame;
pub use frame::Frame;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p protocol frame::tests`
Expected: PASS, 3/3

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/protocol/src/frame.rs` and `crates/protocol/src/lib.rs` — do not compose the
commit message freeform. Suggested subject: `feat(protocol): add Frame enum for RESP2 values`.

---

### Task 2: `RespCodec::encode` — `Frame` → RESP2 bytes

**Files:**
- Create: `crates/protocol/src/codec.rs`
- Modify: `crates/protocol/src/lib.rs`
- Modify: `crates/protocol/Cargo.toml`
- Modify: `Cargo.toml` (workspace root)

**Interfaces:**
- Consumes: `Frame` (Task 1).
- Produces: `pub struct RespCodec;` implementing `Encoder<Frame>` — later tasks (dispatcher, TCP listener) construct one `RespCodec` per connection and reuse it for both encode and decode.

- [ ] **Step 1: Add `tokio-util` to the workspace, then to `protocol`**

This is the first plan in the sprint to need an async/networking-adjacent dependency — add it at the workspace level now so every later plan (`04-tcp-listener.md` adds `tokio`/`futures-util` the same way) can reference it via `.workspace = true` without redeclaring the version.

```toml
# Cargo.toml — add to [workspace.dependencies]
tokio-util = { version = "0.7", features = ["codec"] }
```

```toml
# crates/protocol/Cargo.toml
[package]
name = "protocol"
edition.workspace = true
version.workspace = true

[dependencies]
bytes.workspace = true
tokio-util.workspace = true
```

- [ ] **Step 2: Write the failing test**

```rust
// crates/protocol/src/codec.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;
    use bytes::{Bytes, BytesMut};
    use tokio_util::codec::Encoder;

    #[test]
    fn encodes_simple_string() {
        let mut buf = BytesMut::new();
        RespCodec.encode(Frame::Simple("OK".into()), &mut buf).unwrap();
        assert_eq!(&buf[..], b"+OK\r\n");
    }

    #[test]
    fn encodes_error() {
        let mut buf = BytesMut::new();
        RespCodec.encode(Frame::Error("ERR bad".into()), &mut buf).unwrap();
        assert_eq!(&buf[..], b"-ERR bad\r\n");
    }

    #[test]
    fn encodes_integer() {
        let mut buf = BytesMut::new();
        RespCodec.encode(Frame::Integer(42), &mut buf).unwrap();
        assert_eq!(&buf[..], b":42\r\n");
    }

    #[test]
    fn encodes_bulk_string() {
        let mut buf = BytesMut::new();
        RespCodec.encode(Frame::Bulk(Bytes::from_static(b"hi")), &mut buf).unwrap();
        assert_eq!(&buf[..], b"$2\r\nhi\r\n");
    }

    #[test]
    fn encodes_null_as_resp2_null_bulk_string() {
        let mut buf = BytesMut::new();
        RespCodec.encode(Frame::Null, &mut buf).unwrap();
        assert_eq!(&buf[..], b"$-1\r\n");
    }

    #[test]
    fn encodes_array_by_concatenating_each_items_encoding() {
        let mut buf = BytesMut::new();
        let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"a")), Frame::Integer(1)]);
        RespCodec.encode(frame, &mut buf).unwrap();
        assert_eq!(&buf[..], b"*2\r\n$1\r\na\r\n:1\r\n");
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p protocol codec::tests`
Expected: FAIL — `RespCodec` is not defined yet

- [ ] **Step 4: Write the implementation**

```rust
// crates/protocol/src/codec.rs (above the test module)
use crate::Frame;
use bytes::{BufMut, BytesMut};
use tokio_util::codec::Encoder;

pub struct RespCodec;

impl Encoder<Frame> for RespCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
            Frame::Simple(s) => {
                dst.put_u8(b'+');
                dst.put_slice(s.as_bytes());
                dst.put_slice(b"\r\n");
            }
            Frame::Error(s) => {
                dst.put_u8(b'-');
                dst.put_slice(s.as_bytes());
                dst.put_slice(b"\r\n");
            }
            Frame::Integer(n) => {
                dst.put_u8(b':');
                dst.put_slice(n.to_string().as_bytes());
                dst.put_slice(b"\r\n");
            }
            Frame::Bulk(b) => {
                dst.put_u8(b'$');
                dst.put_slice(b.len().to_string().as_bytes());
                dst.put_slice(b"\r\n");
                dst.put_slice(&b);
                dst.put_slice(b"\r\n");
            }
            Frame::Null => {
                dst.put_slice(b"$-1\r\n");
            }
            Frame::Array(items) => {
                dst.put_u8(b'*');
                dst.put_slice(items.len().to_string().as_bytes());
                dst.put_slice(b"\r\n");
                for item in items {
                    self.encode(item, dst)?;
                }
            }
        }
        Ok(())
    }
}
```

- [ ] **Step 5: Register the new module**

```rust
// crates/protocol/src/lib.rs
pub mod codec;
pub mod frame;
pub use frame::Frame;
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p protocol codec::tests`
Expected: PASS, 6/6

- [ ] **Step 7: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/protocol/src/codec.rs`, `crates/protocol/src/lib.rs`, `crates/protocol/Cargo.toml`,
`Cargo.toml`, and `Cargo.lock` — do not compose the commit message freeform. Suggested
subject: `feat(protocol): add RespCodec::encode (Frame -> RESP2 bytes)`.

---

### Task 3: `RespCodec::decode` — RESP2 bytes → `Frame`, complete frames only

This task only handles frames that arrive whole in one buffer. Task 4 of `02-partial-read-framing.md` proves the incremental "wait for more bytes" behavior — don't test that here, it belongs to that plan.

**Files:**
- Modify: `crates/protocol/src/codec.rs`

**Interfaces:**
- Produces: `impl Decoder for RespCodec`, `type Item = Frame`, `type Error = std::io::Error`. `decode` returns `Ok(None)` when the buffer doesn't yet contain a complete frame, `Ok(Some(frame))` when it does (consuming exactly those bytes from `src`), `Err(_)` on malformed input that can never become valid (e.g. a type byte that isn't one of `+-:$*`).

- [ ] **Step 1: Write the failing test**

```rust
// add to crates/protocol/src/codec.rs tests module
use tokio_util::codec::Decoder;

#[test]
fn decodes_simple_string() {
    let mut buf = BytesMut::from(&b"+OK\r\n"[..]);
    let frame = RespCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(frame, Frame::Simple("OK".into()));
    assert!(buf.is_empty());
}

#[test]
fn decodes_error() {
    let mut buf = BytesMut::from(&b"-ERR bad\r\n"[..]);
    assert_eq!(RespCodec.decode(&mut buf).unwrap().unwrap(), Frame::Error("ERR bad".into()));
}

#[test]
fn decodes_integer() {
    let mut buf = BytesMut::from(&b":42\r\n"[..]);
    assert_eq!(RespCodec.decode(&mut buf).unwrap().unwrap(), Frame::Integer(42));
}

#[test]
fn decodes_bulk_string() {
    let mut buf = BytesMut::from(&b"$2\r\nhi\r\n"[..]);
    assert_eq!(RespCodec.decode(&mut buf).unwrap().unwrap(), Frame::Bulk(Bytes::from_static(b"hi")));
}

#[test]
fn decodes_null_bulk_string() {
    let mut buf = BytesMut::from(&b"$-1\r\n"[..]);
    assert_eq!(RespCodec.decode(&mut buf).unwrap().unwrap(), Frame::Null);
}

#[test]
fn decodes_array_of_bulk_strings_the_shape_a_real_client_sends() {
    let mut buf = BytesMut::from(&b"*3\r\n$3\r\nSET\r\n$3\r\nfoo\r\n$3\r\nbar\r\n"[..]);
    let frame = RespCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(frame, Frame::Array(vec![
        Frame::Bulk(Bytes::from_static(b"SET")),
        Frame::Bulk(Bytes::from_static(b"foo")),
        Frame::Bulk(Bytes::from_static(b"bar")),
    ]));
}

#[test]
fn unknown_type_byte_is_an_error_not_a_panic() {
    let mut buf = BytesMut::from(&b"@nope\r\n"[..]);
    assert!(RespCodec.decode(&mut buf).is_err());
}

#[test]
fn empty_buffer_returns_ok_none_not_an_error() {
    let mut buf = BytesMut::new();
    assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p protocol codec::tests`
Expected: FAIL — no `Decoder` impl yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/protocol/src/codec.rs — add above the Encoder impl
use bytes::Buf;
use std::io;

/// Finds the index of the `\r` in the first `\r\n` in `buf`, if a complete one exists.
fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

/// Parses one complete frame starting at the front of `buf`.
/// Returns `Ok(None)` if `buf` doesn't yet contain a complete frame (caller should wait
/// for more bytes) and never consumes anything in that case. Returns `Ok(Some((frame,
/// consumed)))` on success, where `consumed` is the exact byte count to drop from `buf`.
fn parse_frame(buf: &[u8]) -> io::Result<Option<(Frame, usize)>> {
    if buf.is_empty() {
        return Ok(None);
    }
    match buf[0] {
        b'+' | b'-' | b':' => {
            let Some(crlf) = find_crlf(&buf[1..]) else { return Ok(None) };
            let line = std::str::from_utf8(&buf[1..1 + crlf])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let consumed = 1 + crlf + 2;
            let frame = match buf[0] {
                b'+' => Frame::Simple(line.to_string()),
                b'-' => Frame::Error(line.to_string()),
                b':' => Frame::Integer(
                    line.parse()
                        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad integer"))?,
                ),
                _ => unreachable!(),
            };
            Ok(Some((frame, consumed)))
        }
        b'$' => {
            let Some(crlf) = find_crlf(&buf[1..]) else { return Ok(None) };
            let len_str = std::str::from_utf8(&buf[1..1 + crlf])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let len: i64 = len_str
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad bulk length"))?;
            let header_len = 1 + crlf + 2;
            if len == -1 {
                return Ok(Some((Frame::Null, header_len)));
            }
            let len = len as usize;
            let total = header_len + len + 2;
            if buf.len() < total {
                return Ok(None);
            }
            let data = &buf[header_len..header_len + len];
            Ok(Some((Frame::Bulk(bytes::Bytes::copy_from_slice(data)), total)))
        }
        b'*' => {
            let Some(crlf) = find_crlf(&buf[1..]) else { return Ok(None) };
            let count_str = std::str::from_utf8(&buf[1..1 + crlf])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let count: i64 = count_str
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad array length"))?;
            let mut consumed = 1 + crlf + 2;
            let mut items = Vec::new();
            for _ in 0..count.max(0) {
                match parse_frame(&buf[consumed..])? {
                    Some((item, item_consumed)) => {
                        items.push(item);
                        consumed += item_consumed;
                    }
                    None => return Ok(None),
                }
            }
            Ok(Some((Frame::Array(items), consumed)))
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown RESP type byte: {other:#x}"),
        )),
    }
}

impl Decoder for RespCodec {
    type Item = Frame;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, Self::Error> {
        match parse_frame(src)? {
            Some((frame, consumed)) => {
                src.advance(consumed);
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p protocol codec::tests`
Expected: PASS, 14/14 (6 from Task 2 + 8 new)

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/protocol/src/codec.rs` — do not compose the commit message freeform. Suggested
subject: `feat(protocol): add RespCodec::decode (RESP2 bytes -> Frame)`.
