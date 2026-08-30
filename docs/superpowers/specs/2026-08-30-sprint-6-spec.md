# Sprint 6 — Clustering & Observability: Spec & Design

**Goal:** keys route deterministically across a 3-shard cluster, and a benchmark report shows throughput in the same ballpark as real Redis — matching `../../rocket-mem-sprint-plan.md`'s Sprint 6 goal.

**Scope:** covers Sprint 6's 6 backlog items (see `../../rocket-mem-sprint-plan.md`, Sprint 6, and `../../rocket-mem-production-plan.md`, Weeks 11–12). This doc fixes the shared design decisions — the hash-slot algorithm, the static cluster-assignment config format, where `-MOVED` is checked and how it interacts with Sprint 5's `-READONLY` gate, the exact Prometheus metric names and where they are recorded, the `INFO` field set, and the slow-log's shape — that every implementation plan below assumes as ground truth. Individual plans don't re-derive these; they reference this doc.

**Architecture recap:** this sprint adds a routing decision and an observability surface on top of Sprint 5's server, and changes no engine data path. Cluster routing is a new `server`-crate module (`crates/server/src/cluster.rs`) holding a pure `key_slot(&[u8]) -> u16` function and a `ClusterConfig` parsed once at startup; it is consulted only in `dispatch_and_log`, so plain `dispatch` — the function AOF replay (`aof::replay`) and the follower apply loop (`replication::sync_once`) both call — stays completely cluster-unaware and can never be redirected. Observability is a new `crates/server/src/metrics.rs` (a globally-installed `metrics` recorder plus a small `/metrics` HTTP listener of our own) and a set of counters that live alongside the existing per-node state on `ReplicationHandle`. `dispatch_and_log` is split into a thin instrumented wrapper plus the existing body (renamed `dispatch_and_log_inner`), which is the single place command counts, latency histograms, and slow-log entries are recorded — no instrumentation is duplicated across `dispatch` and `dispatch_and_log`.

## Global Constraints

- **Version floors:** Rust edition 2021 (unchanged), `metrics = "0.24"` (0.24.6 at time of writing), `metrics-exporter-prometheus = { version = "0.18", default-features = false }` (0.18.3). `default-features = false` is load-bearing: the exporter's default features (`http-listener`, `push-gateway`) pull in `hyper`, `hyper-util`, `hyper-rustls`, `rustls`, and `ipnet`, none of which this project needs — we serve `/metrics` from ~50 lines over the `tokio` listener we already depend on. No other new runtime dependency lands anywhere in the workspace this sprint; CRC16 is hand-rolled (see below) rather than pulling a `crc` crate for 15 lines of table-free code.
- **No cluster bus, no gossip, no failover.** Nodes never talk to each other. Every node learns the whole topology from the same static config file, read once at startup. Nothing detects that a peer is down; `CLUSTER NODES` reports every configured node as `connected` because a static config cannot honestly report anything else, and this is stated in its own output section below rather than quietly implied.
- **No live resharding**, per the sprint plan's own risk-table mitigation and the production plan's Week 11 scope-down. Slot ownership is fixed at process start; there is no `CLUSTER SETSLOT`, no `MIGRATE`, no `ASK`/`ASKING` redirection (which exists in real Redis only to cover an in-progress migration — with no migrations there is nothing for it to cover).
- **No cross-node request forwarding / proxying.** A node that does not own a key's slot replies `-MOVED <slot> <host>:<port>` and does nothing else. It never opens a connection to the owning node, never forwards the command, and never returns the other node's answer. Redirection is the *client's* job, exactly as in real Redis Cluster.
- **No replica-of-a-shard topology awareness.** A node can be both a cluster shard and a Sprint-5 replication follower — the two features are orthogonal and both gates run — but `CLUSTER SHARDS` reports only what the static config says, so a follower is never listed as a replica of any shard. This is called out where the reply format is fixed, not left to be discovered.
- **The Prometheus endpoint is unauthenticated and binds separately from the RESP port.** Sprint 8 is the first point authentication exists anywhere in this project; adding it only to `/metrics` now would be inconsistent. The default bind address is loopback-only for exactly this reason.
- **Metric label cardinality is bounded by construction:** the only label this sprint emits is `cmd`, and its value is drawn from a fixed list of the 84 command names the dispatcher knows, with everything else collapsed to `other`. An unknown command name from a hostile or buggy client must never be able to create an unbounded number of Prometheus series.

---

## Decision: "sharding" means two different things in this codebase, and they never interact

This project now has two independent things called sharding, and confusing them would produce a genuinely broken design. They are fixed here, once, and every plan below uses these words in exactly this sense:

| | **Internal shards** (Sprint 1) | **Cluster hash slots** (this sprint) |
|---|---|---|
| What it splits | one process's keyspace across 16 `RwLock`s | one *logical* keyspace across N server processes |
| Count | 16, fixed at `Store::new(16)` (`crates/engine/src/engine.rs:22`) | 16384 slots, fixed by the algorithm |
| Hash | `DefaultHasher(key) % 16` (`crates/engine/src/store.rs:20-24`) | `CRC16-XMODEM(hashtag(key)) % 16384` |
| Purpose | lock striping — concurrency within one node | placement — which *node* owns a key at all |
| Who sees it | `engine` crate only | `server` crate only (`cluster.rs`, `dispatch_and_log`) |
| Documented in | `../../design/sharding-decision.md` | this spec, plus the plan-08 update to that doc |

Every node in a cluster is a **complete, ordinary `rocket-mem` server** — its own `Engine` with its own internal 16 shards, its own AOF, its own snapshot, optionally its own Sprint-5 followers. Cluster membership changes *which keys a node accepts*, and nothing else. In particular: the engine crate gains no knowledge of slots, `Store::shard_for` is untouched, and the 16-shard count has no relationship whatsoever to the 16384-slot count beyond the coincidence of the digits.

`../../design/sharding-decision.md` explicitly defers its "revisit once Sprint 6 (Week 12) benchmarking gives real contention data" question to this sprint; plan 08 answers it with the benchmark's actual finding rather than leaving the note dangling.

## Decision: 16384 slots, `CRC16-CCITT/XMODEM` hand-rolled, hash tags supported

**Decision:** `key_slot(key) = crc16(hash_tag(key)) % 16384`, byte-for-byte the algorithm real Redis Cluster uses, so that any cluster-aware Redis client computes the same slot for the same key without knowing this server is not Redis. Picking a different slot count or a different hash would break every off-the-shelf client's ability to route, which is the entire point of implementing the feature.

**The CRC16 variant is CCITT/XMODEM**: polynomial `0x1021`, initial value `0x0000`, no input/output reflection, no final XOR — the same `crc16.c` real Redis ships. It is hand-rolled bit-by-bit in `crates/server/src/cluster.rs` (16 lines, no table, no dependency):

```rust
// crates/server/src/cluster.rs
/// CRC16-CCITT/XMODEM — poly 0x1021, init 0x0000, no reflection, no final XOR. The exact
/// variant real Redis Cluster uses (`crc16.c`), so slots computed here match any cluster-aware
/// client's own computation. Bit-by-bit rather than table-driven: 8 iterations per byte over
/// key-length input is nowhere near a hot path (it runs once per command, only when cluster
/// mode is enabled), and a 512-byte table would need its own correctness test to earn its place.
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for byte in data {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}
```

**Reference values every implementation must reproduce** (all independently verified against the algorithm while writing this spec):

| Input | Expected |
|---|---|
| `crc16(b"123456789")` | `0x31C3` — the published XMODEM check value; proves the variant, not just the code |
| `key_slot(b"foo")` | `12182` — the reference value named in `../../rocket-mem-production-plan.md`'s Week 11 example test |
| `key_slot(b"bar")` | `5061` |
| `key_slot(b"hello")` | `866` |
| `key_slot(b"user1000")` | `3443` |
| `key_slot(b"{user1000}.following")` | `3443` — same slot as `user1000`, proving hash tags work |
| `key_slot(b"{user1000}.followers")` | `3443` |
| `key_slot(b"foo{bar}{zap}")` | `5061` — only the *first* `{...}` counts, so this equals `key_slot(b"bar")` |
| `key_slot(b"foo{{bar}}zap")` | `4015` — the tag is `{bar` (first `{`, then the first `}` after it) |
| `key_slot(b"{}foo")` | `9500` — an empty tag is no tag; the whole key is hashed |
| `key_slot(b"{user1000")` | `8723` — an unclosed `{` is no tag; the whole key is hashed |

**Hash tags are supported, not scoped out.** They cost ~8 lines and they are the only mechanism a client has to force related keys onto one node — without them, `MSET {u1}:name x {u1}:email y` is impossible in a cluster, and the `CROSSSLOT` rule below would make every multi-key command effectively unusable. Scoping them out would mean shipping a routing scheme that is *nearly* Redis's, which is worse than either extreme. The extraction rule is Redis's exactly:

```rust
// crates/server/src/cluster.rs
/// Returns the substring between the first `{` and the first `}` that follows it, when that
/// substring is non-empty; otherwise the whole key. Matches real Redis Cluster's rule, including
/// its two edge cases: `{}foo` (empty tag) and `{foo` (unclosed) both hash the *whole* key.
fn hash_tag(key: &[u8]) -> &[u8] {
    let Some(open) = key.iter().position(|b| *b == b'{') else {
        return key;
    };
    let Some(close_offset) = key[open + 1..].iter().position(|b| *b == b'}') else {
        return key;
    };
    if close_offset == 0 {
        return key; // `{}` — an empty tag is not a tag
    }
    &key[open + 1..open + 1 + close_offset]
}

/// The slot a key belongs to: `CRC16(hash_tag(key)) mod 16384`. Pure — no config, no state — so
/// `CLUSTER KEYSLOT` answers it identically whether or not this node is in cluster mode.
pub fn key_slot(key: &[u8]) -> u16 {
    crc16(hash_tag(key)) % SLOT_COUNT
}

/// 16384, matching real Redis Cluster. Not configurable: a different count would make every
/// off-the-shelf cluster client compute a different slot for the same key.
pub const SLOT_COUNT: u16 = 16384;
```

## Decision: slot ownership comes from one static text config file, identical on every node

**Decision:** two new environment variables, read once in `main.rs`:

| Variable | Default | Meaning |
|---|---|---|
| `ROCKET_MEM_CLUSTER_CONFIG` | unset | Path to the cluster topology file. **Unset means cluster mode is off** — no `-MOVED`, no `-CROSSSLOT`, `cluster_enabled:0` in `INFO`, exactly today's behavior. |
| `ROCKET_MEM_CLUSTER_NODE_ID` | unset | Which line of that file describes *this* process. Required when `ROCKET_MEM_CLUSTER_CONFIG` is set; startup fails loudly if it is missing or names an id the file doesn't contain. |

**The file format is line-oriented plain text**, not TOML/YAML/JSON: the workspace has no config-file parser today (`figment`/TOML is explicitly a Sprint 8 backlog item), and adding one for four fields would be the tail wagging the dog. Blank lines and `#` comments are skipped; every other line is exactly four whitespace-separated fields:

```
# <node-id> <host:port> <first-slot> <last-slot>
shard-a 127.0.0.1:7001 0     5460
shard-b 127.0.0.1:7002 5461  10922
shard-c 127.0.0.1:7003 10923 16383
```

**Validation is strict and happens at startup, never at request time** — a topology error must stop the process, not surface as mysterious per-key misrouting hours later. `ClusterConfig::parse` rejects, with a message naming the offending line: a line without exactly 4 fields; a non-numeric or `> 16383` slot number; `first > last`; a duplicate node id; a duplicate `host:port`; an empty file; and — the important one — **any slot map that is not an exact, gapless, overlap-free cover of `0..=16383`**. The cover check is: sort the ranges by `first`, require `ranges[0].first == 0`, require `ranges[i].first == ranges[i-1].last + 1` for every subsequent range, and require `ranges.last().last == 16383`. This makes "which node owns slot S" total — `owner_of` can return `&ClusterNode`, never `Option`, and no code path anywhere needs to handle an unowned slot.

```rust
// crates/server/src/cluster.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterNode {
    pub id: String,
    pub addr: String,   // "host:port", verbatim from the config — never re-resolved
    pub first_slot: u16,
    pub last_slot: u16,
}

#[derive(Debug, Clone)]
pub struct ClusterConfig {
    nodes: Vec<ClusterNode>, // sorted by first_slot, validated to cover 0..=16383 exactly
    myself: usize,           // index into `nodes` of the ROCKET_MEM_CLUSTER_NODE_ID entry
}

impl ClusterConfig {
    /// Parses `text` and picks out `node_id` as this process's own entry. Errors are
    /// `std::io::Error(InvalidData, msg)` so `main.rs` can `?` them straight out of its
    /// existing `std::io::Result<()>` return type — no new error type, no new dependency.
    pub fn parse(text: &str, node_id: &str) -> std::io::Result<Self>;
    /// Reads the file at `path` and delegates to `parse`.
    pub fn load(path: &std::path::Path, node_id: &str) -> std::io::Result<Self>;
    /// Total, never `None`: the slot map is validated to cover every slot exactly once.
    pub fn owner_of(&self, slot: u16) -> &ClusterNode;
    pub fn myself(&self) -> &ClusterNode;
    pub fn owns(&self, slot: u16) -> bool;
    /// Every node, ordered by `first_slot` — what `CLUSTER SHARDS`/`CLUSTER NODES` iterate.
    pub fn nodes(&self) -> &[ClusterNode];
}
```

**`addr` is stored and echoed verbatim.** A `-MOVED` reply must name an address the *client* can connect to, which is not necessarily one this process could resolve or would bind; re-resolving or canonicalizing it here would be a way to be wrong, never a way to be right.

**How the config reaches `dispatch_and_log`:** a new `cluster: Option<Arc<ClusterConfig>>` field on `ReplicationHandle` (`crates/server/src/replication.rs:42`), set through a new builder method `with_cluster(mut self, cluster: Arc<ClusterConfig>) -> Self`, exactly mirroring Sprint 5's `with_aof` (`replication.rs:113`) and for the same reason: the ~40 existing `ReplicationHandle::new`/`::default()` call sites (all tests) stay untouched and default to `None`, i.e. cluster mode off. Only `main.rs` and the new `tests/cluster.rs` call it.

**Acknowledged naming debt:** `ReplicationHandle` now carries a snapshot path (Sprint 5), an AOF handle (Sprint 5), a cluster config, a slow-log, and a set of server-wide counters — it is "shared server state," not a replication handle. Renaming it to `ServerState` is a mechanical ~60-site change that would collide with every Sprint 4/5 doc's prose and buys nothing this sprint; **Sprint 7's dual-protocol work already has to touch these signatures**, so the rename is deferred to it and recorded in the README's known-limits list rather than left implicit. No plan in this sprint performs the rename.

## Decision: `CLUSTER` is a dispatcher interception with five subcommands; `CLUSTER SLOTS` is out

`CLUSTER` needs the `ClusterConfig`, which lives on `ReplicationHandle`, which plain `dispatch` has no parameter for. **Decision:** `CLUSTER` is intercepted in `dispatch_and_log` before it would delegate to `dispatch` — the same mechanism `SAVE` (`dispatcher.rs:1062`) and `REPLICAOF` (`dispatcher.rs:1065`) already use — via `fn handle_cluster(items: &[Frame], replication: &ReplicationHandle) -> Option<Frame>`. `dispatch` never learns the command exists, which is correct: neither AOF replay nor a follower's apply loop should ever see a `CLUSTER`.

Exact replies, all verified against what a static config can honestly report:

**`CLUSTER KEYSLOT <key>` → `Frame::Integer(key_slot(key) as i64)`.** Answered **even when cluster mode is off** — it is a pure function of the key, real Redis answers it in non-cluster mode too, and making it conditional would leave the sprint's headline algorithm untestable over the wire on a plain node. Wrong arity → `ERR wrong number of arguments for 'cluster|keyslot' command`.

**`CLUSTER MYID` → `Frame::Bulk(node id)`** when cluster mode is on; when off, a 40-character all-zero id (`"0".repeat(40)`), matching real Redis's "no cluster identity" shape rather than inventing one.

**`CLUSTER INFO` → `Frame::Bulk`**, CRLF-terminated `key:value` lines, exactly these:

```
cluster_enabled:1
cluster_state:ok
cluster_slots_assigned:16384
cluster_known_nodes:3
cluster_size:3
cluster_my_epoch:0
cluster_current_epoch:0
```

`cluster_state` is unconditionally `ok` and every epoch is unconditionally `0` **because a static config has no way to know otherwise** — there is no gossip to learn a peer is down, and no epoch bumping without resharding or failover. Emitting a field we cannot compute is worse than omitting it, so the fields we cannot compute are pinned to the value that is *true by construction of the static config* and documented as such here and in the README. With cluster mode off: `cluster_enabled:0`, `cluster_state:ok`, and every count `0`.

**`CLUSTER SHARDS` → `Frame::Array`**, one entry per configured node, each a `Frame::Map` (RESP3) / flattened pairs — to keep RESP2 and RESP3 clients identical, and to avoid depending on `Protocol` state inside the interception, **each shard entry is a `Frame::Array` of alternating key/value frames**, which is what real Redis emits under RESP2 and what every client parses:

```
1) 1) "slots"
   2) 1) (integer) 0
      2) (integer) 5460
   3) "nodes"
   4) 1) 1) "id"
         2) "shard-a"
         3) "port"
         4) (integer) 7001
         5) "ip"
         6) "127.0.0.1"
         7) "endpoint"
         8) "127.0.0.1"
         9) "role"
        10) "master"
        11) "replication-offset"
        12) (integer) 0
        13) "health"
        14) "online"
```

`role` is always `master` and the `nodes` list always has exactly one entry per shard: this sprint's cluster has no notion of a shard-level replica (see Global Constraints). `replication-offset` is `0` because this project has no replication offsets at all — Sprint 5's Global Constraints made full-resync-only an explicit decision, so there is no offset to report; the field is present because clients parse for it, and pinned at 0 rather than fabricated. `health` is `online` for the same reason `cluster_state` is `ok`. With cluster mode off: an empty array.

**`CLUSTER NODES` → `Frame::Bulk`**, one line per node in real Redis's space-separated format, `\n`-terminated (real Redis uses `\n`, not `\r\n`, inside this particular payload):

```
shard-a 127.0.0.1:7001@17001 myself,master - 0 0 0 connected 0-5460
shard-b 127.0.0.1:7002@17002 master - 0 0 0 connected 5461-10922
shard-c 127.0.0.1:7003@17003 master - 0 0 0 connected 10923-16383
```

Field by field: id; `<addr>@<cport>` where the cluster-bus port is by Redis convention `port + 10000` and is **advertised but not bound** — there is no cluster bus (Global Constraints), and the field's format is not optional in the grammar clients parse, so the conventional value is emitted with this caveat recorded in the README; `myself,master` on this node's own line and `master` on the others; `-` for "no master" (every node is a master); `0 0 0` for ping-sent/pong-received/config-epoch, all unknowable without a bus; `connected`, always, because nothing here can observe a disconnection; and the owned slot range. With cluster mode off: an empty bulk string.

**`CLUSTER SLOTS` is deliberately not implemented.** It has been deprecated since Redis 7.0 in favour of `CLUSTER SHARDS`, it carries the same information in an older shape, and nothing in this sprint's own test suite needs it: the 3-shard DoD test follows `-MOVED` explicitly (see the testing decision below) rather than relying on a third-party client's topology discovery. Any other `CLUSTER` subcommand returns `ERR unknown CLUSTER subcommand '<sub>'`, matching the existing `MEMORY`/`OBJECT` arms' shape (`dispatcher.rs:845`, `:860`).

## Decision: `-MOVED` is checked at the top of `dispatch_and_log`, before `-READONLY`

**Where.** The check is the *first* thing `dispatch_and_log_inner` does — before Sprint 5's `-READONLY` gate (`dispatcher.rs:1054-1060`), before the `SAVE`/`REPLICAOF`/`CLUSTER`/`INFO`/`HELLO`/`SLOWLOG` interceptions, and therefore long before `extract_write_command_name` acquires the AOF ordering lock. A redirected command must never touch the engine, the AOF, the replica fan-out, or any lock.

**Never in `dispatch`.** `dispatch` is called directly by `aof::replay` (`aof.rs:238`) and by the follower apply loop (`replication.rs:sync_once`). A follower applying its leader's stream, or a node replaying its own AOF at boot, must apply *every* frame it is given regardless of slot ownership — redirecting there would silently drop writes on recovery. Keeping the check in `dispatch_and_log` alone makes that impossible by construction rather than by discipline.

**Precedence: `-MOVED` beats `-READONLY`.** A node can be simultaneously a cluster shard and a Sprint-5 follower, so both gates can fire for one command. `-MOVED` wins because it is a statement about *which node should be handling this key at all*, while `-READONLY` is a statement about what this node will do with a key it owns. A cluster-aware client that gets `-READONLY` retries on the same node forever; one that gets `-MOVED` goes to the owning node, where — if *that* node is itself a follower — it correctly receives `-READONLY` and can act on it. The reverse order gives a client no path to success.

**The reply:** `Frame::Error(format!("MOVED {slot} {addr}"))`, where `addr` is the owning node's config `addr` verbatim — rendering on the wire as `-MOVED 12182 127.0.0.1:7002\r\n`. (`Frame::Error` carries the message without the leading `-`; `RespCodec` adds it. Sprint 5's `READONLY ...` error establishes this.)

**Which commands are slot-checked** is decided by a total key-extraction function over the 84 commands the dispatcher knows. There is no "infer the key from position 1" fallback for commands that have no key — that would redirect `PING`:

```rust
// crates/server/src/dispatcher.rs
/// Which arguments of a command are keys. Total over every command `dispatch` handles; the
/// `First` default is correct for the ~70 single-key commands, and every exception is listed.
enum KeySpec {
    None,        // PING, ECHO, SELECT, COMMAND, INFO, HELLO, KEYS, SCAN, RANDOMKEY,
                 // CLUSTER, SAVE, REPLICAOF, PSYNC, SLOWLOG
    First,       // the default: GET, SET, HSET, LPUSH, ZADD, TTL, TYPE, ...
    Second,      // MEMORY USAGE <key>, OBJECT ENCODING <key>
    All,         // DEL, EXISTS, MGET, RENAME, RENAMENX,
                 // SINTER, SUNION, SDIFF, SINTERSTORE, SUNIONSTORE, SDIFFSTORE
    EveryOther,  // MSET, MSETNX — keys at argument indices 0, 2, 4, ...
}
// An *unknown* command name maps to `None`, not `First`: it must fall through to dispatch's
// own "ERR unknown command" error rather than be redirected on a slot computed from an
// argument that isn't a key.
```

`SINTERSTORE`/`SUNIONSTORE`/`SDIFFSTORE` are `All`, not "sources only": the destination is a key this node would *write*, so it must hash to the same slot as the sources or the command would write a key this node does not own. That is precisely what `CROSSSLOT` exists to prevent.

**`CROSSSLOT` is implemented, not skipped.** Without it, `MSET a 1 b 2` (slots 15495 and 3300 — different nodes) would be accepted by whichever node owns `a`'s slot, and would then write `b` onto a node that does not own it — a silent, permanent violation of the routing invariant the whole feature exists to establish, and one no client could detect. The error text matches real Redis exactly: `CROSSSLOT Keys in request don't hash to the same slot`. Hash tags (above) are what let a client legitimately keep multi-key commands working under this rule.

The whole gate is one helper, returning the reply to send or `None` to continue:

```rust
// crates/server/src/dispatcher.rs
/// `None` = this node may handle the command. `Some(frame)` = reply with this instead, without
/// touching the engine, the AOF, or any lock. Returns `None` immediately when cluster mode is
/// off, which is every existing test and every non-cluster deployment.
fn cluster_redirect(frame: &Frame, replication: &ReplicationHandle) -> Option<Frame> {
    let cluster = replication.cluster()?;              // None => cluster mode off
    let keys = command_keys(frame);                    // KeySpec-driven, borrows from `frame`
    let mut slots = keys.into_iter().map(|k| crate::cluster::key_slot(k));
    let first = slots.next()?;                         // no keys => nothing to route
    if !slots.all(|s| s == first) {
        return Some(Frame::Error(
            "CROSSSLOT Keys in request don't hash to the same slot".into(),
        ));
    }
    if cluster.owns(first) {
        return None;
    }
    let owner = cluster.owner_of(first);
    Some(Frame::Error(format!("MOVED {first} {}", owner.addr)))
}
```

**Cost when cluster mode is off** is one `Option` check per command — `replication.cluster()` returns `None` and the function returns before extracting any key. This matters because it sits on the hot path measured in plan 07.

## Decision: Prometheus metrics are recorded in one wrapper around `dispatch_and_log`, and served from our own listener

**The crate choice** is `metrics` (the facade) plus `metrics-exporter-prometheus` (the recorder + text renderer), as named in `../../rocket-mem-production-plan.md`'s Week 12, with `default-features = false` per the Global Constraints. We use the exporter for what it is genuinely good at — a correct, lock-light registry and a correct Prometheus text rendering — and not for its HTTP listener, which would drag in the whole `hyper` stack to serve one route.

**The endpoint.** A new `ROCKET_MEM_METRICS_ADDR` (default `127.0.0.1:9121` — the port the community `redis_exporter` conventionally uses, so existing scrape configs need no thought; loopback-only by default because the endpoint is unauthenticated). `main.rs` binds it and spawns `metrics::serve_metrics(listener, handle)`. **`serve()` does *not* bind it** — this is deliberate: `serve()` is called by every integration test in the workspace, and a fixed metrics port would make those tests collide with each other and with a developer's running server. The metrics test binds `127.0.0.1:0` itself and calls the same public `serve_metrics`.

```rust
// crates/server/src/metrics.rs
/// Installs the process-wide Prometheus recorder exactly once and returns a handle to it.
/// `metrics::set_global_recorder` may only succeed once per process, and the test suite runs
/// many servers in one process, so this is behind a `OnceLock`: the first caller installs, every
/// later caller gets a clone of the same handle. A failed install (something else got there
/// first) is not an error — the handle from the successful install is still the right one.
pub fn recorder_handle() -> PrometheusHandle;

/// Serves `GET /metrics` (and 404s everything else) over `listener` forever. A hand-rolled
/// ~50-line HTTP/1.1 responder rather than a hyper dependency: one route, no keep-alive, no
/// body parsing, `Content-Type: text/plain; version=0.0.4; charset=utf-8`.
pub async fn serve_metrics(listener: tokio::net::TcpListener, handle: PrometheusHandle);

/// Refreshes the gauges that are *sampled* rather than incremented — memory, key counts,
/// connected replicas, eviction totals. Called by `serve_metrics` immediately before each
/// render, so a scrape always reflects the moment it was taken rather than the last write.
pub fn refresh_sampled_gauges(engine: &Engine, replication: &ReplicationHandle);
```

`PrometheusHandle::run_upkeep()` is called on a 5-second `tokio::time::interval` spawned alongside the listener — the exporter's histograms accumulate per-bucket state that upkeep drains; skipping it is a slow leak in a long-running process.

**The metric set**, fixed here so no plan invents a name:

| Name | Type | Labels | Source |
|---|---|---|---|
| `rocket_mem_commands_total` | counter | `cmd` | incremented once per command in the `dispatch_and_log` wrapper |
| `rocket_mem_command_errors_total` | counter | `cmd` | incremented when the reply is a `Frame::Error` |
| `rocket_mem_command_duration_seconds` | histogram | `cmd` | the wrapper's `Instant::elapsed().as_secs_f64()` |
| `rocket_mem_connected_clients` | gauge | — | `ReplicationHandle::connected_clients`, kept by a Drop guard in `handle_connection` |
| `rocket_mem_connections_total` | counter | — | incremented per accepted connection in `serve` |
| `rocket_mem_memory_used_bytes` | gauge | — | `Engine::memory_used()`, sampled at scrape |
| `rocket_mem_keys` | gauge | — | `Engine::key_counts().0`, sampled at scrape |
| `rocket_mem_keys_with_expiry` | gauge | — | `Engine::key_counts().1`, sampled at scrape |
| `rocket_mem_expired_keys_total` | counter | — | added to by `active_expire_loop` with each cycle's removal count |
| `rocket_mem_evicted_keys_total` | counter | — | `counter!(..).absolute(Engine::eviction_count())`, sampled at scrape |
| `rocket_mem_connected_replicas` | gauge | — | `ReplicaRegistry::len()` (new), sampled at scrape |
| `rocket_mem_replication_last_apply_timestamp_seconds` | gauge | — | set by the follower apply loop after each applied frame; `0` if never |
| `rocket_mem_slowlog_entries_total` | counter | — | incremented when the wrapper records a slow-log entry |

**Histogram buckets are set explicitly** via `PrometheusBuilder::set_buckets(&[0.000_05, 0.000_1, 0.000_25, 0.000_5, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0])` — without it the exporter renders histograms as *summaries with quantiles*, which are not aggregatable across instances and are the wrong shape for the "latency histograms per command" the production plan asks for. The ladder starts at 50µs because a local in-memory `GET` is expected to land in the tens of microseconds; a ladder starting at 5ms would put every single command in the first bucket and measure nothing.

**On "replication lag":** the production plan lists it, and this sprint reports the closest thing that is *true*. There is no byte-offset lag to report — Sprint 5's Global Constraints made every sync a full resync with no offset tracking, so no such number exists anywhere in this codebase. What is honestly available is (a) how many replicas a leader currently has connected, and (b) on a follower, when it last applied anything. `rocket_mem_replication_last_apply_timestamp_seconds` is a Unix timestamp, so `time() - <metric>` in PromQL is "seconds since this follower last applied a write" — meaningful on a continuously-written leader, and meaningless (but not *wrong*) on an idle one. That caveat is recorded in the README, not buried.

**Where instrumentation hooks in, exactly once.** `dispatch_and_log`'s current body (`dispatcher.rs:1042-1167`) is renamed `dispatch_and_log_inner` with an unchanged signature, and `dispatch_and_log` becomes a thin wrapper with the *same* public signature as today, so **not one of its ~36 call sites changes**:

```rust
// crates/server/src/dispatcher.rs
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
    // Both extracted before `frame` is moved into the inner call. `first_key` is one `Bytes`
    // clone (a refcount bump, no data copy) so the slow-log can name the key without cloning the
    // whole frame on every command — see the slow-log decision for why entries carry the key and
    // not the full argument list. (`command_name_upper` + the metrics lines land in
    // `04-prometheus-metrics.md`; `command_key_and_arity` + the `maybe_record` line are added on
    // top by `06-slowlog.md`.)
    let name = command_name_upper(&frame);
    let (first_key, arg_count) = command_key_and_arity(&frame);
    let label = metric_label(&name); // bounded cardinality — see Global Constraints
    let started = std::time::Instant::now();

    let reply = dispatch_and_log_inner(engine, aof, replication, frame, protocol, client_id);

    let elapsed = started.elapsed();
    metrics::counter!("rocket_mem_commands_total", "cmd" => label.clone()).increment(1);
    metrics::histogram!("rocket_mem_command_duration_seconds", "cmd" => label.clone())
        .record(elapsed.as_secs_f64());
    if matches!(reply, Frame::Error(_)) {
        metrics::counter!("rocket_mem_command_errors_total", "cmd" => label).increment(1);
    }
    replication
        .slowlog
        .maybe_record(&name, first_key, arg_count, elapsed);
    reply
}
```

This is the single reason the split exists: `dispatch_and_log_inner` has seven early returns (`-MOVED`, `-CROSSSLOT`, `-READONLY`, `SAVE`, `REPLICAOF`, `CLUSTER`, `INFO`/`HELLO`/`SLOWLOG`), and instrumenting each one would guarantee that a future eighth is missed. **`dispatch` itself is not instrumented at all** — it is the function AOF replay and the follower apply loop call, and counting a 5,000-frame boot-time replay as 5,000 client commands would make every dashboard lie about traffic. Commands a follower applies from its leader are therefore invisible to `rocket_mem_commands_total` by design, and that is stated in the README.

`metric_label` bounds cardinality against the sorted `KNOWN_COMMANDS: &[&str]` list (the 84 names `dispatch` and the interceptions handle) that the routing table introduces one plan earlier: a known name becomes its lowercase form, anything else becomes the literal `"other"`. That list must be kept in step with `dispatch`'s match arms from now on — a missing name is not a compile error, it silently means "this command has no keys", i.e. never slot-routed.

## Decision: `INFO` and `HELLO` move out of `dispatch` into interceptions, and report real state

Sprint 5's spec closed with a knowingly-wrong pair: `INFO` returns only a two-line `# Server` section (`dispatcher.rs:829-832`) and `hello_reply` hardcodes `role: master` (`dispatcher.rs:913-915`), both inaccurate on a follower. Both are wrong for the same structural reason — they live in `dispatch`, which has no access to server-level state — and both are fixed the same way.

**Decision:** the `INFO` arm and the `HELLO` arm are *moved* out of `dispatch`'s match into `handle_info` / `handle_hello` interceptions in `dispatch_and_log_inner`, joining `SAVE`/`REPLICAOF`/`CLUSTER`. This establishes a clean rule worth stating once: **`dispatch` answers questions about the keyspace; `dispatch_and_log` answers questions about the server.** `hello_reply` gains a `role: &'static str` parameter. The five existing `HELLO` unit tests (`dispatcher.rs:2087, 2122, 2134, 2169, 3431`) and the one `INFO` test (`dispatcher.rs:1609`) are migrated to call `dispatch_and_log` with `&test_aof().1` and a `ReplicationHandle::default()`; `handle_hello` still takes `&mut Protocol`, so `connection.rs:105`'s `framed.codec_mut().protocol = protocol` continues to observe `HELLO 3`'s switch exactly as before.

**`INFO [section]`** accepts an optional, case-insensitive section name (`server`, `clients`, `memory`, `persistence`, `stats`, `replication`, `cluster`, `keyspace`), plus `all`/`default`/no-argument meaning every section. Output is CRLF-terminated `key:value` lines with `# Section` headers, the format real Redis tooling parses. Every field below is something this codebase actually tracks — **nothing is invented**:

```
# Server
redis_version:rocket-mem-0.1.2        <- unchanged line, kept for tooling that version-gates
rocket_mem_version:0.1.2
redis_mode:standalone|cluster          <- "cluster" iff cluster mode is on
os:linux                               <- std::env::consts::OS
arch_bits:64                           <- usize::BITS
process_id:12345                       <- std::process::id()
uptime_in_seconds:41                   <- ReplicationHandle::started_at.elapsed()
uptime_in_days:0

# Clients
connected_clients:3                    <- the Drop-guarded counter

# Memory
used_memory:81920                      <- Engine::memory_used()
used_memory_human:80.00K
maxmemory:0                            <- Engine::maxmemory() (new getter), 0 when unset
maxmemory_policy:allkeys-lru           <- what Sprint 4's sampling evictor actually does

# Persistence
aof_enabled:1
aof_fsync_policy:everysec|always|no    <- AofWriter::policy()
rdb_last_save_time:1756512000          <- set by handle_save; 0 if SAVE never ran this process
rdb_bgsave_in_progress:0               <- always 0: SAVE is synchronous (Sprint 5, no BGSAVE)

# Stats
total_connections_received:12
total_commands_processed:3401
expired_keys:57                        <- active expiry only; see the caveat below
evicted_keys:0                         <- Engine::eviction_count()

# Replication
role:master|slave                      <- ReplicationHandle::is_replica — the Sprint 5 fix
connected_slaves:2                     <- master only: ReplicaRegistry::len()
master_host:127.0.0.1                  <- slave only
master_port:6379                       <- slave only
master_link_status:up|down             <- slave only

# Cluster
cluster_enabled:0|1

# Keyspace
db0:keys=42,expires=7,avg_ttl=0        <- omitted entirely when keys=0, as real Redis does
```

**`role:slave`, not `role:replica`.** Real Redis still emits `slave` in `INFO replication` for backward compatibility, and `redis-rs`/`redis-py`/`ioredis` all parse for it. Matching the wire is the whole point; the codebase's own prose keeps saying "follower."

**Honest gaps, stated rather than faked:** `keyspace_hits`/`keyspace_misses` are omitted because nothing counts them, and adding a counter to every read path is Sprint-7 work, not a free `INFO` field. `expired_keys` counts only *actively* expired keys (the `active_expire_loop` sweep, `connection.rs:44`); passive expiry — a read finding a key already dead, `shard.rs:37-58` — removes keys without counting them, and threading a counter into `Shard` for it would touch the hottest read path in the project. `tcp_port` is omitted because the dispatcher never learns the listen address. `maxmemory` reports `0` in the shipped binary because `main.rs` builds its `Engine` through `aof::recover`, which calls `Engine::new()`, not `with_maxmemory` — the getter is real, the binary simply has no env var to set it yet, and that gap is the README's to record, not `INFO`'s to paper over.

**New state this requires**, all on `ReplicationHandle` (except the engine getters). The metrics-feeding half — `connected_clients`, `total_connections`, `total_commands`, `expired_keys`, `last_apply_unix`, `ReplicaRegistry::len`, and `Engine::key_counts` — is added by `04-prometheus-metrics.md`, which is what maintains them; the `INFO`-only half — `started_at`, `last_save_unix`, `master_addr`, `link_up`, and `Engine::maxmemory` — is added by `05-info-and-hello-overhaul.md`:

```rust
// crates/server/src/replication.rs — added fields
started_at: std::time::Instant,                       // set in `new`
connected_clients: AtomicUsize,                       // Drop-guarded by handle_connection
total_connections: AtomicU64,                         // bumped in serve's accept loop
total_commands: AtomicU64,                            // bumped in the dispatch_and_log wrapper
expired_keys: AtomicU64,                              // added to by active_expire_loop
last_save_unix: AtomicI64,                            // set by handle_save on success
master_addr: Mutex<Option<String>>,                   // set by start_replicating, cleared by stop
link_up: Arc<AtomicBool>,                             // set by the apply loop; Arc so the
                                                      // spawned 'static task can own a handle
last_apply_unix: Arc<AtomicI64>,                      // ditto; feeds the replication-lag gauge

// crates/engine/src/engine.rs — new getters, both thin facades over Store as CLAUDE.md requires
pub fn key_counts(&self) -> (usize, usize);   // (total unexpired, of which have an expiry)
pub fn maxmemory(&self) -> Option<usize>;
// crates/engine/src/store.rs / shard.rs
pub fn key_counts(&self) -> (usize, usize);   // Store: sums the 16 shards
pub fn counts(&self) -> (usize, usize);       // Shard: one read lock, one pass, skipping expired
// crates/server/src/replication.rs
impl ReplicaRegistry { pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool; }
```

`ReplicaRegistry::is_empty` exists only because `clippy::len_without_is_empty` is a warning and `-D warnings` is the CI gate.

## Decision: the slow log is a fixed 128-entry ring buffer holding the command name and its first key

**Decision:** a `SlowLog` struct in a new `crates/server/src/slowlog.rs`, owned by `ReplicationHandle` (constructed by `new`/`Default`, so no call site changes), recording any command whose wrapper-measured duration meets or exceeds a threshold:

```rust
// crates/server/src/slowlog.rs
/// Fixed capacity; the oldest entry is dropped when full. A `VecDeque` behind a plain
/// `std::sync::Mutex` — every access is a push/drain measured in nanoseconds and never held
/// across an `.await`, matching `ReplicaRegistry`'s choice for the same reason.
pub const SLOWLOG_CAPACITY: usize = 128;

pub struct SlowLogEntry {
    pub id: u64,               // monotonic, never reset by RESET (real Redis behaves this way)
    pub unix_time_secs: i64,
    pub duration_micros: i64,
    pub command: String,       // uppercase command name
    pub key: Option<Bytes>,    // the command's first argument, when it had one
    pub arg_count: usize,      // total arguments after the command name
}

pub struct SlowLog { /* entries: Mutex<VecDeque<SlowLogEntry>>, next_id: AtomicU64, threshold: Duration */ }

impl SlowLog {
    pub fn with_threshold(threshold: std::time::Duration) -> Self;
    /// No-op when `elapsed < threshold`, which is the overwhelmingly common case — this is the
    /// only slow-log work on the hot path.
    pub fn maybe_record(&self, command: &str, key: Option<Bytes>, arg_count: usize, elapsed: std::time::Duration);
    pub fn get(&self, count: usize) -> Vec<SlowLogEntry>;  // newest first, like real Redis
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;                        // clippy::len_without_is_empty
    pub fn reset(&self);
}
```

**Threshold:** `ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS`, default `10000` (10ms), read once in `main.rs`. A value of `0` means **disabled** here, deliberately diverging from real Redis, where `slowlog-log-slower-than 0` means "log everything": filling a 128-entry ring from every command would evict itself faster than an operator could read it, and the person who wants per-command timings already has `rocket_mem_command_duration_seconds`. The divergence is one line in the README's known-limits list. `Default`/`new` use the 10ms default so tests get sane behavior without env fiddling.

**Why entries carry the first key and an argument count, not the full argument list.** `dispatch` consumes the `Frame` (`frame_to_args` moves each `Bulk`'s `Bytes` out rather than cloning), so anything the slow log wants must be captured *before* the call — i.e. on every command, for the benefit of the rare slow one. Cloning one `Bytes` is a refcount bump; cloning the whole frame is a `Vec` allocation plus N bumps, on every command, on the hot path plan 07 exists to shrink. The first argument is the key for ~70 of the 84 commands, which is the field an operator actually needs. `SLOWLOG GET` renders the argument array as `[command, key]` followed by real Redis's own truncation marker `... (N more arguments)` when `arg_count > 1` — a shape real Redis itself emits (it truncates at 32 arguments), so tooling parses it without special-casing.

**Command surface — three subcommands, intercepted in `dispatch_and_log` like `CLUSTER`:**

- `SLOWLOG GET [count]` → array of entries, newest first, `count` defaulting to 10 and clamped to the buffer size; each entry is a 4-element array `[id, unix_time_secs, duration_micros, args_array]`.
- `SLOWLOG LEN` → integer.
- `SLOWLOG RESET` → `+OK`.
- Anything else → `ERR unknown SLOWLOG subcommand '<sub>'`.

Real Redis ≥4.0 emits **6** fields per entry (adding client address and client name). This implementation emits the original **4**, because `dispatch_and_log` never learns the peer address — `handle_connection` has the `TcpStream` but passes only `client_id` down — and threading a `SocketAddr` through six call layers for two cosmetic fields is not worth it this sprint. Clients that index fields positionally read the same first four either way; this is recorded in the README's known-limits list. `SLOWLOG HELP` is out of scope for the same reason `CLUSTER SLOTS` is: nothing in this repo consumes it.

## Decision: the benchmark is a committed script plus a committed report; the profiling pass fixes one named bottleneck

The P1 benchmark and P1 profiling items are process deliverables, not runtime features, and are structured so a fresh engineer can reproduce them.

**Benchmark** (`scripts/benchmark.sh`, committed): checks that `redis-server` and `redis-benchmark` are on `PATH` and exits with a clear message naming the missing binary if not (this must never fail as a mysterious empty report); starts a real `redis-server` on port 7777 and a release-build `rocket-mem` on port 7778, each with its own temp directory; runs the identical `redis-benchmark` matrix against both — `-t set,get -n 100000 -c 50` at payload sizes `-d 3` and `-d 1024`, each once without pipelining and once with `-P 16`; and writes the raw output beside a summary table into `docs/benchmarks/2026-08-30-redis-benchmark.md`. **The report is committed with real numbers from a real run**, and includes a "where we are slower and why" section — a DoD requirement in its own words, and the part a reader will actually judge. It is not a CI test: `redis-server` is not installed on the CI runner, and throughput numbers from a shared CI machine would be noise gated as if it were signal.

**Profiling** (`cargo flamegraph`, dev-only, installed by the engineer running it — not added to `Cargo.toml`, since `cargo-flamegraph` is a binary, not a dependency): profile the release binary under the same `redis-benchmark` load, commit the SVG and the reading of it into `docs/benchmarks/2026-08-30-flamegraph-notes.md`.

**The one bottleneck this sprint fixes is named in advance**, because it is visible by inspection and does not depend on what the flamegraph happens to show: **every command allocates its uppercased name on the heap two to four times.** `dispatch` does `String::from_utf8_lossy(&args[0]).to_ascii_uppercase()` (`dispatcher.rs:67`), `extract_write_command_name` does it again (`dispatcher.rs:933`), and this sprint's wrapper and `cluster_redirect` want it a third and fourth time. Command names are at most 12 bytes; the fix is a stack-allocated `CommandName` in `dispatcher.rs` — a `[u8; 32]` plus a length, with `as_str()` — returned by a single `upper_name(&[u8]) -> Option<CommandName>` used by all four sites, with the `None` case (a name longer than 32 bytes, therefore necessarily unknown) falling back to today's `format!`-based unknown-command error on that cold path alone. No public signature changes. The plan requires before/after `redis-benchmark` numbers in the report; if the measured difference is inside the noise, that result is recorded honestly rather than the change being reverted or oversold.

**The lock-contention rabbit hole stays closed**, per the sprint plan's own risk-table mitigation: if the flamegraph shows shard-lock contention, it is *recorded in the notes and in `../../design/sharding-decision.md`* as the input that doc has been waiting for since Sprint 1 — it is not acted on this sprint.

## Decision: cluster tests run in-process, three nodes, following `-MOVED` explicitly

**Decision:** `crates/server/tests/cluster.rs` follows the shape `tests/replication.rs` already established (`replication.rs:10-38`): three fully independent nodes in one process, each with its own `Engine`, `AofWriter` over its own `tempfile::tempdir()`, and `ReplicationHandle`, spawned via `serve()` on `127.0.0.1:0` listeners. Sharing any of the three between "nodes" would make a test pass for the wrong reason.

**The one thing this shape has to solve** is that the config file's `addr` fields must match the ports the OS actually handed out, which are only known *after* binding. The harness therefore binds all three listeners first, builds the config text with `format!` from the three real addresses, and calls `ClusterConfig::parse(&text, node_id)` — the same parser `main.rs` uses — once per node before spawning any of them.

**The DoD's "cluster-aware client finds them via `MOVED`"** is tested with the `redis` crate's ordinary (non-cluster) client plus an explicit redirect follow: send the command to a node chosen to be the *wrong* one, assert the error is a `MOVED` naming the expected slot and the expected node's address, reconnect to that address, and assert the command succeeds. This is a stronger test than handing the work to a third-party cluster client — it asserts the exact slot number and target address, not merely that some client eventually found the key — and it avoids depending on `CLUSTER SLOTS`, which this sprint does not implement.

---

## Sequencing

Plans depend on each other in this order (living in `../plans/2026-08-30-sprint-6-plans/`):

1. `01-hash-slots-and-cluster-config.md` — `crates/server/src/cluster.rs`: `crc16`, `hash_tag`, `key_slot`, `SLOT_COUNT`, `ClusterNode`/`ClusterConfig` with `parse`/`load`/`owns`/`owner_of`/`myself`/`nodes`, the `ReplicationHandle::with_cluster`/`cluster()` plumbing, and `main.rs`'s two new env vars. Server-crate only, no dispatcher changes — independent of everything else this sprint.
2. `02-cluster-commands-and-moved.md` (depends on 1) — `handle_cluster` (`KEYSLOT`/`MYID`/`INFO`/`SHARDS`/`NODES`), the `KeySpec`/`command_keys` table, `cluster_redirect`, and its placement at the top of `dispatch_and_log` ahead of the `-READONLY` gate.
3. `03-cluster-integration-tests.md` (depends on 1 and 2) — `crates/server/tests/cluster.rs`: the 3-node harness and the DoD test that a wrong-shard client gets `-MOVED` and the right shard serves the key, plus `CROSSSLOT` and hash-tag coverage over real TCP.
4. `04-prometheus-metrics.md` (depends on 2 for `KNOWN_COMMANDS`, and sequenced after it anyway so no two plans edit `dispatch_and_log` at once; otherwise independent of the cluster work) — the `metrics`/`metrics-exporter-prometheus` dependencies, `metrics.rs` (recorder, `/metrics` listener, sampled gauges), the `dispatch_and_log`/`dispatch_and_log_inner` split with `command_name_upper`/`metric_label`, the connected-clients Drop guard, `Engine::key_counts` (plus its `Store`/`Shard` halves) and `ReplicaRegistry::len`, the connection/command/expiry/last-apply counters, `ROCKET_MEM_METRICS_ADDR`, and the end-to-end scrape test.
5. `05-info-and-hello-overhaul.md` (depends on 1 for `cluster_enabled` and on 4 for the counters `INFO` reports) — the `INFO`-only state (`started_at`, `last_save_unix`, `master_addr`, `link_up`) and `Engine::maxmemory`, the eight `INFO` sections, moving `INFO`/`HELLO` into interceptions with a role- and mode-aware `hello_reply`, and migrating their six existing unit tests.
6. `06-slowlog.md` (depends on 4 for the wrapper's timing hook) — `slowlog.rs`, the `ReplicationHandle` field, `ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS`, and the `SLOWLOG GET`/`LEN`/`RESET` interception.
7. `07-benchmark-and-flamegraph.md` (depends on 1–6, since the thing being benchmarked is the finished server) — `scripts/benchmark.sh`, the committed benchmark report, the flamegraph pass and its notes, and the `CommandName` allocation fix with before/after numbers.
8. `08-sprint-6-close.md` (depends on 1–7) — README (new commands, four new env vars, the cluster section, the metric list, and the known limits: no gossip/failover/resharding/forwarding, no `CLUSTER SLOTS`, 4-field slow-log entries, active-only `expired_keys`, no true replication-offset lag, the `ReplicationHandle` naming debt), the `../../design/sharding-decision.md` update answering its own Sprint-6 question, full workspace verification, and the Sprint 6 status/DoD tick in `../../rocket-mem-sprint-plan.md`.

## Definition of done for the sprint

Matches Sprint 6 in `../../rocket-mem-sprint-plan.md`:
- [ ] 3-shard cluster test passes: keys route by hash slot, cluster-aware client finds them via `MOVED`
- [ ] Benchmark report committed to the repo, including where you're slower than real Redis and why
- [ ] Prometheus metrics visible and scraping correctly
- [ ] `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` clean, `cargo fmt --all -- --check` clean (carried forward from Sprints 1–5, not re-stated per item below)
