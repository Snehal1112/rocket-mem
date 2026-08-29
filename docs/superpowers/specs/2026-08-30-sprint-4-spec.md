# Sprint 4 — Expiry, Eviction & AOF Persistence: Spec & Design

**Goal:** data survives a `kill -9` and restart; memory stays bounded under a configured ceiling — matching `../../rocket-mem-sprint-plan.md`'s Sprint 4 goal.

**Scope:** covers Sprint 4's 6 backlog items (see `../../rocket-mem-sprint-plan.md`, Sprint 4, and `../../rocket-mem-production-plan.md`, Weeks 7–8). This doc fixes the shared design decisions — the `Entry`/TTL data model, the AOF wire format and write-command classification, the replay approach, and the LRU approximation — that every implementation plan below assumes as ground truth. Individual plans don't re-derive these; they reference this doc.

**Architecture recap:** this sprint touches all three layers for the first time simultaneously. The storage engine (`crates/engine`) gains TTL and memory-accounting state per key. The `server` crate gains an AOF writer/replayer and two new background `tokio` tasks (active-expiry sweep, periodic fsync) alongside the existing accept loop. The dispatcher stays synchronous and untouched in shape — AOF logging wraps it rather than reaching inside it, so none of the ~250 existing dispatcher-level tests need to change.

---

## Decision: TTL storage lives in a new `Entry` wrapper inside `Shard`, not in `Value`

`Shard`'s map changes from `RwLock<HashMap<Bytes, Value>>` to `RwLock<HashMap<Bytes, Entry>>`, where:

```rust
// crates/engine/src/shard.rs
struct Entry {
    value: Value,
    expires_at: Option<Instant>,
}
```

**Why not a `Value::Expiring(...)` variant instead?** Every `Value`-returning command in `commands/*.rs` (all ~40 of them) pattern-matches on `Value`'s variants directly (`Some(Value::Hash(m)) => ...`). Wrapping expiry in `Value` itself would force every one of those match arms to unwrap an extra layer, rippling through every command file written since Sprint 1. Keeping `Entry` as a `Shard`-internal wrapper means `Engine::get(key) -> Option<Value>` and `Engine::set(key, Value)` keep their existing signatures — every existing command function and every existing test in `crates/engine/src/commands/*.rs` needs zero changes. Only `Shard`, `Store`, and `Engine` change, plus new methods are added alongside the existing ones.

**Why not a separate side-map of expiries?** A `HashMap<Bytes, Instant>` living next to (not inside) the value map means every mutation (`set`, `del`) has to keep two structures in sync under one lock, or risk a race between the two locks. A single map keyed once avoids that entirely.

**Coexistence with `Shard::with_ref`/`with_mut`:** ahead of this sprint, `Shard`/`Store`/`Engine` gained `with_ref`/`with_mut` — borrow-based accessors that let `commands/{hash,list,set,sorted_set}.rs` mutate a stored collection in place instead of cloning it out and writing a replacement back (the old pattern made single-element list push/pop `O(current length)` instead of `O(1)`). `01-ttl-passive-expiry-core.md` keeps these methods, making them expiry-aware (`with_ref` on an expired key runs its closure with `None`, exactly like `get`) rather than removing them — several already-shipped command files call them and would fail to compile otherwise. `07-lru-eviction-maxmemory.md` further threads the recency clock through them (any access counts as "touched," not just `get`/`set`) and re-accounts `bytes_used` around `with_mut`'s closure, since in-place collection growth happens there, not through `set`.

## Decision: passive expiry is "check-then-remove," never silently returned

`Shard::get` takes a read lock first; if the entry exists and isn't expired, it clones and returns the value without ever taking a write lock (the common, hot path). Only when an entry **is** expired does it re-acquire under a write lock to remove it, then returns `None`. This double-checked pattern means the overwhelmingly common case (unexpired reads) never contends on the write lock:

```rust
pub fn get(&self, key: &[u8]) -> Option<Value> {
    {
        let guard = self.map.read();
        match guard.get(key) {
            None => return None,
            Some(entry) if !entry.is_expired() => return Some(entry.value.clone()),
            Some(_) => {} // expired — fall through to remove it under a write lock
        }
    }
    let mut guard = self.map.write();
    if matches!(guard.get(key), Some(e) if e.is_expired()) {
        guard.remove(key);
    }
    None
}
```

`exists`, `del`, and `keys` all route through this same expiry check (`exists` becomes `self.with_ref(key, |v| v.is_some())` — deliberately *not* `self.get(key).is_some()`, which would clone a whole Hash/List/Set out of the map just to discard it, reintroducing exactly the O(collection size) copy `with_ref` was added to remove; `del` treats removing an already-expired entry as "didn't exist," matching real Redis's `DEL` returning 0 for a logically-expired key; `keys` filters expired entries out of its read-lock iteration without removing them, deferring cleanup to `get` or the active sweep). This is `CLAUDE.md`'s existing "missing key ≠ error" convention extended one step: a **logically** missing key (expired) is indistinguishable from a **literally** missing key to every caller.

## Decision: `TtlStatus` enum, not `Option<Option<Duration>>`

Real Redis's `TTL` has three distinct outcomes (key doesn't exist → `-2`, key exists with no TTL → `-1`, key exists with a TTL → seconds remaining), which an `Option<Option<Duration>>` expresses but doesn't name. New `crates/engine/src/engine.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtlStatus {
    NoSuchKey,
    NoExpiry,
    Remaining(Duration),
}
```

`Engine` gains `expire_at(&self, key: &[u8], at: Instant) -> bool` (false if the key doesn't exist or is already expired), `persist(&self, key: &[u8]) -> bool` (false if the key doesn't exist or had no TTL to remove), and `ttl(&self, key: &[u8]) -> TtlStatus`. None of these are `Result`-returning — none of them can fail in a way `common::EngineError` already models (there's no wrong-type case for a TTL operation; every type of value can have a TTL), so a bare `bool`/enum return keeps the API honest about what can actually go wrong. `Store` gets the same three methods, delegating to `shard_for(key)`.

## Decision: `EXPIREAT`/`PEXPIREAT` need `SystemTime`, not just `Instant`

`std::time::Instant` is an opaque, monotonic clock with **no** relationship to wall-clock time — there is no way to construct `Instant` from a Unix timestamp directly. `EXPIRE`/`PEXPIRE` (relative durations) convert straightforwardly: `Instant::now() + Duration::from_secs(n)`. `EXPIREAT`/`PEXPIREAT` (absolute Unix timestamps) need a two-step conversion via `SystemTime`:

```rust
// crates/server/src/dispatcher.rs — used by the EXPIREAT/PEXPIREAT arms in 03-expire-family-and-set-ttl-dispatcher.md
fn instant_from_unix_ms(target_unix_ms: i64) -> std::time::Instant {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let target = UNIX_EPOCH + Duration::from_millis(target_unix_ms.max(0) as u64);
    let delta = target
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO); // target already in the past: expire immediately
    std::time::Instant::now() + delta
}
```

A target already in the past collapses to `Duration::ZERO`, meaning `expires_at` is set to (effectively) right now — the very next passive-expiry check on that key (which happens immediately, since `Instant::now()` always advances) sees it as expired and removes it. This matches real Redis: `EXPIREAT` with a past timestamp deletes the key immediately.

## Decision: active expiry samples one whole shard per tick, not individual keys

Real Redis samples a small random set of *keys with a TTL* across its single keyspace each cycle (it maintains a side-index of "keys with an expiry" for this purpose). Our keyspace is already partitioned into 16 fixed shards (`crates/engine/src/store.rs`) that never resize. **Decision: skip a dedicated "keys with TTL" index — each active-expiry tick sweeps one entire shard (`Shard::remove_expired`, a `HashMap::retain` under one write lock) and rotates to the next shard on the next tick.** At a 100ms tick interval this means every shard gets a full sweep roughly every 1.6 seconds, independent of passive expiry (which still catches any expired key immediately on its next read regardless of sweep timing). This is a deliberate, documented simplification at this project's scale — mirroring the same "accepted tradeoff, not a bug" framing `../../docs/superpowers/specs/2026-08-29-sprint-3-spec.md` used for `ZRANK`'s O(n) scan.

`Engine::active_expire_cycle(&self, shard_idx: usize) -> usize` (returns the count removed, for testability) lives in the engine crate as a **plain synchronous method** — the engine stays free of any async-runtime dependency. The `server` crate's `lib.rs` owns the `tokio::time::interval` loop that calls it, exactly as it already owns the TCP accept loop; this keeps `crates/engine`'s dependency list unchanged (still `common`, `bytes`, `parking_lot`, `rand`, `ordered-float` — no `tokio`).

## Decision: the AOF writer stays synchronous (`std::fs`), dispatcher stays untouched

`dispatch(engine: &Engine, frame: Frame, protocol: &mut Protocol, client_id: u64) -> Frame` is called synchronously from ~250 existing tests across `dispatcher.rs`, `connection.rs`, and `tests/integration.rs`. Changing its signature to thread an AOF handle through it — or making it `async` — would touch every one of those call sites for a concern (durability) that's orthogonal to command dispatch. **Decision: AOF logging is a separate function, `dispatch_and_log`, that wraps `dispatch` rather than modifying it:**

```rust
// crates/server/src/dispatcher.rs — added in 05-aof-dispatch-wiring.md
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
    // clones the frame before dispatch consumes it, so there's still something to log after
    ...
}
```

Only `connection.rs`'s real serving loop calls `dispatch_and_log`; every existing `dispatch(...)` call site — all ~250 of them — is untouched. This also means the AOF writer itself can use plain blocking `std::fs::File` + `std::sync::Mutex<BufWriter<File>>` rather than `tokio::fs`: since `dispatch_and_log` is called synchronously from inside an async task (`connection.rs`'s per-connection loop), a blocking file append is briefly acceptable for a solo project at this scale — the alternative (spinning up a dedicated writer thread and channel, per the production plan's own hedge: "or a dedicated writer thread if async file I/O proves awkward") is a heavier lift for the same effect and is explicitly deferred as a known simplification, not silently skipped.

**Known gap: AOF append order under concurrent connections is not guaranteed to match engine-mutation order.** `dispatch_and_log` calls `dispatch` (which mutates the engine and returns) and only then calls `aof.append`, and that `append` happens outside the engine's per-shard lock. On the multi-threaded tokio runtime, two concurrent clients writing the same key can have their mutations land on the engine in one order but get appended to the AOF in the other order — there is nothing serializing "mutate, then log" across connections. This is a real gap in the argument used below ("AOF logs commands, not verbatim frames") for why command-level (as opposed to state-diff) logging is safe on replay: that argument rests on "same commands, same order, same start state," and this wrapper design does not actually guarantee the "same order" part under concurrency. It is accepted as a known, deferred simplification for this plan rather than fixed now — a proper fix means either appending under the engine's shard lock (which the dispatcher/engine split above was specifically designed to avoid coupling) or funneling all AOF writes through a single ordered channel/writer task, both out of scope here. `06-aof-replay-and-corrupt-recovery.md` should treat this as a standing caveat when reasoning about replay correctness: replay is only guaranteed to reproduce the original state when writes were not, in fact, concurrent on the same keys.

## Decision: AOF logs commands, not verbatim frames — three rewrites are required for correctness

The AOF is "every write command, once applied to the in-memory engine, appended to a log file in RESP format" (`../../rocket-mem-production-plan.md`, Week 8) — but three of Sprint 3/4's commands are **not** safe to log verbatim, because replaying them later would not reproduce the same state:

1. **`SPOP`** picks a *uniformly random* member via `rand::thread_rng()` (`crates/engine/src/commands/set.rs`). Replaying a bare `SPOP key` would pop a *different* random member than the one actually removed at write time. **Fix:** when `dispatch_and_log` sees `"SPOP"` returned `Frame::Bulk(member)` (not `Frame::Null`), it logs `SREM key member` instead — the *effect*, not the command. This is exactly how real Redis's own AOF handles `SPOP`/`SRANDMEMBER`-adjacent nondeterminism.
2. **`EXPIRE`/`PEXPIRE`** (relative durations) logged verbatim would, on replay after a restart, count their `N` seconds from the *replay* time rather than the *original* time — silently extending the key's lifetime by however long the process was down. **Fix:** `dispatch_and_log` rewrites all four of `EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT` into an absolute `PEXPIREAT key <unix-ms>` before logging, computed from `SystemTime::now()` at log time. This does mean the target absolute time is computed twice — once inside `dispatch`'s `Instant`-based `EXPIRE` arm (for the live in-memory expiry), once inside `dispatch_and_log`'s `SystemTime`-based rewrite (for the AOF) — a few microseconds apart. This tiny, harmless duplication is an accepted tradeoff to avoid threading extra "here's the resolved absolute time" data back out through `dispatch`'s `Frame`-only return type.

3. **`SET key value EX n` / `PX n`** (from `03-expire-family-and-set-ttl-dispatcher.md`) carries the same relative-duration problem as `EXPIRE`, just smuggled in as a flag rather than a command name — a static "is this command name in the rewrite list" check catches `EXPIRE` but sails straight past `SET ... EX 100`. **Fix:** when `dispatch_and_log` sees a `SET` that actually applied (reply `Frame::Simple("OK")`, not the `Frame::Null` of a declined `NX`/`XX`), it logs the SET with the `EX`/`PX` flag *and its value* stripped out — every other flag preserved in place — immediately followed by an absolute `PEXPIREAT key <unix-ms>`. A declined `NX`/`XX` `SET` is logged verbatim, TTL flag included: replay re-resolves the condition the same way and applies nothing at all, TTL included, so there is nothing to drift.

No other write command needs a rewrite: everything else is a deterministic function of the arguments and the current stored state, so replaying the verbatim command against the replayed-so-far state reproduces the same final state — including conditional no-ops like `SET k v NX` on an existing key (`Frame::Null`, not an error) or `HSETNX` returning `0`. Logging those verbatim and replaying them is safe because the *replay* state at that point in the sequence is guaranteed to match the *live* state at that point when the command originally ran (same commands, same order, same start state) — the conditional will resolve the same way both times.

## Decision: write-command classification is a static allowlist, not "did the reply look like an error"

`dispatch_and_log` decides whether to log a command by checking its (uppercased) name against a fixed list — not by inspecting whether the reply was `Frame::Error`. A static list is easy to audit against the actual command set and impossible to accidentally widen by a future command whose error handling happens to also return a non-`Error` frame on failure.

```rust
// crates/server/src/aof.rs — added in 04-aof-writer.md
pub const WRITE_COMMANDS: &[&str] = &[
    "SET", "APPEND", "SETRANGE", "GETSET", "MSET", "MSETNX", "INCR", "DECR", "INCRBY",
    "DEL", "EXPIRE", "PEXPIRE", "EXPIREAT", "PEXPIREAT", "PERSIST", "RENAME", "RENAMENX",
    "HSET", "HDEL", "HINCRBY", "HSETNX",
    "RPUSH", "LPUSH", "RPOP", "LPOP", "LSET", "LTRIM", "LREM", "LINSERT",
    "SADD", "SREM", "SPOP", "SINTERSTORE", "SUNIONSTORE", "SDIFFSTORE",
    "ZADD", "ZREM", "ZINCRBY",
];
```

A command is logged when: its name is in `WRITE_COMMANDS`, **and** the reply is not `Frame::Error(_)` (a command that errored — wrong arg count, `WRONGTYPE`, etc. — never reached the engine mutation, so there's nothing to replay).

## Decision: AOF replay calls `dispatch`, never `dispatch_and_log`

Replay reconstructs state by decoding each logged frame back into a command and running it through the plain, non-logging `dispatch` against a fresh `Engine` — calling `dispatch_and_log` during replay would re-append every replayed command back onto the same file being replayed. `crates/server/src/aof.rs` gains `pub fn replay(path: &Path, engine: &Engine) -> std::io::Result<()>`, decoding frames with the same `protocol::codec::RespCodec` the network path already uses (`Decoder::decode` on a growing `BytesMut` filled via `std::fs::read`), stopping cleanly — not panicking — on a truncated/corrupt final frame: any decode error, or a `Ok(None)` that means "more bytes needed" but no more bytes exist (end of file mid-frame), means "stop here, keep everything decoded so far."

## Decision: `MAXMEMORY`/LRU is a size estimate + a recency timestamp, not a true LRU list

Redis's own "approximated LRU" doesn't use a true doubly-linked LRU list either — it samples a handful of keys and evicts whichever sampled key has the oldest access time. This project follows the same shape: `Entry` (from the TTL decision above) gains a `last_touched` field — a logical tick from one `Store`-wide counter, so ticks stay comparable across shards — bumped on every access: `get`/`set` **and** `with_ref`/`with_mut`, since the collection commands only ever touch a key through the latter pair and would otherwise look permanently cold to eviction. `Value` gains `approx_size(&self) -> usize` (a rough byte-size estimate: string/member lengths plus a small per-entry overhead constant — not exact, not meant to be). `Engine::with_maxmemory(bytes: usize) -> Self` opts a new engine into a byte ceiling (`None` by default — unlimited, unless a caller opts in, matching every existing `Engine::new()` call site across ~600 tests staying untouched). After any `set` **or `with_mut`** that pushes `memory_used()` over the ceiling, eviction samples a few entries across shards and removes the ones with the oldest `last_touched`, repeating until back under budget or a bounded attempt limit is hit (never an unbounded loop). `with_mut` is included for the same reason it re-accounts `bytes_used`: an `RPUSH`-only workload grows the keyspace entirely through `with_mut` and would otherwise sit over the ceiling indefinitely with nothing to trigger a sweep.

## Decision: the kill-and-recover test is the one deliberate exception to "in-process only" testing

`../../specs/2026-08-29-sprint-2-spec.md` decided integration tests call `serve()` in-process rather than spawning a subprocess, for speed and determinism. **That decision doesn't extend to the kill-and-recover suite.** Proving a process survives an actual `SIGKILL` — including whatever the OS/filesystem does with in-flight, not-yet-`fsync`'d writes — is exactly the kind of guarantee an in-process test *cannot* simulate (an in-process "pretend to crash" can't reproduce real kernel/filesystem crash semantics). `08-kill-and-recover-tests.md` spawns the actual `rocket-mem` binary (`std::process::Command` on the path Cargo hands the test via `env!("CARGO_BIN_EXE_rocket-mem")` — no manual `cargo build` shell-out, Cargo guarantees the binary is built before the test target runs), sends real `SIGKILL`, and restarts it against the same AOF path. This needs the binary to accept its bind address and AOF path from the environment rather than the hardcoded `127.0.0.1:6379` it uses today (`crates/server/src/main.rs`) — both become overridable via `ROCKET_MEM_ADDR` (default `127.0.0.1:6379`) and `ROCKET_MEM_AOF_PATH` (default `./appendonly.aof`), read once at startup.

## Sequencing

Plans depend on each other in this order (all live in `../plans/2026-08-30-sprint-4-plans/`):

1. `01-ttl-passive-expiry-core.md` — `Entry` wrapper, passive expiry in `Shard`/`Store`/`Engine`, `TtlStatus`, `expire_at`/`persist`/`ttl`. Independent of everything else this sprint.
2. `02-active-expiry-background-task.md` (depends on 1) — `Engine::active_expire_cycle` + the `server`-side sweep loop.
3. `03-expire-family-and-set-ttl-dispatcher.md` (depends on 1) — wires `EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT`/`TTL`/`PTTL`/`PERSIST` (replacing the Sprint 3 stub) and `SET`'s `EX`/`PX` flags (replacing the Sprint 2 stub).
4. `04-aof-writer.md` — `AofWriter`, `FsyncPolicy`, `WRITE_COMMANDS`. Independent of 1–3.
5. `05-aof-dispatch-wiring.md` (depends on 3 for the `EXPIRE`-family rewrite, 4 for `AofWriter`) — `dispatch_and_log`, the `SPOP`→`SREM` and `EXPIRE`-family→`PEXPIREAT` rewrites, wiring into `connection.rs`/`main.rs`.
6. `06-aof-replay-and-corrupt-recovery.md` (Task 1 depends on 4; Task 2's `main.rs` rewrite additionally depends on 5, whose `serve(listener, engine, aof)` signature it calls — and supersedes 5's own `main.rs` version) — replay on startup, corrupt-tail recovery.
7. `07-lru-eviction-maxmemory.md` (depends on 1 for `Entry`, and on 2 — its `shard.rs`/`store.rs` rewrites reproduce `remove_expired`/`active_expire_cycle` wholesale) — memory accounting, recency tracking, eviction.
8. `08-kill-and-recover-tests.md` (depends on 3, 5, 6 all being wired into the real binary) — the durability proof.
9. `09-memory-usage-object-encoding-stubs.md` (depends on 7, for `approx_size`) — `MEMORY USAGE`/`OBJECT ENCODING`.
10. `10-readme-and-sprint-close.md` (depends on 1–9) — README command coverage, final verification, Sprint 4 close-out.

## Definition of done for the sprint

Matches Sprint 4 in `../../rocket-mem-sprint-plan.md`:
- [ ] `kill -9` + restart test passes in CI with all keys intact
- [ ] Corrupt-tail AOF recovery test passes without panicking
- [ ] TTL correctness suite covers both active and passive expiry paths independently
- [ ] `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` clean, `cargo fmt --all -- --check` clean (carried forward from Sprints 1–3, not re-stated per item below)
