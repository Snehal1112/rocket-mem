# Sprint 3 — Full Command Set: Keys, Collections & Sorted Sets: Spec & Design

**Goal:** broad command coverage across every Sprint 1 data type, plus a new sorted-set type, plus a concurrency-safe `SCAN` — matching `../../rocket-mem-sprint-plan.md`'s Sprint 3 backlog and `../../rocket-mem-production-plan.md`'s Weeks 5–6.

**Scope:** covers Sprint 3's 5 backlog items (see `../../rocket-mem-sprint-plan.md`, Sprint 3). This doc fixes the shared design decisions — the EXPIRE-family scope call, the glob-matching feature set, the `SCAN` cursor design, and the sorted-set data structure — that every implementation plan below assumes as ground truth. Individual plans don't re-derive these; they reference this doc.

**Architecture recap:** no layer changes this sprint. Everything here extends the existing three layers (`engine::Value`/`engine::commands::*`, `server::dispatcher::dispatch`) established in Sprints 1–2. No new crates, no new architectural decisions — this is command-surface breadth work.

---

## Decision: `EXPIRE`/`PEXPIRE`/`EXPIREAT`/`TTL`/`PTTL`/`PERSIST` are explicit stubs this sprint

`../../rocket-mem-sprint-plan.md`'s Sprint 3 backlog lists "`EXPIRE` family" under the String/key commands item. **Decision: these six commands return a clear "not supported yet" `Frame::Error`, the same treatment `SET`'s `EX`/`PX` flags already got in Sprint 2** (see `crates/server/src/dispatcher.rs`'s `SET` arm and `CLAUDE.md`'s "Deferred scope" note).

**Why:** any of these commands that actually changes observable behavior needs a place to store a per-key expiry and *something* that checks it — at minimum the passive check-on-read path Week 7 (`../../rocket-mem-production-plan.md`) scopes as its own P0 item, alongside active background sweeping. Building a partial version of that now (e.g. `EXPIRE` that sets a TTL nothing ever checks) would be silently broken, not a real feature, and Sprint 3's own Definition of Done (`../../rocket-mem-sprint-plan.md`) doesn't require TTL correctness — only the command coverage table, `SCAN`, and sorted sets are gated. Building it for real belongs with the rest of Sprint 4's TTL work, where writer and checker land together and get the dedicated test suite Sprint 4 already scopes for it.

**How this shows up:** `01-string-key-commands.md` wires all six names to one dispatcher arm returning `Frame::Error("ERR {name} is not supported yet (planned Sprint 4 — no expiry reaper exists)")`. This is a real, tested response (client gets a clean error, not a hang or a panic) — just not real expiry semantics yet.

## Decision: multi-value `RPUSH`/`LPUSH` stays out of scope

`docs/phase-1-retro.md` flagged single-value-only `RPUSH`/`LPUSH` as debt "for Sprint 3." This spec explicitly does **not** pick that up — `../../rocket-mem-sprint-plan.md`'s actual Sprint 3 backlog table (the authoritative scope for this planning pass) doesn't list it, and pulling it in now would silently expand scope beyond what's tracked there. Left as still-open debt for a future sprint; not silently forgotten, just not this one.

## Glob pattern matching (`KEYS`) — supported syntax

New `crates/engine/src/glob.rs`, `pub fn glob_match(pattern: &[u8], text: &[u8]) -> bool`. Per `../../rocket-mem-sprint-plan.md`'s P2 scope ("polish," not a full implementation) and `../../rocket-mem-production-plan.md`'s Week 5 example tests, this sprint supports exactly three constructs:

| Syntax | Meaning |
|---|---|
| `*` | matches any run of zero or more characters |
| `?` | matches exactly one character |
| `[abc]` | matches exactly one character that appears literally inside the brackets |

**Explicitly out of scope:** character ranges (`[a-z]`), negated classes (`[^abc]`), and escaping (`\*` as a literal star). None of these appear in the production plan's example tests. Note this in the README's command coverage table so it isn't mistaken for a full Redis-glob implementation later.

`KEYS` itself (`engine.keys()` filtered through `glob_match`) is also wired into the dispatcher for the first time this sprint — it existed at the `Engine` level since Sprint 1 but was never reachable over RESP.

## `SCAN` cursor design

Real Redis's `SCAN` cursor survives concurrent rehashing via reverse binary iteration. rocket-mem's keyspace is a **fixed** 16-shard array (`crates/engine/src/store.rs`) that never resizes, so that complexity doesn't apply here. **Decision: the cursor is the next shard index to scan, one shard's full key list per call.**

```rust
// crates/engine/src/store.rs
pub fn scan(&self, cursor: u64) -> (u64, Vec<Bytes>) {
    let idx = cursor as usize;
    if idx >= self.shards.len() {
        return (0, Vec::new());
    }
    let keys = self.shards[idx].keys();
    let next = if idx + 1 >= self.shards.len() { 0 } else { (idx + 1) as u64 };
    (next, keys)
}
```

A full scan is: call with cursor `0`, keep calling with the returned cursor until it comes back `0` again (matching real Redis's "cursor 0 means done" contract). **Correctness guarantee this gives:** a key present in its shard for the entire scan duration is returned exactly once, because each shard is visited exactly once per full scan and `Shard::keys()` takes a read lock and returns a consistent snapshot of that one shard at the moment it's visited. A key added mid-scan to a shard not yet reached will appear; one added to an already-scanned shard won't — this matches Redis's own documented `SCAN` guarantee (no promise about keys added/removed *during* the scan, only about keys present for its entire duration). `03-scan-cursor-iteration.md`'s concurrency stress test asserts exactly this guarantee, not a stronger one.

Wire format: `SCAN cursor` replies with a 2-element array `[cursor-as-bulk-string, array-of-keys]`, matching real Redis's reply shape.

## Sorted set data structure

New `Value::SortedSet(SortedSet)` variant in `crates/engine/src/value.rs`. Per `../../rocket-mem-production-plan.md`'s Week 6 guidance:

```rust
use ordered_float::OrderedFloat;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SortedSet {
    scores: HashMap<Bytes, OrderedFloat<f64>>,
    by_score: BTreeSet<(OrderedFloat<f64>, Bytes)>,
}
```

`OrderedFloat<f64>` (new workspace dependency, `ordered-float = "4"`) is used in **both** maps, not just the `BTreeSet` key — `f64` itself doesn't implement `Eq` (`NaN != NaN`), and `Value` derives `Eq`, so a bare `HashMap<Bytes, f64>` field would break that derive. `zadd`/`zincrby` reject non-finite scores (`NaN`, `inf`) with a syntax error at the dispatcher level before they ever reach `SortedSet`, so `OrderedFloat`'s NaN-ordering behavior never actually matters in practice — this is purely to keep `#[derive(Eq)]` valid.

`by_score` keeps `(score, member)` tuples so ascending iteration is free (`BTreeSet`'s natural order) and ties break lexicographically by member bytes, matching real Redis's tie-break rule. `04-sorted-set-core.md` builds `SortedSet`'s primitives (`insert`/`remove`/`score`/`len`); `05-sorted-set-range-and-rank.md` builds `ZRANGE`/`ZRANK` on top, using a linear `by_score.iter().position(...)` for rank — O(n), not real Redis's O(log n) skip-list rank, which is an accepted simplification at this project's scale (see the production plan's Week 6 deliverable: "reasonable parity, not exact").

`Value::type_name()` gains a `SortedSet(_) => "zset"` arm (Redis's real type name for the type) — this `match` has no wildcard arm today, so the compiler will catch a missing arm if this is skipped.

## `RANDOMKEY` / `SPOP` / `SRANDMEMBER` randomness

New workspace dependency: `rand = "0.8"`. Selecting a uniformly-random element from a `Vec`/`HashSet` without it means hand-rolling a weak PRNG for no real benefit — `rand` is small, standard, and this project already treats "hand-roll vs. pull in a small crate" as a per-feature call (e.g. RESP2 is hand-rolled deliberately for learning value per the production plan; a uniform-random pick isn't a learning goal the same way). Added once in `01-string-key-commands.md` (for `RANDOMKEY`), reused as-is in `08-remaining-set-commands.md` (for `SPOP`/`SRANDMEMBER`).

## `MGET` wrong-type semantics — an intentional divergence from the WRONGTYPE convention

`CLAUDE.md`'s "Correctness conventions" section states WRONGTYPE must never be silently ignored — but `MGET` is Redis's one documented exception: **a non-string key among the requested keys returns `nil` for that key, not a `WRONGTYPE` error for the whole command.** This is real Redis's actual documented behavior (`MGET` never errors), not a rocket-mem shortcut. `01-string-key-commands.md`'s `mget` treats "missing key" and "wrong-type key" identically (both → `None` in the returned `Vec<Option<Bytes>>`) — call this out explicitly in that function's test, since it looks at first glance like the missing-key-semantics convention was misapplied to a wrongtype case.

## `common::EngineError` gains `NoSuchKey`

`RENAME`/`RENAMENX` on a missing source key is a real Redis error (`"no such key"`), not a nil/false return — none of the existing three error paths (`WrongType`, `NotAnInteger`) fit. New variant, `01-string-key-commands.md`:

```rust
#[error("no such key")]
NoSuchKey,
```

Matches the existing `NotAnInteger` variant's convention of no `"ERR "` wire prefix baked into the error text itself (the dispatcher maps `Err(e)` to `Frame::Error(e.to_string())` directly, unchanged from Sprint 2 — see `2026-08-29-sprint-2-spec.md`). This is a known, pre-existing wire-format gap (real Redis prefixes generic errors with `ERR `) inherited from Sprint 2, not something this sprint introduces or is scoped to fix.

## Sequencing

Plans depend on each other in this order (all live in `../plans/2026-08-29-sprint-3-plans/`), numbered by dependency, not backlog-table order:

1. `01-string-key-commands.md` — `GETSET`/`MSET`/`MGET`/`MSETNX`/`RENAME`/`RENAMENX`/`TYPE`/`RANDOMKEY`, plus the `EXPIRE`-family stub decision above and the `NoSuchKey` error variant. Independent of everything else this sprint.
2. `02-glob-pattern-matching-and-keys.md` — `glob_match` + wiring `KEYS` into the dispatcher for the first time. Independent.
3. `03-scan-cursor-iteration.md` — `Store::scan`, `SCAN` dispatcher wiring, concurrency stress test. Independent.
4. `04-sorted-set-core.md` — `Value::SortedSet` + `ZADD`/`ZSCORE`/`ZREM`/`ZCARD`/`ZINCRBY`. Independent (needs the new `ordered-float` dependency).
5. `05-sorted-set-range-and-rank.md` (depends on 4) — `ZRANGE`/`ZRANK`.
6. `06-remaining-list-commands.md` — `LINSERT`/`LSET`/`LREM`/`LTRIM`/`LINDEX`. Independent.
7. `07-remaining-hash-commands.md` — `HINCRBY`/`HKEYS`/`HVALS`/`HMGET`/`HSETNX`. Independent.
8. `08-remaining-set-commands.md` — `SINTER`/`SUNION`/`SDIFF` (+ `STORE` variants)/`SPOP`/`SRANDMEMBER`. Independent (reuses `rand` from item 1).
9. `09-readme-and-sprint-close.md` (depends on 1–8) — README command-coverage table, full workspace verification, Sprint 3 retro note.

## Definition of done for the sprint

Matches Sprint 3 in `../../rocket-mem-sprint-plan.md`:
- [ ] Command coverage table in the repo README updated
- [ ] `SCAN` concurrency stress test passes
- [ ] Sorted set operations covered by tests including score-ordering edge cases
- [ ] `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` clean, `cargo fmt --all -- --check` clean (carried forward from Sprints 1–2, not re-stated per item below)
