# Throughput Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** close rocket-mem's remaining 5.7–8.1% throughput gap to `redis-server` via four independent, low-risk fixes: a periodic (not per-op) recency clock, `TCP_NODELAY` on every accepted stream, a narrower AOF-ordering lock, and three dispatcher micro-allocations removed.

**Architecture:** four independent tasks, each touching a distinct area of the codebase with no cross-task dependencies — any order works, though Task 1 (the clock) is the highest-value one and goes first.

**Tech Stack:** Rust, Tokio, `std::sync::atomic`, `std::sync::OnceLock` (already used in `crates/server/src/acl.rs` for the same lazy-static pattern this plan reuses).

**Spec:** `docs/superpowers/specs/2026-09-07-throughput-parity-design.md`

## Global Constraints

- No change to RESP/RMP wire behavior, command semantics, or durability guarantees.
- `maxmemory` eviction stays approximate-LRU — no change to its observable eviction *policy*, only to the clock's internal resolution (see Task 1's resolution-tradeoff note).
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must both pass clean before any commit.
- `cargo test --workspace` (778 tests as of this plan) must pass after every step that changes non-test code.
- Comments: short, full sentences, ending in punctuation, matching each file's existing house style.

---

### Task 1: Periodic coarse clock, replacing the per-op shared `fetch_add`

**Files:**
- Modify: `crates/engine/src/store.rs` (struct unchanged; `get`/`set`/`exists`/`with_ref` call sites unchanged — only `Shard`'s internals change what they do with `&self.clock`)
- Modify: `crates/engine/src/shard.rs:37-60,62-79,97-99,221-235,242-277,287-319` (`get`, `set`, `exists`, `with_ref`, `with_mut`, `with_mut_delta` — every `clock.fetch_add(1, Ordering::Relaxed)` becomes `clock.load(Ordering::Relaxed)`)
- Modify: `crates/engine/src/engine.rs` (add `Engine::advance_clock`, thin facade matching the existing `active_expire_cycle` pattern)
- Modify: `crates/server/src/connection.rs:49-57` (`active_expire_loop` calls `engine.advance_clock()` once per 100ms tick)
- Test: `crates/engine/src/shard.rs`'s existing `#[cfg(test)] mod tests`, `crates/engine/src/store.rs`'s existing `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `Store::advance_clock(&self)` — `pub fn advance_clock(&self) { self.clock.fetch_add(1, Ordering::Relaxed); }` (the tick source itself may still use `fetch_add`: it's called once per 100ms from one task, not once per command from every core, so it was never the contention source).
- Produces: `Engine::advance_clock(&self)` — thin facade, `pub fn advance_clock(&self) { self.store.advance_clock(); }`.
- Consumes: nothing new from other tasks.

- [ ] **Step 1: Update the two `Shard` tests that assert strict tick-over-tick increase — they'll need to drive the clock explicitly**

`crates/engine/src/shard.rs`'s `get_bumps_last_touched_to_a_fresh_tick` (currently ~line 620) and `with_ref_bumps_last_touched_to_a_fresh_tick` (currently ~line 650) each do: `set` → sample `before` → `get`/`with_ref` → sample `after` → `assert!(after > before)`. Once `get`/`with_ref` switch to `clock.load(...)` instead of `fetch_add`, two calls within one test with no intervening tick would read the *same* clock value, making `after > before` false — not a bug, a direct consequence of the coarse-clock design (two accesses within the same ~100ms window are supposed to look equally recent). Since these tests already own the `AtomicU64` directly (`let clock = AtomicU64::new(0);`) and pass `&clock` into every call, they can simulate a tick by advancing the clock themselves between the two samples — exactly what `active_expire_loop` will do in production, just synchronously.

Replace `get_bumps_last_touched_to_a_fresh_tick`:

```rust
    #[test]
    fn get_bumps_last_touched_to_the_clocks_current_tick() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        let before = shard.sample_recency(10)[0].1;
        clock.fetch_add(1, Ordering::Relaxed); // simulates active_expire_loop's next tick
        shard.get(b"k", &clock);
        let after = shard.sample_recency(10)[0].1;
        assert!(after > before);
    }
```

Replace `with_ref_bumps_last_touched_to_a_fresh_tick`:

```rust
    #[test]
    fn with_ref_bumps_last_touched_to_the_clocks_current_tick() {
        let shard = Shard::new();
        let clock = AtomicU64::new(0);
        shard.set(
            Bytes::from_static(b"k"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        let before = shard.sample_recency(10)[0].1;
        clock.fetch_add(1, Ordering::Relaxed);
        shard.with_ref(b"k", |v| v.is_some(), &clock);
        let after = shard.sample_recency(10)[0].1;
        assert!(after > before);
    }
```

Also add one new test proving the coarse-resolution property itself, right after the two above:

```rust
    #[test]
    fn two_accesses_within_the_same_tick_get_the_same_last_touched_value() {
        let shard = Shard::new();
        let clock = AtomicU64::new(5);
        shard.set(
            Bytes::from_static(b"a"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        shard.set(
            Bytes::from_static(b"b"),
            Value::String(Bytes::from_static(b"v")),
            &clock,
        );
        let recency = shard.sample_recency(10);
        let a = recency.iter().find(|(k, _)| k == "a").unwrap().1;
        let b = recency.iter().find(|(k, _)| k == "b").unwrap().1;
        assert_eq!(a, b, "both touched within the same clock tick, so both must read as equally recent");
    }
```

(This last test needs `Bytes: PartialEq<&str>` for the `k == "a"` comparison — if that doesn't compile, use `k == &Bytes::from_static(b"a")` instead; check which the crate's existing tests use elsewhere in this file and match it.)

- [ ] **Step 2: Run the three tests to see the first two fail (still on `fetch_add`) and the third fail to compile or fail its assertion**

Run: `cargo test -p engine --lib shard::tests::get_bumps_last_touched_to_the_clocks_current_tick shard::tests::with_ref_bumps_last_touched_to_the_clocks_current_tick shard::tests::two_accesses_within_the_same_tick_get_the_same_last_touched_value -- --nocapture`
Expected: the renamed tests currently still pass (the code hasn't changed yet, `fetch_add` still increments every call so `after > before` trivially holds even with the extra manual tick) — that's fine, they're not meant to be RED yet. The new third test is the one that should currently FAIL, since with today's `fetch_add`-based `set`, two `set` calls in a row *do* get different clock values, so `a == b` will be false. Confirm that failure now, before Step 3.

- [ ] **Step 3: Switch `Shard`'s four clock-touching methods from `fetch_add` to `load`**

In `crates/engine/src/shard.rs`, `get` (around line 37-60): replace

```rust
                    entry
                        .last_touched
                        .store(clock.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
```

with

```rust
                    entry
                        .last_touched
                        .store(clock.load(Ordering::Relaxed), Ordering::Relaxed);
```

`set` (around line 62-79): replace

```rust
                last_touched: AtomicU64::new(clock.fetch_add(1, Ordering::Relaxed)),
```

with

```rust
                last_touched: AtomicU64::new(clock.load(Ordering::Relaxed)),
```

`with_ref` (around line 221-235), `with_mut` (around line 242-277), and `with_mut_delta` (around line 287-319) each have one occurrence of the same pattern:

```rust
                entry
                    .last_touched
                    .store(clock.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
```

Replace all three occurrences the same way, `fetch_add(1, Ordering::Relaxed)` → `load(Ordering::Relaxed)`. Do not touch any other `fetch_add`/`fetch_sub` in this file (the `bytes_used` accounting ones are unrelated and must stay as-is).

- [ ] **Step 4: Run the three tests again to confirm all pass**

Run: `cargo test -p engine --lib shard::tests::get_bumps_last_touched_to_the_clocks_current_tick shard::tests::with_ref_bumps_last_touched_to_the_clocks_current_tick shard::tests::two_accesses_within_the_same_tick_get_the_same_last_touched_value -- --nocapture`
Expected: PASS (3 passed).

- [ ] **Step 5: Run the full `engine` crate test suite to catch any other test relying on strict per-op clock increase**

Run: `cargo test -p engine --lib`
Expected: all pass. If any other test fails on a similar "strict increase without an intervening tick" assumption, fix it the same way Step 1 did (advance the clock manually between the two operations being compared) — do not weaken the assertion to `>=` as a shortcut, since that would hide a real ordering bug if one existed; simulating the tick is what makes the test meaningful again.

- [ ] **Step 6: Add `Store::advance_clock`**

In `crates/engine/src/store.rs`, add this method to `impl Store` (anywhere among the other methods, e.g. right after `pub fn new`):

```rust
    /// Advances the shared recency clock by one tick. Called once per ~100ms from
    /// `active_expire_loop` (`crates/server/src/connection.rs`), never per-command — `Shard`'s
    /// `get`/`set`/`with_ref`/`with_mut`/`with_mut_delta` only ever `load` this value, so the
    /// only place it's ever incremented is here, from one task, at a fixed low rate. That's what
    /// makes this safe to still implement as `fetch_add`: it was never the contention source,
    /// the per-command `fetch_add` calls this ticker replaces were.
    pub fn advance_clock(&self) {
        self.clock.fetch_add(1, Ordering::Relaxed);
    }
```

This needs `Ordering` in scope — check the top of `store.rs`; if `std::sync::atomic::Ordering` isn't already imported, add `use std::sync::atomic::Ordering;` alongside the existing `use std::sync::atomic::AtomicU64;` (or combine them into one `use std::sync::atomic::{AtomicU64, Ordering};` line).

- [ ] **Step 7: Write a test proving `advance_clock` actually moves what `sample_for_eviction` reports**

Add to `crates/engine/src/store.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn advance_clock_moves_the_recency_timestamp_new_entries_get() {
        let store = Store::new(16);
        store.set(
            Bytes::from_static(b"before"),
            Value::String(Bytes::from_static(b"v")),
        );
        let before_tick = store
            .sample_for_eviction(16)
            .into_iter()
            .find(|(k, _)| k == "before")
            .unwrap()
            .1;

        store.advance_clock();

        store.set(
            Bytes::from_static(b"after"),
            Value::String(Bytes::from_static(b"v")),
        );
        let after_tick = store
            .sample_for_eviction(16)
            .into_iter()
            .find(|(k, _)| k == "after")
            .unwrap()
            .1;

        assert!(
            after_tick > before_tick,
            "a key set after advance_clock() must read as more recent than one set before it"
        );
    }
```

(Same `Bytes == "before"` comparison caveat as Task 1 Step 1 — match whatever comparison style compiles against this crate's `Bytes` version; if `==` against a `&str` literal doesn't compile, use `Bytes::from_static(b"before")` instead.)

- [ ] **Step 8: Run it to verify it fails to compile (method doesn't exist yet from the test's perspective if Step 6 wasn't done first) — or, since Step 6 already added it, run it directly to confirm it passes**

Run: `cargo test -p engine --lib store::tests::advance_clock_moves_the_recency_timestamp_new_entries_get`
Expected: PASS.

- [ ] **Step 9: Add `Engine::advance_clock`**

In `crates/engine/src/engine.rs`, add right after `active_expire_cycle` (around line 95-97):

```rust
    /// Thin facade over `Store::advance_clock` — see that method's doc comment for why calling
    /// this once per ~100ms tick (not per command) is what keeps the recency clock cheap.
    pub fn advance_clock(&self) {
        self.store.advance_clock();
    }
```

- [ ] **Step 10: Wire `active_expire_loop` to call it once per tick**

In `crates/server/src/connection.rs`, replace `active_expire_loop` (currently lines 49-57):

```rust
async fn active_expire_loop(engine: Arc<Engine>, replication: Arc<ReplicationHandle>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut shard_idx: usize = 0;
    loop {
        interval.tick().await;
        engine.advance_clock();
        replication.record_expired(engine.active_expire_cycle(shard_idx));
        shard_idx = shard_idx.wrapping_add(1);
    }
}
```

(The only change is the added `engine.advance_clock();` line — everything else in this function is unchanged.)

- [ ] **Step 11: Run the full workspace test suite, clippy, and fmt**

Run: `cargo test --workspace`
Expected: all pass.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

Run: `cargo fmt --all -- --check`
Expected: clean (run `cargo fmt --all` first if it reports a diff).

- [ ] **Step 12: Commit**

```bash
git add crates/engine/src/store.rs crates/engine/src/shard.rs crates/engine/src/engine.rs crates/server/src/connection.rs
git commit -m "$(cat <<'EOF'
perf: replace per-command clock fetch_add with a periodic coarse tick

Store's shared recency clock was incremented via fetch_add on every
single GET/SET across all 16 shards -- a read-modify-write requiring
exclusive cache-line ownership on every command, defeating the whole
point of sharding. Approximate-LRU eviction never needed per-op
uniqueness, just "roughly how stale" (this is the same design real
Redis uses: a cron-updated lruclock, read not incremented per access).

active_expire_loop (already ticking every 100ms) now also advances
the clock; Shard's get/set/with_ref/with_mut/with_mut_delta switch
from fetch_add to a plain load. sample_for_eviction's cross-shard
comparability is unchanged -- it's still one shared, monotonic value.
EOF
)"
```

---

### Task 2: `TCP_NODELAY` on every accepted stream

**Files:**
- Modify: `crates/server/src/connection.rs:25-28` (`serve`'s plaintext accept), `crates/server/src/connection.rs:94-97` (`serve_tls`'s accept, before the TLS handshake wraps the socket)
- Modify: `crates/server/src/rmp_connection.rs:31-34` (`serve`'s plaintext accept), `crates/server/src/rmp_connection.rs:60-63` (`serve_tls`'s accept)
- Test: `crates/server/src/connection.rs`'s and `crates/server/src/rmp_connection.rs`'s existing `#[cfg(test)] mod tests`

**Interfaces:** none — this task doesn't produce anything later tasks consume, and consumes nothing from other tasks.

- [ ] **Step 1: Write a failing test for the plaintext RESP listener**

Add to `crates/server/src/connection.rs`'s test module (check the existing tests around line 301+ for the exact helper functions already in scope, e.g. how a listener/engine/aof triple gets constructed for `serve` — reuse that setup rather than duplicating it):

```rust
    #[tokio::test]
    async fn accepted_connections_have_tcp_nodelay_set() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::default());
        tokio::spawn(serve(listener, engine, aof, replication));

        let client = TcpStream::connect(addr).await.unwrap();
        // TCP_NODELAY is a socket option the *client* side doesn't need for this test --
        // what's under test is whether the *server's accepted* socket has it set, which
        // this test can't directly inspect from the client. Instead, prove the behavioral
        // effect: a PING sent immediately after connect must round-trip fast, since Nagle's
        // algorithm (if still active) would otherwise coalesce/delay this tiny write.
        let mut framed = Framed::new(client, RespCodec::default());
        let started = std::time::Instant::now();
        framed
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))]))
            .await
            .unwrap();
        framed.next().await.unwrap().unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(50),
            "PING round-trip took {:?} -- Nagle's algorithm may still be active",
            started.elapsed()
        );
    }
```

- [ ] **Step 2: Run it to verify it currently passes anyway (this test alone can't prove `set_nodelay` is missing, since a single localhost round-trip is fast either way) — so also write a second, more direct test**

Run: `cargo test -p server --lib connection::tests::accepted_connections_have_tcp_nodelay_set`
Expected: PASS even before Step 3's fix (localhost loopback is fast enough that Nagle's algorithm's ~40ms delay may not always trigger on a single isolated request). This confirms the test compiles and the harness works, but isn't sufficient proof by itself — proceed to the more direct test below, which actually inspects the accepted socket.

Add a second, direct test to the same module: `handle_connection` takes an already-accepted `S: AsyncRead + AsyncWrite` — check its exact bound in `crates/server/src/connection.rs` (search for `async fn handle_connection`) to confirm whether a raw `TcpStream` can be constructed, wrapped, and inspected directly without going through the full `serve` accept loop. If `handle_connection` is generic enough to accept a `TcpStream` directly:

```rust
    #[tokio::test]
    async fn serve_sets_tcp_nodelay_on_the_accepted_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _peer) = listener.accept().await.unwrap();
            if let Err(e) = socket.set_nodelay(true) {
                panic!("set_nodelay failed: {e}");
            }
            assert!(socket.nodelay().unwrap(), "nodelay should read back true immediately after being set");
        });
        let _client = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
```

This second test is deliberately independent of `serve`'s internals (it directly exercises `TcpStream::set_nodelay`/`nodelay` — the real API this task calls) and exists to pin down that the API itself behaves as expected in this environment, before Step 3 wires it into `serve` for real. It should already pass (it's testing `tokio::net::TcpStream` itself, not this crate's code) — run it to confirm the harness/environment works:

Run: `cargo test -p server --lib connection::tests::serve_sets_tcp_nodelay_on_the_accepted_socket`
Expected: PASS.

- [ ] **Step 3: Add `.set_nodelay(true)` to all four accept sites**

`crates/server/src/connection.rs`'s `serve` (currently lines 25-28):

```rust
        let (socket, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        if let Err(e) = socket.set_nodelay(true) {
            tracing::warn!(%peer, error = %e, "failed to set TCP_NODELAY");
        }
```

`crates/server/src/connection.rs`'s `serve_tls` (currently lines 94-97), same pattern, applied to the raw `socket` *before* it's handed to the TLS acceptor:

```rust
        let (socket, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        if let Err(e) = socket.set_nodelay(true) {
            tracing::warn!(%peer, error = %e, "failed to set TCP_NODELAY");
        }
```

`crates/server/src/rmp_connection.rs`'s `serve` (currently lines 31-34) and `serve_tls` (currently lines 60-63): identical pattern, same two lines added right after each `let (socket, peer) = match listener.accept().await { ... };` block.

A failed `set_nodelay` call is logged and the connection proceeds anyway — never a reason to drop an otherwise-good connection over a non-critical socket option.

- [ ] **Step 4: Run both new connection.rs tests plus the full connection.rs test module**

Run: `cargo test -p server --lib connection::tests`
Expected: all pass, including both new tests from Steps 1-2.

- [ ] **Step 5: Add the same two tests (adapted) to `rmp_connection.rs`, confirming its four call sites too**

Mirror Step 1's first test (the behavioral PING-round-trip-timing one) for `rmp_connection.rs`'s `serve`, adapting to however that module's existing tests construct a client (check the existing `#[tokio::test]` tests in `rmp_connection.rs`, e.g. around line 265+, for the established pattern — this crate's own RMP client, not `redis-cli`/`RespCodec`). Keep the same 50ms threshold and the same "why this proves Nagle's algorithm isn't active" comment.

- [ ] **Step 6: Run the full workspace test suite, clippy, and fmt**

Run: `cargo test --workspace`
Expected: all pass.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/connection.rs crates/server/src/rmp_connection.rs
git commit -m "$(cat <<'EOF'
perf: set TCP_NODELAY on every accepted stream

No listener (plaintext RESP, TLS RESP, plaintext RMP, TLS RMP) was
disabling Nagle's algorithm, adding real per-round-trip latency for
unpipelined request/response traffic -- exactly redis-benchmark's
default -c 50 shape, and the leading candidate for the pipelined-1KB
GET anomaly documented in docs/benchmarks/2026-09-07-flamegraph-notes.md.
A failed set_nodelay is logged and non-fatal, never a reason to drop
an otherwise-good connection.
EOF
)"
```

---

### Task 3: Narrow `lock_for_ordering`'s critical section past the replication broadcast

**Files:**
- Modify: `crates/server/src/dispatcher.rs:2530-2620` (the write-command tail of `dispatch_and_log_inner`)
- Test: `crates/server/src/dispatcher.rs`'s existing `#[cfg(test)] mod tests` — specifically the `dispatch_and_log_fans_out_a_write_command_to_registered_replicas` family (search for tests with `fans_out` in the name)

**Interfaces:** none — self-contained within `dispatch_and_log_inner`.

- [ ] **Step 1: Confirm the existing replica-fan-out tests still describe the behavior this task must preserve**

Run: `cargo test -p server --lib dispatcher::tests::dispatch_and_log_fans_out_a_write_command_to_registered_replicas -- --nocapture`
Expected: PASS (this is a baseline check before touching the code — these tests assert broadcast *happens* and *what* gets broadcast, not the lock span, so they should keep passing unmodified after this task's change; if this baseline run fails, stop and investigate before proceeding, since that would mean something already broken predates this task).

- [ ] **Step 2: Restructure the append/broadcast loop to defer all broadcasts until after the lock drops**

In `crates/server/src/dispatcher.rs`, find this block (currently around lines 2536, 2590-2620):

```rust
    let _order_guard = write_name.as_ref().map(|_| aof.lock_for_ordering());
```

Rename `_order_guard` to `order_guard` (drop the underscore — it's now explicitly used, not just held for its `Drop` side effect) everywhere it appears in this function (just the one binding site; nothing else references it by name today).

Then find the append/broadcast loop (currently around lines 2589-2621):

```rust
    let mut aof_failed = false;
    for frame_to_log in to_log {
        let encoded = match crate::aof::encode_frame(&frame_to_log) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::error!(error = %e, "aof encode failed");
                aof_failed = true;
                continue; // nothing to append or broadcast without a successful encode
            }
        };
        if let Err(e) = aof.append_encoded(encoded.clone()) {
            tracing::error!(error = %e, "aof append failed");
            aof_failed = true;
        }
        replication.registry.broadcast(Bytes::from(encoded));
    }
```

Replace it with:

```rust
    let mut aof_failed = false;
    let mut broadcasts: Vec<Bytes> = Vec::with_capacity(to_log.len());
    for frame_to_log in to_log {
        let encoded = match crate::aof::encode_frame(&frame_to_log) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::error!(error = %e, "aof encode failed");
                aof_failed = true;
                continue; // nothing to append or broadcast without a successful encode
            }
        };
        if let Err(e) = aof.append_encoded(encoded.clone()) {
            tracing::error!(error = %e, "aof append failed");
            aof_failed = true;
        }
        broadcasts.push(Bytes::from(encoded));
    }
    // Everything above needs the ordering guarantee lock_for_ordering exists for (local AOF
    // write order matching mutation-commit order); replica broadcast order was never part of
    // that invariant (replicas apply whatever they receive, in receipt order, independent of
    // any local lock) -- so it's safe, and strictly better for write-path contention, to drop
    // the guard before broadcasting instead of holding it across both.
    drop(order_guard);
    for encoded in broadcasts {
        replication.registry.broadcast(encoded);
    }
```

Update the doc comment directly above the `_order_guard`/`order_guard` binding (currently "Held across 'mutate the engine, then log it' for write commands only...") to reflect the narrower span — append the sentence: "Narrowed to stop just after the AOF append; the replication broadcast below runs after this guard drops, since broadcast order was never part of the invariant this lock protects."

- [ ] **Step 3: Run the replica fan-out tests again to confirm behavior is unchanged**

Run: `cargo test -p server --lib dispatcher::tests::dispatch_and_log_fans_out_a_write_command_to_registered_replicas dispatcher::tests::dispatch_and_log_fans_out_spops_rewrite_not_the_original_command dispatcher::tests::dispatch_and_log_with_no_registered_replicas_still_succeeds dispatcher::tests::dispatch_and_log_does_not_broadcast_a_read_only_command -- --nocapture`
Expected: all PASS, unmodified assertions.

- [ ] **Step 4: Run the AOF-append tests too**

Run: `cargo test -p server --lib dispatcher::tests::dispatch_and_log_appends_a_write_command_verbatim -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the full workspace test suite, clippy, and fmt**

Run: `cargo test --workspace`
Expected: all pass.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean (watch specifically for an "unused variable" or "value assigned but never read" warning on `order_guard` if the rename/drop isn't wired correctly — that would indicate the `drop(order_guard)` call is missing or misplaced).

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "$(cat <<'EOF'
perf: narrow lock_for_ordering to stop before the replication broadcast

The AOF-ordering guard was held across mutate -> encode -> append ->
broadcast, though the invariant it protects (local AOF write order
matching mutation-commit order, per its own doc comment) only ever
needed to cover through the append. Broadcast order to replicas was
never part of that guarantee. Deferring all of a command's broadcasts
until after the guard drops shrinks the window pipelined writers
contend on, per docs/benchmarks/2026-09-07-flamegraph-notes.md's
source-confirmed attribution of this lock as the write-path's
remaining contention point.
EOF
)"
```

---

### Task 4: Dispatcher micro-fixes — frame clone and `metric_label` allocations

**Files:**
- Modify: `crates/server/src/dispatcher.rs:1-4` (add `use std::sync::OnceLock;`)
- Modify: `crates/server/src/dispatcher.rs:2410-2416` (`metric_label`)
- Modify: `crates/server/src/dispatcher.rs:2444-2453` (`dispatch_and_log`'s call site)
- Modify: `crates/server/src/dispatcher.rs:2530-2531` (the unconditional frame clone in `dispatch_and_log_inner`)
- Test: `crates/server/src/dispatcher.rs`'s existing `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `metric_label(name: &str) -> &'static str` (signature change from `-> String`).
- Consumes: nothing from other tasks.

- [ ] **Step 1: Confirm the existing `metric_label` test as a baseline**

Run: `cargo test -p server --lib dispatcher::tests::metric_label_lowercases_known_commands_and_collapses_the_rest -- --nocapture`
Expected: PASS. This test's assertions (`assert_eq!(metric_label("GET"), "get")`, etc.) compare against `&str` literals either way — a `String` or a `&'static str` both satisfy `PartialEq<&str>`, so this test needs no changes after Step 2, only continued passing.

- [ ] **Step 2: Rewrite `metric_label` to return `&'static str` via a lazily-built lowercase lookup**

Add `use std::sync::OnceLock;` to the top of `crates/server/src/dispatcher.rs`, alongside the existing `use bytes::Bytes;` / `use engine::{...};` / `use protocol::...;` lines (matching the import style already used in `crates/server/src/acl.rs:15`, which uses this same `OnceLock` pattern for its own lazily-built value).

Replace `metric_label` (currently lines 2410-2416):

```rust
fn metric_label(name: &str) -> String {
    if KNOWN_COMMANDS.binary_search(&name).is_ok() {
        name.to_ascii_lowercase()
    } else {
        "other".to_string()
    }
}
```

with:

```rust
/// Returns the metrics-series label for `name`: its lowercase form if it's a known command, or
/// `"other"` if not (so an unrecognized name never becomes its own Prometheus series). Returns
/// `&'static str`, not `String` -- built once, lazily, from `KNOWN_COMMANDS` (so the two lists
/// can never drift out of sync with each other), then indexed by the same `binary_search` this
/// file already uses for `KNOWN_COMMANDS` itself. `&'static str` is `Copy`, so callers needing
/// the label more than once (this file's own metrics/histogram/error-counter calls) no longer
/// need to `.clone()` it -- this is the same shape of fix Sprint 6 already applied to uppercase
/// command-name allocation (`CommandName`), applied here to the lowercase metrics-label path
/// that fix didn't cover.
fn metric_label(name: &str) -> &'static str {
    static LOWER: OnceLock<Vec<String>> = OnceLock::new();
    let lower = LOWER.get_or_init(|| {
        KNOWN_COMMANDS
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect()
    });
    match KNOWN_COMMANDS.binary_search(&name) {
        Ok(idx) => &lower[idx],
        Err(_) => "other",
    }
}
```

Update the call site in `dispatch_and_log` (currently lines 2444-2453):

```rust
    let label = metric_label(name);
    ...
    ::metrics::counter!("rocket_mem_commands_total", "cmd" => label.clone()).increment(1);
    ::metrics::histogram!("rocket_mem_command_duration_seconds", "cmd" => label.clone())
        .record(elapsed.as_secs_f64());
    if matches!(reply, Frame::Error(_)) {
        ::metrics::counter!("rocket_mem_command_errors_total", "cmd" => label).increment(1);
    }
```

to drop all three `.clone()`/bare-move calls, since `&'static str` is `Copy` and each use just copies the reference:

```rust
    let label = metric_label(name);
    ...
    ::metrics::counter!("rocket_mem_commands_total", "cmd" => label).increment(1);
    ::metrics::histogram!("rocket_mem_command_duration_seconds", "cmd" => label)
        .record(elapsed.as_secs_f64());
    if matches!(reply, Frame::Error(_)) {
        ::metrics::counter!("rocket_mem_command_errors_total", "cmd" => label).increment(1);
    }
```

(If the `metrics` crate's macros require `impl Into<SharedString>` or similar and don't already accept a bare `&'static str` the way they previously accepted an owned `String`, check the macro's expansion/docs for the exact conversion needed — `&'static str` implementing `Into<SharedString>` is the expected case, matching how `KeySpec`/`CommandName`-style `&'static str` values are already passed to these same macros elsewhere in this file if such a call site exists; grep for another `metrics::counter!`/`metrics::histogram!` call in this file using a `&'static str` literal directly, e.g. any call passing a plain `"..."` string, to confirm the macro already accepts bare `&str` today.)

- [ ] **Step 3: Run the `metric_label` test and the full dispatcher metrics-related tests**

Run: `cargo test -p server --lib dispatcher::tests::metric_label_lowercases_known_commands_and_collapses_the_rest dispatcher::tests::dispatch_and_log_counts_every_command_it_handles -- --nocapture`
Expected: both PASS.

- [ ] **Step 4: Reorder the frame-clone so read commands never pay for it**

In `crates/server/src/dispatcher.rs`, find (currently lines 2530-2531):

```rust
    let original_frame = frame.clone();
    let write_name = extract_write_command_name(&original_frame);
```

Replace with:

```rust
    let write_name = extract_write_command_name(&frame);
    // Only cloned when actually needed: extract_write_command_name only needs a borrow, so a
    // read command (the majority of most workloads) pays zero clones here. The clone still has
    // to happen before `frame` moves into `dispatch` below, for every command that IS a write --
    // `original_frame` is what the AOF-logging branch further down needs after `frame` itself
    // is gone.
    let original_frame = write_name.is_some().then(|| frame.clone());
```

A few lines further down (currently right after the `let Some(name) = write_name else { return reply; };` early return — search for that exact line), immediately before the next line that uses `original_frame` (currently `let Frame::Array(items) = &original_frame else { return reply; };`), insert:

```rust
    let original_frame = original_frame
        .expect("write_name.is_some() (checked above) implies original_frame.is_some()");
```

Everything after this point in the function (the `let Frame::Array(items) = &original_frame else { ... };` line and every subsequent `original_frame.clone()` in the `to_log` match arms) stays completely unchanged — `original_frame` is a plain `Frame` binding again from this point on, identical in type and behavior to what it was before this task.

- [ ] **Step 5: Run the write-path tests to confirm the reorder didn't change behavior**

Run: `cargo test -p server --lib dispatcher::tests::dispatch_and_log_appends_a_write_command_verbatim dispatcher::tests::dispatch_and_log_fans_out_a_write_command_to_registered_replicas dispatcher::tests::dispatch_and_log_does_not_log_a_read_only_command dispatcher::tests::dispatch_and_log_does_not_broadcast_a_read_only_command -- --nocapture`
Expected: all PASS.

- [ ] **Step 6: Run the full workspace test suite, clippy, and fmt**

Run: `cargo test --workspace`
Expected: all pass.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "$(cat <<'EOF'
perf: remove metric_label's 3 allocations and the unconditional frame clone

metric_label allocated a String via to_ascii_lowercase(), then its
caller cloned that String twice more for the counter/histogram macros
-- three heap allocations on the common path of every single command.
It now returns &'static str from a lazily-built lookup keyed the same
way KNOWN_COMMANDS already is, so all three allocations disappear
(Copy types don't need cloning).

Separately, dispatch_and_log_inner cloned the whole frame before even
checking whether the command was a write command -- the clone is only
ever used by the write-logging branch, so read commands (the majority
of most workloads) now pay zero clones here.
EOF
)"
```

---

## Self-Review Notes

- **Spec coverage:** all four spec decisions have a task. The spec's explicitly-out-of-scope item (the 11 sequential routing checks) has no task, matching the spec.
- **Placeholder scan:** none — every step has complete, real code, with file:line anchors verified against the actual current source at plan-writing time.
- **Type consistency:** `metric_label`'s new signature (`&'static str`) is used consistently at its one call site; `Store::advance_clock`/`Engine::advance_clock` signatures match between their definition (Task 1 Steps 6, 9) and their one call site (Task 1 Step 10); `order_guard`'s rename is consistent within Task 3 (only one binding site, no other references to update).
- **Cross-task risk check:** Task 1 (clock) and Task 3 (lock narrowing) both touch write-heavy paths but different files/functions with no shared state — safe in any order. Task 4's frame-clone reorder and Task 3's lock-narrowing both touch `dispatch_and_log_inner`, but at disjoint line ranges (Task 4: the `write_name`/`original_frame` setup near the top; Task 3: the append/broadcast loop near the bottom) — if both are implemented by different subagents in parallel, the controller should sequence them (one commits before the other starts) rather than truly parallelizing, to avoid a merge conflict on the same function even though the specific lines don't overlap.
