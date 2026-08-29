# Active Expiry Background Task Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** expired keys get removed even if nobody ever reads them again — a periodic sweep, independent of passive expiry, so memory doesn't slowly fill with dead entries nobody happens to touch.

**Architecture:** `Shard::remove_expired` (a `HashMap::retain` under one write lock) and `Engine::active_expire_cycle(shard_idx)` are plain synchronous engine-crate methods — no async runtime dependency added to `crates/engine`. The `server` crate's `serve()` (`crates/server/src/connection.rs`) spawns a `tokio::time::interval` loop alongside the existing accept loop, calling `active_expire_cycle` with a rotating shard index.

**Tech Stack:** `tokio::time::interval` (already available — the workspace's `tokio` dependency already has the `time` feature enabled).

**Spec:** `../../specs/2026-08-30-sprint-4-spec.md` — the "sweep one whole shard per tick" simplification (vs. real Redis's key-level sampling) is an accepted tradeoff, not a bug to fix here.

**Depends on:** `01-ttl-passive-expiry-core.md` (`Entry`, the `Shard` map).

---

### Task 1: `Shard::remove_expired` and `Engine::active_expire_cycle`

**Files:**
- Modify: `crates/engine/src/shard.rs`
- Modify: `crates/engine/src/store.rs`
- Modify: `crates/engine/src/engine.rs`

**Interfaces:**
- Consumes: `Entry::is_expired` (from `01-ttl-passive-expiry-core.md`, private to `shard.rs`).
- Produces: `pub fn remove_expired(&self) -> usize` on `Shard`; `pub fn active_expire_cycle(&self, shard_idx: usize) -> usize` on `Store` and `Engine` (shard count wraps via modulo, so any `shard_idx` is valid input — `02`'s server-side task doesn't need to know the shard count).

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/shard.rs — add to the existing tests module
#[test]
fn remove_expired_deletes_only_expired_entries_and_reports_the_count() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"live"), Value::String(Bytes::from_static(b"v")));
    shard.set(Bytes::from_static(b"dead1"), Value::String(Bytes::from_static(b"v")));
    shard.set(Bytes::from_static(b"dead2"), Value::String(Bytes::from_static(b"v")));
    shard.expire_at(b"dead1", Instant::now() - Duration::from_secs(1));
    shard.expire_at(b"dead2", Instant::now() - Duration::from_secs(1));
    assert_eq!(shard.remove_expired(), 2);
    assert_eq!(shard.keys(), vec![Bytes::from_static(b"live")]);
}

#[test]
fn remove_expired_on_a_shard_with_nothing_expired_removes_nothing() {
    let shard = Shard::new();
    shard.set(Bytes::from_static(b"live"), Value::String(Bytes::from_static(b"v")));
    assert_eq!(shard.remove_expired(), 0);
}
```

```rust
// crates/engine/src/store.rs — add to the existing tests module
#[test]
fn active_expire_cycle_sweeps_the_requested_shard_by_index() {
    let store = Store::new(16);
    // find a key that hashes to shard 0 by trying keys until shard_key_counts confirms it
    let key = Bytes::from_static(b"probe");
    store.set(key.clone(), Value::String(Bytes::from_static(b"v")));
    let shard_idx = store
        .shard_key_counts()
        .iter()
        .position(|&c| c > 0)
        .unwrap();
    store.expire_at(&key, std::time::Instant::now() - std::time::Duration::from_secs(1));
    let removed = store.active_expire_cycle(shard_idx);
    assert_eq!(removed, 1);
}

#[test]
fn active_expire_cycle_wraps_an_out_of_range_shard_index() {
    let store = Store::new(16);
    // shard index 16 wraps to shard 0 (16 % 16 == 0) — must not panic
    assert_eq!(store.active_expire_cycle(16), 0);
}
```

```rust
// crates/engine/src/engine.rs — add to the existing tests module
#[test]
fn active_expire_cycle_removes_expired_keys_in_the_targeted_shard() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    engine.expire_at(b"k", Instant::now() - Duration::from_secs(1));
    // sweep every shard once — the key's shard is wherever it landed
    let total_removed: usize = (0..16).map(|i| engine.active_expire_cycle(i)).sum();
    assert_eq!(total_removed, 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine shard::tests store::tests engine::tests`
Expected: FAIL — `remove_expired`/`active_expire_cycle` not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/shard.rs — add to the `impl Shard` block
pub fn remove_expired(&self) -> usize {
    let mut guard = self.map.write();
    let before = guard.len();
    guard.retain(|_, entry| !entry.is_expired());
    before - guard.len()
}
```

```rust
// crates/engine/src/store.rs — add to the `impl Store` block
pub fn active_expire_cycle(&self, shard_idx: usize) -> usize {
    self.shards[shard_idx % self.shards.len()].remove_expired()
}
```

```rust
// crates/engine/src/engine.rs — add to the `impl Engine` block
pub fn active_expire_cycle(&self, shard_idx: usize) -> usize {
    self.store.active_expire_cycle(shard_idx)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine shard::tests store::tests engine::tests`
Expected: PASS, all tests including the 5 new ones

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/shard.rs`, `crates/engine/src/store.rs`, and `crates/engine/src/engine.rs`
— do not compose the commit message freeform. Suggested subject:
`feat(engine): add active expiry sweep (remove_expired/active_expire_cycle)`.

---

### Task 2: server-side sweep loop

**Files:**
- Modify: `crates/server/src/connection.rs`

**Interfaces:**
- Consumes: `Engine::active_expire_cycle` (Task 1).
- Produces: `serve()` now also spawns a periodic sweep task; no change to `serve()`'s signature or its existing accept-loop behavior.

- [ ] **Step 1: Write the failing test**

This test proves the sweep loop specifically (not passive expiry, which `KEYS *` alone would
also satisfy once `01-ttl-passive-expiry-core.md` lands): it never issues a single read
against the expiring key, only a direct call to `engine.active_expire_cycle` after the sweep
loop has had time to run, confirming the loop already removed it.

```rust
// crates/server/src/connection.rs — add to the existing tests module
#[tokio::test]
async fn serve_actively_expires_a_key_even_without_any_read_touching_it() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(Engine::new());
    engine.set(
        Bytes::from_static(b"k"),
        engine::Value::String(Bytes::from_static(b"v")),
    );
    engine.expire_at(
        b"k",
        std::time::Instant::now() + std::time::Duration::from_millis(20),
    );
    tokio::spawn(serve(listener, engine.clone()));

    // Wait for a *full* rotation, not just a few ticks: the loop sweeps one shard per
    // 100ms tick, so all 16 shards are only guaranteed covered after ~1.6s — and which
    // shard `k` landed in depends on DefaultHasher, which this test can't predict. 2s
    // leaves headroom over that 1.6s floor. Real (unpaused) time is required here:
    // tokio's clock doesn't advance `std::time::Instant`, which is what `Entry`'s expiry
    // is measured against, so `tokio::time::pause()` would tick the loop without ever
    // making the key expired.
    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;

    // sweeping every shard now should find nothing left to remove — the loop already did it
    let total_removed: usize = (0..16).map(|i| engine.active_expire_cycle(i)).sum();
    assert_eq!(total_removed, 0);

    // the server is still alive and serving other requests, proving the loop didn't crash it
    let mut framed = Framed::new(
        TcpStream::connect(addr).await.unwrap(),
        RespCodec::default(),
    );
    framed
        .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))]))
        .await
        .unwrap();
    assert_eq!(
        framed.next().await.unwrap().unwrap(),
        Frame::Simple("PONG".into())
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rocket-mem connection::tests::serve_actively_expires_a_key_even_without_any_read_touching_it`
Expected: FAIL — `active_expire_cycle` sweeping every shard still finds the one expired key (`total_removed == 1`, not `0`), since no sweep loop exists yet to have already removed it

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/connection.rs — replace the existing `pub async fn serve` function
pub async fn serve(listener: TcpListener, engine: Arc<Engine>) {
    tokio::spawn(active_expire_loop(Arc::clone(&engine)));

    let mut next_client_id: u64 = 1;
    loop {
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        let client_id = next_client_id;
        next_client_id += 1;
        let engine = Arc::clone(&engine);
        tokio::spawn(handle_connection(socket, engine, client_id));
    }
}

/// Sweeps one shard per tick, rotating through all 16 — see
/// ../../specs/2026-08-30-sprint-4-spec.md's active-expiry decision for why a whole-shard
/// sweep (not per-key sampling) is the deliberate simplification here.
async fn active_expire_loop(engine: Arc<Engine>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut shard_idx: usize = 0;
    loop {
        interval.tick().await;
        engine.active_expire_cycle(shard_idx);
        shard_idx = shard_idx.wrapping_add(1);
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rocket-mem connection::tests::serve_actively_expires_a_key_even_without_any_read_touching_it`
Expected: PASS

- [ ] **Step 5: Run the full connection test module to confirm no regressions**

Run: `cargo test -p rocket-mem connection::tests`
Expected: PASS, all tests including the 3 pre-existing ones and the 1 new one

- [ ] **Step 6: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 7: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/connection.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): spawn a periodic active-expiry sweep loop in serve()`.
