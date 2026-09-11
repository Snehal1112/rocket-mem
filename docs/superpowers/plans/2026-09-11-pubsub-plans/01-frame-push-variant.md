# Pub/Sub Plan 01: `Frame::Push` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `Frame::Push` variant to `crates/protocol`, encoding as RESP3's `>` type and
falling back to a plain RESP2 array — the wire-format primitive every later plan in this series
uses to deliver `SUBSCRIBE`/`PUBLISH` messages.

**Architecture:** `protocol::Frame` (`crates/protocol/src/frame.rs`) gains one new variant,
`Push(Vec<Frame>)`, alongside the existing `Map(Vec<(Frame, Frame)>)` — which already has a
per-protocol encoding split this plan mirrors exactly. `RespCodec`'s `Encoder<Frame>` impl
(`crates/protocol/src/codec.rs`) gets one new match arm. No decoder change: nothing ever needs to
*parse* a `>`-typed frame — it is a server-to-client-only type, never sent by a client.

**Tech Stack:** Rust, `bytes::Bytes`/`BytesMut`, `tokio_util::codec`.

**Spec:** [../../specs/2026-09-11-pubsub-spec.md](../../specs/2026-09-11-pubsub-spec.md) — see
its "`Frame::Push`" section.

## Global Constraints

Every plan in this `2026-09-11-pubsub-plans/` series inherits these; later plans link back here
instead of restating them.

- **CI gates, all three must pass before any commit:**
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings` (strict: zero warnings, including in
    test code and dead-code lints)
  - `cargo test --workspace`
- **TDD discipline:** for every behavior-carrying step, write the failing test first, run it and
  confirm the failure, then write the minimal implementation, then run it and confirm the pass,
  then commit. Do not write implementation code before its test exists and has been observed to
  fail.
- **Comment style:** short, complete sentences ending in a period. Comment the *why* (a
  non-obvious constraint, an invariant, a workaround), never the *what* — well-named identifiers
  already say what the code does. No multi-paragraph doc comments.
- **Log-capture tests live in `crates/server/tests/logging.rs`**, using that file's existing
  `capture_during` helper — never inline `tracing_subscriber` wiring inside `dispatcher.rs`'s own
  test module.
- **Redaction policy:** engine/protocol code may log key/channel *names* and byte lengths, never
  value/message *contents*. Every new `tracing::debug!`/`trace!` call this series adds must
  respect this.
- **Commit via the `1-git-commit` skill** (or an equivalent single, well-scoped commit per task)
  — never batch multiple tasks' changes into one commit, and never commit unrelated
  already-modified files from another in-progress session's working tree. Check `git status`
  before staging; if files you did not touch are already modified, leave them alone and stage
  only the paths this task actually created or edited.
- **Verify symbols before citing them.** If an implementation step in this plan turns out to
  reference a function, field, or file/line that no longer matches the checked-out code (the repo
  moves fast — other sessions land commits concurrently), re-read the actual current file before
  writing code against it rather than trusting the plan's line numbers blindly.

---

### Task 1: Add `Frame::Push` and its `kind()`/`log_len()` cases

**Files:**
- Modify: `crates/protocol/src/frame.rs`

**Interfaces:**
- Produces: `Frame::Push(Vec<Frame>)` — a new enum variant every later plan in this series
  constructs to build `SUBSCRIBE`/`UNSUBSCRIBE`/`PSUBSCRIBE`/`PUNSUBSCRIBE`/`PUBLISH`-delivery
  messages. `Frame::kind()` returns `"push"` for it; `Frame::log_len()` returns its element count
  (matching `Array`'s and `Map`'s existing convention).

- [ ] **Step 1: Write the failing tests**

Add to `crates/protocol/src/frame.rs`'s existing `#[cfg(test)] mod tests` block (the file
currently ends at line 137 with `}` closing that module — add these before that closing brace,
alongside the existing `kind_names_each_variant_without_touching_its_contents` and
`log_len_reports_element_count_for_container_frames` tests):

```rust
    #[test]
    fn push_frame_holds_nested_frames() {
        let f = Frame::Push(vec![
            Frame::Bulk(Bytes::from_static(b"message")),
            Frame::Bulk(Bytes::from_static(b"chan")),
        ]);
        assert_eq!(
            f,
            Frame::Push(vec![
                Frame::Bulk(Bytes::from_static(b"message")),
                Frame::Bulk(Bytes::from_static(b"chan")),
            ])
        );
    }

    #[test]
    fn push_frames_are_not_equal_to_array_frames_with_the_same_flattened_content() {
        let push = Frame::Push(vec![Frame::Integer(1), Frame::Integer(2)]);
        let array = Frame::Array(vec![Frame::Integer(1), Frame::Integer(2)]);
        assert_ne!(push, array);
    }

    #[test]
    fn kind_names_push_without_touching_its_contents() {
        assert_eq!(Frame::Push(vec![]).kind(), "push");
    }

    #[test]
    fn log_len_reports_element_count_for_push_frames() {
        assert_eq!(
            Frame::Push(vec![Frame::Integer(1), Frame::Integer(2)]).log_len(),
            2
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p protocol frame::tests -- --exact push_frame_holds_nested_frames kind_names_push_without_touching_its_contents log_len_reports_element_count_for_push_frames push_frames_are_not_equal_to_array_frames_with_the_same_flattened_content`
Expected: FAIL to compile — `Frame::Push` does not exist yet (`no variant named 'Push' found for enum 'Frame'`).

- [ ] **Step 3: Add the variant and its two match arms**

In `crates/protocol/src/frame.rs`, add `Push(Vec<Frame>)` to the enum (after `Map`):

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Bytes),
    Null,
    Array(Vec<Frame>),
    Map(Vec<(Frame, Frame)>),
    Push(Vec<Frame>),
}
```

Add one arm to `kind()` (after the `Map` arm):

```rust
            Frame::Push(_) => "push",
```

Add one arm to `log_len()` (after the `Map` arm):

```rust
            Frame::Push(items) => items.len(),
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p protocol frame::tests`
Expected: PASS — every test in the module, old and new.

- [ ] **Step 5: Commit**

```bash
git add crates/protocol/src/frame.rs
git commit -m "$(cat <<'EOF'
Add Frame::Push variant

RESP3 pub/sub messages need their own frame kind, distinct from
Array, so client libraries can route them out-of-band from command
replies. This adds the variant and its kind()/log_len() cases; the
wire encoding lands in the next task.
EOF
)"
```

---

### Task 2: Encode `Frame::Push` as RESP3's `>` type, falling back to a plain array under RESP2

**Files:**
- Modify: `crates/protocol/src/codec.rs`

**Interfaces:**
- Consumes: `Frame::Push(Vec<Frame>)` from Task 1; `Protocol::Resp2`/`Protocol::Resp3` (already
  defined at `crates/protocol/src/codec.rs:7-11`).
- Produces: `RespCodec::encode` correctly serializes `Frame::Push` — `>N\r\n<item1><item2>...`
  under `Protocol::Resp3`, `*N\r\n<item1><item2>...` under `Protocol::Resp2`. Every later plan's
  tests that assert on the bytes a subscribed connection receives depend on this.

- [ ] **Step 1: Write the failing tests**

Add to `crates/protocol/src/codec.rs`'s test module, alongside the existing
`encodes_map_as_flattened_array_under_resp2`/`encodes_map_natively_under_resp3` tests (find them
via `grep -n "encodes_map_as_flattened_array_under_resp2" crates/protocol/src/codec.rs` to locate
the exact insertion point — read the surrounding test to copy its `RespCodec { protocol: ... }`
construction pattern exactly):

```rust
    #[test]
    fn encodes_push_as_flattened_array_under_resp2() {
        let mut codec = RespCodec::default(); // Protocol::Resp2 is the derived Default
        let mut buf = BytesMut::new();
        codec
            .encode(
                Frame::Push(vec![
                    Frame::Bulk(Bytes::from_static(b"message")),
                    Frame::Bulk(Bytes::from_static(b"chan")),
                ]),
                &mut buf,
            )
            .unwrap();
        assert_eq!(
            &buf[..],
            b"*2\r\n$7\r\nmessage\r\n$4\r\nchan\r\n".as_slice()
        );
    }

    #[test]
    fn encodes_push_natively_under_resp3() {
        let mut codec = RespCodec {
            protocol: Protocol::Resp3,
        };
        let mut buf = BytesMut::new();
        codec
            .encode(
                Frame::Push(vec![
                    Frame::Bulk(Bytes::from_static(b"message")),
                    Frame::Bulk(Bytes::from_static(b"chan")),
                ]),
                &mut buf,
            )
            .unwrap();
        assert_eq!(
            &buf[..],
            b">2\r\n$7\r\nmessage\r\n$4\r\nchan\r\n".as_slice()
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p protocol codec::tests -- --exact encodes_push_as_flattened_array_under_resp2 encodes_push_natively_under_resp3`
Expected: FAIL to compile — `no variant named 'Push' found for enum 'Frame'` is now resolved by
Task 1, so this should instead fail because `encode`'s match is non-exhaustive
(`non-exhaustive patterns: 'Frame::Push(_)' not covered`), or simply produce wrong/no output if
the compiler doesn't error first. Confirm the actual failure mode before proceeding.

- [ ] **Step 3: Add the encoder arm**

In `crates/protocol/src/codec.rs`'s `Encoder<Frame> for RespCodec::encode`, add a new arm after
the existing `Frame::Map(pairs) => match self.protocol { ... }` arm:

```rust
            Frame::Push(items) => match self.protocol {
                Protocol::Resp2 => {
                    dst.put_u8(b'*');
                    dst.put_slice(items.len().to_string().as_bytes());
                    dst.put_slice(b"\r\n");
                    for item in items {
                        self.encode(item, dst)?;
                    }
                }
                Protocol::Resp3 => {
                    dst.put_u8(b'>');
                    dst.put_slice(items.len().to_string().as_bytes());
                    dst.put_slice(b"\r\n");
                    for item in items {
                        self.encode(item, dst)?;
                    }
                }
            },
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p protocol`
Expected: PASS — the whole `protocol` crate's test suite, including both new tests and everything
from Task 1.

- [ ] **Step 5: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three pass clean. This is the last task in this plan touching `crates/protocol`, so
this is the point to confirm nothing downstream (RESP2/RESP3 negotiation tests, RMP, etc.) broke
from adding a new `Frame` variant — an unmatched `Frame::Push` anywhere else that pattern-matches
`Frame` exhaustively would show up here as a compile error.

- [ ] **Step 6: Commit**

```bash
git add crates/protocol/src/codec.rs
git commit -m "$(cat <<'EOF'
Encode Frame::Push as RESP3's '>' type

Falls back to a plain '*' array under RESP2, mirroring Frame::Map's
existing per-protocol split. Real RESP3 client libraries watch for
the '>' byte to route pub/sub messages out-of-band from command
replies.
EOF
)"
```

## Next plan

[02-pubsub-registry.md](02-pubsub-registry.md) — the `PubSubRegistry` type and its wiring onto
`ReplicationHandle`.
