# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`rocket-mem` is a from-scratch, RESP-compatible (Redis wire protocol) in-memory data store written in Rust, built as a 16-week solo project. Full roadmap and rationale live in `docs/rocket-mem-production-plan.md` (16-week phase plan + Architecture Decision Record) and `docs/rocket-mem-sprint-plan.md` (2-week sprint breakdown with priorities/DoD). Per-sprint specs and implementation plans live under `docs/superpowers/specs/` and `docs/superpowers/plans/<date>-sprint-N-plans/` — see "Sprint planning docs" below. `docs/superpowers/plans/2026-08-28-sprint-1-plans/` holds the (now-executed) TDD implementation plans for Sprint 1; `docs/superpowers/plans/2026-08-29-sprint-2-plans/` holds Sprint 2's (executed long ago, alongside every sprint through 7).

Sprints 1-7 are built: a protocol-agnostic storage engine, RESP2/RESP3 networking, the full command set (strings/hashes/lists/sets/sorted sets/keys), TTL expiry, AOF persistence, snapshotting, leader/follower replication, hash-slot clustering, Prometheus observability, and a second wire protocol of the project's own (RMP, alongside RESP). See `README.md`'s "Status" section for the sprint-by-sprint detail and known limits; only Sprint 8 (auth, ACLs, TLS, release) remains.

## Commands

```bash
cargo build --workspace                                  # build everything
cargo test --workspace                                    # run all tests
cargo test -p engine                                       # run all engine tests
cargo test -p engine commands::string::tests                # one test module
cargo test -p engine commands::string::tests::incr_by_adds_to_existing_value  # one test
cargo fmt --all -- --check                                 # CI's format check
cargo fmt --all                                             # apply formatting
cargo clippy --workspace -- -D warnings                     # CI's lint gate — must be clean
```

CI (`.github/workflows/ci.yml`) runs exactly those fmt/clippy/test commands on every push and PR. All three must pass locally before committing — `cargo clippy --workspace -- -D warnings` is strict (no warnings at all, including dead-code).

## Workspace layout

Five crates under `crates/`:

- **`common`** — shared `EngineError` enum (`WrongType`, `NotAnInteger`, `NoSuchKey`). Zero dependencies on other crates.
- **`engine`** — the storage engine: `Value`, the 16-shard `Store`, and one free function per command under `commands/`.
- **`protocol`** — wire formats: RESP's `Frame`/`RespCodec` and RMP's envelope/value codec (`rmp` module), both handling split-read reassembly.
- **`server`** — the binary (package name `rocket-mem`): dual RESP/RMP accept loops, the shared command dispatcher every protocol calls, AOF, snapshotting, replication, cluster routing, Prometheus metrics, and the slow log.
- **`rmp-client`** — a minimal async Rust client for RMP.

This is the three-layer architecture (Protocol → Command Dispatcher → Storage Engine) the production plan targeted from the start, now fully built: the engine stayed protocol-agnostic throughout, which is exactly what let RMP (Sprint 7) sit on top of the same dispatcher RESP already used, without touching engine code.

## Engine internals (`crates/engine/src`)

Read `value.rs` → `shard.rs` → `store.rs` → `engine.rs` → `commands/` in that order; each wraps the previous:

- **`value.rs`** — `Value` enum: `String(Bytes) | List(VecDeque<Bytes>) | Hash(HashMap<Bytes,Bytes>) | Set(HashSet<Bytes>) | SortedSet(SortedSet)`. The one place a new data type gets added.
- **`shard.rs`** — `Shard`: an `RwLock<HashMap<Bytes, Entry>>` (an `Entry` wraps a `Value` with an optional TTL `expires_at` and an atomic `last_touched` recency tick for LRU-style eviction) plus an `AtomicUsize` byte-usage counter kept in sync on every mutation.
- **`store.rs`** — `Store`, a fixed array of 16 `Shard`s. A key routes to `DefaultHasher(key) % 16`. This is the concurrency backbone; see `docs/design/sharding-decision.md` for why 16 shards / why `DefaultHasher`, and the production plan's Architecture Decision Record for why sharded-locks over single-thread, thread-per-core, lock-free, or proxy-based alternatives.
- **`engine.rs`** — `Engine`, a thin public facade over `Store` — the single entry point the command dispatcher calls. Grew well beyond `get`/`set`/`del`/`exists`/`keys` across later sprints (TTL, snapshotting, eviction, `scan`, `with_ref`/`with_mut`); read the file directly for the current method list rather than trusting a hardcoded one here.
- **`commands/{string,hash,list,set,sorted_set,keys}.rs`** — one free function per Redis command, signature `fn(&Engine, ...args) -> Result<T, common::EngineError>`. `crates/server/src/dispatcher.rs`'s `dispatch` is the real caller now (both RESP and RMP route through it), calling these directly (e.g. `commands::string::get(engine, &rest[0])`) — they're also still exercised directly by the engine crate's own tests. `commands` stays `pub mod` in `lib.rs` (not private) so the dispatcher can reach it across the crate boundary — keep that visibility when adding new commands.

### Correctness conventions enforced across every command

- **WRONGTYPE**: match on `Value` and return `Err(EngineError::WrongType)` on a type mismatch — never silently coerce or ignore it. Covered by the cross-command sweep in `commands/wrongtype_matrix_tests.rs`.
- **Missing key ≠ error**: a read on a missing key returns `None`/empty (not an error), and a *mutation* that finds nothing to do must not write back a phantom empty collection. `commands/missing_key_semantics_tests.rs` codifies this — it previously caught a real bug where `lpop`/`rpop`/`srem` wrote back an empty List/Set for a key that was never set.
- **`SET`'s `EX`/`PX` flags**: implemented since Sprint 4 (the TTL/expiry sprint) — `SET k v EX n` sets an absolute expiry the same way a following `EXPIRE` would.

## Sprint planning docs

This project's sprint specs and implementation plans follow the Superpowers Claude Code plugin's own default save convention (the `writing-plans`/`brainstorming` skills), adopted here as the project's standing convention:

- `docs/superpowers/specs/<date>-sprint-N-spec.md` — one spec per sprint, fixing shared design decisions (workspace layout, wire formats, architecture calls) that every plan in that sprint assumes as ground truth. Cross-references the master plan/sprint-plan docs and the sibling plans folder with relative paths (`../../rocket-mem-*.md`, `../plans/<date>-sprint-N-plans/`).
- `docs/superpowers/plans/<date>-sprint-N-plans/` — one numbered TDD implementation plan per backlog item for that sprint (`01-*.md`, `02-*.md`, ...), each referencing its sprint's spec via `../../specs/<date>-sprint-N-spec.md`.

`.worktrees/` (gitignored) is a separate Superpowers convention, for the `using-git-worktrees` skill's isolated-workspace creation.

## Manual testing

See `.claude/manual-testing.md` for how to run the server by hand (env vars, standalone/replication/cluster-mode examples, `REPLICAOF` explained).
