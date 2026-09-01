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

> **Project status.** rocket-mem is complete and tested — 731 tests, durability verified under a
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
VERSION=v0.1.3
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

Measured against `redis-server` 8.10.1 on the same host with matching durability settings
(`appendonly yes`, `appendfsync everysec`), via `redis-benchmark -t set,get -n 100000 -c 50`:

| Workload | redis-server | rocket-mem | Ratio |
|---|---:|---:|---:|
| GET, 3B, no pipeline | 109,290 | 105,708 | 1.03x |
| GET, 1KB, no pipeline | 103,093 | 98,619 | 1.05x |
| SET, 1KB, no pipeline | 103,734 | 92,764 | 1.12x |
| SET, 3B, no pipeline | 112,740 | 99,800 | 1.13x |
| GET, 3B, `-P 16` | 1,639,344 | 1,428,571 | 1.15x |
| SET, 1KB, `-P 16` | 450,450 | 245,700 | 1.83x |
| SET, 3B, `-P 16` | 934,579 | 390,625 | 2.39x |
| GET, 1KB, `-P 16` | 1,136,364 | 19,493 | **58.30x** |

Seven of eight cases land within 1.03x–2.39x of real Redis. The eighth does not: pipelined 1KB
`GET` collapses to ~19,500 req/s, an unexplained cliff that no candidate explanation accounts
for and that remains open. Full methodology, the raw traces, and the profiling that followed are
in [`docs/benchmarks/`](docs/benchmarks/).

## Command coverage

| Type | Commands |
|---|---|
| String/Key | `GET`, `SET` (`NX`/`XX`/`EX`/`PX`), `GETSET`, `GETRANGE`, `SETRANGE`, `APPEND`, `STRLEN`, `INCR`/`DECR`/`INCRBY`, `MSET`, `MGET`, `MSETNX`, `RENAME`, `RENAMENX`, `TYPE`, `RANDOMKEY`, `KEYS`, `SCAN`, `DEL`/`EXISTS` (variadic), `EXPIRE`, `PEXPIRE`, `EXPIREAT`, `PEXPIREAT`, `TTL`, `PTTL`, `PERSIST`, `MEMORY USAGE`, `OBJECT ENCODING` |
| Hash | `HSET`, `HGET`, `HDEL`, `HEXISTS`, `HGETALL`, `HLEN`, `HINCRBY`, `HKEYS`, `HVALS`, `HMGET`, `HSETNX`, `HSCAN` |
| List | `LPUSH`, `RPUSH` (variadic), `LPOP`, `RPOP`, `LRANGE`, `LLEN`, `LINDEX`, `LSET`, `LTRIM`, `LREM`, `LINSERT` |
| Set | `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`, `SINTER`, `SUNION`, `SDIFF`, `SINTERSTORE`, `SUNIONSTORE`, `SDIFFSTORE`, `SPOP`, `SRANDMEMBER` |
| Sorted Set | `ZADD`, `ZSCORE`, `ZREM`, `ZCARD`, `ZINCRBY`, `ZRANGE`, `ZRANK` |
| Server/Cluster | `PING`, `ECHO`, `SELECT`, `COMMAND`, `HELLO`, `INFO [section]`, `SAVE`, `REPLICAOF`, `PSYNC`, `DEBUG SLEEP`, `CLUSTER KEYSLOT`/`SHARDS`/`NODES`/`INFO`/`MYID`, `SLOWLOG GET`/`LEN`/`RESET` |
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

Every field, with its TOML key, environment variable, CLI flag, and ACL bootstrap format, is in
[`docs/config-reference.md`](docs/config-reference.md).

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
