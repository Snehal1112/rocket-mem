# Pub/Sub Plan 05: Connection Loop & Disconnect Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A subscribed RESP connection actually receives pushed messages, and a disconnected
connection's registrations are cleaned up from `PubSubRegistry` rather than leaking.

**Architecture:** `connection.rs`'s `handle_connection` read loop
(`crates/server/src/connection.rs:262`) gains a `tokio::select!` between `framed.next()` (the
client's own commands) and `session.push_rx` (messages `PUBLISH` pushed to this connection from
elsewhere), reached only once `push_rx` is `Some` (i.e. the connection has subscribed at least
once). `ClientGuard` (`crates/server/src/connection.rs:198`), which already runs cleanup exactly
once on every one of `handle_connection`'s return paths via `Drop`, gains a `client_id` field and
now also calls a new `PubSubRegistry::remove_all(client_id)` in its `Drop` impl, alongside its
existing `connection_closed()` call.

**Tech Stack:** Rust, `tokio::select!`, `tokio_util::codec::Framed`.

**Spec:** [../../specs/2026-09-11-pubsub-spec.md](../../specs/2026-09-11-pubsub-spec.md) — see
its "Connection loop" and "Disconnect" sections.

**Global Constraints:** see
[01-frame-push-variant.md](01-frame-push-variant.md)'s `## Global Constraints` section.

---

### Task 1: `PubSubRegistry::remove_all` and `ClientGuard` disconnect cleanup

**Files:**
- Modify: `crates/server/src/pubsub.rs` (new `remove_all` method)
- Modify: `crates/server/src/connection.rs:198-204` (`ClientGuard`) and `:276` (its construction)
- Modify: `crates/server/src/rmp_connection.rs:175` (its construction)

**Interfaces:**
- Produces: `pub(crate) fn remove_all(&self, client_id: u64)` on `PubSubRegistry` — removes
  `client_id` from every channel and pattern it's registered under, wherever they are.
  `ClientGuard`'s tuple grows to `ClientGuard(Arc<ReplicationHandle>, u64)`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/pubsub.rs`'s test module:

```rust
    #[test]
    fn remove_all_drops_a_clients_channel_and_pattern_registrations() {
        let registry = PubSubRegistry::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx.clone());
        registry.psubscribe(Bytes::from_static(b"sports.*"), 1, tx);

        registry.remove_all(1);

        assert_eq!(registry.publish(b"news", &Bytes::from_static(b"x")), 0);
        assert_eq!(registry.publish(b"sports.scores", &Bytes::from_static(b"x")), 0);
        assert_eq!(registry.num_pat(), 0);
    }

    #[test]
    fn remove_all_leaves_other_clients_registrations_untouched() {
        let registry = PubSubRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx1);
        registry.subscribe(Bytes::from_static(b"news"), 2, tx2);

        registry.remove_all(1);

        assert_eq!(registry.publish(b"news", &Bytes::from_static(b"x")), 1);
    }
```

Add to `crates/server/src/connection.rs`'s test module (near the existing
`serve_closes_the_connection_cleanly_when_the_client_disconnects` test):

```rust
    #[tokio::test]
    async fn a_disconnected_subscribers_registrations_are_removed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::default());
        tokio::spawn(serve(
            listener,
            engine,
            aof,
            Arc::clone(&replication),
            Arc::from("test-node"),
        ));

        let mut subscriber = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        subscriber
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SUBSCRIBE")),
                Frame::Bulk(Bytes::from_static(b"news")),
            ]))
            .await
            .unwrap();
        subscriber.next().await.unwrap().unwrap(); // the subscribe confirmation

        drop(subscriber);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(
            replication
                .pubsub
                .publish(b"news", &Bytes::from_static(b"hello")),
            0,
            "a disconnected subscriber must not still be counted as delivered-to"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem pubsub::tests -- --exact remove_all_drops_a_clients_channel_and_pattern_registrations remove_all_leaves_other_clients_registrations_untouched`
Expected: FAIL to compile — `remove_all` doesn't exist yet.

Run: `cargo test -p rocket-mem connection::tests::a_disconnected_subscribers_registrations_are_removed`
Expected: FAIL — currently hangs or fails since nothing prunes the dead subscriber on clean
disconnect (only a failed `send` prunes it today, and nothing has published in between to trigger
that lazy prune) -- `publish` would actually still report `1` delivered right up until a
send fails, meaning this test's assertion of `0` fails without the fix. If a dropped `mpsc`
receiver already causes `tx.send` to fail immediately (it does, once the receiver is dropped),
this specific test might pass by coincidence via `publish`'s existing lazy-prune-on-send-failure
path -- confirm this by reading `PubSubRegistry::publish`'s current body (Plan 02) before assuming
this test needs the `remove_all` wiring to pass. If it already passes without Step 3's connection.rs
change, keep the test (it's still valid coverage) but note in the commit message that `remove_all`
Task 1 delivers is exercised directly by the `pubsub.rs` unit tests, while this integration test
mainly proves the wiring doesn't regress once it exists.

- [ ] **Step 3: Implement `remove_all` and wire it into `ClientGuard`**

Add to `crates/server/src/pubsub.rs`:

```rust
    /// Removes `client_id`'s registration from every channel and pattern, wherever they are.
    /// Called once by `ClientGuard::drop` on every connection-close path, so a disconnected
    /// subscriber's entries don't linger until the next failed `publish` send happens to prune
    /// them.
    pub(crate) fn remove_all(&self, client_id: u64) {
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        channels.retain(|_, subs| {
            subs.retain(|s| s.client_id != client_id);
            !subs.is_empty()
        });
        drop(channels);
        let mut patterns = self.patterns.lock().unwrap_or_else(|e| e.into_inner());
        patterns.retain(|_, subs| {
            subs.retain(|s| s.client_id != client_id);
            !subs.is_empty()
        });
    }
```

In `crates/server/src/connection.rs`, change `ClientGuard`:

```rust
pub(crate) struct ClientGuard(pub(crate) Arc<ReplicationHandle>, pub(crate) u64);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.connection_closed();
        self.0.pubsub.remove_all(self.1);
    }
}
```

Update its construction in `connection.rs` (around line 276):

```rust
    let _client_guard = ClientGuard(Arc::clone(&replication), client_id);
```

And in `rmp_connection.rs` (around line 175) — RMP has no `SUBSCRIBE` support in this series (the
spec is RESP-only for v1), so this call always removes nothing for an RMP connection, but is
harmless and keeps `ClientGuard`'s constructor uniform across both protocols:

```rust
    let _client_guard = ClientGuard(Arc::clone(&replication), client_id);
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem pubsub::tests
cargo test -p rocket-mem connection::tests::a_disconnected_subscribers_registrations_are_removed
```

Expected: PASS.

- [ ] **Step 5: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/pubsub.rs crates/server/src/connection.rs crates/server/src/rmp_connection.rs
git commit -m "$(cat <<'EOF'
Clean up pub/sub registrations on disconnect

ClientGuard's Drop already runs exactly once on every
handle_connection return path (decode error, clean EOF, feed
failure); it now also calls PubSubRegistry::remove_all so a
disconnected subscriber's channel/pattern entries don't linger
until the next failed publish send happens to prune them.
EOF
)"
```

---

### Task 2: `tokio::select!` in the read loop

**Files:**
- Modify: `crates/server/src/connection.rs:279-291` (`handle_connection`'s read loop)

**Interfaces:**
- Consumes: `session.push_rx` (Plan 03 Task 1).

- [ ] **Step 1: Write the failing test**

Add to `crates/server/src/connection.rs`'s test module:

```rust
    #[tokio::test]
    async fn a_subscribed_connection_receives_a_published_message_between_its_own_commands() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::default());
        tokio::spawn(serve(
            listener,
            engine,
            aof,
            Arc::clone(&replication),
            Arc::from("test-node"),
        ));

        let mut subscriber = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        subscriber
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SUBSCRIBE")),
                Frame::Bulk(Bytes::from_static(b"news")),
            ]))
            .await
            .unwrap();
        assert_eq!(
            subscriber.next().await.unwrap().unwrap(),
            Frame::Push(vec![
                Frame::Bulk(Bytes::from_static(b"subscribe")),
                Frame::Bulk(Bytes::from_static(b"news")),
                Frame::Integer(1),
            ])
        );

        let mut publisher = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        publisher
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"PUBLISH")),
                Frame::Bulk(Bytes::from_static(b"news")),
                Frame::Bulk(Bytes::from_static(b"hello")),
            ]))
            .await
            .unwrap();
        assert_eq!(publisher.next().await.unwrap().unwrap(), Frame::Integer(1));

        assert_eq!(
            subscriber.next().await.unwrap().unwrap(),
            Frame::Push(vec![
                Frame::Bulk(Bytes::from_static(b"message")),
                Frame::Bulk(Bytes::from_static(b"news")),
                Frame::Bulk(Bytes::from_static(b"hello")),
            ])
        );

        // The connection can still send its own commands afterward -- receiving the push
        // didn't consume its ability to read a next request. PING is in the RESP2
        // subscribe-mode allowed set (see subscribe_mode_gate), so a plain PONG is expected,
        // not another push.
        subscriber
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))]))
            .await
            .unwrap();
        assert_eq!(
            subscriber.next().await.unwrap().unwrap(),
            Frame::Simple("PONG".into())
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem connection::tests::a_subscribed_connection_receives_a_published_message_between_its_own_commands`
Expected: FAIL — either it hangs (the read loop never checks `push_rx`, so the second
`subscriber.next().await` never resolves) or times out. If it hangs, interrupt it and note that
as the observed failure rather than waiting indefinitely.

- [ ] **Step 3: Add the `tokio::select!` branch**

In `crates/server/src/connection.rs`'s `handle_connection`, replace the loop's frame-fetch (the
existing lines at approximately 279-291):

```rust
        let next = match pending.take() {
            Some(n) => n,
            None => framed.next().await,
        };
```

with:

```rust
        let next = match pending.take() {
            Some(n) => n,
            None => {
                let mut rx_guard = session.push_rx.lock().unwrap_or_else(|e| e.into_inner());
                match rx_guard.as_mut() {
                    Some(rx) => {
                        tokio::select! {
                            frame = framed.next() => frame,
                            Some(push) = rx.recv() => {
                                drop(rx_guard);
                                if framed.send(push).await.is_err() {
                                    return; // client went away
                                }
                                continue;
                            }
                        }
                    }
                    None => {
                        drop(rx_guard);
                        framed.next().await
                    }
                }
            }
        };
```

Note the `None` arm explicitly drops `rx_guard` before awaiting `framed.next()` -- holding a
`std::sync::MutexGuard` across an `.await` is a correctness smell (it can't be held across a
`.await` that might yield to another task on the same thread while a *different* task tries the
same lock); dropping it first is cheap insurance even though in practice no other task ever
contends for this connection's own `push_rx`. `session` must already be in scope in this function
(it is -- `dispatcher::Session::with_peer_addr(peer)` a few lines above the loop).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem connection::tests::a_subscribed_connection_receives_a_published_message_between_its_own_commands`
Expected: PASS.

- [ ] **Step 5: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three pass clean. Pay particular attention to `cargo test --workspace` timing out
or hanging anywhere else in `connection.rs`'s existing test suite -- a mis-placed `select!` branch
that accidentally starves `framed.next()` would manifest as a hang in an unrelated, previously
passing test, not necessarily this new one.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/connection.rs
git commit -m "$(cat <<'EOF'
Deliver pushed pub/sub messages on the RESP read loop

tokio::select! races framed.next() (the client's own commands)
against session.push_rx.recv() (messages PUBLISH pushed from
elsewhere), reached only once push_rx is Some -- an ordinary
connection that never subscribes pays one Mutex::lock check per
loop iteration and nothing else.
EOF
)"
```

## Next plan

[06-replication-forwarding.md](06-replication-forwarding.md) — the follower-side `sync_once`
interception that delivers a leader's `PUBLISH` to a follower's own locally-subscribed clients.
