# RESP3 Frame::Map & Stateful RespCodec Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** add the one new `Frame` variant RESP3 needs (`Map`), and make `RespCodec` protocol-aware so `encode()` picks the right wire format for `Null` and `Map` depending on whether RESP2 or RESP3 was negotiated.

**Architecture:** both changes live entirely in `crates/protocol` — this plan does not touch `crates/server` at all, so `cargo build -p protocol` and `cargo test -p protocol` stay green throughout. `cargo build --workspace` will start failing partway through this plan (see Task 2's note) because `crates/server/src/connection.rs` still constructs the now-stateful `RespCodec` as a bare unit value; that's fixed in `02-dispatch-and-connection-wiring.md`, not here.

**Tech Stack:** no new dependencies.

**Spec:** `../../specs/2026-08-29-resp3-design.md` — the `Frame::Map` variant, the `Protocol` enum, `RespCodec`'s new shape, and the RESP2/RESP3 wire-format table for `Null`/`Map` are all authoritative; don't redefine them here.

## Global Constraints

- `decode()` needs **zero** changes in this plan — clients only ever send `Array`-of-`Bulk` commands regardless of negotiated protocol; only `encode()` changes.
- No new `Frame` variants beyond `Map` — `Double`/`Boolean`/`BigNumber`/`Set`/`Verbatim`/`Push` are explicitly out of scope (spec's Non-goals section).

---

### Task 1: `Frame::Map` variant

**Files:**
- Modify: `crates/protocol/src/frame.rs`

**Interfaces:**
- Produces: `Frame::Map(Vec<(Frame, Frame)>)` — Task 2 of this plan and every task in `02-dispatch-and-connection-wiring.md` construct and match on this variant.

- [ ] **Step 1: Write the failing tests**

```rust
// add to crates/protocol/src/frame.rs tests module
#[test]
fn map_frame_holds_key_value_pairs() {
    let f = Frame::Map(vec![(
        Frame::Bulk(Bytes::from_static(b"proto")),
        Frame::Integer(3),
    )]);
    assert_eq!(
        f,
        Frame::Map(vec![(
            Frame::Bulk(Bytes::from_static(b"proto")),
            Frame::Integer(3)
        )])
    );
}

#[test]
fn map_frames_are_not_equal_to_array_frames_with_the_same_flattened_content() {
    let map = Frame::Map(vec![(Frame::Integer(1), Frame::Integer(2))]);
    let array = Frame::Array(vec![Frame::Integer(1), Frame::Integer(2)]);
    assert_ne!(map, array);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p protocol frame::tests`
Expected: FAIL — `Frame::Map` doesn't exist yet (compile error)

- [ ] **Step 3: Add the variant**

```rust
// crates/protocol/src/frame.rs — replace the existing Frame enum with:
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Bytes),
    Null,
    Array(Vec<Frame>),
    Map(Vec<(Frame, Frame)>),
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p protocol frame::tests`
Expected: PASS, 5/5 (3 existing + 2 new)

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/protocol/src/frame.rs` — do not compose the commit message freeform. Suggested
subject: `feat(protocol): add Frame::Map variant for RESP3 replies`.

---

### Task 2: Stateful `RespCodec` — `Protocol` enum + RESP2/RESP3 encoding for `Null` and `Map`

**Files:**
- Modify: `crates/protocol/src/codec.rs`

**Interfaces:**
- Consumes: `Frame::Map` (Task 1).
- Produces: `pub enum Protocol { Resp2, Resp3 }` (implements `Default`, defaulting to `Resp2`), `pub struct RespCodec { pub protocol: Protocol }` (implements `Default`) — every task in `02-dispatch-and-connection-wiring.md` and `03-verification.md` constructs `RespCodec::default()` and reads/writes `Protocol` values.

- [ ] **Step 1: Write the failing tests**

```rust
// add to crates/protocol/src/codec.rs tests module
#[test]
fn encodes_null_as_resp3_null_when_protocol_is_resp3() {
    let mut buf = BytesMut::new();
    let mut codec = RespCodec {
        protocol: Protocol::Resp3,
    };
    codec.encode(Frame::Null, &mut buf).unwrap();
    assert_eq!(&buf[..], b"_\r\n");
}

#[test]
fn encodes_map_as_flattened_array_under_resp2() {
    let mut buf = BytesMut::new();
    let frame = Frame::Map(vec![(
        Frame::Bulk(Bytes::from_static(b"proto")),
        Frame::Integer(2),
    )]);
    RespCodec::default().encode(frame, &mut buf).unwrap();
    assert_eq!(&buf[..], b"*2\r\n$5\r\nproto\r\n:2\r\n");
}

#[test]
fn encodes_map_natively_under_resp3() {
    let mut buf = BytesMut::new();
    let frame = Frame::Map(vec![(
        Frame::Bulk(Bytes::from_static(b"proto")),
        Frame::Integer(3),
    )]);
    let mut codec = RespCodec {
        protocol: Protocol::Resp3,
    };
    codec.encode(frame, &mut buf).unwrap();
    assert_eq!(&buf[..], b"%1\r\n$5\r\nproto\r\n:3\r\n");
}

#[test]
fn encodes_map_with_multiple_pairs_under_resp3() {
    let mut buf = BytesMut::new();
    let frame = Frame::Map(vec![
        (Frame::Bulk(Bytes::from_static(b"a")), Frame::Integer(1)),
        (Frame::Bulk(Bytes::from_static(b"b")), Frame::Integer(2)),
    ]);
    let mut codec = RespCodec {
        protocol: Protocol::Resp3,
    };
    codec.encode(frame, &mut buf).unwrap();
    assert_eq!(&buf[..], b"%2\r\n$1\r\na\r\n:1\r\n$1\r\nb\r\n:2\r\n");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p protocol codec::tests`
Expected: FAIL — `Protocol` doesn't exist yet, `RespCodec` has no `protocol` field, `Frame::Map` has no `Encoder` arm

- [ ] **Step 3: Add the `Protocol` enum and make `RespCodec` stateful**

```rust
// crates/protocol/src/codec.rs — replace the top of the file through `pub struct RespCodec;` with:
use crate::Frame;
use bytes::{Buf, BufMut, BytesMut};
use std::io;
use tokio_util::codec::{Decoder, Encoder};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    #[default]
    Resp2,
    Resp3,
}

#[derive(Default)]
pub struct RespCodec {
    pub protocol: Protocol,
}
```

- [ ] **Step 4: Make `Null` protocol-aware and add the `Map` arm**

```rust
// crates/protocol/src/codec.rs — replace the entire `impl Encoder<Frame> for RespCodec` block with:
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
            Frame::Null => match self.protocol {
                Protocol::Resp2 => dst.put_slice(b"$-1\r\n"),
                Protocol::Resp3 => dst.put_slice(b"_\r\n"),
            },
            Frame::Array(items) => {
                dst.put_u8(b'*');
                dst.put_slice(items.len().to_string().as_bytes());
                dst.put_slice(b"\r\n");
                for item in items {
                    self.encode(item, dst)?;
                }
            }
            Frame::Map(pairs) => match self.protocol {
                Protocol::Resp2 => {
                    dst.put_u8(b'*');
                    dst.put_slice((pairs.len() * 2).to_string().as_bytes());
                    dst.put_slice(b"\r\n");
                    for (k, v) in pairs {
                        self.encode(k, dst)?;
                        self.encode(v, dst)?;
                    }
                }
                Protocol::Resp3 => {
                    dst.put_u8(b'%');
                    dst.put_slice(pairs.len().to_string().as_bytes());
                    dst.put_slice(b"\r\n");
                    for (k, v) in pairs {
                        self.encode(k, dst)?;
                        self.encode(v, dst)?;
                    }
                }
            },
        }
        Ok(())
    }
}
```

Note: RESP2's flattened-array encoding for `Map` (`*{2N}\r\n` with key,value,key,value,...)
is Redis's own real map-emulation convention for RESP2 clients, not a rocket-mem invention.

- [ ] **Step 5: Migrate every existing bare `RespCodec` test value to `RespCodec::default()`**

`RespCodec` was a zero-sized unit struct before Step 3, constructible as a bare value
(`RespCodec.encode(...)`, `RespCodec.decode(...)`). It now has a `protocol` field, so the
bare identifier no longer compiles as a value — every existing use in this file's `tests`
module must become `RespCodec::default()`, which is behaviorally identical (defaults to
`Protocol::Resp2`, matching every existing test's current assertions exactly). There are
23 occurrences to change — 6 in the encode tests (`encodes_simple_string` through
`encodes_array_by_concatenating_each_items_encoding`), 17 in the decode/split-read/
pipelining tests (`decodes_simple_string` through
`decode_only_consumes_one_frame_when_two_full_commands_arrive_pipelined_in_one_read`).
Replace every bare `RespCodec` value with `RespCodec::default()` in the `tests` module —
none of the surrounding assertions change, since `RespCodec::default()` produces the same
RESP2 encoding these tests already verify.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p protocol codec::tests`
Expected: PASS, 27/27 (23 existing, now using `RespCodec::default()`, + 4 new)

- [ ] **Step 7: Confirm the `protocol` crate itself is clean**

Run: `cargo build -p protocol && cargo clippy -p protocol -- -D warnings && cargo fmt --all -- --check`
Expected: all clean.

Do **not** run `cargo build --workspace` yet — it will fail, because
`crates/server/src/connection.rs` still constructs `RespCodec` as a bare unit value
(5 occurrences) and calls `dispatcher::dispatch(&engine, frame)` with the old 2-argument
signature. Both are fixed together in `02-dispatch-and-connection-wiring.md`'s Task 1,
since `dispatcher.rs` and `connection.rs` compile as one crate (`rocket_mem`) and can't be
fixed independently. This is expected — the same staggered-fix pattern this project's
Sprint 2 plans already used for crate-boundary changes (e.g.
`03-command-dispatcher.md`'s Task 1, which also left the build broken until its own
Task 2).

- [ ] **Step 8: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/protocol/src/codec.rs` — do not compose the commit message freeform. Suggested
subject: `feat(protocol): make RespCodec protocol-aware (RESP2/RESP3)`.
