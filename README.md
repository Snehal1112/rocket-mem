# rocket-mem

[![CI](https://github.com/Snehal1112/rocket-mem/actions/workflows/ci.yml/badge.svg)](https://github.com/Snehal1112/rocket-mem/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Snehal1112/rocket-mem)](https://github.com/Snehal1112/rocket-mem/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A Redis-compatible in-memory data store, written from scratch in Rust.

rocket-mem speaks **RESP2 and RESP3**, so `redis-cli`, `redis-py`, `ioredis`, `go-redis` and
every other Redis client connect to it unmodified. It also speaks **RMP**, its own binary
protocol, which adds the one thing RESP structurally cannot do: request multiplexing — many
in-flight requests on one connection, answered in any order and correlated by request id.

Both protocols read and write the same keyspace through the same dispatcher, so persistence,
replication, clustering, and access control apply identically whichever one a client uses.

> **Multi-threaded, unlike Redis.** rocket-mem runs on Tokio's multi-threaded runtime
> (`rt-multi-thread`), spawning one OS worker thread per CPU core plus a dedicated AOF-writer
> thread — confirmed with `ps -T <pid>` (16 `tokio-rt-worker` threads on a 16-core box) and with
> `pidstat -t` showing all of them busy at once under concurrent `redis-benchmark` load. Every
> connection is its own Tokio task, and the keyspace is split into 16 independently-locked shards
> (see [Architecture](#architecture)), so requests against different keys can execute on
> different cores at the same instant. Real Redis is deliberately single-threaded for command
> execution; rocket-mem chose sharded locks over that model instead.

> **Project status.** rocket-mem is complete and tested — 791 tests, durability verified under a
> `kill -9` chaos loop — but it is not yet production-hardened: there is no failover and no live
> resharding. Read [Limitations](#limitations) before deploying it.

## Quick start

**Docker**

```bash
docker run --rm -p 6379:6379 -p 6380:6380 ghcr.io/snehal1112/rocket-mem:latest
```

**Download a binary** — Linux x86-64, macOS arm64, and Windows x86-64 builds are attached to
[every release](https://github.com/Snehal1112/rocket-mem/releases/latest), each with a `.sha256`
checksum and a minisign `.sig`:

```bash
VERSION=v0.1.4
curl -LO https://github.com/Snehal1112/rocket-mem/releases/download/$VERSION/rocket-mem-$VERSION-linux-amd64.tar.gz
curl -LO https://github.com/Snehal1112/rocket-mem/releases/download/$VERSION/rocket-mem-$VERSION-linux-amd64.tar.gz.sha256
sha256sum -c rocket-mem-$VERSION-linux-amd64.tar.gz.sha256
tar -xzf rocket-mem-$VERSION-linux-amd64.tar.gz
./rocket-mem-$VERSION-linux-amd64
```

**Build from source** — needs a stable Rust toolchain:

```bash
git clone https://github.com/Snehal1112/rocket-mem.git
cd rocket-mem
cargo build --release --bin rocket-mem
./target/release/rocket-mem
```

It starts with no configuration file and no environment variables, binding three loopback
listeners:

| Address | Protocol | Notes |
|---|---|---|
| `127.0.0.1:6379` | RESP | Any Redis client connects here |
| `127.0.0.1:6380` | RMP | rocket-mem's own binary protocol |
| `127.0.0.1:9121` | HTTP | Prometheus `/metrics`; loopback because it is unauthenticated |

Talk to it with the client you already have:

```console
$ redis-cli -p 6379 SET user:1 alice
OK
$ redis-cli -p 6379 GET user:1
"alice"
```

Everything beyond this — authentication, TLS, clustering, custom paths — is opt-in. See
[`docs/getting-started.md`](docs/getting-started.md) for a fuller tour.

## Features

- **Redis wire compatibility** — RESP2 and RESP3, with full `HELLO` version negotiation.
- **A second protocol, RMP** — hand-rolled binary framing with request multiplexing, reachable
  on its own port, covering almost the entire command set.
- **Data types** — strings, hashes, lists, sets, and sorted sets, with Redis's `WRONGTYPE` and
  missing-key semantics matched command for command.
- **Durability** — every write is appended to an AOF with a configurable fsync policy, plus
  point-in-time snapshots; startup replays the snapshot and only the AOF tail written after it.
- **Replication** — leader/follower over the ordinary RESP port; followers reject writes with
  `-READONLY` until promoted.
- **Clustering** — Redis-Cluster-compatible hash slots (`CRC16(hash_tag(key)) % 16384`), with
  `-MOVED` redirection and `CROSSSLOT` enforcement.
- **Security** — Argon2-hashed passwords, per-user ACL rules over commands and key patterns, and
  optional TLS listeners for both protocols.
- **Observability** — a Prometheus `/metrics` endpoint, `INFO` in Redis's own format across eight
  sections, and a bounded slow log.

## Architecture

Three layers, with a strict rule: the storage engine knows nothing about any wire protocol.

```
┌──────────────────────────────────────────┐
│  Protocol layer      RESP2/RESP3, RMP    │
├──────────────────────────────────────────┤
│  Command dispatcher  routing, arg checks │
│                      auth, cluster, AOF  │
├──────────────────────────────────────────┤
│  Storage engine      data structures,    │
│                      expiry, persistence │
└──────────────────────────────────────────┘
```

That separation is what let RMP be added on top of the existing dispatcher without a single
change to engine code — both protocols build the same command shape and call the same function.

**Concurrency:** one Tokio task per connection; the keyspace is split into 16 shards, each behind
its own lock, so any task can reach any key by taking that key's shard lock. See
[`docs/design/sharding-decision.md`](docs/design/sharding-decision.md) for why 16, and
[`docs/architecture.md`](docs/architecture.md) for the full design.

## Performance

Measured against `redis-server` 8.10.1 on the same host via `scripts/benchmark.sh`, which pins
matching durability on **both** servers (`appendonly yes`, `appendfsync everysec`), runs both on
loopback, and spreads commands over a 100,000-key keyspace (`-r`) so writes reach all 16 shards.
`redis-benchmark -t set,get -n 100000 -c 50 -r 100000`, median of three sweeps (2026-09-08):

| Workload | redis-server | rocket-mem | Ratio |
|---|---:|---:|---:|
| SET, 3B, no pipeline | 78,247 | 90,662 | **0.86x (rocket faster)** |
| SET, 1KB, `-P 16` | 438,596 | 500,000 | **0.88x (rocket faster)** |
| GET, 1KB, `-P 16` | 746,269 | 763,359 | **0.98x (rocket faster)** |
| GET, 1KB, no pipeline | 99,010 | 97,943 | 1.01x |
| SET, 3B, `-P 16` | 763,359 | 729,927 | 1.05x |
| GET, 3B, no pipeline | 105,597 | 98,039 | 1.08x |
| SET, 1KB, no pipeline | 96,339 | 86,655 | 1.11x |
| GET, 3B, `-P 16` | 1,449,275 | 1,176,471 | 1.23x |

Every workload lands between **0.86x and 1.23x** of real Redis, and rocket-mem is faster on three
of the eight. The two pipelined `SET` rows were 2.78x and 2.18x until the AOF ordering lock was
made per-shard (below); they are now 1.05x and 0.88x.

**Two notes on reading these.** They are medians of a noisy host; run-to-run spread is wide,
especially on the `redis-server` side, so no ratio should be read to two significant figures.
And they are not comparable to figures published here before 2026-09-08, which came from a
single-key benchmark (no `-r`) — a degenerate case with no shard parallelism and a keyspace small
enough to sit in cache. That setup flattered `redis-server` on reads: its pipelined 1KB `GET` fell
from 1,052,632 to 813,008 req/s once keys were spread, while rocket-mem's barely moved.

Latency on the same setup, 3B payload, no pipelining, two runs quoted `run 1 / run 2`:

| Metric | redis-server | rocket-mem |
|---|---:|---:|
| SET `p50` | 0.303 / 0.255 ms | 0.295 / 0.287 ms |
| SET `p99` | 1.023 / 0.735 ms | 1.143 / 0.519 ms |
| GET `p50` | 0.247 / 0.255 ms | 0.263 / 0.279 ms |
| GET `p99` | 0.631 / 0.719 ms | 0.463 / 0.623 ms |

The two are comparable, within a few tens of microseconds either way, with no consistent winner at
either percentile. Worst-case `max` is spiky on both sides — `redis-server` recorded a 9.191ms
`SET` outlier and rocket-mem a 4.735ms one — and nothing anywhere approaches the hundreds of
milliseconds that an earlier AOF-blocking bug used to produce.

### What fixed pipelined `SET`: per-shard AOF ordering guards

Until 2026-09-08 the AOF ordering guard was a single process-wide mutex, acquired *before* the
engine mutation rather than merely around the AOF append. Every write command in the server
serialised on it, so the 16 independently-locked shards bought nothing at all for writes — the
worst of both models, paying multi-threading's coordination costs while executing writes one at a
time. Pipelined 3B `SET` sat at 2.78x of `redis-server`, and pipelined 1KB `SET` at 2.18x.

It is now one guard per shard. A write locks only the shards its own keys live in, so writes to
unrelated keys no longer block each other. Measured over six interleaved rounds, old binary against
new, alternating which ran first:

| Workload | global guard | per-shard | change |
|---|---:|---:|---:|
| SET, 3B, `-P 16` | 261,460 | 662,281 | **+143%** (6/6 rounds) |
| SET, 1KB, `-P 16` | 129,670 | 341,440 | **+160%** (6/6 rounds) |

The correctness argument is that replay only needs ordering *per key* — two commands touching
disjoint keys may be appended in either order and replay identically — and a key lives in exactly
one shard. Guards are acquired in ascending shard index, always, which is what makes deadlock
between overlapping multi-key commands impossible; `AofWriter::lock_shards` sorts and deduplicates
so no call site can get that wrong.

Three paths still take every guard, because they need a view of the whole keyspace rather than of
particular keys: `SAVE` and `BGREWRITEAOF` (which hold it across "read AOF offset, then walk the
keyspace", so `(snapshot, offset)` is a consistent cut), `serve_replica`'s snapshot-then-register,
and the follower apply loop (which must stop a concurrent `SAVE` seeing a multi-key command
half-applied across shards).

Design and testing strategy:
[`2026-09-08-per-shard-aof-ordering-spec.md`](docs/superpowers/specs/2026-09-08-per-shard-aof-ordering-spec.md).

### What fixed the pipelined 1KB `GET` cliff

Until 2026-09-08 that row read **19,448 req/s**, a 52.91x loss and an order of magnitude worse than
anything else in the table. (Every figure in this subsection was measured on the single-key harness
in use at the time, so they compare to each other but not to the multi-key table above.) It was not
slow code. It was a kernel timer, once per pipeline batch:

1. Replies are buffered with `feed()` and flushed only when no more pipelined input is ready
   (`crates/server/src/connection.rs`) — deliberate, and correct.
2. But `Framed` force-flushes inside `feed()` once its write buffer crosses `backpressure_boundary`,
   which defaults to 8 KiB.
3. Sixteen pipelined 1KB `GET` replies are ~1,033 bytes each ≈ 16.5 KiB, so each batch left the
   socket as **two** writes rather than one.
4. On loopback the MSS is ~64 KiB, so that second ~8 KiB write was a sub-MSS segment issued while
   the first was still unacknowledged — exactly what Nagle's algorithm holds back.
5. `redis-benchmark` sends nothing until all 16 replies arrive, so the only thing releasing it was
   Linux's 40ms delayed-ACK timer.

The arithmetic confirms the diagnosis: 16 requests ÷ 0.040s × 50 clients = **20,000 req/s**,
against measured values of 19,429 / 19,444 / 19,451 / 19,486 — a 0.3% spread across four sweeps,
as a fixed timer does not vary. Every other cell stayed under 8 KiB per flush and was unaffected:
`SET` replies are five bytes at any payload size, and sixteen 3B `GET` replies total ~144 bytes.

The fix is `TCP_NODELAY` on every accepted socket, set at all four accept sites (plaintext and TLS,
for both RESP and RMP). That row went from 19,448 to 775,194 req/s — a 40x improvement — and the
matrix lost its cliff entirely. Under the current multi-key harness the same row sits at 793,651
req/s, within 2% of `redis-server`.

**Everything on this page postdates the v0.1.4 release.** A binary downloaded from the releases
page still has the pipelined-`GET` stall and the global write guard; build from source to get
either fix.

All four causes identified in
[`2026-09-07-throughput-parity-design.md`](docs/superpowers/specs/2026-09-07-throughput-parity-design.md)
have now been addressed: `TCP_NODELAY`, the AOF ordering guard, the recency clock (a shared
`AtomicU64` every shard `fetch_add`ed on every access, now written once per 100ms expiry tick and
only read on the hot path), and per-command dispatcher allocations (`metric_label` returns a
`&'static str` instead of allocating, and read commands no longer clone a frame they discard).

The largest remaining per-command cost is not on that list: the `metrics` crate performs a registry
lookup keyed by (name, labels) on **every** command, which a `GET` profile puts at roughly 4.7% of
CPU. Caching the counter and histogram handles would remove far more than eliminating the label
allocation did.

## Command coverage

| Type | Commands |
|---|---|
| String/Key | `GET`, `SET` (`NX`/`XX`/`EX`/`PX`), `GETSET`, `GETRANGE`, `SETRANGE`, `APPEND`, `STRLEN`, `INCR`/`DECR`/`INCRBY`, `MSET`, `MGET`, `MSETNX`, `RENAME`, `RENAMENX`, `TYPE`, `RANDOMKEY`, `KEYS`, `SCAN`, `DEL`/`EXISTS` (variadic), `EXPIRE`, `PEXPIRE`, `EXPIREAT`, `PEXPIREAT`, `TTL`, `PTTL`, `PERSIST`, `MEMORY USAGE`, `OBJECT ENCODING` |
| Hash | `HSET`, `HGET`, `HDEL`, `HEXISTS`, `HGETALL`, `HLEN`, `HINCRBY`, `HKEYS`, `HVALS`, `HMGET`, `HSETNX`, `HSCAN` |
| List | `LPUSH`, `RPUSH` (variadic), `LPOP`, `RPOP`, `LRANGE`, `LLEN`, `LINDEX`, `LSET`, `LTRIM`, `LREM`, `LINSERT` |
| Set | `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`, `SINTER`, `SUNION`, `SDIFF`, `SINTERSTORE`, `SUNIONSTORE`, `SDIFFSTORE`, `SPOP`, `SRANDMEMBER`, `SSCAN` |
| Sorted Set | `ZADD` (single pair only, no `NX`/`XX`/`GT`/`LT`/`CH`/`INCR`), `ZSCORE`, `ZREM`, `ZCARD`, `ZINCRBY`, `ZRANGE`, `ZRANK` |
| Server/Cluster | `PING`, `ECHO`, `SELECT`, `COMMAND`, `HELLO`, `INFO [section]`, `SAVE`, `BGREWRITEAOF`, `REPLICAOF`, `PSYNC`, `DEBUG SLEEP`, `CLUSTER KEYSLOT`/`SHARDS`/`NODES`/`INFO`/`MYID`, `SLOWLOG GET`/`LEN`/`RESET` |
| Auth/ACL | `AUTH` (single-arg and `<user> <pass>`), `ACL SETUSER`/`DELUSER`/`WHOAMI`/`LIST`/`GETUSER` |

Behavioural differences from real Redis — `KEYS` glob support is partial, `OBJECT ENCODING`
reports engine type names rather than Redis's internal encodings, and others — are catalogued in
[`docs/command-compatibility.md`](docs/command-compatibility.md).

## Configuration

Configuration is layered, each level overriding the one before: built-in defaults → a TOML file →
`ROCKET_MEM_*` environment variables → CLI flags. Nothing is required.

| Setting | Default | Purpose |
|---|---|---|
| `addr` | `127.0.0.1:6379` | RESP listener |
| `rmp_addr` | `127.0.0.1:6380` | RMP listener |
| `metrics_addr` | `127.0.0.1:9121` | Prometheus endpoint; loopback because it is unauthenticated |
| `aof_path` | `./appendonly.aof` | Append-only file, replayed at startup |
| `snapshot_path` | `./dump.snapshot` | Snapshot written by `SAVE`, loaded at startup |
| `tls_resp_addr` / `tls_rmp_addr` | unset | TLS listeners, run alongside the plaintext ones |
| `tls_cert_path` / `tls_key_path` | unset | PEM cert and key; required if either TLS address is set |
| `cluster_config` / `cluster_node_id` | unset | Cluster topology file and this node's entry |
| `slowlog_threshold_micros` | `10000` | Commands at or over this are recorded; `0` disables |
| `log_level` | `info` | Log level filter for `tracing`; `RUST_LOG` env var always overrides it |

Every field, with its TOML key, environment variable, CLI flag, and ACL bootstrap format, is in
[`docs/config-reference.md`](docs/config-reference.md).

### On-disk files after a `BGREWRITEAOF`

`aof_path` and `snapshot_path` are the whole on-disk story until the first `BGREWRITEAOF`. Each
rewrite then writes a new *generation* alongside them rather than compacting the files in place:

| File | Meaning |
|---|---|
| `<snapshot_path>.manifest` | The generation currently in use. Absent means generation 0, i.e. the bare configured paths |
| `<aof_path>.<N>` | Generation `N`'s AOF |
| `<snapshot_path>.<N>` | Generation `N`'s snapshot |

Startup reads the manifest and loads only the generation it names, so **a backup must capture the
manifest together with the files it names** — the bare paths are stale once a rewrite has run, and
`SAVE` writes to the current generation, not to them. The generation a rewrite supersedes is
deleted on a best-effort basis; any older ones left behind by an interrupted cleanup are
unreferenced and safe to delete.

## Deployment

### Replication

Start a second node on its own ports, then point it at the leader over its normal RESP
connection:

```bash
# Follower: RESP on 6389, RMP and metrics moved off their defaults so both nodes can bind.
ROCKET_MEM_ADDR=127.0.0.1:6389 \
ROCKET_MEM_RMP_ADDR=127.0.0.1:6390 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9122 \
ROCKET_MEM_AOF_PATH=./follower.aof \
ROCKET_MEM_SNAPSHOT_PATH=./follower.snapshot \
  ./target/release/rocket-mem

redis-cli -p 6389 REPLICAOF 127.0.0.1 6379   # follow the leader on 6379
redis-cli -p 6389 REPLICAOF NO ONE           # promote back to writable
```

The follower receives a full snapshot, then applies every subsequent write the leader logs, and
rejects client writes with `-READONLY` while it is following.

### Clustering

Every node reads the same topology file and is told which entry is its own. Slot ranges must
cover all 16384 slots exactly once — a gap or overlap fails at startup, not at runtime.

```
# cluster.conf — <node-id> <host:port> <first-slot> <last-slot>
shard-a 127.0.0.1:7001 0     5460
shard-b 127.0.0.1:7002 5461  10922
shard-c 127.0.0.1:7003 10923 16383
```

```bash
ROCKET_MEM_ADDR=127.0.0.1:7001 \
ROCKET_MEM_RMP_ADDR=127.0.0.1:7101 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9201 \
ROCKET_MEM_CLUSTER_CONFIG=./cluster.conf \
ROCKET_MEM_CLUSTER_NODE_ID=shard-a \
  ./target/release/rocket-mem
```

A key's slot is computed exactly as real Redis computes it, so any cluster-aware client agrees:

```console
$ redis-cli -p 7001 CLUSTER KEYSLOT foo
(integer) 12182
$ redis-cli -p 7001 GET foo
(error) MOVED 12182 127.0.0.1:7003
```

Use a hash tag to pin related keys to one node: `{user1000}.name` and `{user1000}.city` always
share a slot. Following a `-MOVED` is the client's job — this server never proxies.

### RMP, the multiplexing protocol

Every node listens for RMP unconditionally. A client may send many requests without waiting for
replies, each tagged with a `request_id` the response echoes back:

```rust
let client = rmp_client::RmpClient::connect("127.0.0.1:6380").await?;
client.set("foo", "bar").await?;
assert_eq!(client.get("foo").await?, Some(bytes::Bytes::from_static(b"bar")));
```

Because each request is dispatched on its own task, commands sent back-to-back may also *execute*
out of order — if B must observe A's effect, await A's reply first. Each connection allows 256
requests in flight before applying TCP backpressure.

## Limitations

Stated plainly, so they are not discovered in production:

- **No failover and no live resharding.** Slot ownership is fixed at process start;
  `CLUSTER SETSLOT`, `MIGRATE`, and `ASK` redirection do not exist.
- **No cluster bus or gossip.** Nodes never talk to each other, so `CLUSTER NODES` reports every
  configured node as connected and `cluster_state` is always `ok`.
- **No request forwarding.** A `-MOVED` requires the client to reconnect.
- **Full resync only.** A dropped follower connection triggers a complete resnapshot; there are
  no replication offsets, and therefore no true replication-lag metric.
- **ACL state is in-memory and leader-local.** A runtime `ACL SETUSER` is lost on restart unless
  the user is also in the bootstrap config, and ACL changes never reach followers.
- **No `@category` ACL grants** — only explicit `+CMD`/`-CMD` plus `allcommands`/`nocommands`.
- **A stalled replica's fan-out queue is unbounded** and grows leader memory outside `MAXMEMORY`
  accounting.
- **`/metrics` is unauthenticated**, which is why it binds loopback by default.

[`docs/command-compatibility.md`](docs/command-compatibility.md) collects these together with
every command-level divergence from real Redis.

## Project layout

Five crates under `crates/`:

| Crate | Contents |
|---|---|
| `common` | The shared `EngineError` type. Depends on nothing else. |
| `engine` | `Value`, the 16-shard `Store`, and one function per command. Protocol-agnostic. |
| `protocol` | RESP's `Frame`/`RespCodec` and RMP's envelope codec. Both handle split reads. |
| `server` | The binary: accept loops, dispatcher, AOF, snapshots, replication, cluster routing, metrics, slow log. |
| `rmp-client` | A minimal async Rust client for RMP. |

## Building and testing

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

CI runs exactly those four commands on every push and pull request; all must pass.

## Documentation

| Document | Covers |
|---|---|
| [Getting started](docs/getting-started.md) | Install, first run, first client session, enabling TLS |
| [Configuration reference](docs/config-reference.md) | Every field: TOML key, env var, CLI flag, default |
| [Command compatibility](docs/command-compatibility.md) | Full command table and every divergence from Redis |
| [Architecture](docs/architecture.md) | The three-layer design and concurrency model |
| [Sharding decision](docs/design/sharding-decision.md) | Why 16 shards, and the locking strategy |
| [Benchmarks](docs/benchmarks/) | The `redis-benchmark` head-to-head and profiling notes |
| [QA playbook](docs/qa-playbook.md) | 135 manual test cases |

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the development workflow, code conventions, and
commit and pull-request expectations.

## License

MIT — see [`LICENSE`](LICENSE).
