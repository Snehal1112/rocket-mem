# Pub/Sub Plan 06: Replication Forwarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A follower's own locally-subscribed clients receive a `PUBLISH` issued on the leader.

**Architecture:** `Arc<PubSubRegistry>` (Plan 02 Task 3) gets threaded from `ReplicationHandle`
into the `'static` spawned follower task, the same way `engine: Arc<Engine>` and
`aof: Option<Arc<AofWriter>>` already are:
`start_replicating_inner` → `replication_client_loop` → `connect_and_sync` → `sync_once`. Inside
`sync_once`'s apply loop, a buffered frame named `PUBLISH` is intercepted *before* the existing
`crate::dispatcher::dispatch(engine, buffered, ...)` call — calling `pubsub.publish(channel,
message)` directly and skipping `dispatch()` entirely, since `dispatch()` has no way to reach a
`PubSubRegistry` (see the spec's "Why not `dispatch()`" section for the full reasoning: widening
`dispatch()`'s signature would also touch `aof.rs`'s replay loop, which never sees a `PUBLISH`
frame at all since it's never AOF-logged).

**Tech Stack:** Rust, `Arc`, `tokio::spawn`.

**Spec:** [../../specs/2026-09-11-pubsub-spec.md](../../specs/2026-09-11-pubsub-spec.md) — see
its "Why not `dispatch()`" section.

**Global Constraints:** see
[01-frame-push-variant.md](01-frame-push-variant.md)'s `## Global Constraints` section.

---

### Task 1: Thread `Arc<PubSubRegistry>` through to `sync_once`

**Files:**
- Modify: `crates/server/src/replication.rs` — `start_replicating_inner` (~line 580, the
  `tokio::spawn(replication_client_loop(...))` call), `replication_client_loop` (~line 883),
  `connect_and_sync` (~line 939), `sync_once` (~line 1024), and every test that calls
  `sync_once`/`connect_and_sync` directly (found via
  `grep -n "sync_once(\|connect_and_sync(" crates/server/src/replication.rs`).

**Interfaces:**
- Consumes: `replication.pubsub: Arc<PubSubRegistry>` (Plan 02 Task 3).
- Produces: `sync_once` gains a `pubsub: &crate::pubsub::PubSubRegistry` parameter, named
  `_pubsub` for this task only (Task 2 uses it and drops the underscore) so this task's diff
  compiles cleanly without a premature `unused_variables` warning under `-D warnings`.

This task is pure mechanical plumbing: adding one parameter to a chain of four functions and
fixing every call site the compiler then flags. There is no new test to write — correctness here
means "the workspace still builds and every existing test still passes," which the CI gates at
the end of this task already verify. Do not skip Step 1 below in favor of jumping straight to
Step 2: confirming the *current* call count and shapes before editing is what keeps this
mechanical change from silently missing a site.

- [ ] **Step 1: Enumerate every call site before touching anything**

```bash
grep -n "sync_once(\|connect_and_sync(\|replication_client_loop(" crates/server/src/replication.rs
```

Read each result's surrounding ~10 lines. Most are `#[tokio::test]` functions that construct
`engine`, `aof`, `generation`, `status`, and `identity` directly (no full `ReplicationHandle` in
scope) and call `sync_once` or `connect_and_sync` with them — these need a
`let pubsub = crate::pubsub::PubSubRegistry::default();` added nearby and `&pubsub` appended to
the call's argument list. The two real (non-test) call sites are inside `connect_and_sync`'s own
body (the `Some(config) => { ... sync_once(tls_stream, ...) }` and `None => { sync_once(tcp,
...) }` branches) and inside `start_replicating_inner`'s `tokio::spawn(replication_client_loop(
...))` call.

- [ ] **Step 2: Add the parameter to all four function signatures**

In `crates/server/src/replication.rs`:

`sync_once` (add `#[allow(clippy::too_many_arguments)]` above it, matching the existing precedent
on `connection.rs`'s `handle_connection` — this function already has 7 parameters, and adding an
8th crosses clippy's default threshold):

```rust
#[allow(clippy::too_many_arguments)]
async fn sync_once<S>(
    stream: S,
    engine: &Engine,
    generation: &AtomicU64,
    my_generation: u64,
    aof: Option<&AofWriter>,
    _pubsub: &crate::pubsub::PubSubRegistry,
    status: FollowerStatus<'_>,
    identity: &FollowerIdentity,
) -> std::io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
```

`connect_and_sync` (same attribute, same reasoning):

```rust
#[allow(clippy::too_many_arguments)]
async fn connect_and_sync(
    host_port: &str,
    engine: &Engine,
    generation: &Generation,
    aof: Option<&AofWriter>,
    pubsub: &crate::pubsub::PubSubRegistry,
    status: FollowerStatus<'_>,
    tls_client_config: Option<&Arc<rustls::ClientConfig>>,
    identity: &FollowerIdentity,
) -> std::io::Result<()> {
```

Add `pubsub` to both of its internal `sync_once(...)` calls, in the same argument position.

`replication_client_loop` (same attribute, same reasoning):

```rust
#[allow(clippy::too_many_arguments)]
async fn replication_client_loop(
    host_port: String,
    engine: Arc<Engine>,
    generation: Generation,
    aof: Option<Arc<AofWriter>>,
    pubsub: Arc<crate::pubsub::PubSubRegistry>,
    handles: FollowerHandles,
    tls_client_config: Option<Arc<rustls::ClientConfig>>,
    identity: FollowerIdentity,
) {
```

Add `&pubsub` to its internal `connect_and_sync(...)` call, in the same argument position (deref
coercion from `&Arc<PubSubRegistry>` to `&PubSubRegistry` applies the same way it already does for
`&engine` there).

`start_replicating_inner`'s spawn call: add `Arc::clone(&self.pubsub)` as a new line inside the
`tokio::spawn(replication_client_loop(...))` argument list, in the same position (right after
`aof,` and before `FollowerHandles { ... }`).

- [ ] **Step 3: Fix every remaining call site the compiler flags**

```bash
cargo build --workspace --tests 2>&1 | grep -A3 "error\[E0061\]"
```

For each reported test call site: if it already constructs a `ReplicationHandle` (via `::new` or
`::default()`), pass `&replication.pubsub` (or `&<handle_variable>.pubsub`, deref-coerced from
`Arc<PubSubRegistry>`). If it constructs `engine`/`aof`/`generation`/`status`/`identity` directly
without a full handle, add `let pubsub = crate::pubsub::PubSubRegistry::default();` just above the
call and pass `&pubsub`. Repeat until `cargo build --workspace --tests` reports no more `E0061`
(wrong argument count) errors from this chain.

- [ ] **Step 4: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three pass clean — every existing `replication.rs` test (there are over a dozen
`sync_once`-driven ones) must still pass unchanged, since this task adds a parameter without
changing any existing behavior.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "$(cat <<'EOF'
Thread PubSubRegistry into the follower apply loop

Mechanical plumbing only: sync_once gains a pubsub parameter
(currently unused, named _pubsub) threaded from ReplicationHandle
through replication_client_loop and connect_and_sync, mirroring
how engine/aof already reach the spawned follower task. The next
task uses it to deliver a leader's PUBLISH to this follower's own
locally-subscribed clients.
EOF
)"
```

---

### Task 2: Intercept `PUBLISH` in `sync_once`'s apply loop

**Files:**
- Modify: `crates/server/src/replication.rs` (`sync_once`'s apply loop, ~line 1220-1245, right
  before its `let reply = crate::dispatcher::dispatch(engine, buffered, &mut protocol, 0);` call)

**Interfaces:**
- Consumes: `pubsub: &crate::pubsub::PubSubRegistry` (Task 1, dropping the leading underscore
  now that it's used); `PubSubRegistry::publish` (Plan 02).

- [ ] **Step 1: Write the failing test**

Add to `crates/server/src/replication.rs`'s test module, right after the existing
`sync_once_applies_a_streamed_transaction_as_one_unit` test — this is that same test's exact
mock-leader harness (real `TcpListener`, a spawned "fake leader" task writing raw RESP bytes,
`sync_once` run in its own spawned task and aborted once the assertions are ready to run), with
its streamed payload swapped for a single `PUBLISH` frame instead of a `MULTI`/`SET`/`SET`/`EXEC`
sequence, and a `pubsub` argument threaded through as Task 1 wired it:

```rust
    #[tokio::test]
    async fn sync_once_delivers_a_replicated_publish_to_a_locally_subscribed_client() {
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
                .write_all(b"*3\r\n$7\r\nPUBLISH\r\n$4\r\nnews\r\n$5\r\nhello\r\n")
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let engine = std::sync::Arc::new(engine::Engine::new());
        let pubsub = std::sync::Arc::new(crate::pubsub::PubSubRegistry::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        pubsub.subscribe(bytes::Bytes::from_static(b"news"), 1, tx);

        let host_port = addr.to_string();
        let generation = Arc::new(AtomicU64::new(0));
        let sync_task = {
            let engine = std::sync::Arc::clone(&engine);
            let pubsub = std::sync::Arc::clone(&pubsub);
            let generation = Arc::clone(&generation);
            tokio::spawn(async move {
                let stream = tokio::net::TcpStream::connect(&host_port).await.unwrap();
                sync_once(
                    stream,
                    &engine,
                    &generation,
                    0,
                    None,
                    &pubsub,
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
            rx.try_recv().unwrap(),
            protocol::Frame::Push(vec![
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"message")),
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"news")),
                protocol::Frame::Bulk(bytes::Bytes::from_static(b"hello")),
            ])
        );
        // The engine must be untouched -- PUBLISH never reaches dispatch()'s engine-mutation path.
        assert_eq!(engine.keys().len(), 0);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem replication::tests::sync_once_delivers_a_replicated_publish_to_a_locally_subscribed_client`
Expected: FAIL — today's `sync_once` calls `dispatch()` unconditionally for every buffered frame,
so a `PUBLISH` frame reaches the engine as an unknown-to-`dispatch()` command (or worse, is
silently mishandled) rather than being delivered to `pubsub`.

- [ ] **Step 3: Add the interception**

In `sync_once`'s apply loop, immediately before the existing:

```rust
                            let reply =
                                crate::dispatcher::dispatch(engine, buffered, &mut protocol, 0);
```

add:

```rust
                            if replicated_command_name(&buffered) == "PUBLISH" {
                                if let protocol::Frame::Array(items) = &buffered {
                                    if let (
                                        Some(protocol::Frame::Bulk(channel)),
                                        Some(protocol::Frame::Bulk(message)),
                                    ) = (items.get(1), items.get(2))
                                    {
                                        pubsub.publish(channel, message);
                                    }
                                }
                                tracing::debug!(cmd = "PUBLISH", "applied replicated command");
                                continue;
                            }
```

Rename the function's `_pubsub` parameter (Task 1) to `pubsub` now that it's used.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem replication::tests::sync_once_delivers_a_replicated_publish_to_a_locally_subscribed_client`
Expected: PASS.

- [ ] **Step 5: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "$(cat <<'EOF'
Deliver replicated PUBLISH to a follower's local subscribers

sync_once intercepts a buffered PUBLISH frame before it would
reach dispatch() -- which has no way to reach a PubSubRegistry --
and delivers it directly, skipping the engine entirely. A
follower's own locally-subscribed clients now receive messages
published on the leader.
EOF
)"
```

## Next plan

[07-integration-tests-benchmarks-and-docs.md](07-integration-tests-benchmarks-and-docs.md) — the
series-final integration tests, benchmark verification, and documentation updates.
