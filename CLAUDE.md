# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`rocket-mem` is a from-scratch, RESP-compatible (Redis wire protocol) in-memory data store written in Rust, built as a 16-week solo project. Full roadmap and rationale live in `docs/rocket-mem-production-plan.md` (16-week phase plan + Architecture Decision Record) and `docs/rocket-mem-sprint-plan.md` (2-week sprint breakdown with priorities/DoD). Per-sprint specs and implementation plans live under `docs/superpowers/specs/` and `docs/superpowers/plans/<date>-sprint-N-plans/` — see "Sprint planning docs" below. `docs/superpowers/plans/2026-08-28-sprint-1-plans/` holds the (now-executed) TDD implementation plans for Sprint 1; `docs/superpowers/plans/2026-08-29-sprint-2-plans/` holds Sprint 2's (not yet executed).

Only Sprint 1 is built so far: a protocol-agnostic storage engine with no networking. There is no RESP parser, no dispatcher, and no TCP listener yet — that's Sprint 2.

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

Four crates under `crates/`:

- **`common`** — shared `EngineError` enum (`WrongType`, `NotAnInteger`). Zero dependencies on other crates.
- **`engine`** — the storage engine. Everything implemented so far lives here.
- **`protocol`** — empty placeholder; RESP parser/encoder goes here in Sprint 2.
- **`server`** — empty placeholder binary, package name `rocket-mem` (folder name `server` follows responsibility naming, but `cargo run --bin rocket-mem` is what starts the server once networking exists).

Target end-state architecture (from the production plan) is three layers — Protocol → Command Dispatcher → Storage Engine — with the engine kept protocol-agnostic so RESP and a later custom protocol (Phase 4) can both sit on top without touching engine code. Right now only the bottom layer exists.

## Engine internals (`crates/engine/src`)

Read `value.rs` → `shard.rs` → `store.rs` → `engine.rs` → `commands/` in that order; each wraps the previous:

- **`value.rs`** — `Value` enum: `String(Bytes) | List(VecDeque<Bytes>) | Hash(HashMap<Bytes,Bytes>) | Set(HashSet<Bytes>)`. The one place a new data type gets added.
- **`shard.rs`** — `Shard`, a single `RwLock<HashMap<Bytes, Value>>`.
- **`store.rs`** — `Store`, a fixed array of 16 `Shard`s. A key routes to `DefaultHasher(key) % 16`. This is the concurrency backbone; see `docs/design/sharding-decision.md` for why 16 shards / why `DefaultHasher`, and the production plan's Architecture Decision Record for why sharded-locks over single-thread, thread-per-core, lock-free, or proxy-based alternatives.
- **`engine.rs`** — `Engine`, a thin public facade over `Store` (`get`/`set`/`del`/`exists`/`keys`). This is the single entry point Sprint 2's dispatcher will call.
- **`commands/{string,hash,list,set}.rs`** — one free function per Redis command, signature `fn(&Engine, ...args) -> Result<T, common::EngineError>`. No dispatcher exists yet, so these are exercised directly by tests. `commands` is declared `pub mod` in `lib.rs` (not private) specifically so these functions aren't flagged as dead code by `clippy -D warnings` before Sprint 2 wires a real caller — keep that visibility when adding new commands.

### Correctness conventions enforced across every command

- **WRONGTYPE**: match on `Value` and return `Err(EngineError::WrongType)` on a type mismatch — never silently coerce or ignore it. Covered by the cross-command sweep in `commands/wrongtype_matrix_tests.rs`.
- **Missing key ≠ error**: a read on a missing key returns `None`/empty (not an error), and a *mutation* that finds nothing to do must not write back a phantom empty collection. `commands/missing_key_semantics_tests.rs` codifies this — it previously caught a real bug where `lpop`/`rpop`/`srem` wrote back an empty List/Set for a key that was never set.
- **Deferred scope**: `SET`'s `EX`/`PX` flags are intentionally not implemented (only `NX`/`XX`) — there's no expiry reaper until Sprint 4, so time-based flags would be dead code until then.

## Sprint planning docs

This project's sprint specs and implementation plans follow the Superpowers Claude Code plugin's own default save convention (the `writing-plans`/`brainstorming` skills), adopted here as the project's standing convention:

- `docs/superpowers/specs/<date>-sprint-N-spec.md` — one spec per sprint, fixing shared design decisions (workspace layout, wire formats, architecture calls) that every plan in that sprint assumes as ground truth. Cross-references the master plan/sprint-plan docs and the sibling plans folder with relative paths (`../../rocket-mem-*.md`, `../plans/<date>-sprint-N-plans/`).
- `docs/superpowers/plans/<date>-sprint-N-plans/` — one numbered TDD implementation plan per backlog item for that sprint (`01-*.md`, `02-*.md`, ...), each referencing its sprint's spec via `../../specs/<date>-sprint-N-spec.md`.

`.worktrees/` (gitignored) is a separate Superpowers convention, for the `using-git-worktrees` skill's isolated-workspace creation.
