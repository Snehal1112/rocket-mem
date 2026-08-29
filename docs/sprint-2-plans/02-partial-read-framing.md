# Partial-Read Framing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** prove `RespCodec::decode` correctly handles a command that arrives split across multiple TCP reads — this is Sprint 2's other named risk (per `../rocket-mem-sprint-plan.md`: "Partial-read framing bugs are subtle — works with `redis-cli`, silently breaks under pipelining. Write the split-write test before declaring networking done, not after.").

**Architecture:** no new production code — `parse_frame`/`decode` from `01-resp-frame-and-parser.md` were already written to return `Ok(None)` on an incomplete buffer without consuming anything, which is exactly the property that makes split reads safe. This plan adds the tests that prove it, plus a pipelining test (two full commands arriving in one read) since that's the mirror-image bug (over-consuming, not under-consuming).

**Tech Stack:** `bytes::BytesMut`, `tokio_util::codec::Decoder`. No real sockets needed — feeding a `BytesMut` in stages against the same `Decoder` instance a real `Framed<TcpStream, _>` would use is sufficient to prove this property, and is far less flaky than an actual split TCP write.

**Spec:** `00-sprint-2-spec.md`.

**Depends on:** `01-resp-frame-and-parser.md` must be complete.

---

### Task 1: Split-read tests for each frame type that can meaningfully split

**Files:**
- Modify: `crates/protocol/src/codec.rs`

**Interfaces:**
- Consumes: `RespCodec`, `Frame` (Task 1/3 of `01-resp-frame-and-parser.md`) — no signature changes.

- [ ] **Step 1: Write the failing tests**

```rust
// add to crates/protocol/src/codec.rs tests module
#[test]
fn decode_returns_none_on_a_bulk_string_split_mid_header() {
    let mut buf = BytesMut::from(&b"$3\r\nfo"[..]); // header + 2 of 3 body bytes
    assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
    assert_eq!(&buf[..], b"$3\r\nfo"); // nothing consumed on an incomplete frame
}

#[test]
fn decode_reassembles_a_bulk_string_split_across_two_reads() {
    let mut buf = BytesMut::from(&b"$3\r\nfo"[..]);
    assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
    buf.extend_from_slice(b"o\r\n"); // the rest arrives in a second read
    let frame = RespCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(frame, Frame::Bulk(Bytes::from_static(b"foo")));
    assert!(buf.is_empty());
}

#[test]
fn decode_reassembles_a_full_command_split_across_three_reads() {
    // mirrors a real client sending `SET foo bar` split at arbitrary byte boundaries
    let mut buf = BytesMut::from(&b"*3\r\n$3\r\nSET\r\n"[..]);
    assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
    buf.extend_from_slice(b"$3\r\nfo");
    assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
    buf.extend_from_slice(b"o\r\n$3\r\nbar\r\n");
    let frame = RespCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(frame, Frame::Array(vec![
        Frame::Bulk(Bytes::from_static(b"SET")),
        Frame::Bulk(Bytes::from_static(b"foo")),
        Frame::Bulk(Bytes::from_static(b"bar")),
    ]));
}

#[test]
fn decode_returns_none_when_only_the_array_header_has_arrived() {
    let mut buf = BytesMut::from(&b"*2\r\n"[..]);
    assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
    assert_eq!(&buf[..], b"*2\r\n");
}

#[test]
fn decode_only_consumes_one_frame_when_two_full_commands_arrive_pipelined_in_one_read() {
    let mut buf = BytesMut::from(&b"+OK\r\n+ALSO OK\r\n"[..]);
    let first = RespCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(first, Frame::Simple("OK".into()));
    assert_eq!(&buf[..], b"+ALSO OK\r\n"); // second frame untouched, ready for the next decode() call
    let second = RespCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(second, Frame::Simple("ALSO OK".into()));
    assert!(buf.is_empty());
}
```

- [ ] **Step 2: Run tests to verify current behavior**

Run: `cargo test -p protocol codec::tests`
Expected: if `01-resp-frame-and-parser.md` was implemented as specified, these PASS immediately — this plan's purpose is to catch a regression if `parse_frame`'s "don't consume on `Ok(None)`" contract was violated, not to add new behavior. Treat any failure here as a bug in the Task 3 implementation from `01-resp-frame-and-parser.md`, not a reason to weaken these tests.

- [ ] **Step 3: If anything failed, fix `parse_frame`**

The contract to restore: `parse_frame` must never call anything that mutates the input slice, and must return `Ok(None)` — not a partial frame, not a panic, not an out-of-bounds slice — the moment it would need to read past `buf.len()`. Every `buf[a..b]` slice in `parse_frame` must be preceded by a length check (`buf.len() < needed`) that returns `Ok(None)` first.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p protocol codec::tests`
Expected: PASS, all tests including the 5 new ones

- [ ] **Step 5: Commit**

```bash
git add crates/protocol/src/codec.rs
git commit -m "test(protocol): prove RespCodec::decode handles split reads and pipelining"
```
