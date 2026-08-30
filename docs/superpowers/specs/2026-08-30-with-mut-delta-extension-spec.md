# `with_mut_delta` Extension to Hash/Set/SortedSet — Spec

## Why

`redis-benchmark -t SET,SADD,HSET,LPUSH -n 100000` showed `LPUSH` throughput degrading
monotonically as the benchmarked list grew (18744 → 9484 req/s over the run), while
`SET`/`SADD`/`HSET` stayed flat. Root cause (confirmed by direct reproduction, documented in
this session's conversation, not re-derived here): `Shard::with_mut`
(`crates/engine/src/shard.rs`) re-accounts `bytes_used` for `MAXMEMORY` by calling
`Value::approx_size()` — a full O(current collection size) scan — once before and once after
every mutating call. For a list, this turns "push one element" into O(n) work, so n sequential
pushes cost O(n²) total. `SADD`/`HSET` didn't show it in that benchmark only because
`redis-benchmark`'s default (non-`-r`) key reuse keeps their target collections at size 1 the
whole run — the same O(n) scan is there, just never exercised past a trivial `n`. Any workload
that grows a `Hash`, `Set`, or `SortedSet` to non-trivial size via repeated `HSET`/`SADD`/`ZADD`
hits the identical wall `LPUSH` did.

**Already fixed and merged, out of scope here:** `crates/engine/src/commands/list.rs`'s
`rpush`/`lpush`/`rpop`/`lpop`/`lset`/`lrem`/`linsert` were converted to a new
`Shard`/`Store`/`Engine::with_mut_delta` method (added alongside `with_mut`, not replacing it)
that lets the caller report the exact byte delta its mutation caused instead of triggering the
before/after rescan. That infrastructure is generic over `Value` already — extending it to
`Hash`/`Set`/`SortedSet` requires **zero changes** to `shard.rs`/`store.rs`/`engine.rs`. This
spec covers only the three remaining command modules:
`crates/engine/src/commands/hash.rs`, `set.rs`, `sorted_set.rs`.

## Decision: reuse `with_mut_delta` exactly as `list.rs` does — no new API

```rust
// crates/engine/src/engine.rs (already exists, unchanged by this work)
pub fn with_mut_delta<F, R>(&self, key: &[u8], f: F) -> R
where
    F: FnOnce(Option<&mut Value>) -> (R, isize),
{
    let result = self.store.with_mut_delta(key, f);
    self.maybe_evict();
    result
}
```

Every function converted by this spec changes its `engine.with_mut(...)` call to
`engine.with_mut_delta(...)`, and its closure's return type from `R` to `(R, isize)` — the
second element is the exact number of bytes `bytes_used` should grow (positive) or shrink
(negative) by, computed from data the closure already has in hand (the field/member/value it
just touched), never by re-scanning the rest of the collection. `Shard::with_mut` and every
command module's *read*-only functions (`hget`, `smembers`, `zscore`, etc., which go through
`with_ref`/`Engine::get`, not `with_mut`) are untouched — this spec is exclusively about
mutating functions.

## Decision: exact per-type delta formulas, taken directly from `Value::approx_size`

`crates/engine/src/value.rs`'s `approx_size` (unchanged by this work — these formulas are
copied from it, not modified) is the ground truth every delta below must reproduce exactly, so
that `bytes_used` after a delta-based mutation is bit-for-bit identical to what the old
before/after-scan approach would have produced. A dedicated test enforces this per type (see
each plan's TDD steps) — the same pattern `list.rs`'s
`with_mut_delta_based_list_mutations_keep_memory_used_exactly_in_sync` test already
established: after every mutation, assert `engine.memory_used()` equals independently
recomputing `key.len() + value.approx_size()` from the resulting value.

- **`Hash`** (`crates/engine/src/commands/hash.rs`) — `approx_size` charges
  `field.len() + value.len() + 16` per field/value pair.
  - `hset`: `HashMap::insert` returns `Option<Bytes>` — the field's *previous* value, if any.
    Capture it. New field (`None` returned): delta `= field.len() + val.len() + 16`. Overwritten
    field (`Some(old_val)`): the field's own length is unchanged, so delta
    `= val.len() as isize - old_val.len() as isize`.
  - `hdel`: `HashMap::remove` returns the removed value if the field existed. Delta
    `= -(field.len() + removed_val.len() + 16)` if removed, `0` if the field was already absent.
  - `hincrby`: read the field's current `Bytes` (if any) *before* overwriting, so its `.len()`
    is available for the delta the same way `hset`'s does — the current code already calls
    `map.get(&field)` before `map.insert`, so this only needs the byte length kept alongside the
    parsed integer, not an extra lookup. New field: delta `= field.len() + next_bytes.len() +
    16`. Existing field: delta `= next_bytes.len() as isize - old_bytes.len() as isize`.
  - `hsetnx`: only ever inserts when the field is absent (a no-op otherwise). Delta `=
    field.len() + val.len() + 16` when it inserts, `0` when it doesn't.

- **`Set`** (`crates/engine/src/commands/set.rs`) — `approx_size` charges `member.len() + 8`
  per member.
  - `sadd`: already loops over `members`, only counting ones `HashSet::insert` reports as newly
    added (its `bool` return). Accumulate `delta += member.len() as isize + 8` in that same
    branch, for the same members already counted in `added`.
  - `srem`: already loops over `members`, only counting ones `HashSet::remove` reports as
    actually present (its `bool` return). Accumulate `delta -= member.len() as isize + 8` in
    that same branch.
  - `spop`: removes the one member it randomly chose. Delta `= -(popped_member.len() + 8)` when
    something was popped, `0` on an empty/missing set.

- **`SortedSet`** (`crates/engine/src/commands/sorted_set.rs`) — `approx_size` charges
  `member.len() + 24` per member; a member's **score is not part of the size formula at all**
  (`SortedSet::insert` on an already-present member only changes its score/position, not its
  stored length), so any mutation that only updates an existing member's score has delta `0`.
  - `zadd`: `SortedSet::score(&member).is_none()` (already computed by the existing code, as
    `is_new`) tells you which case you're in *before* calling `insert`. New member: delta `=
    member.len() + 24`. Existing member (score updated only): delta `= 0`.
  - `zrem`: `SortedSet::remove` returns `bool` (already the function's return value). Delta `=
    -(member.len() + 24)` if it returned `true`, `0` otherwise.
  - `zincrby`: same `is_new` check as `zadd`, computed the same way, before the score update.
    New member: delta `= member.len() + 24`. Existing member: delta `= 0`.

`sinterstore`/`sunionstore`/`sdiffstore` (`set.rs`) build a whole new `Set` and call
`Engine::set`, not `with_mut` — that path already costs O(result size) once, which is
unavoidable and appropriate for a full replace (not a repeated-call pattern), so it is **out of
scope** for this spec. Likewise `list.rs`'s `ltrim`, already excluded for the identical reason
when the original `with_mut_delta` work landed.

## Testing convention (applies to every plan below)

Each converted function needs no new *behavioral* test — the existing test suite for that
function already covers its return values and `WRONGTYPE`/missing-key semantics, and none of
that changes. What's new per module is exactly one `bytes_used`-correctness test, following
`list.rs`'s established shape: build a small sequence of calls through the module's mutating
functions (covering both new-key/new-field/new-member and overwrite/update cases, since those
take different delta branches above), and after each call assert `engine.memory_used()` equals
independently recomputing `key.len() + value.approx_size()` from the current stored value. This
is a correctness safeguard, not a timing benchmark — this project's own convention (see
`docs/superpowers/specs/2026-08-30-sprint-5-spec.md`'s replication-benchmark note) is to never
gate CI on wall-clock timing, so no new perf-timing test is added; the O(n)→O(1) improvement
itself is verified once per plan via a manual `redis-benchmark` run recorded in that plan's
final step, the same way the List fix was verified, not via an automated timing assertion.

## Sequencing

The three plans below are **fully independent** — each touches exactly one command module and
its own test module, none share a data type or a file, and none depends on infrastructure this
spec doesn't already have (see the "already fixed and merged" note above). They can be
implemented and reviewed in any order, or in parallel:

1. `01-hash-with-mut-delta.md` — `hset`/`hdel`/`hincrby`/`hsetnx` in `hash.rs`.
2. `02-set-with-mut-delta.md` — `sadd`/`srem`/`spop` in `set.rs`.
3. `03-sorted-set-with-mut-delta.md` — `zadd`/`zrem`/`zincrby` in `sorted_set.rs`.

## Definition of done

- [ ] `hset`/`hdel`/`hincrby`/`hsetnx`, `sadd`/`srem`/`spop`, `zadd`/`zrem`/`zincrby` all use
      `with_mut_delta`, not `with_mut`.
- [ ] Each module has a `bytes_used`-correctness test passing.
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace -- -D warnings`,
      `cargo test --workspace` all clean.
- [ ] A manual `redis-benchmark -t HSET -n 100000` (and the `SADD`/sorted-set equivalent, via
      `redis-cli` since `redis-benchmark` has no built-in `ZADD` test) with `-r` (unique
      members, so the collection actually grows past size 1) shows flat throughput, no
      degradation as the collection grows — recorded in each plan's closing step.
