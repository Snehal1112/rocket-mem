# Pub/Sub Plan 02: `PubSubRegistry` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new `PubSubRegistry` type that maps channel/pattern names to live subscriber
connections, structurally mirroring `replication.rs`'s existing `ReplicaRegistry`, wired onto
`ReplicationHandle` so every later plan in this series can reach it from `dispatcher.rs`.

**Architecture:** A new file, `crates/server/src/pubsub.rs`, holding `PubSubRegistry` and its
`Subscriber` helper type — no dependency on `dispatcher.rs`, `Session`, or `connection.rs`, so it
can be unit-tested in complete isolation the same way `ReplicaRegistry` already is. One
`PubSubRegistry` instance lives on `ReplicationHandle` as a new `pub pubsub` field, alongside the
existing `pub registry: ReplicaRegistry`.

**Tech Stack:** Rust, `std::sync::Mutex` + `std::collections::HashMap`, `tokio::sync::mpsc`,
`bytes::Bytes`, `engine::glob::glob_match`.

**Spec:** [../../specs/2026-09-11-pubsub-spec.md](../../specs/2026-09-11-pubsub-spec.md) — see
its "`PubSubRegistry`" section.

**Global Constraints:** see
[01-frame-push-variant.md](01-frame-push-variant.md)'s `## Global Constraints` section — CI
gates, TDD discipline, comment style, log-capture-test location, redaction policy, commit
discipline, and the symbol-verification rule all apply here unchanged.

---

### Task 1: `PubSubRegistry` — subscribe/unsubscribe/publish, exact-channel matching only

**Files:**
- Create: `crates/server/src/pubsub.rs`
- Modify: `crates/server/src/lib.rs:9-10` (insert `pub mod pubsub;` between the existing
  `pub mod metrics;` and `pub mod replication;` lines, keeping the file's alphabetical module
  order)

**Interfaces:**
- Produces: `pub(crate) struct PubSubRegistry` with `pub(crate) fn subscribe(&self, channel:
  bytes::Bytes, client_id: u64, tx: tokio::sync::mpsc::UnboundedSender<protocol::Frame>) ->
  usize` (returns this connection's new total channel-subscription count),
  `pub(crate) fn unsubscribe(&self, channel: &[u8], client_id: u64) -> usize` (returns the
  remaining count for that channel; `0` if the channel had no such subscriber, which is not an
  error), and `pub(crate) fn publish(&self, channel: &[u8], message: &bytes::Bytes) -> usize`
  (returns delivered count). This task covers exact-channel matching only — `patterns` and the
  `p`-prefixed methods are Task 2.

- [ ] **Step 1: Write the failing tests**

Create `crates/server/src/pubsub.rs` with this test module (no implementation yet — this step
only adds the file with its tests, which will fail to compile since the types don't exist):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn subscribe_returns_the_subscriber_count_for_that_channel() {
        let registry = PubSubRegistry::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let count = registry.subscribe(Bytes::from_static(b"news"), 1, tx);
        assert_eq!(count, 1);
    }

    #[test]
    fn publish_delivers_to_every_subscriber_of_that_exact_channel() {
        let registry = PubSubRegistry::default();
        let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx1);
        registry.subscribe(Bytes::from_static(b"news"), 2, tx2);

        let delivered = registry.publish(b"news", &Bytes::from_static(b"hello"));

        assert_eq!(delivered, 2);
        assert_eq!(
            rx1.try_recv().unwrap(),
            protocol::Frame::Push(vec![
                protocol::Frame::Bulk(Bytes::from_static(b"message")),
                protocol::Frame::Bulk(Bytes::from_static(b"news")),
                protocol::Frame::Bulk(Bytes::from_static(b"hello")),
            ])
        );
        assert_eq!(
            rx2.try_recv().unwrap(),
            protocol::Frame::Push(vec![
                protocol::Frame::Bulk(Bytes::from_static(b"message")),
                protocol::Frame::Bulk(Bytes::from_static(b"news")),
                protocol::Frame::Bulk(Bytes::from_static(b"hello")),
            ])
        );
    }

    #[test]
    fn publish_to_a_channel_with_no_subscribers_delivers_to_nobody() {
        let registry = PubSubRegistry::default();
        assert_eq!(registry.publish(b"nobody-listening", &Bytes::from_static(b"x")), 0);
    }

    #[test]
    fn publish_prunes_a_subscriber_whose_receiver_was_dropped() {
        let registry = PubSubRegistry::default();
        let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
        drop(rx1); // receiver gone -- this subscriber is dead
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx1);
        registry.subscribe(Bytes::from_static(b"news"), 2, tx2);

        let delivered = registry.publish(b"news", &Bytes::from_static(b"hello"));

        assert_eq!(delivered, 1); // only the live subscriber counted
        assert!(rx2.try_recv().is_ok());

        // The dead subscriber is gone from the registry, not just skipped this once.
        assert_eq!(registry.publish(b"news", &Bytes::from_static(b"again")), 1);
    }

    #[test]
    fn unsubscribe_returns_the_remaining_count_for_that_channel() {
        let registry = PubSubRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx1);
        registry.subscribe(Bytes::from_static(b"news"), 2, tx2);

        let remaining = registry.unsubscribe(b"news", 1);

        assert_eq!(remaining, 1);
    }

    #[test]
    fn unsubscribe_from_a_channel_never_subscribed_to_returns_zero_without_panicking() {
        let registry = PubSubRegistry::default();
        assert_eq!(registry.unsubscribe(b"never-subscribed", 1), 0);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem pubsub::tests`
Expected: FAIL to compile — `PubSubRegistry` does not exist yet. (`rocket-mem` is the `server`
crate's package name, per `crates/server/Cargo.toml`; this matches how every other in-crate test
module in this series so far has been run, e.g. `cargo test -p rocket-mem replication::tests`.)

- [ ] **Step 3: Implement `PubSubRegistry` (exact-channel matching)**

At the top of `crates/server/src/pubsub.rs`, above the test module, add:

```rust
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Mutex;

/// One connection's registration under a channel or pattern: which connection (for
/// `PUBSUB NUMSUB`-style bookkeeping and `remove_all`'s disconnect cleanup) and the outbound
/// channel `PubSubRegistry::publish` pushes messages through.
struct Subscriber {
    client_id: u64,
    tx: tokio::sync::mpsc::UnboundedSender<protocol::Frame>,
}

/// Maps channel/pattern names to their live subscribers. Structurally the same
/// "register a sender, broadcast prunes dead ones" shape `replication::ReplicaRegistry` uses,
/// keyed by channel/pattern instead of by replica address. Both maps use `std::sync::Mutex`, not
/// `tokio::sync::Mutex`: every access here is a quick, synchronous map operation, never held
/// across an `.await` -- matching `ReplicaRegistry`'s own justification for the same choice.
#[derive(Default)]
pub(crate) struct PubSubRegistry {
    channels: Mutex<HashMap<Bytes, Vec<Subscriber>>>,
}

impl PubSubRegistry {
    /// Registers `client_id`'s outbound channel under `channel`, returning how many
    /// subscribers `channel` now has (including this one) -- the count `SUBSCRIBE`'s reply
    /// needs.
    pub(crate) fn subscribe(
        &self,
        channel: Bytes,
        client_id: u64,
        tx: tokio::sync::mpsc::UnboundedSender<protocol::Frame>,
    ) -> usize {
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        let subs = channels.entry(channel).or_default();
        subs.push(Subscriber { client_id, tx });
        subs.len()
    }

    /// Removes `client_id`'s registration under `channel`, if any, returning the remaining
    /// subscriber count for that channel. Returns `0`, not an error, when `client_id` was never
    /// subscribed to `channel` at all -- matching real Redis, which never errors on an
    /// `UNSUBSCRIBE` of a channel the client never subscribed to.
    pub(crate) fn unsubscribe(&self, channel: &[u8], client_id: u64) -> usize {
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        let Some(subs) = channels.get_mut(channel) else {
            return 0;
        };
        subs.retain(|s| s.client_id != client_id);
        subs.len()
    }

    /// Delivers `message` on `channel` to every current subscriber, pruning any whose receiver
    /// has been dropped (its connection died). Returns how many sends succeeded. Never itself
    /// returns an error: one dead subscriber must not affect delivery to the others.
    pub(crate) fn publish(&self, channel: &[u8], message: &Bytes) -> usize {
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        let Some(subs) = channels.get_mut(channel) else {
            return 0;
        };
        let frame = protocol::Frame::Push(vec![
            protocol::Frame::Bulk(Bytes::from_static(b"message")),
            protocol::Frame::Bulk(Bytes::copy_from_slice(channel)),
            protocol::Frame::Bulk(message.clone()),
        ]);
        let mut delivered = 0;
        subs.retain(|s| {
            let alive = s.tx.send(frame.clone()).is_ok();
            if alive {
                delivered += 1;
            }
            alive
        });
        delivered
    }
}
```

Add `pub mod pubsub;` to `crates/server/src/lib.rs`, between `pub mod metrics;` and
`pub mod replication;` (alphabetical order, matching the file's existing convention).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem pubsub::tests`
Expected: PASS — all six tests.

- [ ] **Step 5: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/pubsub.rs crates/server/src/lib.rs
git commit -m "$(cat <<'EOF'
Add PubSubRegistry for exact-channel subscriptions

New crates/server/src/pubsub.rs, structurally mirroring
ReplicaRegistry's register/broadcast/prune-on-dead-receiver shape.
Pattern (PSUBSCRIBE) matching lands in the next task.
EOF
)"
```

---

### Task 2: Pattern matching (`PSUBSCRIBE`) and `PUBSUB` introspection reads

**Files:**
- Modify: `crates/server/src/pubsub.rs`

**Interfaces:**
- Consumes: `engine::glob::glob_match(pattern: &[u8], text: &[u8]) -> bool` (already exists at
  `crates/engine/src/glob.rs:6`, used today by `KEYS`).
- Produces: `pub(crate) fn psubscribe(&self, pattern: Bytes, client_id: u64, tx: ...) -> usize`,
  `pub(crate) fn punsubscribe(&self, pattern: &[u8], client_id: u64) -> usize`,
  `pub(crate) fn channels(&self) -> Vec<Bytes>`,
  `pub(crate) fn num_sub(&self, channels: &[Bytes]) -> Vec<(Bytes, usize)>`,
  `pub(crate) fn num_pat(&self) -> usize`. `publish` (Task 1) is extended to also deliver to
  matching patterns as `pmessage` frames.

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/pubsub.rs`'s test module:

```rust
    #[test]
    fn publish_delivers_to_a_matching_pattern_as_a_pmessage_frame() {
        let registry = PubSubRegistry::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        registry.psubscribe(Bytes::from_static(b"news.*"), 1, tx);

        let delivered = registry.publish(b"news.sports", &Bytes::from_static(b"hello"));

        assert_eq!(delivered, 1);
        assert_eq!(
            rx.try_recv().unwrap(),
            protocol::Frame::Push(vec![
                protocol::Frame::Bulk(Bytes::from_static(b"pmessage")),
                protocol::Frame::Bulk(Bytes::from_static(b"news.*")),
                protocol::Frame::Bulk(Bytes::from_static(b"news.sports")),
                protocol::Frame::Bulk(Bytes::from_static(b"hello")),
            ])
        );
    }

    #[test]
    fn publish_delivers_to_both_exact_and_pattern_subscribers_of_the_same_channel() {
        let registry = PubSubRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news.sports"), 1, tx1);
        registry.psubscribe(Bytes::from_static(b"news.*"), 2, tx2);

        assert_eq!(registry.publish(b"news.sports", &Bytes::from_static(b"x")), 2);
    }

    #[test]
    fn punsubscribe_returns_the_remaining_count_for_that_pattern() {
        let registry = PubSubRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.psubscribe(Bytes::from_static(b"news.*"), 1, tx1);
        registry.psubscribe(Bytes::from_static(b"news.*"), 2, tx2);

        assert_eq!(registry.punsubscribe(b"news.*", 1), 1);
    }

    #[test]
    fn channels_lists_every_channel_with_at_least_one_subscriber() {
        let registry = PubSubRegistry::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx);

        assert_eq!(registry.channels(), vec![Bytes::from_static(b"news")]);
    }

    #[test]
    fn num_sub_reports_the_subscriber_count_per_requested_channel() {
        let registry = PubSubRegistry::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx);

        assert_eq!(
            registry.num_sub(&[Bytes::from_static(b"news"), Bytes::from_static(b"empty")]),
            vec![
                (Bytes::from_static(b"news"), 1),
                (Bytes::from_static(b"empty"), 0),
            ]
        );
    }

    #[test]
    fn num_pat_counts_distinct_registered_patterns() {
        let registry = PubSubRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.psubscribe(Bytes::from_static(b"news.*"), 1, tx1);
        registry.psubscribe(Bytes::from_static(b"sports.*"), 2, tx2);

        assert_eq!(registry.num_pat(), 2);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem pubsub::tests`
Expected: FAIL to compile — `psubscribe`/`punsubscribe`/`channels`/`num_sub`/`num_pat` don't
exist yet, and `publish`'s current implementation never produces a `pmessage` frame.

- [ ] **Step 3: Add the `patterns` map and its methods; extend `publish`**

Add a second field to `PubSubRegistry`:

```rust
#[derive(Default)]
pub(crate) struct PubSubRegistry {
    channels: Mutex<HashMap<Bytes, Vec<Subscriber>>>,
    patterns: Mutex<HashMap<Bytes, Vec<Subscriber>>>,
}
```

Add the pattern-side methods, mirroring `subscribe`/`unsubscribe` exactly:

```rust
    pub(crate) fn psubscribe(
        &self,
        pattern: Bytes,
        client_id: u64,
        tx: tokio::sync::mpsc::UnboundedSender<protocol::Frame>,
    ) -> usize {
        let mut patterns = self.patterns.lock().unwrap_or_else(|e| e.into_inner());
        let subs = patterns.entry(pattern).or_default();
        subs.push(Subscriber { client_id, tx });
        subs.len()
    }

    pub(crate) fn punsubscribe(&self, pattern: &[u8], client_id: u64) -> usize {
        let mut patterns = self.patterns.lock().unwrap_or_else(|e| e.into_inner());
        let Some(subs) = patterns.get_mut(pattern) else {
            return 0;
        };
        subs.retain(|s| s.client_id != client_id);
        subs.len()
    }

    pub(crate) fn channels(&self) -> Vec<Bytes> {
        self.channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    pub(crate) fn num_sub(&self, channels: &[Bytes]) -> Vec<(Bytes, usize)> {
        let map = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        channels
            .iter()
            .map(|c| (c.clone(), map.get(c.as_ref()).map_or(0, Vec::len)))
            .collect()
    }

    pub(crate) fn num_pat(&self) -> usize {
        self.patterns.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
```

Replace `publish`'s body to also deliver to matching patterns:

```rust
    pub(crate) fn publish(&self, channel: &[u8], message: &Bytes) -> usize {
        let mut delivered = 0;
        {
            let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(subs) = channels.get_mut(channel) {
                let frame = protocol::Frame::Push(vec![
                    protocol::Frame::Bulk(Bytes::from_static(b"message")),
                    protocol::Frame::Bulk(Bytes::copy_from_slice(channel)),
                    protocol::Frame::Bulk(message.clone()),
                ]);
                subs.retain(|s| {
                    let alive = s.tx.send(frame.clone()).is_ok();
                    if alive {
                        delivered += 1;
                    }
                    alive
                });
            }
        }
        {
            let mut patterns = self.patterns.lock().unwrap_or_else(|e| e.into_inner());
            patterns.retain(|pattern, subs| {
                if engine::glob::glob_match(pattern, channel) {
                    let frame = protocol::Frame::Push(vec![
                        protocol::Frame::Bulk(Bytes::from_static(b"pmessage")),
                        protocol::Frame::Bulk(pattern.clone()),
                        protocol::Frame::Bulk(Bytes::copy_from_slice(channel)),
                        protocol::Frame::Bulk(message.clone()),
                    ]);
                    subs.retain(|s| {
                        let alive = s.tx.send(frame.clone()).is_ok();
                        if alive {
                            delivered += 1;
                        }
                        alive
                    });
                }
                !subs.is_empty() || !engine::glob::glob_match(pattern, channel)
            });
        }
        delivered
    }
```

Note the `patterns.retain` outer closure: a pattern that matched this publish and is now empty
(every one of its subscribers just got pruned) is removed from the map entirely, so `num_pat`
never counts an empty pattern — a pattern that *didn't* match this publish is left untouched
regardless of whether it's currently empty (this method has no reason to touch it).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem pubsub::tests`
Expected: PASS — every test in the module.

- [ ] **Step 5: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/pubsub.rs
git commit -m "$(cat <<'EOF'
Add pattern matching and PUBSUB introspection to PubSubRegistry

PSUBSCRIBE/PUNSUBSCRIBE against a new patterns map, matched via
engine::glob::glob_match (the same matcher KEYS already uses).
publish() now also delivers pmessage frames to matching patterns.
channels()/num_sub()/num_pat() back PUBSUB's three subcommands.
EOF
)"
```

---

### Task 3: Wire `PubSubRegistry` onto `ReplicationHandle`

**Files:**
- Modify: `crates/server/src/replication.rs:209-213` (the `ReplicationHandle` struct definition)
  and `:380-410` (its `new` constructor)

**Interfaces:**
- Consumes: `PubSubRegistry` (Task 1/2, `pub(crate)` in `crate::pubsub`).
- Produces: `replication.pubsub` — an `Arc<PubSubRegistry>` field every later plan in this series
  reads from `dispatcher.rs`'s command handlers and `replication.rs`'s own follower-apply loop.
  Wrapped in `Arc`, not a bare `PubSubRegistry`, for the same reason `engine: Arc<Engine>` and
  `aof: Option<Arc<AofWriter>>` already are: Plan 06 threads it into the `'static` spawned
  follower task (`replication_client_loop`), which needs its own owned handle rather than a
  borrow of `ReplicationHandle`'s own field.

- [ ] **Step 1: Write the failing test**

Add to `crates/server/src/replication.rs`'s test module (search for
`mod tests` near the end of the file, alongside the existing `ReplicaRegistry`-focused tests
like `broadcast_delivers_to_every_registered_sender`):

```rust
    #[test]
    fn replication_handle_exposes_a_pubsub_registry() {
        let replication = ReplicationHandle::default();
        // Not previously reachable at all -- this only needs to compile and not panic to prove
        // the field exists and is usable from outside replication.rs's own module.
        assert_eq!(replication.pubsub.channels(), Vec::<bytes::Bytes>::new());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem replication::tests::replication_handle_exposes_a_pubsub_registry`
Expected: FAIL to compile — `no field 'pubsub' on type 'ReplicationHandle'`.

- [ ] **Step 3: Add the field**

In `crates/server/src/replication.rs`, add to the `ReplicationHandle` struct (right after the
existing `pub registry: ReplicaRegistry,` field, around line 213):

```rust
    /// Cross-connection pub/sub state: which connections are subscribed to which
    /// channels/patterns. A per-process singleton, like `registry` just above -- single-node
    /// delivery only (see the pub/sub spec's "Cluster scope" section). `Arc`-wrapped, like
    /// `engine`/`aof` below, because Plan 06 threads it into the `'static` spawned follower
    /// task, which needs its own owned handle rather than a borrow of this struct's field.
    pub pubsub: Arc<crate::pubsub::PubSubRegistry>,
```

In `ReplicationHandle::new`'s body (around line 382, right after
`registry: ReplicaRegistry::default(),`), add:

```rust
            pubsub: Arc::new(crate::pubsub::PubSubRegistry::default()),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem replication::tests::replication_handle_exposes_a_pubsub_registry`
Expected: PASS.

- [ ] **Step 5: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three pass clean. `ReplicationHandle` is constructed in many places across
`main.rs` and test modules throughout the workspace via `::new(...)` or `::default()` — since
this task added a field with a default-constructing initializer in both paths, no external call
site needs to change. If any call site constructs `ReplicationHandle` via struct-literal syntax
instead (unlikely, but confirm via `grep -rn "ReplicationHandle {" crates/`), it needs the new
field added too.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "$(cat <<'EOF'
Wire PubSubRegistry onto ReplicationHandle

One PubSubRegistry per process, alongside the existing
ReplicaRegistry -- both are per-process singletons reachable from
dispatcher.rs's command handlers.
EOF
)"
```

## Next plan

[03-session-state-and-subscribe-commands.md](03-session-state-and-subscribe-commands.md) —
`Session` additions and `SUBSCRIBE`/`UNSUBSCRIBE`/`PSUBSCRIBE`/`PUNSUBSCRIBE` command handling.
