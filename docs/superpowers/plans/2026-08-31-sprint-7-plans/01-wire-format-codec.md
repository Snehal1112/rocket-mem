# RMP Wire Format & Codec Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a `protocol::rmp` module that can encode and decode RMP's envelope (magic/version/msg_type/request_id/payload_len) and its payload's recursive `Frame` value encoding, over `tokio_util::codec`, byte-for-byte matching the layouts fixed in the spec.

**Architecture:** one new file, `crates/protocol/src/rmp.rs`, with two layers: a private `encode_frame`/`decode_frame` pair that recursively (de)serializes a `protocol::Frame` using a 1-byte tag scheme, and a public `RmpCodec` (`Decoder`/`Encoder<RmpMessage>`) that wraps a value in the 16-byte envelope. `RmpMessage { request_id: u64, msg_type: MsgType, frame: Frame }` is the single public type both the server (Plan 02) and the client (Plan 03) build and consume — a Request's `frame` is always a `Frame::Array` of `Frame::Bulk`s (command name + args), a Response's `frame` is whatever `dispatcher::dispatch_and_log` returned.

**Tech Stack:** `bytes` (`Buf`/`BufMut` on `BytesMut`), `tokio_util::codec::{Decoder, Encoder}` — both already workspace dependencies of the `protocol` crate. No new dependency.

**Spec:** [`../../specs/2026-08-31-sprint-7-spec.md`](../../specs/2026-08-31-sprint-7-spec.md) — "Decision: wire format — a request-ID envelope around the existing `Frame` value model" is authoritative for every byte layout in this plan.

## Global Constraints

- **No new runtime dependency.** RMP is hand-rolled over `tokio_util::codec`, exactly like `RespCodec`.
- **Exactly one wire version.** `version` is always `0x01`; there is no negotiation. A decoded message with any other version byte is a hard decode error (closes the connection), not a silent fallback.
- **`MAX_RMP_FRAME_LEN = 64 * 1024 * 1024` (64 MiB)** bounds the envelope's `payload_len` and every `u32` length/count field found while decoding the payload's `Frame` value. A value exceeding it is rejected immediately, before any allocation sized by that value — this is the only defense a length-prefixed format has against a forged multi-gigabyte length.
- **All multi-byte integers are big-endian.**
- **The payload, once fully buffered, decodes with no partial-read handling of its own.** `RmpCodec::decode` only produces a payload buffer once `payload_len` bytes have actually arrived (mirroring how `RespCodec` waits for a bulk string's declared length before reading it) — `decode_frame` never needs to return "not enough bytes yet", only "malformed".

---

### Task 1: Value encoding — `encode_frame` / `decode_frame`

**Files:**
- Create: `crates/protocol/src/rmp.rs`
- Modify: `crates/protocol/src/lib.rs` (currently `pub mod codec; pub mod frame; pub use frame::Frame;`)

**Interfaces:**
- Consumes: `protocol::Frame` (already defined in `crates/protocol/src/frame.rs`: `Simple(String) | Error(String) | Integer(i64) | Bulk(Bytes) | Null | Array(Vec<Frame>) | Map(Vec<(Frame, Frame)>)`).
- Produces: `pub(crate) fn encode_frame(frame: &Frame, dst: &mut BytesMut) -> io::Result<()>` and `pub(crate) fn decode_frame(src: &mut BytesMut) -> io::Result<Frame>`, consumed only by Task 2 in this same file.

- [ ] **Step 1: Declare the module and write the tag constants**

```rust
// crates/protocol/src/lib.rs
pub mod codec;
pub mod frame;
pub mod rmp;
pub use frame::Frame;
```

```rust
// crates/protocol/src/rmp.rs — file skeleton
use crate::Frame;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::io;

/// Bounds the envelope's `payload_len` and every length/count field decoded from inside the
/// payload, checked before any allocation sized by that value. The only defense a length-prefixed
/// format has against a forged multi-gigabyte length claim.
pub const MAX_RMP_FRAME_LEN: u32 = 64 * 1024 * 1024;

const TAG_NULL: u8 = 0x00;
const TAG_SIMPLE: u8 = 0x01;
const TAG_ERROR: u8 = 0x02;
const TAG_INTEGER: u8 = 0x03;
const TAG_BULK: u8 = 0x04;
const TAG_ARRAY: u8 = 0x05;
const TAG_MAP: u8 = 0x06;

fn invalid_data(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}
```

- [ ] **Step 2: Write the failing round-trip tests**

```rust
// crates/protocol/src/rmp.rs — append
#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(frame: Frame) -> Frame {
        let mut buf = BytesMut::new();
        encode_frame(&frame, &mut buf).unwrap();
        decode_frame(&mut buf).unwrap()
    }

    #[test]
    fn null_round_trips() {
        assert_eq!(round_trip(Frame::Null), Frame::Null);
    }

    #[test]
    fn simple_round_trips() {
        assert_eq!(round_trip(Frame::Simple("OK".into())), Frame::Simple("OK".into()));
    }

    #[test]
    fn error_round_trips() {
        assert_eq!(
            round_trip(Frame::Error("ERR boom".into())),
            Frame::Error("ERR boom".into())
        );
    }

    #[test]
    fn integer_round_trips_including_negative_values() {
        assert_eq!(round_trip(Frame::Integer(42)), Frame::Integer(42));
        assert_eq!(round_trip(Frame::Integer(-1)), Frame::Integer(-1));
    }

    #[test]
    fn bulk_round_trips() {
        assert_eq!(
            round_trip(Frame::Bulk(Bytes::from_static(b"bar"))),
            Frame::Bulk(Bytes::from_static(b"bar"))
        );
    }

    #[test]
    fn empty_bulk_round_trips() {
        assert_eq!(
            round_trip(Frame::Bulk(Bytes::new())),
            Frame::Bulk(Bytes::new())
        );
    }

    #[test]
    fn array_of_bulk_strings_round_trips_the_shape_a_command_uses() {
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"GET")),
            Frame::Bulk(Bytes::from_static(b"foo")),
        ]);
        assert_eq!(round_trip(frame.clone()), frame);
    }

    #[test]
    fn nested_array_round_trips() {
        let frame = Frame::Array(vec![
            Frame::Integer(1),
            Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"x")), Frame::Null]),
        ]);
        assert_eq!(round_trip(frame.clone()), frame);
    }

    #[test]
    fn map_round_trips() {
        let frame = Frame::Map(vec![
            (
                Frame::Bulk(Bytes::from_static(b"proto")),
                Frame::Integer(1),
            ),
            (
                Frame::Bulk(Bytes::from_static(b"role")),
                Frame::Bulk(Bytes::from_static(b"master")),
            ),
        ]);
        assert_eq!(round_trip(frame.clone()), frame);
    }

    #[test]
    fn the_get_foo_request_matches_the_spec_worked_example_byte_for_byte() {
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"GET")),
            Frame::Bulk(Bytes::from_static(b"foo")),
        ]);
        let mut buf = BytesMut::new();
        encode_frame(&frame, &mut buf).unwrap();
        assert_eq!(
            &buf[..],
            &[
                0x05, 0x00, 0x00, 0x00, 0x02, // Array, count 2
                0x04, 0x00, 0x00, 0x00, 0x03, b'G', b'E', b'T', // Bulk "GET"
                0x04, 0x00, 0x00, 0x00, 0x03, b'f', b'o', b'o', // Bulk "foo"
            ][..]
        );
    }

    #[test]
    fn the_bar_response_matches_the_spec_worked_example_byte_for_byte() {
        let mut buf = BytesMut::new();
        encode_frame(&Frame::Bulk(Bytes::from_static(b"bar")), &mut buf).unwrap();
        assert_eq!(
            &buf[..],
            &[0x04, 0x00, 0x00, 0x00, 0x03, b'b', b'a', b'r'][..]
        );
    }

    #[test]
    fn a_bulk_length_over_the_max_frame_len_is_rejected_without_reading_it() {
        let mut buf = BytesMut::new();
        buf.put_u8(TAG_BULK);
        buf.put_u32(MAX_RMP_FRAME_LEN + 1);
        // No payload bytes follow -- if decode_frame checked the length against remaining
        // buffer size first, it would report "truncated" instead of "too large". Asserting
        // the specific error message pins that the MAX_RMP_FRAME_LEN check runs first.
        let err = decode_frame(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("exceeds"));
    }

    #[test]
    fn an_unknown_tag_byte_is_a_decode_error() {
        let mut buf = BytesMut::from(&[0xFFu8][..]);
        assert!(decode_frame(&mut buf).is_err());
    }

    #[test]
    fn a_truncated_value_is_a_decode_error_not_a_panic() {
        let mut buf = BytesMut::from(&[TAG_INTEGER, 0x00, 0x00][..]); // needs 8 bytes, has 2
        assert!(decode_frame(&mut buf).is_err());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail (or don't compile yet)**

Run: `cargo test -p protocol rmp:: -- --nocapture`
Expected: compile error — `encode_frame`/`decode_frame` don't exist yet.

- [ ] **Step 4: Implement `encode_frame`**

```rust
// crates/protocol/src/rmp.rs — above the tests module
pub(crate) fn encode_frame(frame: &Frame, dst: &mut BytesMut) -> io::Result<()> {
    match frame {
        Frame::Null => dst.put_u8(TAG_NULL),
        Frame::Simple(s) => {
            dst.put_u8(TAG_SIMPLE);
            dst.put_u32(s.len() as u32);
            dst.put_slice(s.as_bytes());
        }
        Frame::Error(s) => {
            dst.put_u8(TAG_ERROR);
            dst.put_u32(s.len() as u32);
            dst.put_slice(s.as_bytes());
        }
        Frame::Integer(n) => {
            dst.put_u8(TAG_INTEGER);
            dst.put_i64(*n);
        }
        Frame::Bulk(b) => {
            dst.put_u8(TAG_BULK);
            dst.put_u32(b.len() as u32);
            dst.put_slice(b);
        }
        Frame::Array(items) => {
            dst.put_u8(TAG_ARRAY);
            dst.put_u32(items.len() as u32);
            for item in items {
                encode_frame(item, dst)?;
            }
        }
        Frame::Map(pairs) => {
            dst.put_u8(TAG_MAP);
            dst.put_u32(pairs.len() as u32);
            for (k, v) in pairs {
                encode_frame(k, dst)?;
                encode_frame(v, dst)?;
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Implement `decode_frame` and its bounds-checked read helpers**

```rust
// crates/protocol/src/rmp.rs — above encode_frame (or below; module order doesn't matter)
fn take_u8(src: &mut BytesMut) -> io::Result<u8> {
    if src.is_empty() {
        return Err(invalid_data("truncated rmp value: expected a tag byte"));
    }
    Ok(src.get_u8())
}

fn take_len(src: &mut BytesMut) -> io::Result<usize> {
    if src.remaining() < 4 {
        return Err(invalid_data("truncated rmp value: expected a u32 length"));
    }
    let len = src.get_u32();
    if len > MAX_RMP_FRAME_LEN {
        return Err(invalid_data("rmp value length exceeds MAX_RMP_FRAME_LEN"));
    }
    Ok(len as usize)
}

fn take_i64(src: &mut BytesMut) -> io::Result<i64> {
    if src.remaining() < 8 {
        return Err(invalid_data("truncated rmp value: expected an i64"));
    }
    Ok(src.get_i64())
}

fn take_bytes(src: &mut BytesMut, len: usize) -> io::Result<Bytes> {
    if src.remaining() < len {
        return Err(invalid_data("truncated rmp value: declared length exceeds buffered bytes"));
    }
    Ok(src.split_to(len).freeze())
}

fn take_string(src: &mut BytesMut, len: usize) -> io::Result<String> {
    let bytes = take_bytes(src, len)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| invalid_data("rmp Simple/Error value is not valid UTF-8"))
}

pub(crate) fn decode_frame(src: &mut BytesMut) -> io::Result<Frame> {
    let tag = take_u8(src)?;
    match tag {
        TAG_NULL => Ok(Frame::Null),
        TAG_SIMPLE => {
            let len = take_len(src)?;
            Ok(Frame::Simple(take_string(src, len)?))
        }
        TAG_ERROR => {
            let len = take_len(src)?;
            Ok(Frame::Error(take_string(src, len)?))
        }
        TAG_INTEGER => Ok(Frame::Integer(take_i64(src)?)),
        TAG_BULK => {
            let len = take_len(src)?;
            Ok(Frame::Bulk(take_bytes(src, len)?))
        }
        TAG_ARRAY => {
            let count = take_len(src)?;
            // Pre-allocation is capped regardless of the declared count -- a forged count of
            // several billion must not itself trigger a huge allocation; the loop below fails
            // fast on the first truncated read once real buffered bytes run out.
            let mut items = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                items.push(decode_frame(src)?);
            }
            Ok(Frame::Array(items))
        }
        TAG_MAP => {
            let count = take_len(src)?;
            let mut pairs = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                let k = decode_frame(src)?;
                let v = decode_frame(src)?;
                pairs.push((k, v));
            }
            Ok(Frame::Map(pairs))
        }
        _ => Err(invalid_data("unknown rmp value tag")),
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p protocol rmp:: -- --nocapture`
Expected: all tests in Step 2 PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/protocol/src/rmp.rs crates/protocol/src/lib.rs
git commit -m "feat(protocol): RMP value encoding (Frame <-> tagged binary)"
```

---

### Task 2: Envelope — `RmpMessage`, `MsgType`, `RmpCodec`

**Files:**
- Modify: `crates/protocol/src/rmp.rs`

**Interfaces:**
- Consumes: `encode_frame`/`decode_frame` from Task 1.
- Produces: `pub struct RmpMessage { pub request_id: u64, pub msg_type: MsgType, pub frame: Frame }`, `pub enum MsgType { Request, Response }` (both `Debug + Clone + PartialEq`, `MsgType` additionally `Copy + Eq`), and `pub struct RmpCodec` implementing `Decoder<Item = RmpMessage, Error = io::Error>` and `Encoder<RmpMessage, Error = io::Error>`. Consumed by Plan 02 (`crates/server/src/rmp_connection.rs`) and Plan 03 (`crates/rmp-client`).

- [ ] **Step 1: Write the failing envelope tests**

```rust
// crates/protocol/src/rmp.rs — inside `mod tests`, alongside the Task 1 tests
use tokio_util::codec::{Decoder, Encoder};

fn encode_message(msg: RmpMessage) -> BytesMut {
    let mut buf = BytesMut::new();
    RmpCodec.encode(msg, &mut buf).unwrap();
    buf
}

#[test]
fn a_request_round_trips_through_encode_decode() {
    let msg = RmpMessage {
        request_id: 1,
        msg_type: MsgType::Request,
        frame: Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"GET")),
            Frame::Bulk(Bytes::from_static(b"foo")),
        ]),
    };
    let mut buf = encode_message(msg.clone());
    let decoded = RmpCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn a_response_round_trips_through_encode_decode() {
    let msg = RmpMessage {
        request_id: 1,
        msg_type: MsgType::Response,
        frame: Frame::Bulk(Bytes::from_static(b"bar")),
    };
    let mut buf = encode_message(msg.clone());
    let decoded = RmpCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn the_get_foo_request_envelope_matches_the_spec_worked_example_byte_for_byte() {
    let msg = RmpMessage {
        request_id: 1,
        msg_type: MsgType::Request,
        frame: Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"GET")),
            Frame::Bulk(Bytes::from_static(b"foo")),
        ]),
    };
    let buf = encode_message(msg);
    assert_eq!(
        &buf[..],
        &[
            0x52, 0x4D, // magic "RM"
            0x01, // version
            0x00, // msg_type = Request
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // request_id = 1
            0x00, 0x00, 0x00, 0x15, // payload_len = 21
            0x05, 0x00, 0x00, 0x00, 0x02, // Array, count 2
            0x04, 0x00, 0x00, 0x00, 0x03, b'G', b'E', b'T',
            0x04, 0x00, 0x00, 0x00, 0x03, b'f', b'o', b'o',
        ][..]
    );
}

#[test]
fn the_bar_response_envelope_matches_the_spec_worked_example_byte_for_byte() {
    let msg = RmpMessage {
        request_id: 1,
        msg_type: MsgType::Response,
        frame: Frame::Bulk(Bytes::from_static(b"bar")),
    };
    let buf = encode_message(msg);
    assert_eq!(
        &buf[..],
        &[
            0x52, 0x4D, 0x01, 0x01, // magic, version, msg_type = Response
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // request_id = 1
            0x00, 0x00, 0x00, 0x08, // payload_len = 8
            0x04, 0x00, 0x00, 0x00, 0x03, b'b', b'a', b'r',
        ][..]
    );
}

#[test]
fn decode_returns_none_when_only_part_of_the_header_has_arrived() {
    let mut buf = BytesMut::from(&[0x52, 0x4D, 0x01, 0x00, 0x00][..]); // 5 of 16 header bytes
    assert_eq!(RmpCodec.decode(&mut buf).unwrap(), None);
    // nothing consumed -- the next call with more bytes must still see all 5
    assert_eq!(buf.len(), 5);
}

#[test]
fn decode_reassembles_a_header_split_across_two_reads() {
    let full = encode_message(RmpMessage {
        request_id: 9,
        msg_type: MsgType::Request,
        frame: Frame::Bulk(Bytes::from_static(b"x")),
    });
    let mut buf = BytesMut::from(&full[..10]); // splits inside the header
    assert_eq!(RmpCodec.decode(&mut buf).unwrap(), None);
    buf.extend_from_slice(&full[10..]);
    let decoded = RmpCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(decoded.request_id, 9);
}

#[test]
fn decode_reassembles_a_payload_split_across_two_reads() {
    let full = encode_message(RmpMessage {
        request_id: 2,
        msg_type: MsgType::Response,
        frame: Frame::Bulk(Bytes::from_static(b"hello world")),
    });
    let mut buf = BytesMut::from(&full[..20]); // full header (16) plus a few payload bytes
    assert_eq!(RmpCodec.decode(&mut buf).unwrap(), None);
    buf.extend_from_slice(&full[20..]);
    let decoded = RmpCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(decoded.frame, Frame::Bulk(Bytes::from_static(b"hello world")));
}

#[test]
fn decode_only_consumes_one_message_when_two_arrive_pipelined_in_one_read() {
    let mut buf = encode_message(RmpMessage {
        request_id: 1,
        msg_type: MsgType::Request,
        frame: Frame::Integer(1),
    });
    buf.extend_from_slice(&encode_message(RmpMessage {
        request_id: 2,
        msg_type: MsgType::Request,
        frame: Frame::Integer(2),
    }));
    let first = RmpCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(first.request_id, 1);
    let second = RmpCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(second.request_id, 2);
    assert!(buf.is_empty());
}

#[test]
fn a_bad_magic_is_a_decode_error() {
    let mut buf = BytesMut::from(&[0xAA, 0xBB, 0x01, 0x00, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0][..]);
    assert!(RmpCodec.decode(&mut buf).is_err());
}

#[test]
fn an_unsupported_version_is_a_decode_error() {
    let mut buf = BytesMut::from(&[0x52, 0x4D, 0x02, 0x00, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0][..]);
    assert!(RmpCodec.decode(&mut buf).is_err());
}

#[test]
fn a_bad_msg_type_byte_is_a_decode_error() {
    let mut buf = BytesMut::from(&[0x52, 0x4D, 0x01, 0x02, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0][..]);
    assert!(RmpCodec.decode(&mut buf).is_err());
}

#[test]
fn a_payload_len_over_the_max_is_a_decode_error_without_waiting_for_the_bytes() {
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&[0x52, 0x4D, 0x01, 0x00]); // magic, version, Request
    buf.extend_from_slice(&1u64.to_be_bytes()); // request_id
    buf.extend_from_slice(&(MAX_RMP_FRAME_LEN + 1).to_be_bytes()); // payload_len
    // No payload bytes follow at all -- this must still error, not return Ok(None).
    assert!(RmpCodec.decode(&mut buf).is_err());
}
```

- [ ] **Step 2: Run the tests to verify they fail (or don't compile)**

Run: `cargo test -p protocol rmp:: -- --nocapture`
Expected: compile error — `RmpMessage`, `MsgType`, `RmpCodec` don't exist yet.

- [ ] **Step 3: Implement `MsgType` and `RmpMessage`**

```rust
// crates/protocol/src/rmp.rs — above the tests module
const MAGIC: [u8; 2] = [0x52, 0x4D];
const VERSION: u8 = 0x01;
const HEADER_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    Request,
    Response,
}

impl MsgType {
    fn to_byte(self) -> u8 {
        match self {
            MsgType::Request => 0x00,
            MsgType::Response => 0x01,
        }
    }

    fn from_byte(b: u8) -> io::Result<Self> {
        match b {
            0x00 => Ok(MsgType::Request),
            0x01 => Ok(MsgType::Response),
            _ => Err(invalid_data("unknown rmp msg_type byte")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RmpMessage {
    pub request_id: u64,
    pub msg_type: MsgType,
    pub frame: Frame,
}
```

- [ ] **Step 4: Implement `RmpCodec`**

```rust
// crates/protocol/src/rmp.rs — above the tests module
#[derive(Debug, Default)]
pub struct RmpCodec;

impl Encoder<RmpMessage> for RmpCodec {
    type Error = io::Error;

    fn encode(&mut self, item: RmpMessage, dst: &mut BytesMut) -> io::Result<()> {
        let mut payload = BytesMut::new();
        encode_frame(&item.frame, &mut payload)?;
        if payload.len() as u64 > MAX_RMP_FRAME_LEN as u64 {
            return Err(invalid_data("rmp payload exceeds MAX_RMP_FRAME_LEN"));
        }
        dst.reserve(HEADER_LEN + payload.len());
        dst.put_slice(&MAGIC);
        dst.put_u8(VERSION);
        dst.put_u8(item.msg_type.to_byte());
        dst.put_u64(item.request_id);
        dst.put_u32(payload.len() as u32);
        dst.put_slice(&payload);
        Ok(())
    }
}

impl Decoder for RmpCodec {
    type Item = RmpMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<RmpMessage>> {
        if src.len() < HEADER_LEN {
            return Ok(None);
        }
        if src[0..2] != MAGIC {
            return Err(invalid_data("bad rmp magic"));
        }
        if src[2] != VERSION {
            return Err(invalid_data("unsupported rmp version"));
        }
        let msg_type = MsgType::from_byte(src[3])?;
        let request_id = u64::from_be_bytes(src[4..12].try_into().unwrap());
        let payload_len = u32::from_be_bytes(src[12..16].try_into().unwrap());
        if payload_len > MAX_RMP_FRAME_LEN {
            return Err(invalid_data("rmp payload_len exceeds MAX_RMP_FRAME_LEN"));
        }
        let total_len = HEADER_LEN + payload_len as usize;
        if src.len() < total_len {
            src.reserve(total_len - src.len());
            return Ok(None);
        }
        src.advance(HEADER_LEN);
        let mut payload = src.split_to(payload_len as usize);
        let frame = decode_frame(&mut payload)?;
        Ok(Some(RmpMessage {
            request_id,
            msg_type,
            frame,
        }))
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p protocol rmp:: -- --nocapture`
Expected: all tests in Step 1 PASS.

- [ ] **Step 6: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy -p protocol -- -D warnings && cargo test -p protocol`
Expected: all green.

```bash
git add crates/protocol/src/rmp.rs
git commit -m "feat(protocol): RMP envelope codec (RmpMessage, MsgType, RmpCodec)"
```
