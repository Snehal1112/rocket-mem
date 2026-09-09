# Verbose Logging Plan 16: Snapshot Events

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Instrument the snapshot subsystem per the spec's catalogue row: save start/finish with `path` + `bytes` + `duration` at `info`, and load with the same fields at `info`.

**Architecture — where these events live, and why:** The catalogue row names `engine/snapshot.rs` and `server/aof.rs` as the two candidate homes. This plan puts every new event in `crates/server`, not `crates/engine`, for a concrete reason: `Engine::snapshot(&self, aof_offset: u64) -> Vec<u8>` (`crates/engine/src/engine.rs:112`) and `Engine::load_snapshot(&self, bytes: &[u8]) -> Result<u64, SnapshotError>` (`crates/engine/src/engine.rs:122`) take and return only bytes and an offset — neither one ever sees a file path, because neither one touches a filesystem. Reading or writing the actual snapshot *file* — `std::fs::read`, `write_snapshot_atomically`'s tmp-write-fsync-rename — happens entirely in `crates/server` (`dispatcher.rs`'s `handle_save`/`write_snapshot_atomically`, `aof.rs`'s `recover`). A "save start/finish with path" event genuinely cannot be written inside `Engine::snapshot`, since `path` is not a value that function has; moving the byte-length and duration halves into the engine and leaving only `path` for the server to bolt on afterward would split one logical event across a crate boundary for no benefit. So both the `SAVE` path and the startup load path get their events entirely in `crates/server`, where `path`, the byte count, and the timing are all already in scope together.

This codebase implements only `SAVE`, not a separate `BGSAVE` command (`dispatcher.rs`'s `is_save_command` matches `"SAVE"` only, `dispatcher.rs:1498`) — `BGREWRITEAOF` is the closest thing to a background save, and its own start/finish events were added in [plan 15](15-aof-events.md), Task 2. This plan's "save" event therefore covers `SAVE` alone.

**Tech Stack:** Rust 2021, `tracing 0.1`, `crates/server/src/dispatcher.rs`, `crates/server/src/aof.rs`.

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the Event catalogue's Snapshot row.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting. The load-bearing ones here:

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` must all pass, with every pre-existing test passing **unchanged**.
- Throughput at the default `info` level must stay within **2%** of the baseline in `docs/benchmarks/2026-09-09-pre-logging-baseline.md`. (Neither event this plan adds is on the write hot path — `SAVE` is an infrequent operator-invoked command and snapshot load runs once at startup — so no benchmark gate task is warranted here; see Task 2's closing note.)
- No `format!` outside a log macro's argument list.
- All log fields use the `%` (Display) or `?` (Debug) sigil so formatting is lazy; plain integers (byte counts, microseconds) need no sigil, and `path.display()` is passed with `%` since it is a `Display` value.
- `Bytes` is never logged via `Debug` on the hot path — not applicable here: neither event logs value contents, only a path, a byte count, and a duration.
- No new atomic counters on the hot path — not applicable here for the same reason.
- Redaction policy lives only in `crates/server`. Nothing in this plan logs value contents, so no redaction is needed.

---

### Task 1: `SAVE` start/finish events

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (`handle_save`, lines 2722–2758)

**Interfaces:**
- Consumes: nothing new — `handle_save` already computes `path` (the resolved, generation-aware snapshot path) and `bytes` (the serialized snapshot) before writing.
- Produces: an `info`-level `"snapshot save starting"` event with `path`, and an `info`-level `"snapshot save finished"` event with `path`, `bytes`, `elapsed_us` on success. A failure path gets no new event — the existing `Frame::Error` replies already surface the failure to the client, and `handle_save`'s two `Err` returns require no new log line to stay within this plan's scope.

`handle_save`'s only new state is a `std::time::Instant` captured once at the top of the function — there is no new return value, no new branch a caller could observe, and no way to unit-assert a log line without a capture harness. Per the Global Constraints' testing exception, verification is the pre-existing `SAVE`-path test suite (`dispatcher.rs`, tests using `cmd(&[b"SAVE"])` at lines 9055, 9082, 9103, 9154, 9392, 9469) staying green, plus the manual check in Step 4.

- [ ] **Step 1: Add the start/finish log calls**

`handle_save` currently reads (lines 2722–2758):

```rust
fn handle_save(
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> Frame {
    // Resolved, never bare: once a rewrite has committed, the manifest names the only
    // snapshot/AOF pair `recover` will ever read, so a `SAVE` written to the bare path would be
    // a permanent no-op that still reports success.
    //
    // The rewrite lock spans the generation read *and* the write below. Without it a rewrite
    // committing in between would leave this snapshot filed under generation `G` while the
    // offset it embeds was measured against generation `G + 1`'s freshly-rotated AOF -- and if
    // that rewrite then failed before its own commit, recovery would pair generation `G`'s AOF
    // with an offset that means nothing in it.
    let _rewrite_guard = aof.lock_for_rewrite();
    let gen = match crate::aof::read_generation(replication.snapshot_path()) {
        Ok(g) => g,
        Err(e) => return Frame::Error(format!("ERR failed to read AOF generation: {e}")),
    };
    let path = crate::aof::generation_path(replication.snapshot_path(), gen);

    let bytes = {
        let _order_guard = aof.lock_all_shards();
        let offset = match aof.current_offset() {
            Ok(o) => o,
            Err(e) => return Frame::Error(format!("ERR failed to read AOF offset: {e}")),
        };
        replication.engine().snapshot(offset)
    };

    match write_snapshot_atomically(&path, &bytes) {
        Ok(()) => {
            replication.record_save();
            Frame::Simple("OK".into())
        }
        Err(e) => Frame::Error(format!("ERR failed to write snapshot: {e}")),
    }
}
```

Change it to:

```rust
fn handle_save(
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> Frame {
    let started = std::time::Instant::now();
    // Resolved, never bare: once a rewrite has committed, the manifest names the only
    // snapshot/AOF pair `recover` will ever read, so a `SAVE` written to the bare path would be
    // a permanent no-op that still reports success.
    //
    // The rewrite lock spans the generation read *and* the write below. Without it a rewrite
    // committing in between would leave this snapshot filed under generation `G` while the
    // offset it embeds was measured against generation `G + 1`'s freshly-rotated AOF -- and if
    // that rewrite then failed before its own commit, recovery would pair generation `G`'s AOF
    // with an offset that means nothing in it.
    let _rewrite_guard = aof.lock_for_rewrite();
    let gen = match crate::aof::read_generation(replication.snapshot_path()) {
        Ok(g) => g,
        Err(e) => return Frame::Error(format!("ERR failed to read AOF generation: {e}")),
    };
    let path = crate::aof::generation_path(replication.snapshot_path(), gen);
    tracing::info!(path = %path.display(), "snapshot save starting");

    let bytes = {
        let _order_guard = aof.lock_all_shards();
        let offset = match aof.current_offset() {
            Ok(o) => o,
            Err(e) => return Frame::Error(format!("ERR failed to read AOF offset: {e}")),
        };
        replication.engine().snapshot(offset)
    };

    match write_snapshot_atomically(&path, &bytes) {
        Ok(()) => {
            replication.record_save();
            tracing::info!(
                path = %path.display(),
                bytes = bytes.len(),
                elapsed_us = started.elapsed().as_micros() as u64,
                "snapshot save finished"
            );
            Frame::Simple("OK".into())
        }
        Err(e) => Frame::Error(format!("ERR failed to write snapshot: {e}")),
    }
}
```

- [ ] **Step 2: Run the `SAVE`-path tests**

```bash
cargo test -p rocket-mem dispatcher::tests -- --test-threads=1 save
```

Expected: every existing `SAVE`-exercising test still passes unchanged (the tests at `dispatcher.rs:9055, 9082, 9103, 9154, 9392, 9469` all assert on the `Frame` reply and/or the file written to disk, neither of which this step touches).

- [ ] **Step 3: Full workspace check**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, all green.

- [ ] **Step 4: Manual verification**

```bash
RUST_LOG=info cargo run -p rocket-mem -- --port 7777 --aof-path /tmp/rm.aof --snapshot-path /tmp/rm.snapshot &
redis-cli -p 7777 set a 1
redis-cli -p 7777 save
# expect "snapshot save starting" with path=/tmp/rm.snapshot, then "snapshot save finished"
# with the same path, a bytes field, and elapsed_us
kill %1
rm -f /tmp/rm.aof* /tmp/rm.snapshot*
```

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(logging): info-level SAVE start/finish events with path, bytes, duration"
```

---

### Task 2: Startup snapshot-load event

**Files:**
- Modify: `crates/server/src/aof.rs` (`recover`, lines 600–648)

**Interfaces:**
- Consumes: nothing new — `recover` already reads the snapshot bytes and calls `engine.load_snapshot(&bytes)`.
- Produces: an `info`-level `"snapshot loaded"` event with `path`, `bytes`, `elapsed_us`, fired only on a successful load. The two existing failure paths (`snapshot offset past end of AOF`, `snapshot unreadable`) already log at `warn` — see `aof.rs:621` and `aof.rs:635` — and this plan does not touch either of those, per the task's instruction to leave existing call sites at their current level and trigger condition.

- [ ] **Step 1: Add the load event**

`recover` currently reads (lines 600–648):

```rust
pub fn recover(aof_path: &Path, snapshot_path: &Path) -> std::io::Result<engine::Engine> {
    let gen = read_generation(snapshot_path)?;
    let aof_path = &generation_path(aof_path, gen);
    let snapshot_path = &generation_path(snapshot_path, gen);

    let engine = engine::Engine::new();
    let start_at = match std::fs::read(snapshot_path) {
        Ok(bytes) => match engine.load_snapshot(&bytes) {
            Ok(offset) => {
                // A missing AOF is distinct from a zero-length one: the former means the
                // snapshot alone is the recovered state (per the spec's hybrid-recovery
                // decision), the latter means the offset genuinely overshoots and the
                // snapshot/AOF pair has diverged.
                let aof_len = match std::fs::metadata(aof_path) {
                    Ok(m) => Some(m.len()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e),
                };
                match aof_len {
                    None => return Ok(engine),
                    Some(len) if offset > len => {
                        tracing::warn!(
                            snapshot_path = %snapshot_path.display(),
                            offset,
                            aof_len = len,
                            "snapshot offset past end of AOF; discarding snapshot and replaying full AOF"
                        );
                        let fresh = engine::Engine::new();
                        let stats = replay_with_stats(aof_path, &fresh, 0)?;
                        tracing::info!(
                            commands = stats.commands,
                            bytes = stats.bytes,
                            elapsed_us = stats.elapsed.as_micros() as u64,
                            "aof recovery replay complete"
                        );
                        return Ok(fresh);
                    }
                    Some(_) => offset,
                }
            }
            Err(e) => {
                tracing::warn!(
                    snapshot_path = %snapshot_path.display(),
                    error = %e,
                    "snapshot unreadable; falling back to full AOF replay"
                );
                0
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
    let stats = replay_with_stats(aof_path, &engine, start_at)?;
    tracing::info!(
        commands = stats.commands,
        bytes = stats.bytes,
        elapsed_us = stats.elapsed.as_micros() as u64,
        "aof recovery replay complete"
    );
    Ok(engine)
}
```

(the `replay_with_stats`/`"aof recovery replay complete"` lines above are [plan 15](15-aof-events.md)'s Task 2 — this task builds on top of it, so if this plan is executed out of order against a tree that still has plain `replay`, adapt the surrounding lines accordingly and keep the `replay`/`replay_with_stats` choice consistent with whichever landed).

Add the load event right after a successful `load_snapshot`, timing only the read-plus-deserialize span:

```rust
    let engine = engine::Engine::new();
    let start_at = match std::fs::read(snapshot_path) {
        Ok(bytes) => {
            let load_started = std::time::Instant::now();
            match engine.load_snapshot(&bytes) {
                Ok(offset) => {
                    tracing::info!(
                        path = %snapshot_path.display(),
                        bytes = bytes.len(),
                        elapsed_us = load_started.elapsed().as_micros() as u64,
                        "snapshot loaded"
                    );
                    // A missing AOF is distinct from a zero-length one: the former means the
                    // snapshot alone is the recovered state (per the spec's hybrid-recovery
                    // decision), the latter means the offset genuinely overshoots and the
                    // snapshot/AOF pair has diverged.
                    let aof_len = match std::fs::metadata(aof_path) {
                        Ok(m) => Some(m.len()),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                        Err(e) => return Err(e),
                    };
                    match aof_len {
                        None => return Ok(engine),
                        Some(len) if offset > len => {
                            tracing::warn!(
                                snapshot_path = %snapshot_path.display(),
                                offset,
                                aof_len = len,
                                "snapshot offset past end of AOF; discarding snapshot and replaying full AOF"
                            );
                            let fresh = engine::Engine::new();
                            let stats = replay_with_stats(aof_path, &fresh, 0)?;
                            tracing::info!(
                                commands = stats.commands,
                                bytes = stats.bytes,
                                elapsed_us = stats.elapsed.as_micros() as u64,
                                "aof recovery replay complete"
                            );
                            return Ok(fresh);
                        }
                        Some(_) => offset,
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        snapshot_path = %snapshot_path.display(),
                        error = %e,
                        "snapshot unreadable; falling back to full AOF replay"
                    );
                    0
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
```

The rest of `recover` (the trailing `replay_with_stats` call and its `"aof recovery replay complete"` log) is unchanged.

Only the successful-load branch moved — from `Ok(offset) => { ... }` to `Ok(offset) => { tracing::info!(...); ... }` — so every branch's actual behavior (which value `start_at` ends up being, which `Ok`/`Err` variant `recover` returns) is identical to before this step.

- [ ] **Step 2: Run the recovery test suite**

```bash
cargo test -p rocket-mem aof::tests::recover
cargo test -p rocket-mem dispatcher::tests -- recover
cargo test -p rocket-mem --test kill_and_recover
```

Expected: every pre-existing `recover_*` test passes unchanged, including `recover_with_a_matching_snapshot_and_offset_loads_the_snapshot_then_only_the_aof_tail` (`aof.rs:1532`) and `recover_with_an_unreadable_snapshot_falls_back_to_a_full_aof_replay` (`aof.rs:1560`) — both exercise branches this step touches only by wrapping them in a new log call, never by changing which branch runs or what it returns.

- [ ] **Step 3: Full workspace check**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, all green.

- [ ] **Step 4: Manual verification**

```bash
RUST_LOG=info cargo run -p rocket-mem -- --port 7777 --aof-path /tmp/rm.aof --snapshot-path /tmp/rm.snapshot &
redis-cli -p 7777 set a 1
redis-cli -p 7777 save
kill %1
RUST_LOG=info cargo run -p rocket-mem -- --port 7777 --aof-path /tmp/rm.aof --snapshot-path /tmp/rm.snapshot &
# expect "snapshot loaded" with path=/tmp/rm.snapshot, a bytes field, and elapsed_us,
# followed by "aof recovery replay complete" for whatever AOF tail follows the snapshot's offset
kill %1
rm -f /tmp/rm.aof* /tmp/rm.snapshot*
```

This plan adds no benchmark-gated code: `SAVE` is an infrequent, operator-invoked command (not on the per-request write path AOF append/fsync sit on), and the snapshot-load event fires exactly once, at startup, before the server accepts any connection — neither can plausibly move the `redis-benchmark` `SET`/`GET` numbers the gate in [plan 15](15-aof-events.md) measures. No benchmark task is included here for that reason.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/aof.rs
git commit -m "feat(logging): info-level snapshot load event on startup recovery"
```

---

## Next plan

[`17-replication-events.md`](17-replication-events.md) — instruments `server/replication.rs` with the PSYNC handshake, replica register/prune, offset progress, and per-command apply events from the spec's Replication catalogue row.
