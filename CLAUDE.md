# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`rocket-mem` is a from-scratch, RESP-compatible (Redis wire protocol) in-memory data store written in Rust, originally built as a 16-week solo project. Full roadmap and rationale live in `docs/rocket-mem-production-plan.md` (16-week phase plan + Architecture Decision Record) and `docs/rocket-mem-sprint-plan.md` (2-week sprint breakdown with priorities/DoD). Per-sprint specs and implementation plans live under `docs/superpowers/specs/` and `docs/superpowers/plans/<date>-sprint-N-plans/` — see "Sprint planning docs" below.

All 8 sprints of the original plan are built and shipped: a protocol-agnostic storage engine, RESP2/RESP3 + a second wire protocol of the project's own (RMP), the full command set (strings/hashes/lists/sets/sorted sets/keys), TTL expiry, AOF persistence, snapshotting, leader/follower replication, hash-slot clustering, Prometheus observability, and (Sprint 8) Argon2 auth, per-user ACLs, and optional TLS. See README.md's "Features", "Command coverage", and "Limitations" sections for exactly what's live and what isn't — don't trust a hardcoded feature list here, it will drift.

Post-v1, the project continues against an unscoped Phase 5-9 backlog (`rocket-mem-production-plan.md`'s "Where this could go next" section, prioritized 2026-09-10). **Shipped:** Phase 5 (`MULTI`/`EXEC`/`DISCARD` transactions — spec: `docs/superpowers/specs/2026-09-10-multi-exec-transactions-spec.md`) and Phase 6 (pub/sub — spec: `docs/superpowers/specs/2026-09-11-pubsub-spec.md`). **Not started, no spec yet:** Phase 7 (Lua `EVAL`), Phase 8 (streams), Phase 9 (live cluster resharding).

`examples/` holds standalone client-usage demos (Go, using `go-redis`) that exercise rocket-mem as an ordinary Redis-wire client would — not part of the Cargo workspace, not built or tested by CI.

## Commands

```bash
cargo build --workspace                                  # build everything
cargo test --workspace                                    # run all tests
cargo fmt --all -- --check                                 # CI's format check
cargo clippy --workspace --all-targets -- -D warnings        # CI's lint gate — must be clean
```

CI (`.github/workflows/ci.yml`) runs exactly those four commands on every push and PR — `cargo clippy --workspace --all-targets -- -D warnings` is strict (no warnings at all, including dead-code) and lints test code too, not just lib targets. To narrow `cargo test` while iterating:

```bash
cargo test -p engine                                       # one crate
cargo test -p engine commands::string::tests                # one test module
cargo test -p engine commands::string::tests::incr_by_adds_to_existing_value  # one test
```

## Workspace layout

Five crates under `crates/` — see README.md's "Project layout" table for the one-line-per-crate summary. Notes beyond that table:

- **`engine`** is strictly protocol-agnostic: it knows nothing about RESP or RMP. This is the three-layer architecture (Protocol → Command Dispatcher → Storage Engine) the production plan targeted from the start — the separation that let RMP (Sprint 7) and, later, transactions/pub/sub sit on top of the existing dispatcher without touching engine code.
- **Logging is the one permitted cross-cutting dependency.** `engine` and `protocol` both depend on `tracing` (since 2026-09-09). This does not weaken the protocol-agnostic rule: `tracing` is a facade crate with no runtime of its own, its macros compile to a level check that is never true when no subscriber is installed, and an instrumented engine still knows nothing about RESP or RMP. Redaction policy deliberately does *not* live here — `engine` and `protocol` log key names and byte lengths only, never value contents, so `crates/server/src/logging.rs` stays the single auditable place a secret could reach a log. See [the verbose logging spec](docs/superpowers/specs/2026-09-09-verbose-logging-design.md).

## Engine internals (`crates/engine/src`)

Read `value.rs` → `shard.rs` → `store.rs` → `engine.rs` → `commands/` in that order; each wraps the previous:

- **`value.rs`** — `Value` enum: `String(Bytes) | List(VecDeque<Bytes>) | Hash(HashMap<Bytes,Bytes>) | Set(HashSet<Bytes>) | SortedSet(SortedSet)`. The one place a new data type gets added.
- **`shard.rs`** — `Shard`: a `parking_lot::RwLock<HashMap<Bytes, Entry>>` (an `Entry` wraps a `Value` with an optional TTL `expires_at` and an atomic `last_touched` recency tick for LRU-style eviction) plus an `AtomicUsize` byte-usage counter kept in sync on every mutation. Expiry is lazy on every access path (`is_expired()` checked in `get`/`exists`/`del`/`keys`/...) plus an active sweep that walks one whole shard per 100ms tick — a key past its TTL is invisible to every read/write path immediately, even before the sweep physically removes it.
- **`store.rs`** — `Store`, a fixed array of 16 `Shard`s. A key routes to `DefaultHasher(key) % 16`. This is the concurrency backbone; see `docs/design/sharding-decision.md` for why 16 shards / why `DefaultHasher`, and the production plan's Architecture Decision Record for why sharded-locks over single-thread, thread-per-core, lock-free, or proxy-based alternatives.
- **`engine.rs`** — `Engine`, a thin public facade over `Store` — the single entry point the command dispatcher calls, exposing closure-scoped `with_ref`/`with_mut` (lock acquired, closure run, lock released — never held open across an `.await`) plus `shard_index(key) -> usize`, a pure stateless routing primitive with no Frame/command coupling. Grew well beyond `get`/`set`/`del`/`exists`/`keys` across later sprints (TTL, snapshotting, eviction, `scan`); read the file directly for the current method list rather than trusting a hardcoded one here.
- **`commands/{string,hash,list,set,sorted_set,keys}.rs`** — one free function per Redis command, signature `fn(&Engine, ...args) -> Result<T, common::EngineError>`. `crates/server/src/dispatcher.rs`'s `dispatch` is the real caller now (both RESP and RMP route through it), calling these directly (e.g. `commands::string::get(engine, &rest[0])`) — they're also still exercised directly by the engine crate's own tests. `commands` stays `pub mod` in `lib.rs` (not private) so the dispatcher can reach it across the crate boundary — keep that visibility when adding new commands.

### Correctness conventions enforced across every command

- **WRONGTYPE**: match on `Value` and return `Err(EngineError::WrongType)` on a type mismatch — never silently coerce or ignore it. Covered by the cross-command sweep in `commands/wrongtype_matrix_tests.rs`.
- **Missing key ≠ error**: a read on a missing key returns `None`/empty (not an error), and a *mutation* that finds nothing to do must not write back a phantom empty collection. `commands/missing_key_semantics_tests.rs` codifies this — it previously caught a real bug where `lpop`/`rpop`/`srem` wrote back an empty List/Set for a key that was never set.
- **`SET`'s `EX`/`PX` flags**: implemented since Sprint 4 (the TTL/expiry sprint) — `SET k v EX n` sets an absolute expiry the same way a following `EXPIRE` would.

## Server internals (`crates/server/src`)

Everything protocol-specific and stateful lives here — `dispatcher.rs`'s `dispatch`/`dispatch_and_log_gated` is the shared entry point both RESP (`connection.rs`) and RMP (`rmp_connection.rs`) call. The dispatch path is fully synchronous (no `.await` while a shard lock is held) — see the transactions spec's "Performance" section for why that matters and what a future feature holding a lock across CPU-bound work (e.g. Lua scripting) would need to change.

- **`aof.rs`** — AOF persistence and replay. `lock_shards(&shards)` is the per-shard *ordering* mutex (`AofWriter.order`, distinct from `Shard`'s own data lock) that every write acquires before mutating the engine and holds through the AOF append — this is what makes `MULTI`/`EXEC` real write-write mutual exclusion when the guard is widened across a whole batch, not just append-ordering.
- **`transaction_grouping.rs`** — groups a decoded frame stream into transaction units for the two paths that apply frames outside the normal per-connection gated dispatch: AOF replay and replication apply.
- **`pubsub.rs`** — `PubSubRegistry`: channel/pattern → subscriber sender maps, structurally mirroring `ReplicaRegistry`'s "register a sender, prune on send failure" shape. Single-node delivery only.
- **`acl.rs`** — the whole ACL subsystem in one module: rule parsing, Argon2 password hashing, and permission checks.
- **`tls.rs`**, **`cluster.rs`**, **`cluster_health.rs`** — TLS listener setup; hash-slot routing (fixed at 16384 slots, matching real Redis Cluster for wire compatibility); peer-liveness probing (observational only — never promotes a node or rewrites cluster topology).
- **`replication.rs`**, **`slowlog.rs`**, **`metrics.rs`**, **`config.rs`**, **`logging.rs`** — leader/follower sync; the bounded slow log; Prometheus `/metrics`; TOML/env/CLI config layering; the redaction boundary (see above).

## Sprint planning docs

This project's sprint specs and implementation plans follow the Superpowers Claude Code plugin's own default save convention (the `writing-plans`/`brainstorming` skills), adopted here as the project's standing convention — still in active use post-v1 (e.g. the transactions and pub/sub specs above):

- `docs/superpowers/specs/<date>-sprint-N-spec.md` or `<date>-<feature>-spec.md` — one spec per sprint/feature, fixing shared design decisions (workspace layout, wire formats, architecture calls) that every plan assumes as ground truth. Cross-references the master plan/sprint-plan docs and the sibling plans folder with relative paths (`../../rocket-mem-*.md`, `../plans/<date>-sprint-N-plans/`).
- `docs/superpowers/plans/<date>-sprint-N-plans/` (or `<date>-<feature>-plans/`) — one numbered TDD implementation plan per backlog item, each referencing its spec via a relative `../../specs/...` path.

`.worktrees/` (gitignored) is a separate Superpowers convention, for the `using-git-worktrees` skill's isolated-workspace creation.

## Manual testing & operations

- `.claude/manual-testing.md` — how to run the server by hand (env vars, standalone/replication/cluster-mode examples, `REPLICAOF` explained).
- `.claude/runbook-failover.md` — the failover runbook (this project has no automated failover — see README's "Limitations" — so promoting a replica is a manual, documented procedure).
