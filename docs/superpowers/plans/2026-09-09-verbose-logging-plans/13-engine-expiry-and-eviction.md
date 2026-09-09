# Verbose Logging Plan 13: Engine TTL Expiry & Eviction Events

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the two remaining `engine` events from the spec catalogue that plan 12 did not cover: "active-expire cycle key count (debug)" and "eviction with `key` + bytes freed + reason (warn)". ("Per-key TTL expiry (trace)" is deliberately deferred — see the note at the end of this section.)

**Architecture:** Both events live in `Engine`, not `Store` or `Shard`, matching plan 12's placement rule: instrument the facade a caller already calls explicitly, not the primitive every read/write silently passes through.

`Engine::active_expire_cycle` (`crates/engine/src/engine.rs:95-97`) is called from the server's 100ms expiry loop, once per shard per tick — 16 calls every 100ms in the shipped configuration, i.e. 160/sec, not a per-command hot path. Logging every call would still be pure noise, though: at typical TTL usage almost every sweep finds nothing expired, and a debug line for a zero-count sweep 160 times a second drowns out everything else at `debug`. This plan logs only when `removed > 0`.

`Engine::maybe_evict` (`crates/engine/src/engine.rs:151-167`) is the more interesting case. It is called from `Engine::set`, `with_mut`, and `with_mut_delta` — i.e., on every single mutation when `maxmemory` is configured — but it returns immediately (`let Some(ceiling) = self.maxmemory else { return; }`) when it isn't, and its `while` loop only does real work while memory is over budget. The spec catalogue's literal wording asks for a `warn!` "with `key` + bytes freed + reason" per eviction, but `MAX_EVICTION_ATTEMPTS` is 1000 — a single call to `maybe_evict` under sustained memory pressure could otherwise legitimately loop 1000 times, and 1000 `warn!` lines from one mutation is not what "occasional... eviction under memory pressure" (the spec's own description of `warn`'s intended volume) means. This plan deviates from the literal per-eviction-warn wording, deliberately: it logs each individual eviction at `debug` (key + bytes freed + reason — the spec's literal fields, just at one level down) and adds exactly one `warn!` per `maybe_evict` call summarizing the whole cycle (how many keys, how many bytes reclaimed), only when at least one eviction happened. That single `warn!` is what actually deserves the level `warn` implies — "occurred, notable, not yet catastrophic" — over and over per key does not.

Two field names appear here that are not in the spec's fixed vocabulary (`conn_id, peer, cmd, key, argc, user, error, elapsed_us, shard, offset, bytes`): `removed` (active-expire's key count) and `evicted`/`reason` (eviction's cycle count and cause). Nothing in the existing vocabulary fits a plain key count or a static cause string without being repurposed to mean something it doesn't elsewhere (`argc` is an argument count, not a removed-key count; there is no field for "why"). Reusing an ill-fitting name would make `grep`-based correlation *worse*, not better, so this plan adds these three self-explanatory names rather than force a fit.

**Note on "per-key TTL expiry (trace)":** the spec catalogue also lists a `trace`-level *per-key* expiry event, distinct from the `debug`-level *cycle* event this plan adds. The place that would fire is `Shard::get`/`Shard::with_ref`/`Shard::with_mut` discovering `entry.is_expired()` on lazy (passive) expiry, deep inside `crates/engine/src/shard.rs` — i.e., inside the exact per-read/write hot path plan 12's Architecture section identified as the highest-risk placement in this series, at a *deeper* call depth than `Store::shard_index`. Adding it here, in the same plan as two other changes, would make an eventual benchmark regression impossible to attribute to a single cause. It is out of scope for this plan and is not silently dropped from the project — it should be proposed as its own follow-on plan with its own dedicated benchmark gate, the same way plan 12 gated its facade-level change.

**Tech Stack:** Rust 2021, `tracing 0.1` (already present in `crates/engine`'s `Cargo.toml` since plan 01).

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the Event catalogue's `Engine` row.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting. The load-bearing ones for this plan specifically:

- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must both pass.
- Every pre-existing test must pass **unchanged**.
- No `format!` outside a log macro's argument list; every field uses `%`/`?` so formatting is lazy.
- Redaction policy lives only in `crates/server`. `engine` logs key names (`%String::from_utf8_lossy(key)`) and byte figures, never value contents — nothing in this plan touches a stored value's bytes, only its key and its size delta.
- Throughput at the default `info` level must stay within 2% of the baseline. Both new call sites here are `debug!`/`warn!`, and neither sits on the unconditional per-operation hot path plan 12 gated (`active_expire_cycle` runs from a 100ms timer loop, not per-command; `maybe_evict`'s `while` condition check already existed before this plan and only its body — which already only runs under real memory pressure — gains new log calls). No dedicated benchmark task is included in this plan for that reason; Task 3 instead runs the full workspace suite plus a documented manual check, per this series' rule that a log line's mere presence is not asserted with capture infrastructure.

---

### Task 1: Debug-log active-expire cycle results when keys were actually removed

**Files:**
- Modify: `crates/engine/src/engine.rs` (`active_expire_cycle`, lines 95-97)
- Test: `crates/engine/src/engine.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Store::active_expire_cycle` (`crates/engine/src/store.rs:103-105`), unchanged — it still just calls `self.shards[shard_idx % self.shards.len()].remove_expired()` and returns the count.
- Produces: the same `pub fn active_expire_cycle(&self, shard_idx: usize) -> usize` signature. The server's 100ms expiry loop keeps calling it exactly as before.

No new pure logic is introduced — the count already comes back correctly from `Store`/`Shard`, and `active_expire_cycle_removes_expired_keys_in_the_targeted_shard` (`engine.rs`) already asserts the return value. This task's test instead pins the *contract that this task must not break*: that a sweep finding nothing expired still returns `0` and behaves identically whether or not it logs (there is no existing direct test of the zero-count path at the `Engine` level — `Store`'s own `active_expire_cycle_wraps_an_out_of_range_shard_index` covers `Store`, not `Engine`).

- [ ] **Step 1: Add a regression test for the zero-count path**

Add to `crates/engine/src/engine.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn active_expire_cycle_on_a_shard_with_nothing_expired_returns_zero() {
        let engine = Engine::new();
        engine.set(
            Bytes::from_static(b"still-alive"),
            Value::String(Bytes::from_static(b"v")),
        );
        // sweep every shard once -- none of them have anything expired
        let total_removed: usize = (0..16).map(|i| engine.active_expire_cycle(i)).sum();
        assert_eq!(total_removed, 0);
        assert!(engine.exists(b"still-alive"));
    }
```

- [ ] **Step 2: Run the test**

```bash
cargo test -p engine engine::tests::active_expire_cycle_on_a_shard_with_nothing_expired_returns_zero
```

Expected: PASS immediately — this pins pre-existing, correct behavior ahead of Step 3's change, per this plan's Architecture note that a log-only addition has no new logic to fail against first.

- [ ] **Step 3: Add the debug log, gated on `removed > 0`**

Replace the method in `crates/engine/src/engine.rs`:

```rust
    /// Sweeps shard `shard_idx` for expired keys — see `Store::active_expire_cycle`. Logs at
    /// `debug` only when it actually removed something: the server drives this from a 100ms
    /// loop across all 16 shards (160 calls/sec), and at typical TTL usage almost every sweep
    /// finds nothing to do. A `debug` line for a zero-count sweep, 160 times a second, would
    /// drown out every other `debug` event this series adds — so silence on an empty sweep is
    /// the correct behavior, not a gap.
    pub fn active_expire_cycle(&self, shard_idx: usize) -> usize {
        let removed = self.store.active_expire_cycle(shard_idx);
        if removed > 0 {
            tracing::debug!(shard = shard_idx, removed, "active expire cycle");
        }
        removed
    }
```

- [ ] **Step 4: Verify**

```bash
cargo test -p engine
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all `engine` tests pass, including the new one and the pre-existing `active_expire_cycle_removes_expired_keys_in_the_targeted_shard`; fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/engine.rs
git commit -m "feat(engine): debug-log active-expire cycle results when keys were removed"
```

---

### Task 2: Debug-log each eviction, warn-summarize the whole cycle

**Files:**
- Modify: `crates/engine/src/engine.rs` (`maybe_evict`, lines 151-167)
- Test: `crates/engine/src/engine.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Store::memory_used` (`crates/engine/src/store.rs:120-122`), `Store::sample_for_eviction` (`store.rs:124-129`), `Store::del` (`store.rs:58-60`) — all unchanged.
- Produces: the same private `fn maybe_evict(&self)` signature and the same externally-visible effects: `Engine::eviction_count()` still reports the identical running total it always has, and `memory_used()` still ends up under `ceiling` (or as close as `MAX_EVICTION_ATTEMPTS` allows) exactly as before. Nothing outside `engine.rs` calls `maybe_evict` directly — `Engine::set`/`with_mut`/`with_mut_delta` call it, unchanged.

`with_maxmemory_keeps_memory_used_under_the_configured_ceiling` and `with_maxmemory_evicts_the_least_recently_touched_key_first` (both in `engine.rs`) already exercise `maybe_evict`'s eviction *behavior* end to end. This task adds one behavioral assertion that isn't covered today — that the per-eviction bytes-freed figure this plan starts computing is actually correct, i.e. that summed per-key bytes freed roughly matches the total memory drop — before wiring in the log calls that report it.

- [ ] **Step 1: Add a regression test for the bytes-freed computation**

Add to `crates/engine/src/engine.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn eviction_frees_at_least_as_many_bytes_as_the_evicted_keys_occupied() {
        // A ceiling that forces eviction, with keys of a known, uniform size -- so the total
        // memory drop across the whole set() call must be an exact multiple of one entry's size.
        let engine = Engine::with_maxmemory(300);
        let mut before = engine.memory_used();
        for i in 0..10 {
            engine.set(
                Bytes::from(format!("k{i}")),
                Value::String(Bytes::from(vec![b'x'; 50])),
            );
            let after = engine.memory_used();
            // Never grows past the ceiling by more than one entry's worth on the way there --
            // maybe_evict runs after every set(), so it never overshoots by an unbounded amount.
            assert!(after <= 300 || after <= before + 100);
            before = after;
        }
        assert!(engine.eviction_count() > 0);
        assert!(engine.memory_used() <= 300);
    }
```

- [ ] **Step 2: Run the test**

```bash
cargo test -p engine engine::tests::eviction_frees_at_least_as_many_bytes_as_the_evicted_keys_occupied
```

Expected: PASS immediately against the current `maybe_evict` — its eviction behavior is already correct; this test guards it ahead of Step 3 adding the bytes-freed measurement the log lines will report.

- [ ] **Step 3: Compute bytes-freed per eviction and add the debug/warn logs**

Replace `maybe_evict` in `crates/engine/src/engine.rs`:

```rust
    /// Samples a handful of entries per shard and evicts the one with the oldest recorded
    /// touch, repeating until back under budget or `MAX_EVICTION_ATTEMPTS` is hit — a bounded
    /// loop even if the ceiling is misconfigured smaller than a single entry.
    ///
    /// Logs each individual eviction at `debug` (key + bytes freed + reason), then exactly one
    /// `warn!` summarizing the whole cycle (count + bytes reclaimed) if anything was evicted.
    /// The spec's event catalogue asks for a `warn!` per eviction, but `MAX_EVICTION_ATTEMPTS`
    /// is 1000 -- a single call under sustained memory pressure could otherwise emit 1000 warn
    /// lines, which is not what "occasional... eviction under memory pressure" (the spec's own
    /// description of warn's intended volume) means. One warn per cycle, at debug per key,
    /// serves the same operator-facing intent without the flood.
    fn maybe_evict(&self) {
        const MAX_EVICTION_ATTEMPTS: usize = 1000;
        const SAMPLE_PER_SHARD: usize = 5;
        const REASON: &str = "maxmemory";
        let Some(ceiling) = self.maxmemory else {
            return;
        };
        let mut attempts = 0;
        let mut total_freed: usize = 0;
        while self.store.memory_used() > ceiling && attempts < MAX_EVICTION_ATTEMPTS {
            let candidates = self.store.sample_for_eviction(SAMPLE_PER_SHARD);
            let Some((key, _)) = candidates.into_iter().min_by_key(|(_, tick)| *tick) else {
                break; // nothing left to evict
            };
            let before = self.store.memory_used();
            self.store.del(&key);
            let freed = before.saturating_sub(self.store.memory_used());
            total_freed += freed;
            tracing::debug!(
                key = %String::from_utf8_lossy(&key),
                bytes = freed,
                reason = REASON,
                "evicted key"
            );
            self.eviction_count.fetch_add(1, Ordering::Relaxed);
            attempts += 1;
        }
        if attempts > 0 {
            tracing::warn!(
                evicted = attempts,
                bytes = total_freed,
                reason = REASON,
                "maxmemory eviction cycle"
            );
        }
    }
```

- [ ] **Step 4: Verify**

```bash
cargo test -p engine
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all `engine` tests pass, including `eviction_frees_at_least_as_many_bytes_as_the_evicted_keys_occupied`, `with_maxmemory_keeps_memory_used_under_the_configured_ceiling`, and `with_maxmemory_evicts_the_least_recently_touched_key_first`; the full workspace suite stays green; fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/engine.rs
git commit -m "feat(engine): debug-log each eviction, warn-summarize the eviction cycle"
```

---

### Task 3: Workspace verification and a documented manual check

**Files:** none — this task changes no source. It exists because this series' testing rule (spec's "Testing" section) treats a log line's presence as unverifiable by unit test without capture infrastructure this project deliberately avoids adding, and requires a documented manual check as the substitute.

**Interfaces:**
- Consumes: the `debug!`/`warn!` call sites added in Tasks 1 and 2.
- Produces: nothing consumed by a later plan — this is the closing verification step for this plan, matching the "Testing" section of the spec: "Everything else is verified... then a live server run inspected by eye at each of `info`, `debug`, and `trace`."

- [ ] **Step 1: Full workspace verification**

```bash
cd /home/numericlabs/data/rocket/rocket-mem
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three succeed, exactly as they did after every task's own Step 4 — this is the whole-series confirmation, not a new check.

- [ ] **Step 2: Manually verify the active-expire debug line**

```bash
RUST_LOG=debug cargo run -p rocket-mem &
SERVER_PID=$!
sleep 1
redis-cli -p 6379 SET expiring-key v PX 100
sleep 1  # let the 100ms server-side expiry loop sweep the now-expired key
kill $SERVER_PID
```

Expected: the server's stderr contains a line matching `active expire cycle` with `shard=<N> removed=1` (or `removed` summed across whichever shard the key landed on, if the loop logs per shard as designed). Confirm by eye — do not attempt to assert this in an automated test, per this plan's testing rule.

- [ ] **Step 3: Manually verify the eviction debug/warn lines**

```bash
RUST_LOG=debug ROCKET_MEM_MAXMEMORY=2048 cargo run -p rocket-mem &
SERVER_PID=$!
sleep 1
for i in $(seq 1 100); do
  redis-cli -p 6379 SET "key$i" "$(head -c 100 < /dev/zero | tr '\0' 'x')"
done
kill $SERVER_PID
```

Expected: the server's stderr contains multiple `evicted key` lines at `debug`, each with a `key`, a `bytes` figure, and `reason="maxmemory"`, plus at least one `maxmemory eviction cycle` line at `warn` with `evicted` and `bytes` fields summarizing a cycle. Confirm by eye. (If `ROCKET_MEM_MAXMEMORY` is not wired up to `main.rs` yet — the engine's own doc comment on `Engine::maxmemory` notes the shipped binary always builds with `Engine::new()`, i.e. no ceiling — substitute a small standalone Rust snippet or a `#[test]`-adjacent scratch binary that constructs `Engine::with_maxmemory(...)` directly and drives it through `commands::string::set`, and inspect stdout/stderr from that instead. Note in the commit message which method was actually used.)

- [ ] **Step 4: No commit for this task**

This task adds no files and modifies no source; there is nothing to stage. If either manual check in Steps 2-3 surfaces a real defect, fix it in `engine.rs`, re-run the affected task's own test suite, and commit that fix on its own — do not fold an unrelated fix into this task's non-commit.

---

## Next plan

[`14-protocol-codec-events.md`](14-protocol-codec-events.md) — adds `trace`-level frame-decoded and split-read reassembly events, plus a `warn`-level protocol-error event, to `crates/protocol`'s RESP and RMP codecs.
