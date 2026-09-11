# Pub/Sub Plan 03: Session State & Subscribe Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `Session` gains subscription-tracking state, and `SUBSCRIBE`/`UNSUBSCRIBE`/
`PSUBSCRIBE`/`PUNSUBSCRIBE` become real commands, including the RESP2 restricted-mode gate and
the `MULTI`/`EXEC` interaction (subscription commands rejected at queue time).

**Architecture:** Four new fields on `Session` (`crates/server/src/dispatcher.rs`):
`subscriptions`/`psubscriptions` (what this connection currently has open), an
`AtomicUsize subscription_count` fast-path flag, and a `push_tx`/`push_rx` pair — the connection's
own lazily-created `mpsc` channel, whose sender half gets cloned into `PubSubRegistry` on every
`SUBSCRIBE`/`PSUBSCRIBE` and whose receiver half `connection.rs` (Plan 05) will drain. A new
`intercept_for_pubsub`, called from `dispatch_and_log_gated` alongside the existing
`handle_client` call, handles all four commands. A new `subscribe_mode_gate`, called from
`dispatch_and_log_inner` right after `auth_gate` (before `intercept_for_transaction`, so it can
also block a subscribed RESP2 connection from opening `MULTI`), enforces the RESP2 restriction.
`intercept_for_transaction`'s existing per-command queuing branch gets one new check rejecting
`SUBSCRIBE`-family commands at queue time.

**Tech Stack:** Rust, `tokio::sync::mpsc`, `std::sync::{Mutex, atomic::AtomicUsize}`.

**Spec:** [../../specs/2026-09-11-pubsub-spec.md](../../specs/2026-09-11-pubsub-spec.md) — see
its "`Session` additions", "Command semantics", "Interception point", and "Interaction with
`MULTI`/`EXEC`" sections. Note: the spec describes `push_rx` alone; this plan adds a `push_tx`
field too, a necessary implementation detail the spec's design-level description didn't need to
spell out — the connection's `mpsc::Sender` half must be kept somewhere so a second `SUBSCRIBE`
on the same connection can clone it again to register another channel, rather than only being
handed once to `push_rx`'s creator and then lost.

**Global Constraints:** see
[01-frame-push-variant.md](01-frame-push-variant.md)'s `## Global Constraints` section.

---

### Task 1: `Session` fields, `KNOWN_COMMANDS`, and `key_spec` entries

**Files:**
- Modify: `crates/server/src/dispatcher.rs:34-51` (`Session` struct + its `new()`)
- Modify: `crates/server/src/dispatcher.rs:1361-1453` (`KNOWN_COMMANDS`)
- Modify: `crates/server/src/dispatcher.rs:1502-1521` (`key_spec`)

**Interfaces:**
- Produces: `Session`'s new fields (all private, accessed directly by later tasks in this same
  file — matching how `tx`/`in_transaction` are accessed directly by `intercept_for_transaction`
  rather than through wrapper methods):
  `subscriptions: Mutex<HashSet<Bytes>>`, `psubscriptions: Mutex<HashSet<Bytes>>`,
  `subscription_count: AtomicUsize`, `push_tx: Mutex<Option<mpsc::UnboundedSender<Frame>>>`,
  `push_rx: Mutex<Option<mpsc::UnboundedReceiver<Frame>>>`. `KNOWN_COMMANDS` and `key_spec` gain
  entries for `SUBSCRIBE`/`UNSUBSCRIBE`/`PSUBSCRIBE`/`PUNSUBSCRIBE`/`PUBLISH`/`PUBSUB` — later
  tasks and Plan 04 rely on these being present.

- [ ] **Step 1: Write the failing test**

Add to `crates/server/src/dispatcher.rs`'s test module (search for an existing `COMMAND INFO`
test, e.g. `command_info_reports_a_known_commands_key_spec` or similar, to match its exact
`cmd(&[...])`/`dispatch(...)` helper usage):

```rust
    #[test]
    fn command_info_recognizes_every_new_pubsub_command() {
        let engine = Engine::new();
        for name in [
            "SUBSCRIBE",
            "UNSUBSCRIBE",
            "PSUBSCRIBE",
            "PUNSUBSCRIBE",
            "PUBLISH",
            "PUBSUB",
        ] {
            let reply = dispatch(
                &engine,
                cmd(&[b"COMMAND", b"INFO", name.as_bytes()]),
                &mut Protocol::default(),
                1,
            );
            let Frame::Array(entries) = reply else {
                panic!("expected COMMAND INFO to reply with an array");
            };
            assert_ne!(
                entries[0],
                Frame::Null,
                "{name} should be a known command by now"
            );
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem dispatcher::tests::command_info_recognizes_every_new_pubsub_command`
Expected: FAIL — each of the six names is currently unknown to `KNOWN_COMMANDS`, so
`COMMAND INFO <name>` replies `Frame::Null` for all of them.

- [ ] **Step 3: Add the `Session` fields**

In `crates/server/src/dispatcher.rs`, add to the `Session` struct (after the existing `tx`
field):

```rust
    /// Channels this connection currently has an open `SUBSCRIBE` on. A separate lock from
    /// `tx`/`protocol`/etc: nothing about subscription state needs to block an unrelated read of
    /// this connection's transaction or auth state.
    subscriptions: std::sync::Mutex<std::collections::HashSet<Bytes>>,
    /// Patterns this connection currently has an open `PSUBSCRIBE` on.
    psubscriptions: std::sync::Mutex<std::collections::HashSet<Bytes>>,
    /// Fast path for the overwhelmingly common case (no subscription ever opened): checked with
    /// a relaxed load before `subscriptions`/`psubscriptions` are ever touched. See the spec's
    /// "Performance" section.
    subscription_count: std::sync::atomic::AtomicUsize,
    /// This connection's own outbound channel for pushed pub/sub messages, created lazily on the
    /// first `SUBSCRIBE`/`PSUBSCRIBE`. Kept alongside `push_rx` so a later `SUBSCRIBE` on the
    /// same connection can clone it again to register another channel, rather than the sender
    /// half being handed once to the registry and then unreachable.
    push_tx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Frame>>>,
    /// The receiving half of `push_tx`'s channel. `connection.rs`'s read loop drains this once
    /// it is `Some` (Plan 05) -- `None` for a connection that has never subscribed.
    push_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Frame>>>,
```

In `Session::new()`, add the corresponding initializers (after the existing `tx:
std::sync::Mutex::new(TransactionState::Idle),` line):

```rust
            subscriptions: std::sync::Mutex::new(std::collections::HashSet::new()),
            psubscriptions: std::sync::Mutex::new(std::collections::HashSet::new()),
            subscription_count: std::sync::atomic::AtomicUsize::new(0),
            push_tx: std::sync::Mutex::new(None),
            push_rx: std::sync::Mutex::new(None),
```

- [ ] **Step 4: Add the `KNOWN_COMMANDS` entries**

In `crates/server/src/dispatcher.rs`'s `KNOWN_COMMANDS` array, insert (keeping the array's
existing alphabetical order — `binary_search` requires it):
- `"PSUBSCRIBE",` between `"PING",` and `"PSYNC",`
- `"PUBLISH",` and `"PUBSUB",` between `"PTTL",` and `"RANDOMKEY",` (in that order — `PUBLISH` <
  `PUBSUB` alphabetically)
- `"PUNSUBSCRIBE",` between the just-added `"PUBSUB",` and `"RANDOMKEY",`
- `"SUBSCRIBE",` between `"STRLEN",` and `"SUNION",`
- `"UNSUBSCRIBE",` between `"TYPE",` and `"ZADD",`

- [ ] **Step 5: Add the `key_spec` entries**

In `crates/server/src/dispatcher.rs`'s `key_spec` function, add all six names to the existing
`KeySpec::None`-returning arm (the one currently listing `"PING" | "ECHO" | ... | "CLIENT"`):

```rust
        "PING" | "ECHO" | "SELECT" | "COMMAND" | "INFO" | "HELLO" | "KEYS" | "SCAN"
        | "RANDOMKEY" | "CLUSTER" | "SAVE" | "BGREWRITEAOF" | "REPLICAOF" | "PSYNC" | "SLOWLOG"
        | "DEBUG" | "AUTH" | "ACL" | "DBSIZE" | "CONFIG" | "CLIENT" | "SUBSCRIBE"
        | "UNSUBSCRIBE" | "PSUBSCRIBE" | "PUNSUBSCRIBE" | "PUBLISH" | "PUBSUB" => {
            // Channel/pattern names are not routable keyspace keys -- without this, the
            // `KNOWN_COMMANDS` catch-all below would default them to `KeySpec::First`, which in
            // cluster mode would hash a channel name to a slot and potentially -MOVED it.
            KeySpec::None
        }
```

(This replaces the existing arm's body comment/close — keep the existing `AUTH`/`ACL` doc comment
above it if your editor view still shows it there, and fold this plan's own comment about
channel/pattern names into the same doc comment rather than stacking two separate ones.)

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p rocket-mem dispatcher::tests::command_info_recognizes_every_new_pubsub_command`
Expected: PASS.

- [ ] **Step 7: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three pass clean — in particular, confirm no existing test asserted
`COMMAND`'s exact reply length/contents in a way that a six-entry-longer `KNOWN_COMMANDS` now
breaks (search first: `grep -rn "COMMAND\"\]" crates/server/src/dispatcher.rs` to find any such
test before running).

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "$(cat <<'EOF'
Add Session pub/sub state and register the six new commands

Session gains subscriptions/psubscriptions/subscription_count/
push_tx/push_rx, all unused until the next two tasks wire real
command handling to them. SUBSCRIBE/UNSUBSCRIBE/PSUBSCRIBE/
PUNSUBSCRIBE/PUBLISH/PUBSUB are added to KNOWN_COMMANDS and given
KeySpec::None (channel/pattern names are not routable keys).
EOF
)"
```

---

### Task 2: `SUBSCRIBE`/`UNSUBSCRIBE` command handling

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (new `intercept_for_pubsub` function, called from
  `dispatch_and_log_gated` around line 3821, right after the existing
  `if let Some(reply) = handle_client(&frame, session, client_id) { return reply; }`)

**Interfaces:**
- Consumes: `Session`'s new fields (Task 1); `PubSubRegistry::subscribe`/`unsubscribe` (Plan 02
  Task 1, reached via `replication.pubsub`).
- Produces: `fn intercept_for_pubsub(frame: &Frame, session: &Session, replication:
  &crate::replication::ReplicationHandle, client_id: u64) -> Option<Frame>` — Plan 04 extends
  this same function's `match` with `PUBLISH`/`PUBSUB` arms, and Task 3 below extends it with
  `PSUBSCRIBE`/`PUNSUBSCRIBE`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/dispatcher.rs`'s test module:

```rust
    #[test]
    fn subscribe_replies_with_one_push_frame_per_channel_naming_the_running_count() {
        let session = Session::new();
        let replication = crate::replication::ReplicationHandle::default();
        let reply = intercept_for_pubsub(
            &cmd(&[b"SUBSCRIBE", b"news", b"sports"]),
            &session,
            &replication,
            1,
        )
        .unwrap();
        assert_eq!(
            reply,
            Frame::Array(vec![
                Frame::Push(vec![
                    Frame::Bulk(Bytes::from_static(b"subscribe")),
                    Frame::Bulk(Bytes::from_static(b"news")),
                    Frame::Integer(1),
                ]),
                Frame::Push(vec![
                    Frame::Bulk(Bytes::from_static(b"subscribe")),
                    Frame::Bulk(Bytes::from_static(b"sports")),
                    Frame::Integer(2),
                ]),
            ])
        );
    }

    #[test]
    fn subscribe_actually_registers_with_the_pubsub_registry() {
        let session = Session::new();
        let replication = crate::replication::ReplicationHandle::default();
        intercept_for_pubsub(&cmd(&[b"SUBSCRIBE", b"news"]), &session, &replication, 1);

        let delivered = replication
            .pubsub
            .publish(b"news", &Bytes::from_static(b"hello"));
        assert_eq!(delivered, 1);
    }

    #[test]
    fn unsubscribe_with_no_arguments_leaves_every_subscribed_channel() {
        let session = Session::new();
        let replication = crate::replication::ReplicationHandle::default();
        intercept_for_pubsub(
            &cmd(&[b"SUBSCRIBE", b"news", b"sports"]),
            &session,
            &replication,
            1,
        );

        let reply = intercept_for_pubsub(&cmd(&[b"UNSUBSCRIBE"]), &session, &replication, 1);

        assert_eq!(replication.pubsub.publish(b"news", &Bytes::from_static(b"x")), 0);
        assert_eq!(replication.pubsub.publish(b"sports", &Bytes::from_static(b"x")), 0);
        // Both channels' own reply frames are present, in subscription order, each counting
        // down: 1 remaining after leaving "news" (still on "sports"), 0 after leaving "sports".
        assert_eq!(
            reply,
            Some(Frame::Array(vec![
                Frame::Push(vec![
                    Frame::Bulk(Bytes::from_static(b"unsubscribe")),
                    Frame::Bulk(Bytes::from_static(b"news")),
                    Frame::Integer(1),
                ]),
                Frame::Push(vec![
                    Frame::Bulk(Bytes::from_static(b"unsubscribe")),
                    Frame::Bulk(Bytes::from_static(b"sports")),
                    Frame::Integer(0),
                ]),
            ]))
        );
    }

    #[test]
    fn unsubscribe_of_a_channel_never_subscribed_to_still_replies_once() {
        let session = Session::new();
        let replication = crate::replication::ReplicationHandle::default();
        let reply = intercept_for_pubsub(
            &cmd(&[b"UNSUBSCRIBE", b"never-subscribed"]),
            &session,
            &replication,
            1,
        );
        assert_eq!(
            reply,
            Some(Frame::Array(vec![Frame::Push(vec![
                Frame::Bulk(Bytes::from_static(b"unsubscribe")),
                Frame::Bulk(Bytes::from_static(b"never-subscribed")),
                Frame::Integer(0),
            ])]))
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests -- --exact subscribe_replies_with_one_push_frame_per_channel_naming_the_running_count subscribe_actually_registers_with_the_pubsub_registry unsubscribe_with_no_arguments_leaves_every_subscribed_channel unsubscribe_of_a_channel_never_subscribed_to_still_replies_once`
Expected: FAIL to compile — `intercept_for_pubsub` does not exist yet.

- [ ] **Step 3: Implement `intercept_for_pubsub` (SUBSCRIBE/UNSUBSCRIBE only)**

Add to `crates/server/src/dispatcher.rs`, near `intercept_for_transaction`:

```rust
/// Returns this connection's `push_tx`, creating it (and the matching `push_rx`) on first use.
/// `connection.rs`'s read loop (Plan 05) takes `push_rx` out to drain it once it is `Some`.
fn ensure_push_channel(session: &Session) -> tokio::sync::mpsc::UnboundedSender<Frame> {
    let mut tx_guard = session.push_tx.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(tx) = tx_guard.as_ref() {
        return tx.clone();
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *tx_guard = Some(tx.clone());
    drop(tx_guard);
    *session.push_rx.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
    tx
}

/// The combined channel + pattern subscription count this connection currently holds -- what
/// every `subscribe`/`unsubscribe`/`psubscribe`/`punsubscribe` reply reports, and what
/// `subscribe_mode_gate` (Task 3) checks via `subscription_count`'s fast-path atomic.
fn total_subscription_count(session: &Session) -> usize {
    session.subscriptions.lock().unwrap_or_else(|e| e.into_inner()).len()
        + session.psubscriptions.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Handles `SUBSCRIBE`/`UNSUBSCRIBE`/`PSUBSCRIBE`/`PUNSUBSCRIBE`/`PUBLISH`/`PUBSUB`. Called from
/// `dispatch_and_log_gated`, alongside `handle_client` -- see the spec's "Interception point"
/// section for why this placement (not `dispatch_and_log_inner`, alongside
/// `intercept_for_transaction`) is what makes a queued `PUBLISH` replay correctly at `EXEC` time.
/// This task implements `SUBSCRIBE`/`UNSUBSCRIBE` only; `PSUBSCRIBE`/`PUNSUBSCRIBE` are Task 3,
/// `PUBLISH`/`PUBSUB` are Plan 04.
fn intercept_for_pubsub(
    frame: &Frame,
    session: &Session,
    replication: &crate::replication::ReplicationHandle,
    client_id: u64,
) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name_bytes)) = items.first() else {
        return None;
    };
    let name = upper_name(name_bytes)?;
    match name.as_str() {
        "SUBSCRIBE" => {
            let tx = ensure_push_channel(session);
            let mut replies = Vec::new();
            for item in &items[1..] {
                let Frame::Bulk(channel) = item else { continue };
                let mut subs = session.subscriptions.lock().unwrap_or_else(|e| e.into_inner());
                subs.insert(channel.clone());
                drop(subs);
                replication.pubsub.subscribe(channel.clone(), client_id, tx.clone());
                let count = total_subscription_count(session);
                session
                    .subscription_count
                    .store(count, std::sync::atomic::Ordering::Relaxed);
                tracing::debug!(client_id, channel = %crate::logging::escape_ident(&String::from_utf8_lossy(channel)), count, "subscription changed");
                replies.push(Frame::Push(vec![
                    Frame::Bulk(Bytes::from_static(b"subscribe")),
                    Frame::Bulk(channel.clone()),
                    Frame::Integer(count as i64),
                ]));
            }
            Some(Frame::Array(replies))
        }
        "UNSUBSCRIBE" => {
            let explicit: Vec<Bytes> = items[1..]
                .iter()
                .filter_map(|f| match f {
                    Frame::Bulk(b) => Some(b.clone()),
                    _ => None,
                })
                .collect();
            let targets = if explicit.is_empty() {
                session
                    .subscriptions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                explicit
            };
            let mut replies = Vec::new();
            for channel in targets {
                session
                    .subscriptions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&channel);
                replication.pubsub.unsubscribe(&channel, client_id);
                let count = total_subscription_count(session);
                session
                    .subscription_count
                    .store(count, std::sync::atomic::Ordering::Relaxed);
                tracing::debug!(client_id, channel = %crate::logging::escape_ident(&String::from_utf8_lossy(&channel)), count, "subscription changed");
                replies.push(Frame::Push(vec![
                    Frame::Bulk(Bytes::from_static(b"unsubscribe")),
                    Frame::Bulk(channel),
                    Frame::Integer(count as i64),
                ]));
            }
            Some(Frame::Array(replies))
        }
        _ => None,
    }
}
```

Wire it into `dispatch_and_log_gated`, right after the existing `handle_client` call (around line
3821):

```rust
    if let Some(reply) = handle_client(&frame, session, client_id) {
        return reply;
    }
    if let Some(reply) = intercept_for_pubsub(&frame, session, replication, client_id) {
        return reply;
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests -- --exact subscribe_replies_with_one_push_frame_per_channel_naming_the_running_count subscribe_actually_registers_with_the_pubsub_registry unsubscribe_with_no_arguments_leaves_every_subscribed_channel unsubscribe_of_a_channel_never_subscribed_to_still_replies_once`
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
Implement SUBSCRIBE and UNSUBSCRIBE

intercept_for_pubsub is called from dispatch_and_log_gated
alongside handle_client, so a queued PUBLISH (Plan 04) replays
through the same path at EXEC time. Each channel gets its own
Push-framed reply naming the connection's running subscription
count, matching real Redis's per-channel reply sequence.
EOF
)"
```

---

### Task 3: `PSUBSCRIBE`/`PUNSUBSCRIBE`, the RESP2 restricted-mode gate, and the `MULTI` interaction

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (extend `intercept_for_pubsub`; add
  `subscribe_mode_gate`, called from `dispatch_and_log_inner`; extend
  `intercept_for_transaction`'s queuing branch)

**Interfaces:**
- Consumes: `PubSubRegistry::psubscribe`/`punsubscribe` (Plan 02 Task 2); `total_subscription_count`,
  `ensure_push_channel` (Task 2).
- Produces: the RESP2 restricted-mode gate is now enforced for every command; `SUBSCRIBE`-family
  commands are rejected at `MULTI` queue time.

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/dispatcher.rs`'s test module:

```rust
    #[test]
    fn psubscribe_delivers_pmessage_on_publish() {
        let session = Session::new();
        let replication = crate::replication::ReplicationHandle::default();
        intercept_for_pubsub(&cmd(&[b"PSUBSCRIBE", b"news.*"]), &session, &replication, 1);

        let delivered = replication
            .pubsub
            .publish(b"news.sports", &Bytes::from_static(b"hello"));
        assert_eq!(delivered, 1);
    }

    #[test]
    fn a_resp2_connection_with_an_active_subscription_rejects_an_ordinary_command() {
        let session = Session::new();
        let replication = crate::replication::ReplicationHandle::default();
        intercept_for_pubsub(&cmd(&[b"SUBSCRIBE", b"news"]), &session, &replication, 1);

        let reply = subscribe_mode_gate(&cmd(&[b"GET", b"k"]), &session);

        assert_eq!(
            reply,
            Some(Frame::Error(
                "ERR only (P)SUBSCRIBE / (P)UNSUBSCRIBE / PING / QUIT / RESET are allowed in this context"
                    .into()
            ))
        );
    }

    #[test]
    fn a_resp2_connection_with_an_active_subscription_still_allows_ping_and_more_subscribe() {
        let session = Session::new();
        let replication = crate::replication::ReplicationHandle::default();
        intercept_for_pubsub(&cmd(&[b"SUBSCRIBE", b"news"]), &session, &replication, 1);

        assert_eq!(subscribe_mode_gate(&cmd(&[b"PING"]), &session), None);
        assert_eq!(subscribe_mode_gate(&cmd(&[b"SUBSCRIBE", b"more"]), &session), None);
        assert_eq!(subscribe_mode_gate(&cmd(&[b"UNSUBSCRIBE"]), &session), None);
    }

    #[test]
    fn a_resp3_connection_with_an_active_subscription_allows_an_ordinary_command() {
        let session = Session::new();
        session.set_protocol(Protocol::Resp3);
        let replication = crate::replication::ReplicationHandle::default();
        intercept_for_pubsub(&cmd(&[b"SUBSCRIBE", b"news"]), &session, &replication, 1);

        assert_eq!(subscribe_mode_gate(&cmd(&[b"GET", b"k"]), &session), None);
    }

    #[test]
    fn subscribe_queued_inside_a_transaction_is_rejected_at_queue_time() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = crate::replication::ReplicationHandle::default();
        let session = Session::new();
        dispatch_and_log(&engine, &aof, &replication, cmd(&[b"MULTI"]), &session, 1);

        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SUBSCRIBE", b"news"]),
            &session,
            1,
        );

        assert_eq!(
            reply,
            Frame::Error("ERR SUBSCRIBE is not allowed in transactions".into())
        );
        let exec_reply =
            dispatch_and_log(&engine, &aof, &replication, cmd(&[b"EXEC"]), &session, 1);
        assert_eq!(
            exec_reply,
            Frame::Error("EXECABORT Transaction discarded because of previous errors".into())
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests -- --exact psubscribe_delivers_pmessage_on_publish a_resp2_connection_with_an_active_subscription_rejects_an_ordinary_command a_resp2_connection_with_an_active_subscription_still_allows_ping_and_more_subscribe a_resp3_connection_with_an_active_subscription_allows_an_ordinary_command subscribe_queued_inside_a_transaction_is_rejected_at_queue_time`
Expected: FAIL — `PSUBSCRIBE` isn't handled yet (falls through `intercept_for_pubsub`'s `_ =>
None`, so it reaches the generic dispatch path and errors as unknown); `subscribe_mode_gate`
doesn't exist; the `MULTI`-queuing test currently queues `SUBSCRIBE` normally (it's in
`KNOWN_COMMANDS` since Task 1) instead of rejecting it.

- [ ] **Step 3: Add `PSUBSCRIBE`/`PUNSUBSCRIBE` to `intercept_for_pubsub`**

Add two more arms to `intercept_for_pubsub`'s `match name.as_str()`, mirroring `SUBSCRIBE`/
`UNSUBSCRIBE` exactly but against `psubscriptions`/the registry's `p`-prefixed methods and the
`psubscribe`/`punsubscribe` message names:

```rust
        "PSUBSCRIBE" => {
            let tx = ensure_push_channel(session);
            let mut replies = Vec::new();
            for item in &items[1..] {
                let Frame::Bulk(pattern) = item else { continue };
                session
                    .psubscriptions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(pattern.clone());
                replication.pubsub.psubscribe(pattern.clone(), client_id, tx.clone());
                let count = total_subscription_count(session);
                session
                    .subscription_count
                    .store(count, std::sync::atomic::Ordering::Relaxed);
                tracing::debug!(client_id, channel = %crate::logging::escape_ident(&String::from_utf8_lossy(pattern)), count, "subscription changed");
                replies.push(Frame::Push(vec![
                    Frame::Bulk(Bytes::from_static(b"psubscribe")),
                    Frame::Bulk(pattern.clone()),
                    Frame::Integer(count as i64),
                ]));
            }
            Some(Frame::Array(replies))
        }
        "PUNSUBSCRIBE" => {
            let explicit: Vec<Bytes> = items[1..]
                .iter()
                .filter_map(|f| match f {
                    Frame::Bulk(b) => Some(b.clone()),
                    _ => None,
                })
                .collect();
            let targets = if explicit.is_empty() {
                session
                    .psubscriptions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                explicit
            };
            let mut replies = Vec::new();
            for pattern in targets {
                session
                    .psubscriptions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&pattern);
                replication.pubsub.punsubscribe(&pattern, client_id);
                let count = total_subscription_count(session);
                session
                    .subscription_count
                    .store(count, std::sync::atomic::Ordering::Relaxed);
                tracing::debug!(client_id, channel = %crate::logging::escape_ident(&String::from_utf8_lossy(&pattern)), count, "subscription changed");
                replies.push(Frame::Push(vec![
                    Frame::Bulk(Bytes::from_static(b"punsubscribe")),
                    Frame::Bulk(pattern),
                    Frame::Integer(count as i64),
                ]));
            }
            Some(Frame::Array(replies))
        }
```

- [ ] **Step 4: Add `subscribe_mode_gate`**

Add near `auth_gate`:

```rust
/// Real Redis's RESP2 "subscribe mode" restriction: while a RESP2 connection has at least one
/// active channel/pattern subscription, only these commands are legal -- everything else gets
/// this exact error text, matching real Redis's own. RESP3 connections are exempt: push messages
/// arrive out-of-band there (as `Frame::Push`, distinct from command replies), so an ordinary
/// command sent on the same connection stays unambiguous. Checked before
/// `intercept_for_transaction` so a subscribed RESP2 connection cannot open `MULTI` either --
/// `MULTI` is not in real Redis's own allowed set for this mode.
fn subscribe_mode_gate(frame: &Frame, session: &Session) -> Option<Frame> {
    if session
        .subscription_count
        .load(std::sync::atomic::Ordering::Relaxed)
        == 0
        || session.protocol() != Protocol::Resp2
    {
        return None;
    }
    let name = command_name_upper(frame)?;
    match name.as_str() {
        "SUBSCRIBE" | "UNSUBSCRIBE" | "PSUBSCRIBE" | "PUNSUBSCRIBE" | "PING" | "QUIT" => None,
        _ => Some(Frame::Error(
            "ERR only (P)SUBSCRIBE / (P)UNSUBSCRIBE / PING / QUIT / RESET are allowed in this context"
                .into(),
        )),
    }
}
```

Wire it into `dispatch_and_log_inner`, right after the existing `auth_gate` call and before
`intercept_for_transaction`:

```rust
    if let Some(reply) = auth_gate(replication, session, &frame) {
        return reply;
    }
    if let Some(reply) = subscribe_mode_gate(&frame, session) {
        return reply;
    }
    if let Some(reply) =
        intercept_for_transaction(&frame, session, engine, aof, replication, client_id)
    {
        return reply;
    }
```

- [ ] **Step 5: Reject `SUBSCRIBE`-family commands at `MULTI` queue time**

In `intercept_for_transaction`'s `_ => { ... }` branch (the per-command queuing logic), add a
check before the existing `KNOWN_COMMANDS` unknown-command check:

```rust
            if matches!(
                name.as_str(),
                "SUBSCRIBE" | "UNSUBSCRIBE" | "PSUBSCRIBE" | "PUNSUBSCRIBE"
            ) {
                *dirty = true;
                tracing::debug!(command = %name.as_str(), "transaction marked dirty");
                return Some(Frame::Error(format!(
                    "ERR {} is not allowed in transactions",
                    name.as_str()
                )));
            }
            if KNOWN_COMMANDS.binary_search(&name.as_str()).is_err() {
                // ... existing unknown-command handling, unchanged
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests -- --exact psubscribe_delivers_pmessage_on_publish a_resp2_connection_with_an_active_subscription_rejects_an_ordinary_command a_resp2_connection_with_an_active_subscription_still_allows_ping_and_more_subscribe a_resp3_connection_with_an_active_subscription_allows_an_ordinary_command subscribe_queued_inside_a_transaction_is_rejected_at_queue_time`
Expected: PASS.

- [ ] **Step 7: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "$(cat <<'EOF'
Add PSUBSCRIBE/PUNSUBSCRIBE, RESP2 subscribe mode, MULTI rejection

subscribe_mode_gate runs right after auth_gate so a subscribed
RESP2 connection can't open MULTI either, matching real Redis's
allowed-command set for that mode. SUBSCRIBE-family commands
queued inside a transaction are rejected at queue time -- a
connection's subscription state is not a deferrable batch
operation, unlike PUBLISH (Plan 04), which real Redis does allow
to run at EXEC time.
EOF
)"
```

## Next plan

[04-publish-and-pubsub-introspection.md](04-publish-and-pubsub-introspection.md) — `PUBLISH` and
`PUBSUB CHANNELS`/`NUMSUB`/`NUMPAT`, wired into `dispatch_and_log_gated` via the same
`intercept_for_pubsub` this plan built.
