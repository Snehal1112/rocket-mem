# Pub/Sub Plan 04: `PUBLISH` & `PUBSUB` Introspection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `PUBLISH` and `PUBSUB CHANNELS`/`NUMSUB`/`NUMPAT`, extending the same
`intercept_for_pubsub` Plan 03 built. `PUBLISH` never touches the AOF, is forwarded to replicas
over the existing `ReplicaRegistry` broadcast, and is queueable inside `MULTI` (unlike the
`SUBSCRIBE` family, which Plan 03 already rejects at queue time).

**Architecture:** Two more `match` arms in `intercept_for_pubsub`
(`crates/server/src/dispatcher.rs`), still reached from `dispatch_and_log_gated` alongside
`handle_client` — this is what makes a `PUBLISH` queued inside a transaction replay correctly at
`EXEC` time, since `EXEC`'s per-queued-frame loop calls `dispatch_and_log_gated` directly. No
`aof.lock_shards` guard, no engine mutation: `PUBLISH` only touches `replication.pubsub` and
`replication.registry`.

**Tech Stack:** Rust, `engine::glob::glob_match`, `crate::aof::encode_frame`.

**Spec:** [../../specs/2026-09-11-pubsub-spec.md](../../specs/2026-09-11-pubsub-spec.md) — see
its "Command semantics" (`PUBLISH`/`PUBSUB` bullets) and "AOF & replication" sections.

**Global Constraints:** see
[01-frame-push-variant.md](01-frame-push-variant.md)'s `## Global Constraints` section.

---

### Task 1: `PUBLISH` — local delivery, never AOF-logged, forwarded to replicas

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (extend `intercept_for_pubsub`)

**Interfaces:**
- Consumes: `PubSubRegistry::publish` (Plan 02); `crate::aof::encode_frame(frame: &Frame) ->
  std::io::Result<Vec<u8>>` (already exists, `crates/server/src/aof.rs:48`);
  `replication.registry.broadcast(bytes: bytes::Bytes)` (already exists, the `ReplicaRegistry`
  method).
- Produces: `PUBLISH` reachable both from top-level dispatch and from `EXEC`'s replay loop, since
  both paths call `dispatch_and_log_gated`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/dispatcher.rs`'s test module:

```rust
    #[test]
    fn publish_delivers_locally_and_returns_the_delivered_count() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let subscriber_session = Session::new();
        intercept_for_pubsub(
            &cmd(&[b"SUBSCRIBE", b"news"]),
            &subscriber_session,
            &replication,
            1,
        );

        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"PUBLISH", b"news", b"hello"]),
            &Session::new(),
            2,
        );

        assert_eq!(reply, Frame::Integer(1));
    }

    #[test]
    fn publish_is_never_written_to_the_aof() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"PUBLISH", b"news", b"hello"]),
            &Session::new(),
            1,
        );
        aof.fsync().unwrap();

        assert_eq!(read_aof(&_dir), "");
    }

    #[test]
    fn publish_is_forwarded_to_replicas() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        replication.registry.register(None, tx);

        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"PUBLISH", b"news", b"hello"]),
            &Session::new(),
            1,
        );

        let forwarded = rx.try_recv().unwrap();
        assert_eq!(
            forwarded,
            bytes::Bytes::from(
                crate::aof::encode_frame(&cmd(&[b"PUBLISH", b"news", b"hello"])).unwrap()
            )
        );
    }

    #[test]
    fn publish_is_queueable_and_replays_at_exec_time() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        let subscriber_session = Session::new();
        intercept_for_pubsub(
            &cmd(&[b"SUBSCRIBE", b"news"]),
            &subscriber_session,
            &replication,
            1,
        );

        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 2);
        let queued_reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"PUBLISH", b"news", b"hello"]),
            &session,
            2,
        );
        assert_eq!(queued_reply, Frame::Simple("QUEUED".into()));

        let exec_reply =
            dispatch_and_log(&engine, &aof, &replication, cmd(&[b"EXEC"]), &session, 2);

        assert_eq!(exec_reply, Frame::Array(vec![Frame::Integer(1)]));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests -- --exact publish_delivers_locally_and_returns_the_delivered_count publish_is_never_written_to_the_aof publish_is_forwarded_to_replicas publish_is_queueable_and_replays_at_exec_time`
Expected: FAIL — `PUBLISH` isn't handled by `intercept_for_pubsub` yet, so it falls through to
the generic dispatch path and errors as an unknown command (even though it's now in
`KNOWN_COMMANDS` since Plan 03 Task 1 — being *known* only satisfies the queue-time check;
nothing executes it yet).

- [ ] **Step 3: Implement the `PUBLISH` arm**

Add to `intercept_for_pubsub`'s `match name.as_str()`, alongside the existing arms:

```rust
        "PUBLISH" => {
            let (Some(Frame::Bulk(channel)), Some(Frame::Bulk(message))) =
                (items.get(1), items.get(2))
            else {
                return Some(Frame::Error(
                    "ERR wrong number of arguments for 'publish' command".into(),
                ));
            };
            let started = std::time::Instant::now();
            let delivered = replication.pubsub.publish(channel, message);
            // Never AOF-appended -- not a keyspace mutation, nothing for AOF replay to redo.
            // Forwarded to replicas so a follower's own locally-subscribed clients still
            // receive it; see the spec's "Why not dispatch()" section for why the follower side
            // (Plan 06) intercepts this in its apply loop rather than routing it through
            // dispatch()'s engine-mutation path.
            if let Ok(encoded) = crate::aof::encode_frame(frame) {
                replication.registry.broadcast(Bytes::from(encoded));
            }
            tracing::debug!(
                channel = %crate::logging::escape_ident(&String::from_utf8_lossy(channel)),
                delivered_count = delivered,
                elapsed_us = started.elapsed().as_micros(),
                "message published"
            );
            Some(Frame::Integer(delivered as i64))
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests -- --exact publish_delivers_locally_and_returns_the_delivered_count publish_is_never_written_to_the_aof publish_is_forwarded_to_replicas publish_is_queueable_and_replays_at_exec_time`
Expected: PASS.

- [ ] **Step 5: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "$(cat <<'EOF'
Implement PUBLISH

Never AOF-appended -- not a keyspace mutation. Forwarded to
replicas over the existing ReplicaRegistry broadcast so a
follower's locally-subscribed clients still receive it. Reachable
from EXEC's replay loop the same way CLIENT already is, since both
go through dispatch_and_log_gated -- so PUBLISH queued inside a
transaction runs at EXEC time, matching real Redis.
EOF
)"
```

---

### Task 2: `PUBSUB CHANNELS`/`NUMSUB`/`NUMPAT`

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (extend `intercept_for_pubsub`)

**Interfaces:**
- Consumes: `PubSubRegistry::channels`/`num_sub`/`num_pat` (Plan 02 Task 2);
  `engine::glob::glob_match`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/dispatcher.rs`'s test module:

```rust
    #[test]
    fn pubsub_channels_lists_every_subscribed_channel() {
        let replication = ReplicationHandle::default();
        intercept_for_pubsub(&cmd(&[b"SUBSCRIBE", b"news"]), &Session::new(), &replication, 1);

        let reply = intercept_for_pubsub(
            &cmd(&[b"PUBSUB", b"CHANNELS"]),
            &Session::new(),
            &replication,
            2,
        );

        assert_eq!(
            reply,
            Some(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"news"))]))
        );
    }

    #[test]
    fn pubsub_channels_with_a_pattern_filters_the_result() {
        let replication = ReplicationHandle::default();
        intercept_for_pubsub(&cmd(&[b"SUBSCRIBE", b"news"]), &Session::new(), &replication, 1);
        intercept_for_pubsub(&cmd(&[b"SUBSCRIBE", b"sports"]), &Session::new(), &replication, 2);

        let reply = intercept_for_pubsub(
            &cmd(&[b"PUBSUB", b"CHANNELS", b"news"]),
            &Session::new(),
            &replication,
            3,
        );

        assert_eq!(
            reply,
            Some(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"news"))]))
        );
    }

    #[test]
    fn pubsub_numsub_reports_a_count_per_requested_channel() {
        let replication = ReplicationHandle::default();
        intercept_for_pubsub(&cmd(&[b"SUBSCRIBE", b"news"]), &Session::new(), &replication, 1);

        let reply = intercept_for_pubsub(
            &cmd(&[b"PUBSUB", b"NUMSUB", b"news", b"empty"]),
            &Session::new(),
            &replication,
            2,
        );

        assert_eq!(
            reply,
            Some(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"news")),
                Frame::Integer(1),
                Frame::Bulk(Bytes::from_static(b"empty")),
                Frame::Integer(0),
            ]))
        );
    }

    #[test]
    fn pubsub_numpat_counts_registered_patterns() {
        let replication = ReplicationHandle::default();
        intercept_for_pubsub(&cmd(&[b"PSUBSCRIBE", b"news.*"]), &Session::new(), &replication, 1);

        let reply = intercept_for_pubsub(
            &cmd(&[b"PUBSUB", b"NUMPAT"]),
            &Session::new(),
            &replication,
            2,
        );

        assert_eq!(reply, Some(Frame::Integer(1)));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests -- --exact pubsub_channels_lists_every_subscribed_channel pubsub_channels_with_a_pattern_filters_the_result pubsub_numsub_reports_a_count_per_requested_channel pubsub_numpat_counts_registered_patterns`
Expected: FAIL — `PUBSUB` isn't handled by `intercept_for_pubsub` yet.

- [ ] **Step 3: Implement the `PUBSUB` arm**

Add to `intercept_for_pubsub`'s `match name.as_str()`:

```rust
        "PUBSUB" => {
            let Some(Frame::Bulk(sub_bytes)) = items.get(1) else {
                return Some(Frame::Error(
                    "ERR wrong number of arguments for 'pubsub' command".into(),
                ));
            };
            let sub = String::from_utf8_lossy(sub_bytes).to_ascii_uppercase();
            Some(match sub.as_str() {
                "CHANNELS" => {
                    let pattern = items.get(2).and_then(|f| match f {
                        Frame::Bulk(b) => Some(b.clone()),
                        _ => None,
                    });
                    let mut channels: Vec<Bytes> = replication
                        .pubsub
                        .channels()
                        .into_iter()
                        .filter(|c| {
                            pattern
                                .as_ref()
                                .map_or(true, |p| engine::glob::glob_match(p, c))
                        })
                        .collect();
                    channels.sort();
                    Frame::Array(channels.into_iter().map(Frame::Bulk).collect())
                }
                "NUMSUB" => {
                    let requested: Vec<Bytes> = items[2..]
                        .iter()
                        .filter_map(|f| match f {
                            Frame::Bulk(b) => Some(b.clone()),
                            _ => None,
                        })
                        .collect();
                    let mut reply = Vec::new();
                    for (channel, count) in replication.pubsub.num_sub(&requested) {
                        reply.push(Frame::Bulk(channel));
                        reply.push(Frame::Integer(count as i64));
                    }
                    Frame::Array(reply)
                }
                "NUMPAT" => Frame::Integer(replication.pubsub.num_pat() as i64),
                _ => Frame::Error(format!("ERR unknown PUBSUB subcommand '{sub}'")),
            })
        }
```

`channels.sort()` makes the `PUBSUB CHANNELS` reply order deterministic for tests -- the
underlying registry is a `HashMap`, whose iteration order is not otherwise guaranteed. Real
Redis's own `PUBSUB CHANNELS` order is unspecified too, so this is a safe, test-friendly choice
rather than a compatibility requirement.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests -- --exact pubsub_channels_lists_every_subscribed_channel pubsub_channels_with_a_pattern_filters_the_result pubsub_numsub_reports_a_count_per_requested_channel pubsub_numpat_counts_registered_patterns`
Expected: PASS.

- [ ] **Step 5: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "$(cat <<'EOF'
Implement PUBSUB CHANNELS/NUMSUB/NUMPAT

Pure reads over PubSubRegistry's current state. CHANNELS accepts
an optional glob pattern via the same engine::glob::glob_match
KEYS already uses, and sorts its reply for deterministic test
output (real Redis's own ordering is unspecified too).
EOF
)"
```

## Next plan

[05-connection-loop-and-cleanup.md](05-connection-loop-and-cleanup.md) — the `tokio::select!`
addition to `connection.rs`'s read loop, and disconnect cleanup.
