# MULTI/EXEC Transactions — Plan 03: AOF and Replication Framing

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wrap a transaction's AOF/replication output in `MULTI`/`EXEC` markers, so a `kill -9`
mid-`EXEC` never replays half a transaction — and teach the two places that apply a raw decoded
frame stream directly (`aof::replay_with_stats` and `replication::sync_once`) to buffer between
those markers and apply (or discard) a transaction as one unit.

**Architecture:** `handle_exec` (Plan 02) already runs every queued command through
`dispatch_and_log_gated` unchanged, so each queued *write* already gets its own correct AOF
append and replica broadcast during the loop — that part needs no change. This plan only adds the
markers *around* that loop, and a new shared `TransactionGrouper` (new file,
`crates/server/src/transaction_grouping.rs`) that both `aof::replay_with_stats` and
`replication::sync_once` use to recognize those markers and buffer between them, so a stream that
ends mid-transaction is discarded exactly like today's single-frame corrupt tail is.

**Tech Stack:** Rust 2021, the existing `protocol::codec::RespCodec` decoder.

**Spec:** [`../../specs/2026-09-10-multi-exec-transactions-spec.md`](../../specs/2026-09-10-multi-exec-transactions-spec.md)

**Global Constraints:** See
[`01-session-state-and-queuing.md`](01-session-state-and-queuing.md)'s "Global Constraints"
section — every rule there applies here too. Task 1, Step 1 re-confirms the build is still green
(baseline now `BASELINE + 18` from Plan 02) before continuing.

---

### Task 1: `MULTI`/`EXEC` AOF markers around the batch

**Files:**
- Modify: `crates/server/src/dispatcher.rs` — `handle_exec`
- Test: `crates/server/src/dispatcher.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::aof::encode_frame(&Frame) -> std::io::Result<Vec<u8>>`,
  `aof.append_encoded(Vec<u8>) -> std::io::Result<()>`,
  `replication.advance_master_repl_offset(u64) -> u64`,
  `replication.registry.broadcast(bytes::Bytes)`, `extract_write_command_name` — all pre-existing.
- Produces: `fn append_transaction_marker(aof: &crate::aof::AofWriter, replication:
  &crate::replication::ReplicationHandle, name: &'static str)` — Task 2's replay work and Task
  3's follower-apply work both need to recognize exactly the bytes this function writes, so its
  frame shape (`Frame::Array(vec![Frame::Bulk(name)])`, no arguments) is load-bearing for both.

- [ ] **Step 1: Confirm the workspace still builds**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: PASS, total `BASELINE + 18`.

- [ ] **Step 2: Write the failing tests**

```rust
    #[test]
    fn exec_wraps_its_writes_in_multi_and_exec_aof_markers() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k1", b"v1"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"SET", b"k2", b"v2"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"EXEC"]), &session, 1);
        aof.fsync().unwrap();

        let on_disk = std::fs::read(_dir.path().join("test.aof")).unwrap();
        let expected = [
            crate::aof::encode_frame(&cmd(&[b"MULTI"])).unwrap(),
            crate::aof::encode_frame(&cmd(&[b"SET", b"k1", b"v1"])).unwrap(),
            crate::aof::encode_frame(&cmd(&[b"SET", b"k2", b"v2"])).unwrap(),
            crate::aof::encode_frame(&cmd(&[b"EXEC"])).unwrap(),
        ]
        .concat();
        assert_eq!(on_disk, expected);
    }

    #[test]
    fn a_read_only_transaction_writes_nothing_to_the_aof_at_all() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"GET", b"nope"]), &session, 1);
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"EXEC"]), &session, 1);
        aof.fsync().unwrap();

        let on_disk = std::fs::read(_dir.path().join("test.aof")).unwrap();
        assert!(on_disk.is_empty(), "a transaction with no writes must not touch the AOF at all");
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --lib dispatcher::tests -- exec_wraps_its_writes a_read_only_transaction_writes_nothing
```

Expected: `exec_wraps_its_writes_in_multi_and_exec_aof_markers` FAILs (no markers written yet —
`on_disk` is missing the `MULTI`/`EXEC` frames on both ends). `a_read_only_transaction_writes_
nothing_to_the_aof_at_all` already PASSes today (no write commands means `dispatch_and_log_gated`
already appends nothing) — that is expected; it exists to pin the behavior this task must not
break once markers are added.

- [ ] **Step 4: Implement `append_transaction_marker` and wire it into `handle_exec`**

Add next to `handle_exec`:

```rust
/// Writes a bare `MULTI`/`EXEC` marker frame (no arguments) to the AOF and broadcasts it to
/// replicas, exactly like `dispatch_and_log_gated`'s own per-command append does for a real
/// write, but without going through the command dispatcher -- these two frames are never
/// dispatched as commands by anything but a replayer, which special-cases them (Task 2).
fn append_transaction_marker(
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    name: &'static str,
) {
    let marker = Frame::Array(vec![Frame::Bulk(Bytes::from_static(name.as_bytes()))]);
    let encoded = match crate::aof::encode_frame(&marker) {
        Ok(encoded) => encoded,
        Err(e) => {
            tracing::error!(error = %e, marker = name, "aof encode failed");
            return;
        }
    };
    if let Err(e) = aof.append_encoded(encoded.clone()) {
        tracing::error!(error = %e, marker = name, "aof append failed");
    }
    let bytes = Bytes::from(encoded);
    replication.advance_master_repl_offset(bytes.len() as u64);
    replication.registry.broadcast(bytes);
}
```

In `handle_exec`, after computing `queued` and before computing `shard_set`, add:

```rust
    let has_write = queued
        .iter()
        .any(|f| extract_write_command_name(f).is_some());
```

Wrap the existing command loop with the markers, guarded by `has_write` (a batch with no writes
gets no markers, matching a single read command's own "nothing logged" behavior):

```rust
    if has_write {
        append_transaction_marker(aof, replication, "MULTI");
    }
    let mut replies = Vec::with_capacity(queued.len());
    for queued_frame in queued.iter() {
        replies.push(dispatch_and_log_gated(
            engine,
            aof,
            replication,
            queued_frame.clone(),
            session,
            client_id,
            false,
        ));
    }
    if has_write {
        append_transaction_marker(aof, replication, "EXEC");
    }
    drop(_batch_guard);
```

(this replaces the loop's existing `for queued_frame in queued.iter() { ... }` block from Plan
02 Task 1 — only the two marker calls are new; the loop body itself is unchanged.)

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --lib dispatcher::tests -- exec_wraps_its_writes a_read_only_transaction_writes_nothing
```

Expected: PASS, both.

- [ ] **Step 6: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, total `BASELINE + 18 + 2 = BASELINE + 20`.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(transactions): wrap EXEC's AOF/replication output in MULTI/EXEC markers

A transaction with at least one write now brackets its per-command
AOF appends with bare MULTI and EXEC marker frames, under the same
batch guard. A transaction with no writes still logs nothing, matching
a single read command. Replay and follower-apply don't understand
these markers yet -- Tasks 2 and 3 teach them."
```

---

### Task 2: `TransactionGrouper` and `aof::replay_with_stats`

**Files:**
- Create: `crates/server/src/transaction_grouping.rs`
- Modify: `crates/server/src/lib.rs` (`pub mod transaction_grouping;`), `crates/server/src/aof.rs`
  (`replay_with_stats`)
- Test: `crates/server/src/transaction_grouping.rs` (`#[cfg(test)] mod tests`),
  `crates/server/src/aof.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::dispatcher::upper_name(&[u8]) -> Option<CommandName>` (`pub(crate)`, already
  used identically elsewhere).
- Produces:
  ```rust
  pub(crate) struct TransactionGrouper { /* private */ }
  impl TransactionGrouper {
      pub(crate) fn new() -> Self;
      /// Feeds one decoded frame. `Ok(frames)` are the frames to apply now -- empty while
      /// buffering, one frame for an ordinary non-transaction command, or every buffered command
      /// in order once a transaction's matching EXEC arrives. `Err` names what was wrong (an EXEC
      /// with no MULTI, or a nested MULTI) -- treat it exactly like today's "corrupt tail".
      pub(crate) fn feed(&mut self, frame: protocol::Frame) -> Result<Vec<protocol::Frame>, &'static str>;
      /// True while a MULTI has been seen with no matching EXEC yet.
      pub(crate) fn is_mid_transaction(&self) -> bool;
  }
  ```
  Task 3 uses the same type, unchanged, in `replication::sync_once`.

- [ ] **Step 1: Confirm the workspace still builds**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: PASS, total `BASELINE + 20`.

- [ ] **Step 2: Write the failing tests for `TransactionGrouper`**

Create `crates/server/src/transaction_grouping.rs`:

```rust
//! Groups a decoded frame stream into transaction units, for the two places that apply frames
//! directly via `dispatcher::dispatch` outside the normal gated path: `aof::replay_with_stats`
//! and `replication::sync_once`. See
//! `docs/superpowers/specs/2026-09-10-multi-exec-transactions-spec.md`.

use bytes::Bytes;
use protocol::Frame;

fn marker(frame: &Frame) -> Option<&'static str> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let [Frame::Bulk(name_bytes)] = items.as_slice() else {
        return None; // MULTI/EXEC markers always have exactly zero arguments
    };
    match crate::dispatcher::upper_name(name_bytes)?.as_str() {
        "MULTI" => Some("MULTI"),
        "EXEC" => Some("EXEC"),
        _ => None,
    }
}

pub(crate) struct TransactionGrouper {
    buffer: Option<Vec<Frame>>,
}

impl TransactionGrouper {
    pub(crate) fn new() -> Self {
        Self { buffer: None }
    }

    pub(crate) fn is_mid_transaction(&self) -> bool {
        self.buffer.is_some()
    }

    pub(crate) fn feed(&mut self, frame: Frame) -> Result<Vec<Frame>, &'static str> {
        match marker(&frame) {
            Some("MULTI") => {
                if self.buffer.is_some() {
                    return Err("MULTI seen while already buffering a transaction");
                }
                self.buffer = Some(Vec::new());
                Ok(Vec::new())
            }
            Some("EXEC") => self
                .buffer
                .take()
                .ok_or("EXEC seen with no matching MULTI"),
            _ => {
                if let Some(buffer) = self.buffer.as_mut() {
                    buffer.push(frame);
                    Ok(Vec::new())
                } else {
                    Ok(vec![frame])
                }
            }
        }
    }
}

fn bulk(s: &str) -> Frame {
    Frame::Bulk(Bytes::from(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(parts: &[&str]) -> Frame {
        Frame::Array(parts.iter().map(|p| bulk(p)).collect())
    }

    #[test]
    fn an_ordinary_frame_outside_any_transaction_passes_through_immediately() {
        let mut grouper = TransactionGrouper::new();
        let result = grouper.feed(cmd(&["SET", "k", "v"])).unwrap();
        assert_eq!(result, vec![cmd(&["SET", "k", "v"])]);
        assert!(!grouper.is_mid_transaction());
    }

    #[test]
    fn a_complete_transaction_yields_every_buffered_command_at_exec_in_order() {
        let mut grouper = TransactionGrouper::new();
        assert_eq!(grouper.feed(cmd(&["MULTI"])).unwrap(), Vec::<Frame>::new());
        assert!(grouper.is_mid_transaction());
        assert_eq!(grouper.feed(cmd(&["SET", "a", "1"])).unwrap(), Vec::<Frame>::new());
        assert_eq!(grouper.feed(cmd(&["SET", "b", "2"])).unwrap(), Vec::<Frame>::new());
        let result = grouper.feed(cmd(&["EXEC"])).unwrap();
        assert_eq!(result, vec![cmd(&["SET", "a", "1"]), cmd(&["SET", "b", "2"])]);
        assert!(!grouper.is_mid_transaction());
    }

    #[test]
    fn exec_with_no_matching_multi_is_an_error() {
        let mut grouper = TransactionGrouper::new();
        assert_eq!(
            grouper.feed(cmd(&["EXEC"])).unwrap_err(),
            "EXEC seen with no matching MULTI"
        );
    }

    #[test]
    fn a_nested_multi_is_an_error() {
        let mut grouper = TransactionGrouper::new();
        grouper.feed(cmd(&["MULTI"])).unwrap();
        assert_eq!(
            grouper.feed(cmd(&["MULTI"])).unwrap_err(),
            "MULTI seen while already buffering a transaction"
        );
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --lib transaction_grouping::tests
```

Expected: **compile error** — the module does not exist in `lib.rs` yet.

- [ ] **Step 4: Register the module**

In `crates/server/src/lib.rs`, add `pub mod transaction_grouping;` alphabetically after `pub mod
tls;` (or in whatever position keeps the existing alphabetical list sorted — check the current
list with `grep -n "^pub mod" crates/server/src/lib.rs` first).

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --lib transaction_grouping::tests
```

Expected: PASS, all four.

- [ ] **Step 6: Write the failing tests for `replay_with_stats`**

Add to `crates/server/src/aof.rs`'s `mod tests`, using this file's existing `write_raw`/`frame`
helpers (confirmed at `encode_frame_matches_append_s_existing_wire_format` and the corrupt-tail
test around line 1080):

```rust
    #[test]
    fn replay_applies_a_complete_transaction_as_one_unit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        write_raw(&path, &encode_frame(&frame(&[b"MULTI"])).unwrap());
        write_raw(&path, &encode_frame(&frame(&[b"SET", b"a", b"1"])).unwrap());
        write_raw(&path, &encode_frame(&frame(&[b"SET", b"b", b"2"])).unwrap());
        write_raw(&path, &encode_frame(&frame(&[b"EXEC"])).unwrap());

        let engine = Engine::new();
        replay(&path, &engine, 0).unwrap();

        assert_eq!(engine.get(b"a"), Some(Value::String(Bytes::from_static(b"1"))));
        assert_eq!(engine.get(b"b"), Some(Value::String(Bytes::from_static(b"2"))));
    }

    #[test]
    fn replay_discards_a_transaction_truncated_before_its_exec_and_truncates_the_file_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let complete_prefix = encode_frame(&frame(&[b"SET", b"before", b"1"])).unwrap();
        write_raw(&path, &complete_prefix);
        write_raw(&path, &encode_frame(&frame(&[b"MULTI"])).unwrap());
        write_raw(&path, &encode_frame(&frame(&[b"SET", b"mid-tx", b"1"])).unwrap());
        // No EXEC -- simulates a kill -9 mid-transaction. No corrupt bytes either: this is a
        // structurally valid RESP stream that simply never closes its transaction.

        let engine = Engine::new();
        replay(&path, &engine, 0).unwrap();

        assert_eq!(engine.get(b"before"), Some(Value::String(Bytes::from_static(b"1"))));
        assert_eq!(
            engine.get(b"mid-tx"),
            None,
            "a command inside an unterminated transaction must never apply"
        );
        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(
            on_disk, complete_prefix,
            "the file must be truncated back to before the unterminated MULTI, not just before \
             the last decodable frame"
        );
    }
```

- [ ] **Step 7: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --lib aof::tests -- replay_applies_a_complete_transaction replay_discards_a_transaction_truncated
```

Expected: FAIL — `replay_with_stats` dispatches `MULTI`/`EXEC` as ordinary commands today (they
don't exist as commands outside `intercept_for_transaction`'s session-bound path, and
`dispatch()` has no `Session` at all), so `MULTI`/`EXEC`, sent through `dispatch()` directly,
each independently return `ERR unknown command` and every "queued" command in between applies
immediately and unconditionally — `a` and `b` still end up set in the first test (so that one may
coincidentally pass), but the second test fails: `mid-tx` gets applied immediately since nothing
buffers it, and the file is never truncated.

- [ ] **Step 8: Wire `TransactionGrouper` into `replay_with_stats`**

In `crates/server/src/aof.rs`, inside `replay_with_stats`'s decode loop, replace the body of the
`Ok(Some(frame)) => { ... }` arm. Today it reads:

```rust
            Ok(Some(frame)) => {
                valid_len += before - buf.len();
                commands += 1;
                let mut protocol = protocol::codec::Protocol::default();
                crate::dispatcher::dispatch(engine, frame, &mut protocol, 0);
            }
```

Replace it with:

```rust
            Ok(Some(frame)) => {
                consumed_total += before - buf.len();
                match grouper.feed(frame) {
                    Ok(to_apply) => {
                        if !grouper.is_mid_transaction() {
                            // Either an ordinary command (`to_apply` has one frame) or a
                            // transaction that just closed on this EXEC (`to_apply` has every
                            // buffered command) -- both are a safe truncation point.
                            valid_len = consumed_total;
                        }
                        for f in to_apply {
                            commands += 1;
                            let mut protocol = protocol::codec::Protocol::default();
                            crate::dispatcher::dispatch(engine, f, &mut protocol, 0);
                        }
                    }
                    Err(reason) => {
                        tracing::warn!(reason, "aof replay: malformed transaction framing");
                        tail_reason = "corrupt";
                        break;
                    }
                }
            }
```

Add `let mut consumed_total = start;` and `let mut grouper =
crate::transaction_grouping::TransactionGrouper::new();` next to the existing `let mut valid_len =
start;` line, and delete that original line (`consumed_total` replaces it as the running total;
`valid_len` now only ever gets *assigned* `consumed_total`'s current value at a safe boundary,
never incremented directly).

After the loop, if `grouper.is_mid_transaction()` is still true (the file ended with a `MULTI`
that never got its `EXEC`), that is exactly today's "incomplete tail" case and needs no new code:
`valid_len` was never advanced past the point before that `MULTI`, so the existing
`if valid_len < raw.len() { ... truncate ... }` block below already truncates correctly, with
`tail_reason` staying at its default `"incomplete"`.

- [ ] **Step 9: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --lib aof::tests -- replay_applies_a_complete_transaction replay_discards_a_transaction_truncated
```

Expected: PASS, both.

- [ ] **Step 10: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, total `BASELINE + 20 + 4 + 2 = BASELINE + 26`.

- [ ] **Step 11: Commit**

```bash
git add crates/server/src/transaction_grouping.rs crates/server/src/lib.rs crates/server/src/aof.rs
git commit -m "feat(transactions): apply a replayed transaction as one crash-safe unit

New TransactionGrouper buffers frames between MULTI and EXEC, shared
by aof::replay_with_stats here and replication::sync_once next. A
transaction truncated before its EXEC is discarded and the file is
truncated to before the MULTI, not just before the last decodable
frame."
```

---

### Task 3: `replication::sync_once` follower-apply grouping

**Files:**
- Modify: `crates/server/src/replication.rs` — `sync_once`'s frame-apply loop
- Test: `crates/server/src/replication.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::transaction_grouping::TransactionGrouper` from Task 2, unchanged.
- Produces: nothing new for later tasks — this is the last piece of the spec's AOF/replication
  framing section.

- [ ] **Step 1: Confirm the workspace still builds**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: PASS, total `BASELINE + 26`.

- [ ] **Step 2: Write the failing test**

This reuses `sync_once_loads_the_snapshot_then_applies_streamed_frames`'s exact mock-leader
harness (confirmed at `replication.rs:1535`), just with a `MULTI`/`SET`/`SET`/`EXEC` stream
instead of one bare `SET`:

```rust
    #[tokio::test]
    async fn sync_once_applies_a_streamed_transaction_as_one_unit() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let fake_leader = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut psync_bytes = [0u8; 15];
            socket.read_exact(&mut psync_bytes).await.unwrap();

            let snapshot_engine = engine::Engine::new();
            let blob = snapshot_engine.snapshot(0);
            socket
                .write_all(&(blob.len() as u64).to_le_bytes())
                .await
                .unwrap();
            socket.write_all(&blob).await.unwrap();

            socket
                .write_all(b"*1\r\n$5\r\nMULTI\r\n")
                .await
                .unwrap();
            socket
                .write_all(b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n")
                .await
                .unwrap();
            socket
                .write_all(b"*3\r\n$3\r\nSET\r\n$1\r\nb\r\n$1\r\n2\r\n")
                .await
                .unwrap();
            socket.write_all(b"*1\r\n$4\r\nEXEC\r\n").await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let engine = std::sync::Arc::new(engine::Engine::new());
        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
        let sync_task = {
            let engine = std::sync::Arc::clone(&engine);
            let generation = Arc::clone(&generation);
            tokio::spawn(async move {
                let stream = tokio::net::TcpStream::connect(&host_port).await.unwrap();
                sync_once(
                    stream,
                    &engine,
                    &generation,
                    0,
                    None,
                    FollowerStatus {
                        last_apply: &AtomicI64::new(0),
                        link_up: &AtomicBool::new(false),
                        slave_offset: &AtomicU64::new(0),
                    },
                    &FollowerIdentity::default(),
                )
                .await
            })
        };

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        sync_task.abort();
        fake_leader.await.unwrap();

        assert_eq!(
            engine.get(b"a"),
            Some(engine::Value::String(bytes::Bytes::from_static(b"1")))
        );
        assert_eq!(
            engine.get(b"b"),
            Some(engine::Value::String(bytes::Bytes::from_static(b"2")))
        );
    }
```

- [ ] **Step 3: Run the test to verify it fails**

```bash
cargo test -p rocket-mem --lib replication::tests::sync_once_applies_a_streamed_transaction
```

Expected: FAIL — `MULTI`/`EXEC` sent through `dispatch()` are unknown commands today (nothing
buffers what comes between them), so neither `a` nor `b` gets set.

- [ ] **Step 4: Wire `TransactionGrouper` into `sync_once`**

`sync_once`'s per-frame apply body (`replication.rs`, inside the `tokio::select!`'s
`incoming = framed.next()` arm, around lines 1202–1231) reads, today:

```rust
                let frame = result?;
                frames_applied += 1;
                let name = replicated_command_name(&frame);
                let frame_len = crate::aof::encode_frame(&frame)?.len() as u64;
                let mut protocol = protocol::codec::Protocol::default();
                let _order_guard = aof.map(|a| a.lock_all_shards());
                let reply = crate::dispatcher::dispatch(engine, frame, &mut protocol, 0);
                tracing::debug!(cmd = %crate::logging::escape_ident(&name), "applied replicated command");
                if let protocol::Frame::Error(e) = reply {
                    tracing::error!(error = %e, "failed to apply replicated command");
                }
                status.last_apply.store(unix_now_secs(), Ordering::Relaxed);
                // (unchanged below: offset/lag bookkeeping keyed on frame_len)
```

Replace it with:

```rust
                let frame = result?;
                frames_applied += 1;
                // Computed before the frame is fed to the grouper (which may consume it into its
                // buffer): this is the byte-exact length the leader counted in its own
                // master_repl_offset for *this* frame, marker or not -- append_transaction_marker
                // advances the leader's offset for MULTI/EXEC markers too (see Plan 03 Task 1), so
                // this follower's offset must advance for every decoded frame the same way,
                // regardless of whether it is buffered, released, or itself a marker.
                let frame_len = crate::aof::encode_frame(&frame)?.len() as u64;
                match grouper.feed(frame) {
                    Ok(to_apply) => {
                        for buffered in to_apply {
                            let name = replicated_command_name(&buffered);
                            let mut protocol = protocol::codec::Protocol::default();
                            // Scoped to one buffered command's own dispatch call, exactly as
                            // today -- deliberately NOT widened to span a whole transaction's
                            // worth of buffered commands. Widening it is a separate, larger
                            // behavior change (holding out a concurrent SAVE for a whole
                            // transaction instead of one command) and is out of scope here.
                            let _order_guard = aof.map(|a| a.lock_all_shards());
                            let reply =
                                crate::dispatcher::dispatch(engine, buffered, &mut protocol, 0);
                            tracing::debug!(
                                cmd = %crate::logging::escape_ident(&name),
                                "applied replicated command"
                            );
                            if let protocol::Frame::Error(e) = reply {
                                tracing::error!(error = %e, "failed to apply replicated command");
                            }
                        }
                    }
                    Err(reason) => {
                        tracing::warn!(
                            reason,
                            "replication: malformed transaction framing from leader"
                        );
                    }
                }
                status.last_apply.store(unix_now_secs(), Ordering::Relaxed);
                // (unchanged below: offset/lag bookkeeping keyed on frame_len, exactly as today)
```

Add `let mut grouper = crate::transaction_grouping::TransactionGrouper::new();` once, before the
`loop { tokio::select! { ... } }` this body lives inside (next to `frames_applied`'s own `let mut
frames_applied = 0;` declaration).

Nothing below this block — the offset/lag bookkeeping that consumes `frame_len` — changes at all;
it already runs unconditionally per decoded frame, which is exactly the behavior a marker frame
also needs.

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo test -p rocket-mem --lib replication::tests::sync_once_applies_a_streamed_transaction
```

Expected: PASS.

- [ ] **Step 6: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, total `BASELINE + 26 + 1 = BASELINE + 27`.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "feat(transactions): apply a replicated transaction as one unit on followers

sync_once now feeds every streamed frame through the same
TransactionGrouper aof::replay_with_stats uses, so a follower never
observes a torn transaction from its leader's stream."
```

---

## Next plan

[`04-benchmark-verification-and-docs.md`](04-benchmark-verification-and-docs.md) — the required
before/after `scripts/benchmark.sh` run proving the non-transaction hot path did not regress, a
new `MULTI`/`EXEC` benchmark scenario, and documentation marking the feature released.
