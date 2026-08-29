# README Update & Sprint 4 Close Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** close out Sprint 4's Definition of Done — update the README's status/command-coverage sections to reflect TTL, AOF persistence, and eviction, run the full workspace verification one more time end-to-end, and mark the sprint complete in the sprint plan.

**Architecture:** documentation-only — no source changes. Kept as its own last plan (not folded into plan 01) because it needs the *complete* Sprint 4 picture, which isn't final until plans 01–09 have all landed.

**Tech Stack:** none.

**Spec:** `../../specs/2026-08-30-sprint-4-spec.md` — the Definition of Done list is authoritative for what this closes out. `../../../rocket-mem-sprint-plan.md`'s Sprint 4 section is authoritative for the sprint's own Definition of Done (`kill -9` + restart proof, corrupt-tail recovery, TTL correctness coverage).

**Depends on:** `01-ttl-passive-expiry-core.md` through `09-memory-usage-object-encoding-stubs.md` must all be complete.

---

### Task 1: Update the README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the "Status" section**

Add a new paragraph after the existing Sprint 3 paragraph (leave Sprints 1–3's text
unchanged):

```markdown
**Sprint 4 (expiry, eviction & AOF persistence) — done.** Keys can now carry a TTL: the
`EXPIRE` family (`EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT`/`TTL`/`PTTL`/`PERSIST`) and `SET`'s
`EX`/`PX` flags — both stubs since Sprint 3 — are fully implemented, backed by passive
expiry (a read finds an expired key gone) and an active background sweep (one shard swept
every 100ms, so memory doesn't quietly fill with dead entries nobody happens to read). Every
write command is appended to an on-disk append-only file (`AofWriter`, configurable
`fsync` policy — `Always`/`EverySecond`/`Never`) and replayed on startup, with a corrupted
tail truncated rather than merely skipped in memory — data now survives a `kill -9` and
restart, this sprint's headline goal, proven by a real-subprocess-and-SIGKILL integration
test. `Engine::with_maxmemory` bounds memory usage via approximated LRU eviction (a
`Store`-wide recency clock plus per-shard sampling, matching real Redis's own
"approximated LRU" rather than a maintained-list-based exact one). `MEMORY USAGE` and
`OBJECT ENCODING` respond usefully for tooling that probes them, rather than "unknown
command." See `docs/superpowers/specs/2026-08-30-sprint-4-spec.md` for the full set of
design decisions (why `Entry` wraps `Value` instead of a new `Value` variant, why AOF
rewrites `SPOP`→`SREM` and the `EXPIRE` family→absolute `PEXPIREAT`, why eviction samples
instead of maintaining an exact LRU list).

Remaining sprints (replication, clustering, a custom protocol, ACLs/TLS) are scoped in the
[sprint plan](docs/rocket-mem-sprint-plan.md) but not started.
```

Remove the now-stale trailing "Remaining sprints..." line that currently follows the
Sprint 3 paragraph (it's superseded by the one shown above, now following Sprint 4's
paragraph instead).

- [ ] **Step 2: Replace the command coverage table**

The current table and its footnote are stale in three ways: `RPUSH`/`LPUSH` are no longer
single-value-only, `GETRANGE`/`SETRANGE`/`HSCAN` were added but never documented, and the
`EXPIRE` family / `SET`'s `EX`/`PX` are no longer a stub. Replace both:

```markdown
### Command coverage

| Type | Implemented |
|---|---|
| String/Key | `GET`, `SET` (`NX`/`XX`/`EX`/`PX`), `GETSET`, `GETRANGE`, `SETRANGE`, `APPEND`, `STRLEN`, `INCR`/`DECR`/`INCRBY`, `MSET`, `MGET`, `MSETNX`, `RENAME`, `RENAMENX`, `TYPE`, `RANDOMKEY`, `KEYS` (glob: `*`, `?`, `[abc]` only), `SCAN`, `DEL`/`EXISTS` (variadic), `EXPIRE`, `PEXPIRE`, `EXPIREAT`, `PEXPIREAT`, `TTL`, `PTTL`, `PERSIST`, `MEMORY USAGE`, `OBJECT ENCODING` |
| Hash | `HSET`, `HGET`, `HDEL`, `HEXISTS`, `HGETALL`, `HLEN`, `HINCRBY`, `HKEYS`, `HVALS`, `HMGET`, `HSETNX`, `HSCAN` |
| List | `LPUSH`, `RPUSH` (both variadic), `LPOP`, `RPOP`, `LRANGE`, `LLEN`, `LINDEX`, `LSET`, `LTRIM`, `LREM`, `LINSERT` |
| Set | `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`, `SINTER`, `SUNION`, `SDIFF`, `SINTERSTORE`, `SUNIONSTORE`, `SDIFFSTORE`, `SPOP`, `SRANDMEMBER` |
| Sorted Set | `ZADD`, `ZSCORE`, `ZREM`, `ZCARD`, `ZINCRBY`, `ZRANGE`, `ZRANK` |

`KEYS`'s glob support is intentionally partial: no character ranges (`[a-z]`), negation
(`[^abc]`), or escaping. Active expiry sweeps one whole shard per 100ms tick rather than
sampling individual keys within a shard the way real Redis does — an accepted
simplification, not a bug (see the Sprint 4 spec). `OBJECT ENCODING` reports this engine's
own type name (`string`/`list`/`hash`/`set`/`zset` — exactly what `TYPE` returns, since both
come from `Value::type_name()`), not real Redis's actual internal
encodings (`embstr`/`listpack`/etc.), which this engine doesn't implement. All of the above
are exercised directly by engine tests and reachable over RESP through the dispatcher.
```

- [ ] **Step 3: Document the AOF-related environment variables**

Add a new subsection right after the command coverage table's footnote paragraph:

```markdown
### Running with persistence

The server binary reads two environment variables at startup:

| Variable | Default | Purpose |
|---|---|---|
| `ROCKET_MEM_ADDR` | `127.0.0.1:6379` | TCP address to bind |
| `ROCKET_MEM_AOF_PATH` | `./appendonly.aof` | Append-only file path — replayed on startup if it already exists, then opened for appending with an `EverySecond` fsync policy |
```

- [ ] **Step 4: Fix the two stale "Workspace layout" bullets**

Both predate Sprint 2/3 and are now flatly wrong; this is the README plan, so they get fixed
here rather than left for a later sprint to trip over:

```markdown
- **`common`** — shared `EngineError` enum (`WrongType`, `NotAnInteger`, `NoSuchKey`). No dependencies on the other crates.
- **`server`** — the binary (package name `rocket-mem`): Tokio TCP accept loop, per-connection task, command dispatcher, AOF writer/replayer, and the active-expiry and fsync background loops.
```

Leave the `engine` and `protocol` bullets as they are.

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`README.md` — do not compose the commit message freeform. Suggested subject:
`docs: update README for Sprint 4 completion`.

---

### Task 2: Full workspace verification and Sprint 4 close note

**Files:**
- Modify: `docs/rocket-mem-sprint-plan.md`

**Interfaces:** none — this task is verification plus one status annotation, matching the
pattern this file already uses for Sprints 1–3.

- [ ] **Step 1: Run the complete verification suite**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all three clean/green — this is the same trio CI (`.github/workflows/ci.yml`) runs
on every push, so a clean local run here means CI will pass too. This run must include
`crates/server/tests/kill_and_recover.rs` (from `08-kill-and-recover-tests.md`) — it's a
`cargo test --workspace` target like any other integration test, no special invocation
needed.

- [ ] **Step 2: Confirm the sprint's Definition of Done against the spec and sprint plan**

Manually check off each item:
- `kill -9` + restart test passes → `08-kill-and-recover-tests.md`'s two tests, confirmed
  passing in Step 1's run
- Corrupt-tail AOF recovery test passes without panicking →
  `06-aof-replay-and-corrupt-recovery.md`'s
  `replay_truncates_the_corrupt_tail_off_the_file_on_disk` test, confirmed passing
- TTL correctness suite covers both active and passive expiry paths independently →
  `01-ttl-passive-expiry-core.md`'s passive-expiry `Shard`/`Engine` tests and
  `02-active-expiry-background-task.md`'s `serve_actively_expires_a_key_even_without_any_read_touching_it`
  (which deliberately never reads the expiring key, isolating the active path)

- [ ] **Step 3: Update `docs/rocket-mem-sprint-plan.md`'s Sprint 4 section**

Following the exact pattern already used for Sprints 1–3 in this same file, add a
**Status** line right under the `**Sprint goal:**` line:

```markdown
**Status:** ✅ Complete — full P0/P1 scope shipped, plus the P2 `MEMORY`/`OBJECT` stubs. See
`docs/superpowers/specs/2026-08-30-sprint-4-spec.md` and
`docs/superpowers/plans/2026-08-30-sprint-4-plans/`.
```

And tick its Definition of Done:

```markdown
### Definition of done
- [x] `kill -9` + restart test passes in CI with all keys intact
- [x] Corrupt-tail AOF recovery test passes without panicking
- [x] TTL correctness suite covers both active and passive expiry paths independently
```

- [ ] **Step 4: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`docs/rocket-mem-sprint-plan.md` — do not compose the commit message freeform. Suggested
subject: `docs: mark Sprint 4 complete in the sprint plan`.
