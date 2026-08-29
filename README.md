# rocket-mem

![CI](https://github.com/Snehal1112/rocket-mem/actions/workflows/ci.yml/badge.svg)

A from-scratch, RESP-compatible (Redis wire protocol) in-memory data store written in Rust. The goal is a server real Redis clients (`redis-cli`, `redis-py`, `ioredis`, `go-redis`, ...) can talk to unmodified, built on a storage engine that stays protocol-agnostic so a custom binary protocol can be layered on top later without a rewrite.

This is a 16-week solo build, tracked in 2-week sprints. Full rationale and week-by-week detail live in [`docs/rocket-mem-production-plan.md`](docs/rocket-mem-production-plan.md); sprint capacity/priorities/DoD live in [`docs/rocket-mem-sprint-plan.md`](docs/rocket-mem-sprint-plan.md).

## Architecture

Three layers, fixed in Sprint 1 and respected throughout:

```
┌─────────────────────────────────────────┐
│  Protocol Layer (RESP2/RESP3, later:     │
│  a custom binary protocol)               │
├─────────────────────────────────────────┤
│  Command Dispatcher (maps commands →     │
│  engine calls, arg validation)           │
├─────────────────────────────────────────┤
│  Storage Engine (data structures,        │
│  persistence, expiry, protocol-agnostic) │
└─────────────────────────────────────────┘
```

Concurrency model: one Tokio task per client connection, keyspace split into 16 shards each behind its own lock — any task can read/write any key by acquiring that key's shard lock. See [`docs/design/sharding-decision.md`](docs/design/sharding-decision.md) for why.

## Status

**Sprint 1 (engine core & core data types) — done.** A protocol-agnostic, sharded storage engine with full String/Hash/List/Set command coverage and a WRONGTYPE/missing-key test matrix. No networking yet.

**Sprint 2 (RESP protocol, networking & client compatibility) — in progress.** The `protocol` crate has a protocol-aware `Frame` enum (RESP2 plus RESP3's `Map`) and a `RespCodec` that encodes/decodes both, including split-read reassembly. The `server` crate has a Tokio TCP accept loop, a per-connection task, and a dispatcher wired to the full engine command surface (String/Hash/List/Set, table below), plus `PING`/`ECHO`/`SELECT`/`COMMAND`/`INFO`. `HELLO` implements full RESP2/RESP3 negotiation — reporting the current protocol, switching via `HELLO 2`/`HELLO 3`, and returning `NOPROTO`/syntax errors for unsupported versions or malformed args.

Remaining sprints (persistence, replication, clustering, a custom protocol, ACLs/TLS) are scoped in the [sprint plan](docs/rocket-mem-sprint-plan.md) but not started.

### Command coverage

| Type | Implemented |
|---|---|
| String | `GET`, `SET` (`NX`/`XX`), `APPEND`, `STRLEN`, `INCRBY` |
| Hash | `HSET`, `HGET`, `HDEL`, `HEXISTS`, `HGETALL`, `HLEN` |
| List | `LPUSH`, `RPUSH`, `LPOP`, `RPOP`, `LRANGE`, `LLEN` |
| Set | `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD` |

`SET`'s `EX`/`PX` flags are intentionally deferred — there's no expiry reaper until Sprint 4, so time-based flags would be dead code until then. All of the above are exercised directly by engine tests and reachable over RESP through the dispatcher.

## Workspace layout

Four crates under `crates/`:

- **`common`** — shared `EngineError` enum (`WrongType`, `NotAnInteger`). No dependencies on the other crates.
- **`engine`** — the storage engine: `Value` enum, 16-shard `Store`, and one free function per command under `commands/`. Everything in "Status" above lives here.
- **`protocol`** — RESP wire format. Currently just the `Frame` type; parser/encoder is Sprint 2 work in progress.
- **`server`** — placeholder binary (package name `rocket-mem`); becomes the TCP listener once networking lands.

## Building & testing

```bash
cargo build --workspace                 # build everything
cargo test --workspace                  # run all tests
cargo fmt --all -- --check              # CI's format check
cargo clippy --workspace -- -D warnings # CI's lint gate — must be clean
```

CI (`.github/workflows/ci.yml`) runs exactly those fmt/clippy/test commands on every push and PR.

## Documentation

- [`docs/rocket-mem-production-plan.md`](docs/rocket-mem-production-plan.md) — 16-week phase plan and the architecture decision record (why sharded locks, task-per-connection).
- [`docs/rocket-mem-sprint-plan.md`](docs/rocket-mem-sprint-plan.md) — 2-week sprint breakdown with capacity, priorities, risks, and definition of done.
- [`docs/design/sharding-decision.md`](docs/design/sharding-decision.md) — the Sprint 1 design doc on shard count and locking strategy.
- [`docs/superpowers/specs/`](docs/superpowers/specs/) and [`docs/superpowers/plans/`](docs/superpowers/plans/) — per-sprint specs and numbered TDD implementation plans.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the development workflow, code conventions, and commit/PR expectations.

## License

MIT — see [`LICENSE`](LICENSE).
