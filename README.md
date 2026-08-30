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

**Sprint 2 (RESP protocol, networking & client compatibility) — done.** The `protocol` crate has a protocol-aware `Frame` enum (RESP2 plus RESP3's `Map`) and a `RespCodec` that encodes/decodes both, including split-read reassembly. The `server` crate has a Tokio TCP accept loop, a per-connection task, and a dispatcher wired to the full engine command surface (String/Hash/List/Set, table below), plus `PING`/`ECHO`/`SELECT`/`COMMAND`/`INFO`. `HELLO` implements full RESP2/RESP3 negotiation — reporting the current protocol, switching via `HELLO 2`/`HELLO 3`, and returning `NOPROTO`/syntax errors for unsupported versions or malformed args.

**Sprint 3 (full command set: keys, collections & sorted sets) — done.** `KEYS` now supports glob patterns (`*`, `?`, `[abc]`); `SCAN` walks the keyspace one shard per call without blocking it the way `KEYS` can, proven safe under concurrent writes by a stress test. A new `SortedSet` type backs `ZADD`/`ZSCORE`/`ZREM`/`ZCARD`/`ZINCRBY`/`ZRANGE`/`ZRANK`. String/key commands gained `GETSET`/`MSET`/`MGET`/`MSETNX`/`RENAME`/`RENAMENX`/`TYPE`/`RANDOMKEY` — the `EXPIRE` family (`EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT`/`TTL`/`PTTL`/`PERSIST`) is an explicit stub returning a clear error, deferred to Sprint 4 alongside the expiry reaper it actually needs (see `docs/superpowers/specs/2026-08-29-sprint-3-spec.md`). Lists, Hashes, and Sets each gained their remaining command coverage (table below).

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

**Sprint 5 (snapshotting & replication) — done.** `SAVE` writes a full, consistent
point-in-time snapshot (`bincode`-encoded, atomically written via write-then-rename) to
`ROCKET_MEM_SNAPSHOT_PATH`; startup loads that snapshot plus only the AOF bytes written
after it — the offset is embedded in the snapshot itself — instead of Sprint 4's
full-AOF-replay-from-empty, cutting recovery time (numbers below). `PSYNC`/`REPLICAOF <host>
<port>` add real leader→follower replication over the server's normal RESP port: a follower
receives a full snapshot, then applies every subsequent write the leader's AOF already logs —
inheriting the `SPOP`→`SREM`/`EXPIRE`-family→`PEXPIREAT` rewrites for free — while rejecting
client-originated writes of its own with a `READONLY` error until `REPLICAOF NO ONE` returns
it to normal operation. Every (re)sync, first or after a dropped connection, is a full resync;
there is no partial-resync/offset-resume support this sprint. See
`docs/superpowers/specs/2026-08-30-sprint-5-spec.md` for the full set of design decisions (why
replication piggybacks on the AOF's already-rewritten frame stream instead of a separate
mechanism, why a follower keeps no AOF of its own, the `SAVE`/`PSYNC` atomicity arguments).

Known limits, called out explicitly rather than left to be discovered: no partial resync (a
dropped follower connection always triggers a full resnapshot, per above); no authentication
on `PSYNC` (any client that sends it is treated as a legitimate replica — Sprint 8 is the
first point auth exists anywhere in this project); a stalled replica's fan-out queue is
unbounded and grows the leader's memory invisibly to `MAXMEMORY` accounting rather than
stalling every writer; `HELLO`/`INFO` still hardcode `role: master`/a bare `# Server` section
regardless of actual replica status.

Recovery-time benchmark (5,000 keys, `cargo test -p rocket-mem --test replication
snapshot_plus_tail_recovery -- --nocapture`): full AOF replay took `14.170081ms`, snapshot+tail took
`10.958109ms`.

Remaining sprints (clustering, a custom protocol, ACLs/TLS) are scoped in the
[sprint plan](docs/rocket-mem-sprint-plan.md) but not started.

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

### Running with persistence and replication

The server binary reads three environment variables at startup:

| Variable | Default | Purpose |
|---|---|---|
| `ROCKET_MEM_ADDR` | `127.0.0.1:6379` | TCP address to bind |
| `ROCKET_MEM_AOF_PATH` | `./appendonly.aof` | Append-only file path — replayed on startup if it already exists, then opened for appending with an `EverySecond` fsync policy |
| `ROCKET_MEM_SNAPSHOT_PATH` | `./dump.snapshot` | Snapshot file path — loaded on startup if present (together with only the AOF bytes written after the offset embedded in it), written by the `SAVE` command |

Turn a running node into a follower with `REPLICAOF <host> <port>` (sent over its own RESP
connection, e.g. via `redis-cli -p <port> replicaof <host> <port>`); `REPLICAOF NO ONE`
returns it to normal, writable operation. A follower rejects client-originated writes with a
`READONLY` error for as long as it's replicating.

## Workspace layout

Four crates under `crates/`:

- **`common`** — shared `EngineError` enum (`WrongType`, `NotAnInteger`, `NoSuchKey`). No dependencies on the other crates.
- **`engine`** — the storage engine: `Value` enum, 16-shard `Store`, and one free function per command under `commands/`. Everything in "Status" above lives here.
- **`protocol`** — RESP wire format: the `Frame` type (RESP2 plus RESP3's `Map`) and `RespCodec`, encoding/decoding both including split-read reassembly.
- **`server`** — the binary (package name `rocket-mem`): Tokio TCP accept loop, per-connection task, command dispatcher, AOF writer/replayer, snapshotting, leader/follower replication, and the active-expiry and fsync background loops.

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
