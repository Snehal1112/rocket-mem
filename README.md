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
stalling every writer
(`HELLO` and `INFO` now report the real role — that Sprint 5 limitation was fixed in Sprint 6).

Recovery-time benchmark (5,000 keys, `cargo test -p rocket-mem --test replication
snapshot_plus_tail_recovery -- --nocapture`): full AOF replay took `14.170081ms`, snapshot+tail took
`10.958109ms`.

**Sprint 6 (clustering & observability) — done.** Keys now route across a multi-node cluster by
Redis-Cluster-compatible hash slot: `CLUSTER KEYSLOT` computes `CRC16(hash_tag(key)) % 16384`
byte-for-byte the way real Redis does (hash tags included, so `{user1000}.name` and
`{user1000}.city` are guaranteed to share a node), and a node handed a key it doesn't own replies
`-MOVED <slot> <host>:<port>` without touching its engine, its AOF, or any lock. Slot ownership
comes from one static config file every node reads at startup, validated to cover all 16384 slots
exactly once — see "Running a cluster" below. `CLUSTER SHARDS`/`NODES`/`INFO`/`MYID` report that
topology to cluster-aware clients. On the observability side, every command is counted and timed
into a Prometheus registry served from its own `/metrics` listener, `INFO` grew the eight real
sections tooling parses (server, clients, memory, persistence, stats, replication, cluster,
keyspace), and a bounded slow log records commands over a configurable threshold
(`SLOWLOG GET`/`LEN`/`RESET`). A head-to-head `redis-benchmark` report against real Redis is
committed at [`docs/benchmarks/2026-08-30-redis-benchmark.md`](docs/benchmarks/2026-08-30-redis-benchmark.md),
alongside the flamegraph pass that motivated this sprint's one performance fix
([`docs/benchmarks/2026-08-30-flamegraph-notes.md`](docs/benchmarks/2026-08-30-flamegraph-notes.md)).
See `docs/superpowers/specs/2026-08-30-sprint-6-spec.md` for the full set of design decisions
(why `-MOVED` takes precedence over `-READONLY`, why `CROSSSLOT` is enforced rather than skipped,
why `INFO`/`HELLO` moved out of `dispatch`).

Known limits, called out explicitly rather than left to be discovered: **there is no cluster bus
and no gossip** — nodes never talk to each other, so `CLUSTER NODES` reports every configured node
as `connected` and `cluster_state` is always `ok`, because a static config cannot honestly say
otherwise, and the `@<port+10000>` cluster-bus port shown in `CLUSTER NODES` is advertised by
convention but never bound; **no live resharding and no failover** (slot ownership is fixed at process start;
`CLUSTER SETSLOT`, `MIGRATE`, and `ASK`/`ASKING` redirection do not exist, and `ASK` would have
nothing to cover without migrations); **no request forwarding** — a `-MOVED` reply requires the
*client* to reconnect, this server never proxies to another shard; `CLUSTER SLOTS` is not
implemented (deprecated since Redis 7.0 in favour of `CLUSTER SHARDS`); a shard has exactly one
node, so cluster-level replicas are not represented even when a node is separately a Sprint-5
replication follower; slow-log entries carry 4 fields, not real Redis's 6 (the client address and
name are omitted — the dispatcher never learns the peer address); a slow-log entry records the
command name and its first argument rather than the full argument list, with real Redis's
`... (N more arguments)` marker standing in for the rest; `INFO`'s `expired_keys` counts only
*actively* expired keys, since passive expiry would need a counter on the hottest read path;
`INFO` omits `keyspace_hits`/`keyspace_misses` and `tcp_port` entirely rather than faking them;
`maxmemory` always reports 0 in the shipped binary because there is no env var to set a ceiling
yet; there is no true replication-*offset* lag metric, because Sprint 5's full-resync-only design
means no offsets exist — `rocket_mem_replication_last_apply_timestamp_seconds` is the honest
substitute; the `/metrics` endpoint is unauthenticated (hence its loopback default); and
`ReplicationHandle` is now misnamed — it carries the snapshot path, AOF handle, cluster config,
slow log, and server counters — with the rename to `ServerState` deferred to Sprint 7, whose
dual-protocol work already has to touch those signatures.

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
| Server/Cluster | `PING`, `ECHO`, `SELECT`, `COMMAND`, `HELLO`, `INFO [section]`, `SAVE`, `REPLICAOF`, `PSYNC`, `CLUSTER KEYSLOT`/`SHARDS`/`NODES`/`INFO`/`MYID`, `SLOWLOG GET`/`LEN`/`RESET` |

`KEYS`'s glob support is intentionally partial: no character ranges (`[a-z]`), negation
(`[^abc]`), or escaping. Active expiry sweeps one whole shard per 100ms tick rather than
sampling individual keys within a shard the way real Redis does — an accepted
simplification, not a bug (see the Sprint 4 spec). `OBJECT ENCODING` reports this engine's
own type name (`string`/`list`/`hash`/`set`/`zset` — exactly what `TYPE` returns, since both
come from `Value::type_name()`), not real Redis's actual internal
encodings (`embstr`/`listpack`/etc.), which this engine doesn't implement. All of the above
are exercised directly by engine tests and reachable over RESP through the dispatcher.

### Running with persistence and replication

The server binary reads these environment variables at startup:

| Variable | Default | Purpose |
|---|---|---|
| `ROCKET_MEM_ADDR` | `127.0.0.1:6379` | TCP address to bind |
| `ROCKET_MEM_AOF_PATH` | `./appendonly.aof` | Append-only file path — replayed on startup if it already exists, then opened for appending with an `EverySecond` fsync policy |
| `ROCKET_MEM_SNAPSHOT_PATH` | `./dump.snapshot` | Snapshot file path — loaded on startup if present (together with only the AOF bytes written after the offset embedded in it), written by the `SAVE` command |
| `ROCKET_MEM_CLUSTER_CONFIG` | unset | Path to the cluster topology file. Unset means cluster mode is off (no `-MOVED`, no `-CROSSSLOT`). Must be set together with `ROCKET_MEM_CLUSTER_NODE_ID` |
| `ROCKET_MEM_CLUSTER_NODE_ID` | unset | Which line of that file describes this process. Startup fails if it is missing or names an unknown id |
| `ROCKET_MEM_METRICS_ADDR` | `127.0.0.1:9121` | Where the Prometheus `/metrics` endpoint listens. Loopback by default because it is unauthenticated |
| `ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS` | `10000` (10ms) | Commands at or over this duration are recorded in the slow log. `0` disables it |

Turn a running node into a follower with `REPLICAOF <host> <port>` (sent over its own RESP
connection, e.g. via `redis-cli -p <port> replicaof <host> <port>`); `REPLICAOF NO ONE`
returns it to normal, writable operation. A follower rejects client-originated writes with a
`READONLY` error for as long as it's replicating.

### Running a cluster

Every node reads the same topology file and is told which line is its own. Slot ranges must cover
all 16384 slots exactly once — a gap or an overlap is a startup error, not a runtime surprise.

```
# cluster.conf — <node-id> <host:port> <first-slot> <last-slot>
shard-a 127.0.0.1:7001 0     5460
shard-b 127.0.0.1:7002 5461  10922
shard-c 127.0.0.1:7003 10923 16383
```

```bash
ROCKET_MEM_ADDR=127.0.0.1:7001 \
ROCKET_MEM_CLUSTER_CONFIG=./cluster.conf \
ROCKET_MEM_CLUSTER_NODE_ID=shard-a \
  cargo run --release --bin rocket-mem
```

A key's slot is `CRC16(hash_tag(key)) % 16384`, identical to real Redis Cluster, so any
cluster-aware client computes the same answer:

```
$ redis-cli -p 7001 cluster keyslot foo
(integer) 12182
$ redis-cli -p 7001 get foo
(error) MOVED 12182 127.0.0.1:7003
```

Multi-key commands must have all their keys in one slot, or they are rejected with
`CROSSSLOT Keys in request don't hash to the same slot` — use a hash tag (`{user1000}.name`,
`{user1000}.city`) to force related keys onto one node. This server never forwards a command to
another node: following a `-MOVED` is the client's job.

### Observability

`GET http://$ROCKET_MEM_METRICS_ADDR/metrics` serves a Prometheus text-format registry:

| Metric | Type | Labels |
|---|---|---|
| `rocket_mem_commands_total` | counter | `cmd` |
| `rocket_mem_command_errors_total` | counter | `cmd` |
| `rocket_mem_command_duration_seconds` | histogram | `cmd` |
| `rocket_mem_connected_clients` | gauge | — |
| `rocket_mem_connections_total` | counter | — |
| `rocket_mem_memory_used_bytes` | gauge | — |
| `rocket_mem_keys` / `rocket_mem_keys_with_expiry` | gauge | — |
| `rocket_mem_expired_keys_total` | counter | — |
| `rocket_mem_evicted_keys_total` | counter | — |
| `rocket_mem_connected_replicas` | gauge | — |
| `rocket_mem_replication_last_apply_timestamp_seconds` | gauge | — |
| `rocket_mem_slowlog_entries_total` | counter | — |

The `cmd` label is drawn from a fixed list of known command names, with everything else collapsed
to `other`, so an unknown command cannot create unbounded series. Commands a follower applies from
its leader are not counted: only client-originated commands reach the instrumented path.

`INFO [section]` reports the same state in real Redis's own format — `server`, `clients`,
`memory`, `persistence`, `stats`, `replication`, `cluster`, `keyspace`, or all of them at once.
`SLOWLOG GET [count]` / `SLOWLOG LEN` / `SLOWLOG RESET` read and clear the last
`ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS`-exceeding commands.

## Workspace layout

Four crates under `crates/`:

- **`common`** — shared `EngineError` enum (`WrongType`, `NotAnInteger`, `NoSuchKey`). No dependencies on the other crates.
- **`engine`** — the storage engine: `Value` enum, 16-shard `Store`, and one free function per command under `commands/`. Everything in "Status" above lives here.
- **`protocol`** — RESP wire format: the `Frame` type (RESP2 plus RESP3's `Map`) and `RespCodec`, encoding/decoding both including split-read reassembly.
- **`server`** — the binary (package name `rocket-mem`): Tokio TCP accept loop, per-connection task, command dispatcher, AOF writer/replayer, snapshotting, leader/follower replication, the active-expiry and fsync background loops, cluster hash-slot routing and `-MOVED` redirection, the Prometheus metrics endpoint, and the slow log.

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
- [`docs/benchmarks/`](docs/benchmarks/) — the committed `redis-benchmark` head-to-head report and the flamegraph profiling notes.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the development workflow, code conventions, and commit/PR expectations.

## License

MIT — see [`LICENSE`](LICENSE).
