# README Command Coverage & Sprint 3 Close Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** close out Sprint 3's Definition of Done — update the README's command coverage table and status section, run the full workspace verification one more time end-to-end, and record what's still open going into Sprint 4.

**Architecture:** documentation-only — no source changes. This plan exists as its own step (rather than folding "update the README" into plan 01's Task 4) because it's the one place that needs the *complete* Sprint 3 command list, which isn't final until plans 01–08 have all landed.

**Tech Stack:** none.

**Spec:** `../../specs/2026-08-29-sprint-3-spec.md` — the Definition of Done list is authoritative for what this closes out.

**Depends on:** `01-string-key-commands.md` through `08-remaining-set-commands.md` must all be complete.

---

### Task 1: Update the README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the "Status" section**

Replace the `## Status` section's Sprint 2 "in progress" paragraph and missing Sprint 3 paragraph with:

```markdown
**Sprint 2 (RESP protocol, networking & client compatibility) — done.** ...
(keep the existing Sprint 2 paragraph text as-is, wording unchanged — only its status flips from "in progress" to "done," matching `docs/rocket-mem-sprint-plan.md`'s Sprint 2 Definition of Done, all four boxes of which are now checked.)

**Sprint 3 (full command set: keys, collections & sorted sets) — done.** `KEYS` now supports glob patterns (`*`, `?`, `[abc]`); `SCAN` walks the keyspace one shard per call without blocking it the way `KEYS` can, proven safe under concurrent writes by a stress test. A new `SortedSet` type backs `ZADD`/`ZSCORE`/`ZREM`/`ZCARD`/`ZINCRBY`/`ZRANGE`/`ZRANK`. String/key commands gained `GETSET`/`MSET`/`MGET`/`MSETNX`/`RENAME`/`RENAMENX`/`TYPE`/`RANDOMKEY` — the `EXPIRE` family (`EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT`/`TTL`/`PTTL`/`PERSIST`) is an explicit stub returning a clear error, deferred to Sprint 4 alongside the expiry reaper it actually needs (see `docs/superpowers/specs/2026-08-29-sprint-3-spec.md`). Lists, Hashes, and Sets each gained their remaining command coverage (table below).

Remaining sprints (persistence, replication, clustering, a custom protocol, ACLs/TLS) are scoped in the [sprint plan](docs/rocket-mem-sprint-plan.md) but not started.
```

- [ ] **Step 2: Replace the command coverage table**

```markdown
### Command coverage

| Type | Implemented |
|---|---|
| String/Key | `GET`, `SET` (`NX`/`XX`), `GETSET`, `APPEND`, `STRLEN`, `INCR`/`DECR`/`INCRBY`, `MSET`, `MGET`, `MSETNX`, `RENAME`, `RENAMENX`, `TYPE`, `RANDOMKEY`, `KEYS` (glob: `*`, `?`, `[abc]` only), `SCAN` |
| Hash | `HSET`, `HGET`, `HDEL`, `HEXISTS`, `HGETALL`, `HLEN`, `HINCRBY`, `HKEYS`, `HVALS`, `HMGET`, `HSETNX` |
| List | `LPUSH`, `RPUSH`, `LPOP`, `RPOP`, `LRANGE`, `LLEN`, `LINDEX`, `LSET`, `LTRIM`, `LREM`, `LINSERT` |
| Set | `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`, `SINTER`, `SUNION`, `SDIFF`, `SINTERSTORE`, `SUNIONSTORE`, `SDIFFSTORE`, `SPOP`, `SRANDMEMBER` |
| Sorted Set | `ZADD`, `ZSCORE`, `ZREM`, `ZCARD`, `ZINCRBY`, `ZRANGE`, `ZRANK` |

`SET`'s `EX`/`PX` flags and the entire `EXPIRE` command family (`EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT`/`TTL`/`PTTL`/`PERSIST`) are intentionally deferred — there's no expiry reaper until Sprint 4, so time-based semantics would be silently broken (a TTL nothing ever checks) rather than dead code. `KEYS`'s glob support is intentionally partial: no character ranges (`[a-z]`), negation (`[^abc]`), or escaping. `RPUSH`/`LPUSH` still accept exactly one value per call (documented debt from `docs/phase-1-retro.md`, not yet picked up). All of the above are exercised directly by engine tests and reachable over RESP through the dispatcher.
```

- [ ] **Step 3: Update the crate-layout description for `protocol`**

The `## Workspace layout` section's `protocol` bullet still says "parser/encoder is Sprint 2 work in progress" — that's now stale (Sprint 2 shipped it). Update it:

```markdown
- **`protocol`** — RESP wire format: the `Frame` type (RESP2 plus RESP3's `Map`) and `RespCodec`, encoding/decoding both including split-read reassembly.
```

- [ ] **Step 4: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`README.md` — do not compose the commit message freeform. Suggested subject:
`docs: update README for Sprint 3 completion`.

---

### Task 2: Full workspace verification and Sprint 3 close note

**Files:**
- Modify: `docs/rocket-mem-sprint-plan.md`

**Interfaces:** none — this task is verification plus one status annotation, matching the pattern `docs/rocket-mem-sprint-plan.md` already uses for Sprints 1 and 2.

- [ ] **Step 1: Run the complete verification suite**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all three clean/green — this is the same trio CI (`.github/workflows/ci.yml`) runs on every push, so a clean local run here means CI will pass too

- [ ] **Step 2: Confirm the sprint's Definition of Done against the spec**

Manually check off each item in `../../specs/2026-08-29-sprint-3-spec.md`'s "Definition of done for the sprint" section against what's actually landed:
- Command coverage table in the repo README updated → done in Task 1
- `SCAN` concurrency stress test passes → `03-scan-cursor-iteration.md`'s `scan_visits_every_pre_existing_key_at_least_once_under_concurrent_writes`, confirmed passing in Step 1's `cargo test --workspace` run
- Sorted set operations covered by tests including score-ordering edge cases → `04-sorted-set-core.md`/`05-sorted-set-range-and-rank.md`'s tests, including the tied-score lexicographic tie-break case

- [ ] **Step 3: Update `docs/rocket-mem-sprint-plan.md`'s Sprint 3 section**

Following the exact pattern already used for Sprints 1 and 2 in this same file:

```markdown
## Sprint 3 — Full command set: keys, collections & sorted sets
**Maps to:** Weeks 5-6 | **Dates:** Day 29–42

**Status:** ✅ Complete — full P0/P1/P2 scope shipped. See `docs/superpowers/specs/2026-08-29-sprint-3-spec.md` and `docs/superpowers/plans/2026-08-29-sprint-3-plans/`.

**Sprint goal:** Broad command coverage, including a working sorted-set implementation and a concurrency-safe `SCAN`.
```

And tick its Definition of Done:

```markdown
### Definition of done
- [x] Command coverage table in the repo README updated
- [x] `SCAN` concurrency stress test passes
- [x] Sorted set operations covered by tests including score-ordering edge cases
```

- [ ] **Step 4: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`docs/rocket-mem-sprint-plan.md` — do not compose the commit message freeform. Suggested
subject: `docs: mark Sprint 3 complete in the sprint plan`.
