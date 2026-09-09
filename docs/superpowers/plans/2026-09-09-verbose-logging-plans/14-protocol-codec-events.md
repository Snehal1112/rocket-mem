# Verbose Logging Plan 14: Protocol Codec Events

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the three `protocol` events from the spec catalogue: "frame decoded, kind + length (trace); split-read reassembly (trace); protocol error (warn)" to both wire formats `crates/protocol` implements — `crates/protocol/src/codec.rs`'s RESP `RespCodec` and `crates/protocol/src/rmp.rs`'s RMP `RmpCodec`.

**Architecture:** Both codecs' `Decoder::decode` are the real entry points — `RespCodec::decode` (`codec.rs:167-180`) delegates to the free function `parse_frame` (`codec.rs:87-165`, recursive for `Array`), and `RmpCodec::decode` (`rmp.rs:232-266`) reads a fixed 16-byte header before delegating to `decode_frame`/`decode_frame_inner` (`rmp.rs:124-173`, also recursive for `Array`/`Map`). Both already implement split-read reassembly deliberately: `parse_frame` returns `Ok(None)` without consuming anything the moment it finds an incomplete line or an incomplete bulk body (`codec.rs:97,115,129,139,155`), and `RmpCodec::decode` returns `Ok(None)` — after `src.reserve(...)`ing the shortfall — the moment the buffered bytes are short of either the 16-byte header (`rmp.rs:237-239`) or the declared `payload_len` (`rmp.rs:252-256`). Both `Decoder` implementations get called again on the next read with the previously-buffered bytes still in `src` (`tokio_util::codec` guarantees this), which is exactly what the existing tests named `decode_reassembles_...`/`decode_returns_none_on_a_bulk_string_split_mid_header` (`codec.rs`) and `decode_reassembles_a_header_split_across_two_reads`/`decode_reassembles_a_payload_split_across_two_reads` (`rmp.rs`) already pin down.

This plan instruments at exactly those two `decode` entry points, not inside the recursive `parse_frame`/`decode_frame_inner` functions: a `SET` with a 10-element array argument would otherwise log 11 times (once per recursive call) for what is, from an operator's point of view, one decoded frame. Logging once at the outer `decode` — after the whole frame (nested or not) has been fully parsed — matches what "frame decoded" means in the spec catalogue, and it means an error raised deep inside a recursive call still only produces one `warn!`, at the one place both codecs already convert an `Err` into what their caller sees.

Frame *kind* must never be logged via `{:?}` (`Debug`) on `Frame` itself: `Frame::Bulk` and `Frame::Array` both carry the actual RESP/RMP payload, and `#[derive(Debug)]` on `Frame` (`crates/protocol/src/frame.rs:3`) would render it byte-for-byte — exactly the value-content leak the critical constraint below forbids. This plan instead adds `Frame::kind(&self) -> &'static str`, a real match returning one of a fixed set of static string literals (`"simple"`, `"error"`, `"integer"`, `"bulk"`, `"null"`, `"array"`, `"map"`) with no value bytes anywhere near it, plus `Frame::log_len(&self) -> usize` for the "length" half of "kind + length" (byte length for `Simple`/`Error`/`Bulk`, element count for `Array`/`Map`, `0` for `Integer`/`Null`, which have no natural length). Both are genuinely new, pure, side-effect-free logic — unlike plans 12 and 13's log-only changes, these get a real TDD cycle: write a failing test against a stub, watch it fail, implement, watch it pass.

**CRITICAL CONSTRAINT** (repeated from the spec, and from plans 12/13): `engine` and `protocol` log key names and byte *lengths* only, **never value contents**. `crates/server/src/logging.rs`'s `fmt_value`/`redact_args` are not importable from `protocol` — `server` depends on `protocol`, not the other way around, so calling either from here would be a circular dependency even before considering that redaction policy is deliberately scoped to `crates/server` alone. Every log call this plan adds uses `Frame::kind()`/`Frame::log_len()` or an error's `Display` — never a raw `Bulk`'s bytes, never `{:?}` on a `Frame`.

**Tech Stack:** Rust 2021, `tracing 0.1` (already present in `crates/protocol`'s `Cargo.toml` since plan 01), `tokio_util::codec::Decoder`/`Encoder`.

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the Event catalogue's `Protocol` row.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting. The load-bearing ones for this plan specifically:

- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must both pass.
- Every pre-existing test must pass **unchanged** — in particular, every `decode_reassembles_...`/`decode_returns_none_...` test in both `codec.rs` and `rmp.rs` must keep passing with byte-for-byte identical return values; this plan only adds log calls alongside them, never changes what gets returned or consumed.
- No `format!` outside a log macro's argument list; every field uses `%`/`?` so formatting is lazy and never runs when the level is disabled.
- `Bytes` (and `Frame`) are never logged via `Debug`. `Frame::kind()` returns a `&'static str`; log it with `%` or bare (both are fine for a `&str`), never `?` on the `Frame` itself.
- Redaction policy lives only in `crates/server`. This plan calls neither `fmt_value` nor `redact_args`, and no code path in this plan reaches for one.
- Throughput at the default `info` level must stay within 2% of the baseline. `decode()` is called once per frame on every single incoming command in both wire protocols — this is at least as hot as anything plan 12 gated, arguably hotter (it runs before dispatch even begins). All three new log sites here are `trace!`/`warn!`, both disabled at `info`, so the default-level cost should be limited to the same "relaxed atomic load and a branch" the spec assumes. This plan does not include a dedicated benchmark task of its own — Task 3 runs the full workspace suite as its gate — but this residual risk should be folded into whichever plan performs this series' cumulative throughput sign-off, since `decode()`'s call frequency makes it a stronger regression candidate than either of plan 12's Engine-facade sites.

---

### Task 1: `Frame::kind()`/`Frame::log_len()`, and the frame-decoded trace in both codecs

**Files:**
- Modify: `crates/protocol/src/frame.rs` (add `impl Frame { kind, log_len }`)
- Modify: `crates/protocol/src/codec.rs` (`RespCodec::decode`, lines 171-180)
- Modify: `crates/protocol/src/rmp.rs` (`RmpCodec::decode`, lines 232-266)
- Test: `crates/protocol/src/frame.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn kind(&self) -> &'static str` and `pub fn log_len(&self) -> usize` on `Frame`. Task 2 and Task 3 in this plan use both; nothing outside this plan does yet, but they are `pub` so a future `server`-side log site (e.g. `dispatcher.rs`'s per-command trace, out of this plan's scope) can reuse them instead of re-deriving a kind string.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/protocol/src/frame.rs`, and add stub implementations above it so the tests compile and fail on behavior, not on a missing symbol:

```rust
impl Frame {
    /// A static discriminant naming this frame's kind, safe to log at any level without ever
    /// rendering value contents -- unlike `{:?}` on `Frame`, which would dump a `Bulk`'s or an
    /// `Array`'s actual payload byte-for-byte.
    pub fn kind(&self) -> &'static str {
        "unknown"
    }

    /// This frame's length for logging: byte length for `Simple`/`Error`/`Bulk`, element count
    /// for `Array`/`Map`, `0` for `Integer`/`Null` (neither has a natural length). Never the
    /// content itself.
    pub fn log_len(&self) -> usize {
        0
    }
}
```

```rust
    #[test]
    fn kind_names_each_variant_without_touching_its_contents() {
        assert_eq!(Frame::Simple("OK".into()).kind(), "simple");
        assert_eq!(Frame::Error("ERR".into()).kind(), "error");
        assert_eq!(Frame::Integer(1).kind(), "integer");
        assert_eq!(Frame::Bulk(Bytes::from_static(b"x")).kind(), "bulk");
        assert_eq!(Frame::Null.kind(), "null");
        assert_eq!(Frame::Array(vec![]).kind(), "array");
        assert_eq!(Frame::Map(vec![]).kind(), "map");
    }

    #[test]
    fn log_len_reports_byte_length_for_string_shaped_frames() {
        assert_eq!(Frame::Simple("hello".into()).log_len(), 5);
        assert_eq!(Frame::Error("boom".into()).log_len(), 4);
        assert_eq!(Frame::Bulk(Bytes::from_static(b"abc")).log_len(), 3);
    }

    #[test]
    fn log_len_reports_element_count_for_container_frames() {
        assert_eq!(
            Frame::Array(vec![Frame::Integer(1), Frame::Integer(2)]).log_len(),
            2
        );
        assert_eq!(
            Frame::Map(vec![(Frame::Integer(1), Frame::Integer(2))]).log_len(),
            1
        );
    }

    #[test]
    fn log_len_is_zero_for_frames_with_no_natural_length() {
        assert_eq!(Frame::Integer(42).log_len(), 0);
        assert_eq!(Frame::Null.log_len(), 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p protocol frame::tests::kind_names_each_variant_without_touching_its_contents
cargo test -p protocol frame::tests::log_len_reports_byte_length_for_string_shaped_frames
```

Expected: FAIL. `kind_names_each_variant_without_touching_its_contents` reports, for its first assertion:

```
assertion `left == right` failed
  left: "unknown"
 right: "simple"
```

`log_len_reports_byte_length_for_string_shaped_frames` reports a left value of `0` against a right value of `5` for its first assertion. `log_len_is_zero_for_frames_with_no_natural_length` passes vacuously against the stub (both sides already return `0`), which is fine — it is a boundary guard, not the driver of this task.

- [ ] **Step 3: Implement `kind` and `log_len`**

Replace the stub `impl Frame` block in `crates/protocol/src/frame.rs`:

```rust
impl Frame {
    /// A static discriminant naming this frame's kind, safe to log at any level without ever
    /// rendering value contents -- unlike `{:?}` on `Frame`, which would dump a `Bulk`'s or an
    /// `Array`'s actual payload byte-for-byte.
    pub fn kind(&self) -> &'static str {
        match self {
            Frame::Simple(_) => "simple",
            Frame::Error(_) => "error",
            Frame::Integer(_) => "integer",
            Frame::Bulk(_) => "bulk",
            Frame::Null => "null",
            Frame::Array(_) => "array",
            Frame::Map(_) => "map",
        }
    }

    /// This frame's length for logging: byte length for `Simple`/`Error`/`Bulk`, element count
    /// for `Array`/`Map`, `0` for `Integer`/`Null` (neither has a natural length). Never the
    /// content itself.
    pub fn log_len(&self) -> usize {
        match self {
            Frame::Simple(s) => s.len(),
            Frame::Error(s) => s.len(),
            Frame::Integer(_) => 0,
            Frame::Bulk(b) => b.len(),
            Frame::Null => 0,
            Frame::Array(items) => items.len(),
            Frame::Map(pairs) => pairs.len(),
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p protocol frame::tests
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all four new tests pass (plus every pre-existing `frame::tests` test), fmt clean, clippy clean.

- [ ] **Step 5: Add the frame-decoded trace to `RespCodec::decode`**

Replace the `Decoder for RespCodec` impl's `decode` method in `crates/protocol/src/codec.rs`:

```rust
impl Decoder for RespCodec {
    type Item = Frame;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, Self::Error> {
        match parse_frame(src)? {
            Some((frame, consumed)) => {
                src.advance(consumed);
                tracing::trace!(kind = frame.kind(), len = frame.log_len(), "frame decoded");
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }
}
```

- [ ] **Step 6: Add the frame-decoded trace to `RmpCodec::decode`**

In `crates/protocol/src/rmp.rs`, change the tail of `Decoder for RmpCodec`'s `decode` method from:

```rust
        src.advance(HEADER_LEN);
        let mut payload = src.split_to(payload_len as usize);
        let frame = decode_frame(&mut payload)?;
        Ok(Some(RmpMessage {
            request_id,
            msg_type,
            frame,
        }))
```

to:

```rust
        src.advance(HEADER_LEN);
        let mut payload = src.split_to(payload_len as usize);
        let frame = decode_frame(&mut payload)?;
        tracing::trace!(kind = frame.kind(), len = frame.log_len(), "frame decoded");
        Ok(Some(RmpMessage {
            request_id,
            msg_type,
            frame,
        }))
```

- [ ] **Step 7: Verify the whole crate**

```bash
cargo test -p protocol
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: every pre-existing `codec::tests` and `rmp::tests` test passes unchanged (in particular every `decodes_...`/`round_trips`/`a_request_round_trips_through_encode_decode` test, which asserts the exact returned `Frame`/`RmpMessage` — untouched by adding a trace call alongside the `Ok(Some(...))` return), the full workspace suite stays green, fmt clean, clippy clean.

- [ ] **Step 8: Commit**

```bash
git add crates/protocol/src/frame.rs crates/protocol/src/codec.rs crates/protocol/src/rmp.rs
git commit -m "feat(protocol): add Frame::kind/log_len and trace frame decode in both codecs"
```

---

### Task 2: Split-read reassembly trace in both codecs

**Files:**
- Modify: `crates/protocol/src/codec.rs` (`RespCodec::decode`)
- Modify: `crates/protocol/src/rmp.rs` (`RmpCodec::decode`)

**Interfaces:**
- Consumes: nothing new.
- Produces: the same `decode` signatures in both codecs, unchanged return values. `tokio_util::codec`'s `Framed`/`FramedRead` call sites in `crates/server` (`connection.rs`, `rmp_connection.rs`) keep working exactly as before — this task adds log calls only on the `Ok(None)` branches, never changing which branch is taken or what it returns.

Neither codec's split-read behavior is new logic — it already exists and is already covered by `decode_returns_none_on_a_bulk_string_split_mid_header`, `decode_reassembles_a_bulk_string_split_across_two_reads`, `decode_reassembles_a_full_command_split_across_three_reads`, and `decode_returns_none_when_only_the_array_header_has_arrived` in `codec.rs`, plus `decode_returns_none_when_only_part_of_the_header_has_arrived`, `decode_reassembles_a_header_split_across_two_reads`, and `decode_reassembles_a_payload_split_across_two_reads` in `rmp.rs`. Per this plan's Architecture section and this series' testing rule, a log line's presence is not itself unit-testable without capture infrastructure this project avoids adding; this task's TDD step is therefore to re-run that existing coverage before and after the change, confirming the trace calls change no return value.

- [ ] **Step 1: Run the existing reassembly tests to confirm current behavior**

```bash
cargo test -p protocol codec::tests::decode_returns_none
cargo test -p protocol codec::tests::decode_reassembles
cargo test -p protocol rmp::tests::decode_returns_none
cargo test -p protocol rmp::tests::decode_reassembles
```

Expected: PASS. These already pass today and establish the exact `Ok(None)` / eventual `Ok(Some(...))` contract that Step 2 and Step 3 must not disturb.

- [ ] **Step 2: Add the trace to `RespCodec::decode`'s `None` branch**

Replace `decode` in `crates/protocol/src/codec.rs` (building on Task 1 Step 5's version):

```rust
impl Decoder for RespCodec {
    type Item = Frame;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, Self::Error> {
        match parse_frame(src)? {
            Some((frame, consumed)) => {
                src.advance(consumed);
                tracing::trace!(kind = frame.kind(), len = frame.log_len(), "frame decoded");
                Ok(Some(frame))
            }
            None => {
                if !src.is_empty() {
                    // A genuine split read: some bytes have arrived but not enough to complete
                    // a frame yet. An empty buffer (nothing arrived at all) is not reassembly in
                    // progress, so it stays silent -- otherwise every idle connection between
                    // commands would trace on every poll.
                    tracing::trace!(
                        buffered = src.len(),
                        "split-read reassembly: awaiting more bytes"
                    );
                }
                Ok(None)
            }
        }
    }
}
```

- [ ] **Step 3: Add the trace to `RmpCodec::decode`'s two `None` branches**

In `crates/protocol/src/rmp.rs`, change the header-short-read branch:

```rust
        if src.len() < HEADER_LEN {
            return Ok(None);
        }
```

to:

```rust
        if src.len() < HEADER_LEN {
            if !src.is_empty() {
                tracing::trace!(
                    buffered = src.len(),
                    needed = HEADER_LEN,
                    "split-read reassembly: awaiting rmp header"
                );
            }
            return Ok(None);
        }
```

and the payload-short-read branch:

```rust
        let total_len = HEADER_LEN + payload_len as usize;
        if src.len() < total_len {
            src.reserve(total_len - src.len());
            return Ok(None);
        }
```

to:

```rust
        let total_len = HEADER_LEN + payload_len as usize;
        if src.len() < total_len {
            tracing::trace!(
                buffered = src.len(),
                needed = total_len,
                "split-read reassembly: awaiting rmp payload"
            );
            src.reserve(total_len - src.len());
            return Ok(None);
        }
```

- [ ] **Step 4: Re-run the reassembly tests to confirm nothing changed**

```bash
cargo test -p protocol codec::tests::decode_returns_none
cargo test -p protocol codec::tests::decode_reassembles
cargo test -p protocol rmp::tests::decode_returns_none
cargo test -p protocol rmp::tests::decode_reassembles
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: identical PASS results to Step 1 — same tests, same assertions, same return values. fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/protocol/src/codec.rs crates/protocol/src/rmp.rs
git commit -m "feat(protocol): trace split-read reassembly in both codecs"
```

---

### Task 3: Protocol error warn in both codecs

**Files:**
- Modify: `crates/protocol/src/codec.rs` (`RespCodec::decode`)
- Modify: `crates/protocol/src/rmp.rs` (`RmpCodec::decode`)

**Interfaces:**
- Consumes: nothing new.
- Produces: the same `decode` signatures, unchanged `Err` values and `io::ErrorKind`s. Every existing error-path test (`unknown_type_byte_is_an_error_not_a_panic` in `codec.rs`; `a_bad_magic_is_a_decode_error`, `an_unsupported_version_is_a_decode_error`, `a_bad_msg_type_byte_is_a_decode_error`, `a_payload_len_over_the_max_is_a_decode_error_without_waiting_for_the_bytes` in `rmp.rs`) keeps receiving the identical `Err` it does today — this task wraps the existing error return points with a `warn!` before propagating, it does not change what gets returned.

`parse_frame` is recursive (for `Frame::Array`), and any error raised inside a nested call already bubbles up through the outer `parse_frame(src)?` in `RespCodec::decode` via `?` — so wrapping that single call site catches every RESP decode error, at every nesting depth, without touching the recursive function itself. `RmpCodec::decode`'s errors come from a handful of explicit `return Err(...)` sites plus one `?` on `decode_frame(&mut payload)` (itself recursive via `decode_frame_inner`) — each is wrapped individually below since, unlike `codec.rs`, `rmp.rs`'s `decode` has several distinct error-producing statements rather than one delegating call.

No new pure logic is introduced by wrapping an existing `Err` path in a log call; per this series' testing rule, the existing error-path tests listed above are what this task verifies against, both before and after.

- [ ] **Step 1: Run the existing error-path tests to confirm current behavior**

```bash
cargo test -p protocol codec::tests::unknown_type_byte_is_an_error_not_a_panic
cargo test -p protocol rmp::tests::a_bad_magic_is_a_decode_error
cargo test -p protocol rmp::tests::an_unsupported_version_is_a_decode_error
cargo test -p protocol rmp::tests::a_bad_msg_type_byte_is_a_decode_error
cargo test -p protocol rmp::tests::a_payload_len_over_the_max_is_a_decode_error_without_waiting_for_the_bytes
```

Expected: PASS. These pin the exact `is_err()` contract Steps 2-3 must not disturb.

- [ ] **Step 2: Wrap `RespCodec::decode`'s error path**

Replace `decode` in `crates/protocol/src/codec.rs` (building on Task 2 Step 2's version):

```rust
impl Decoder for RespCodec {
    type Item = Frame;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, Self::Error> {
        match parse_frame(src) {
            Ok(Some((frame, consumed))) => {
                src.advance(consumed);
                tracing::trace!(kind = frame.kind(), len = frame.log_len(), "frame decoded");
                Ok(Some(frame))
            }
            Ok(None) => {
                if !src.is_empty() {
                    tracing::trace!(
                        buffered = src.len(),
                        "split-read reassembly: awaiting more bytes"
                    );
                }
                Ok(None)
            }
            Err(e) => {
                // `e`'s Display text (e.g. "unknown RESP type byte: 0x40") never contains stored
                // value bytes -- `parse_frame`'s error messages are fixed strings, at most
                // embedding a single type-tag byte or a malformed length/integer literal, never
                // the bulk-string body itself.
                tracing::warn!(error = %e, "resp protocol error");
                Err(e)
            }
        }
    }
}
```

- [ ] **Step 3: Wrap `RmpCodec::decode`'s error paths**

Replace the whole `Decoder for RmpCodec` impl's `decode` method in `crates/protocol/src/rmp.rs` (building on Task 2 Step 3's version):

```rust
impl Decoder for RmpCodec {
    type Item = RmpMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<RmpMessage>> {
        if src.len() < HEADER_LEN {
            if !src.is_empty() {
                tracing::trace!(
                    buffered = src.len(),
                    needed = HEADER_LEN,
                    "split-read reassembly: awaiting rmp header"
                );
            }
            return Ok(None);
        }
        if src[0..2] != MAGIC {
            let err = invalid_data("bad rmp magic");
            tracing::warn!(error = %err, "rmp protocol error");
            return Err(err);
        }
        if src[2] != VERSION {
            let err = invalid_data("unsupported rmp version");
            tracing::warn!(error = %err, "rmp protocol error");
            return Err(err);
        }
        let msg_type = match MsgType::from_byte(src[3]) {
            Ok(t) => t,
            Err(err) => {
                tracing::warn!(error = %err, "rmp protocol error");
                return Err(err);
            }
        };
        let request_id = u64::from_be_bytes(src[4..12].try_into().unwrap());
        let payload_len = u32::from_be_bytes(src[12..16].try_into().unwrap());
        if payload_len > MAX_RMP_FRAME_LEN {
            let err = invalid_data("rmp payload_len exceeds MAX_RMP_FRAME_LEN");
            tracing::warn!(error = %err, "rmp protocol error");
            return Err(err);
        }
        let total_len = HEADER_LEN + payload_len as usize;
        if src.len() < total_len {
            tracing::trace!(
                buffered = src.len(),
                needed = total_len,
                "split-read reassembly: awaiting rmp payload"
            );
            src.reserve(total_len - src.len());
            return Ok(None);
        }
        src.advance(HEADER_LEN);
        let mut payload = src.split_to(payload_len as usize);
        let frame = match decode_frame(&mut payload) {
            Ok(f) => f,
            Err(err) => {
                tracing::warn!(error = %err, "rmp protocol error");
                return Err(err);
            }
        };
        tracing::trace!(kind = frame.kind(), len = frame.log_len(), "frame decoded");
        Ok(Some(RmpMessage {
            request_id,
            msg_type,
            frame,
        }))
    }
}
```

- [ ] **Step 4: Verify the whole crate and the whole workspace**

```bash
cargo test -p protocol
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: every pre-existing `protocol` test passes unchanged, including every error-path test listed in Step 1 and every reassembly/round-trip test from Tasks 1-2; the full workspace suite stays green; fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/protocol/src/codec.rs crates/protocol/src/rmp.rs
git commit -m "feat(protocol): warn-log protocol decode errors in both codecs"
```

---

## Next plan

[`15-aof-events.md`](15-aof-events.md) — adds `trace`-level AOF append offset/bytes, `debug`-level fsync, and `info`-level rewrite start/finish and recovery replay summary events to `crates/server/src/aof.rs`.
