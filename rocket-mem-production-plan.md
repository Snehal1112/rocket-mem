# rocket-mem: A Production-Grade RESP-Compatible Redis Server in Rust
### A 16-week execution plan

**Approach:** RESP (REdis Serialization Protocol) compatible from day one, so existing Redis clients in any language work against your server unmodified. The storage engine is kept protocol-agnostic so a custom extended protocol can be layered on top later (Phase 4) without a rewrite.

**Architecture (3 layers, established Week 1, respected throughout):**
```
┌─────────────────────────────────────────┐
│  Protocol Layer (RESP2/RESP3, later:     │
│  your own binary protocol)               │
├─────────────────────────────────────────┤
│  Command Dispatcher (maps commands →     │
│  engine calls, arg validation)           │
├─────────────────────────────────────────┤
│  Storage Engine (data structures,        │
│  persistence, expiry, protocol-agnostic) │
└─────────────────────────────────────────┘
```

**Core crates you'll lean on throughout:** `tokio` (async runtime/networking), `bytes` (zero-copy buffers), `dashmap` or custom sharded locks (concurrent maps), `tracing` (structured logging), `criterion` (benchmarking), `serde`/`bincode` (persistence serialization).

---

## Architecture decision record

**Chosen: layered, sharded, lock-based, task-per-connection.** One Tokio task per client connection; the keyspace is split into N shards (e.g. 16), each behind its own `RwLock`; any task can read/write any key by acquiring that key's shard lock. This is decided in Week 1 and is load-bearing for everything after it — retrofitting sharding later is painful, so it's built in from day one even though early command counts don't need it yet.

**Alternatives considered, and why they're not the starting point:**

| Alternative | Why not now | Revisit when |
|---|---|---|
| Single-threaded event loop, no locks (real Redis's own design) | Simplest correctness model, but sidesteps Rust's concurrency story almost entirely — caps throughput at one core and wastes the reason for building this in Rust | Not planned — this is a deliberate divergence from Redis's own architecture, not a gap |
| Thread-per-core, shared-nothing (Scylla/Seastar style) | Better scaling ceiling, but means building a custom request-routing runtime instead of leaning on Tokio's task model — a multi-month detour on its own | Only if Phase 3 benchmarking shows the sharded-lock design hits a hard scaling wall Phase 4 can't fix |
| Lock-free shards (`dashmap`-style) | Structurally identical to the chosen design (same shards) — but lock-free correctness bugs are brutal to debug, and that's a fight to have after the engine already works, not during | **Week 12** — if benchmarking shows lock contention is the actual bottleneck, swap each shard's internal structure; no other layer changes |
| Proxy-based clustering (Codis/Twemproxy style) | This is an operational topology decision (where does routing logic live: in the node, or in a separate tier?), not an alternative to the node's own internal architecture — it's a different question | **Week 11** — the cluster design there is closer to this than to fully embedded routing; worth a deliberate compare at that point |

This keeps one clear escape hatch open (shard internals, Week 12) without committing to a full-runtime rewrite (thread-per-core) or giving up Rust's concurrency model entirely (single-threaded).

---

## Phase 1 — Foundation (Weeks 1–4)
Goal: a Tokio TCP server that speaks real RESP and handles basic string/hash/list/set commands. By end of Phase 1, `redis-cli` and real client libraries (redis-py, ioredis, go-redis) can connect and run basic commands against your server.

### Week 1 — Project Setup & Storage Engine Core
- **Sub-tasks:**
  - Scaffold a Cargo workspace with separate crates: `engine` (storage), `protocol` (RESP), `server` (binary + networking), `common` (shared types/errors)
  - Define the core `Value` enum: `String(Bytes)`, `List(VecDeque<Bytes>)`, `Hash(HashMap<Bytes,Bytes>)`, `Set(HashSet<Bytes>)`, `SortedSet(...)` (stub for now)
  - Design the keyspace container: decide between a single `RwLock<HashMap<Bytes, Value>>` (simple, contention-prone) vs. a **sharded map** (e.g., 16 shards, each its own `RwLock` or `Mutex`, key hashed to a shard) — build the sharded version now, since retrofitting sharding later is painful
  - Implement `get`, `set`, `del`, `exists`, `keys` (pattern-free) against the engine directly (no networking yet), with unit tests
  - Set up `tracing` for structured logs and `thiserror`/`anyhow` for error handling conventions across the workspace
  - Set up CI skeleton: `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` on every push
- **Crates:** `tokio`, `bytes`, `parking_lot` (faster locks than std), `tracing`, `tracing-subscriber`, `thiserror`
- **Example tests to write:**
  ```rust
  #[test]
  fn set_then_get_returns_value() {
      let engine = Engine::new();
      engine.set(b"foo", Value::String(b"bar".into()));
      assert_eq!(engine.get(b"foo"), Some(Value::String(b"bar".into())));
  }

  #[test]
  fn keys_distribute_reasonably_evenly_across_shards() {
      let engine = Engine::new();
      for i in 0..10_000 { engine.set(format!("key{i}").as_bytes(), Value::String(b"v".into())); }
      assert!(engine.shard_load_variance() < 0.1);
  }
  ```
- **Deliverable / test:** `cargo test` passes for get/set/del/exists against the in-memory engine; a design doc (1-2 pages) recording the sharding decision and why

### Week 2 — Core Data Types & Command Semantics
- **Sub-tasks:**
  - Implement String commands: `SET` (with `EX`/`PX`/`NX`/`XX` flags), `GET`, `APPEND`, `STRLEN`, `INCR`/`DECR`/`INCRBY`
  - Implement Hash commands: `HSET`, `HGET`, `HDEL`, `HGETALL`, `HEXISTS`, `HLEN`
  - Implement List commands: `LPUSH`/`RPUSH`, `LPOP`/`RPOP`, `LRANGE`, `LLEN`
  - Implement Set commands: `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`
  - Handle **type errors correctly** — e.g., calling `HGET` on a key holding a String must return Redis's exact `WRONGTYPE` error, not panic
  - Write a test matrix: for every command, test wrong-type key, missing key, empty value edge cases
- **Crates:** no new ones — deepen what Week 1 set up
- **Example tests to write:**
  ```rust
  #[test]
  fn hget_on_string_key_returns_wrongtype_error() {
      let engine = Engine::new();
      dispatch(&engine, "SET", &[b"k", b"v"]).unwrap();
      let err = dispatch(&engine, "HGET", &[b"k", b"field"]).unwrap_err();
      assert_eq!(err, RespError::WrongType);
  }

  #[test]
  fn incr_on_missing_key_initializes_to_one() {
      let engine = Engine::new();
      assert_eq!(dispatch(&engine, "INCR", &[b"counter"]).unwrap(), RespValue::Integer(1));
  }
  ```
- **Deliverable / test:** ~25 commands implemented with full unit test coverage including error paths; a table in the repo README tracking command coverage vs. real Redis

### Week 3 — RESP Protocol & TCP Networking
- **Sub-tasks:**
  - Implement a RESP2 parser/serializer from scratch (learning value is here — resist pulling in a full RESP crate): Simple Strings (`+`), Errors (`-`), Integers (`:`), Bulk Strings (`$`), Arrays (`*`)
  - Handle partial reads correctly — a command can arrive split across multiple TCP packets; use `tokio_util::codec::Decoder` to buffer until a full frame is available
  - Build the command dispatcher: parse incoming RESP array → match on command name (case-insensitive) → validate arg count/types → call engine → serialize response
  - Stand up the Tokio TCP listener: accept connections, spawn a task per connection, each task owns a decoder/encoder loop
  - Wire real `redis-cli` against it locally and manually verify `SET foo bar`, `GET foo`, `HSET`, etc.
- **Crates:** `tokio` (`TcpListener`, `tokio::spawn`), `tokio-util` (`Decoder`/`Encoder`/`Framed`), `bytes`
- **Example tests to write:**
  ```rust
  #[test]
  fn decoder_parses_full_resp_array_of_bulk_strings() {
      let mut buf: BytesMut = b"*3\r\n$3\r\nSET\r\n$3\r\nfoo\r\n$3\r\nbar\r\n"[..].into();
      let frame = RespDecoder::default().decode(&mut buf).unwrap().unwrap();
      assert_eq!(frame, Frame::Array(vec![bulk("SET"), bulk("foo"), bulk("bar")]));
  }

  #[tokio::test]
  async fn decoder_reassembles_a_command_split_across_two_tcp_writes() {
      // write "*3\r\n$3\r\nSET\r\n" then, after a delay, "$3\r\nfoo\r\n$3\r\nbar\r\n"
      // assert exactly one complete SET command is dispatched, not a parse error
  }
  ```
- **Deliverable / test:** `redis-cli -p <port>` connects and runs all Week 2 commands correctly, including against split/pipelined input; integration test that opens a raw TCP socket and sends malformed RESP to confirm graceful error responses (not a crash)

### Week 4 — Wiring, Client Compatibility & Test Harness
- **Sub-tasks:**
  - Implement `PING`, `ECHO`, `SELECT` (stub, single DB is fine for now), `COMMAND` (so clients that probe capabilities don't choke), `INFO` (minimal stub)
  - Run real client libraries against it: redis-py, ioredis (Node), go-redis — fix whatever handshake/edge-case issues surface (many clients send `HELLO` for RESP3 negotiation on connect — decide now whether to support RESP3 or force RESP2 and reject `HELLO`)
  - Build an integration test harness: spin up your server as a subprocess in tests, run a suite of commands via a real Redis client crate (`redis-rs`), assert responses
  - Load-test lightly with `redis-benchmark` (the real Redis CLI tool) just to confirm no immediate panics or deadlocks under concurrent load
  - Write up Phase 1 retro: what surprised you, what's technical debt to revisit in Phase 3
- **Crates:** `redis` (the `redis-rs` client crate, as a **test dependency only**, to drive integration tests)
- **Example tests to write:**
  ```rust
  #[tokio::test]
  async fn redis_rs_client_can_set_and_get_over_real_tcp() {
      let server = spawn_test_server().await;
      let mut con = redis::Client::open(server.url()).unwrap().get_connection().unwrap();
      let _: () = con.set("foo", "bar").unwrap();
      assert_eq!(con.get::<_, String>("foo").unwrap(), "bar");
  }
  ```
  Plus a manual checklist: run the same SET/GET/HSET smoke sequence via `redis-py` and `ioredis`, record results in the test harness notes.
- **Deliverable / test:** three different language client libraries connect and run a basic workload without errors; integration test suite runs in CI against a real spawned server instance

---

## Phase 2 — Depth (Weeks 5–8)
Goal: full-ish command coverage, correct expiry/eviction, and durability so data survives a restart.

### Week 5 — Expanding the Command Set: Strings & Keys
- **Sub-tasks:**
  - `GETSET`, `SETRANGE`, `GETRANGE`, `MSET`/`MGET`, `MSETNX`
  - `EXPIRE`, `PEXPIRE`, `TTL`, `PTTL`, `PERSIST`, `EXPIREAT`
  - `RENAME`, `RENAMENX`, `TYPE`, `RANDOMKEY`
  - `KEYS` with real glob pattern matching (implement or use a small glob-matching crate), `SCAN` with cursor-based iteration (important: `SCAN` must not block the whole keyspace like `KEYS` can — this is a good exercise in cursor design over your sharded map)
- **Crates:** consider `glob-match` or hand-roll simple Redis-style pattern matching (`*`, `?`, `[abc]`)
- **Example tests to write:**
  ```rust
  #[test]
  fn keys_glob_pattern_matches_prefix_wildcard() {
      assert!(glob_match("user:*", b"user:123"));
      assert!(!glob_match("user:*", b"session:123"));
  }

  #[tokio::test]
  async fn scan_visits_every_pre_existing_key_exactly_once_under_concurrent_writes() {
      // insert 5,000 keys, spawn a background writer inserting more concurrently,
      // drive SCAN to cursor 0, assert every one of the original 5,000 keys appeared exactly once
  }
  ```
- **Deliverable / test:** `SCAN` correctly iterates the full keyspace across multiple calls without missing or double-counting keys under concurrent writes (write a stress test for this specifically)

### Week 6 — Expanding the Command Set: Collections
- **Sub-tasks:**
  - Lists: `LINSERT`, `LSET`, `LREM`, `LTRIM`, `LINDEX`
  - Hashes: `HINCRBY`, `HKEYS`, `HVALS`, `HMGET`, `HSETNX`
  - Sets: `SINTER`, `SUNION`, `SDIFF` (+ `STORE` variants), `SPOP`, `SRANDMEMBER`
  - Sorted Sets (new data structure — this is the meaty one): design using a skip list or a `BTreeMap<(f64 ordering-safe wrapper, Bytes), ()>` plus a `HashMap<Bytes, f64>` for O(1) score lookup; implement `ZADD`, `ZSCORE`, `ZRANGE`, `ZRANK`, `ZINCRBY`, `ZREM`
  - Note: `f64` doesn't implement `Ord` — you'll need a wrapper type (`OrderedFloat` crate or hand-rolled) for the BTreeMap key
- **Crates:** `ordered-float` (for sorted set scores)
- **Example tests to write:**
  ```rust
  #[test]
  fn zadd_then_zrange_returns_members_ordered_by_score() {
      let engine = Engine::new();
      dispatch(&engine, "ZADD", &[b"lb", b"5", b"alice", b"2", b"bob"]).unwrap();
      let result = dispatch(&engine, "ZRANGE", &[b"lb", b"0", b"-1"]).unwrap();
      assert_eq!(result, resp_array(&["bob", "alice"]));
  }

  #[test]
  fn sinter_returns_only_members_present_in_all_sets() {
      let engine = Engine::new();
      dispatch(&engine, "SADD", &[b"a", b"x", b"y"]).unwrap();
      dispatch(&engine, "SADD", &[b"b", b"y", b"z"]).unwrap();
      assert_eq!(dispatch(&engine, "SINTER", &[b"a", b"b"]).unwrap(), resp_set(&["y"]));
  }
  ```
- **Deliverable / test:** sorted set operations benchmark reasonably against real Redis for 10k/100k member sets (rough parity, not exact); full command coverage table updated

### Week 7 — Expiry, Eviction & Memory Management
- **Sub-tasks:**
  - Implement **active expiry**: a background Tokio task that periodically samples keys with TTLs and removes expired ones (mirror Redis's probabilistic sampling approach rather than scanning everything every cycle)
  - Implement **passive expiry**: check TTL on every read/write access to a key, lazily remove if expired, even if the active sweep hasn't gotten to it yet
  - Add memory usage tracking (approximate — track allocated bytes per shard) and implement at least one eviction policy (`allkeys-lru` is the natural first one — you'll need an access-order tracking structure per shard)
  - Add `MAXMEMORY` config and `OBJECT ENCODING`/`MEMORY USAGE` stub commands for observability
  - Stress test: fill memory close to a configured limit, confirm eviction kicks in and doesn't deadlock with concurrent writes
- **Crates:** none new necessarily; if implementing LRU cleanly, `linked-hash-map` or hand-rolled intrusive list per shard
- **Example tests to write:**
  ```rust
  #[tokio::test]
  async fn passive_expiry_removes_key_on_read_after_ttl_elapses() {
      let engine = Engine::new();
      dispatch(&engine, "SET", &[b"k", b"v", b"PX", b"10"]).unwrap();
      tokio::time::sleep(Duration::from_millis(20)).await;
      assert_eq!(dispatch(&engine, "GET", &[b"k"]).unwrap(), RespValue::Null);
  }

  #[test]
  fn lru_eviction_keeps_memory_under_configured_ceiling() {
      let engine = Engine::with_maxmemory(1_000_000);
      for i in 0..100_000 { dispatch(&engine, "SET", &[fmt_key(i), b"x".repeat(100)]).unwrap(); }
      assert!(engine.memory_used() <= 1_000_000);
      assert!(engine.eviction_count() > 0);
  }
  ```
- **Deliverable / test:** TTL correctness test suite (set with short TTL, confirm both active and passive expiry paths independently catch it); eviction stress test under a low memory ceiling

### Week 8 — Persistence: Append-Only File (AOF)
- **Sub-tasks:**
  - Design AOF format: every write command, once applied to the in-memory engine, gets appended to a log file in RESP format (this doubles as documentation — the log IS a replayable RESP command stream)
  - Implement the writer: buffered, periodic `fsync` (configurable: always/every-second/never — mirror Redis's `appendfsync` policies since they represent a real durability/performance tradeoff worth understanding)
  - Implement AOF replay on startup: read the file, replay every command through the dispatcher to rebuild state
  - Handle the crash-mid-write case: a truncated/corrupt last line in the AOF shouldn't crash startup — detect and truncate to the last valid command
  - Test: kill `-9` the server mid-write-load, restart, confirm data matches expectations (this is your first real durability test — treat it seriously)
- **Crates:** `tokio::fs` for async file I/O, or a dedicated writer thread if async file I/O proves awkward under sustained load (worth benchmarking both)
- **Example tests to write:**
  ```rust
  #[tokio::test]
  async fn kill_dash_nine_then_restart_preserves_all_written_keys() {
      let dir = tmp_dir();
      let mut server = spawn_server_with_aof(&dir).await;
      for i in 0..1000 { server.client().set(format!("k{i}"), "v").await.unwrap(); }
      server.kill_dash_nine();
      let server = restart_server_with_aof(&dir).await;
      for i in 0..1000 { assert_eq!(server.client().get(format!("k{i}")).await.unwrap(), "v"); }
  }

  #[test]
  fn aof_replay_truncates_corrupt_trailing_line_without_panicking() {
      let dir = tmp_dir();
      write_aof_with_corrupt_last_line(&dir);
      let engine = Engine::replay_from_aof(&dir).expect("should recover, not panic");
      assert!(engine.key_count() > 0);
  }
  ```
- **Deliverable / test:** kill-and-recover test in CI: start server, load N keys, `kill -9`, restart, verify all N keys present; corrupt-tail recovery test

---

## Phase 3 — Production Hardening (Weeks 9–12)
Goal: the properties that make this trustworthy for real workloads — fast recovery, replication, horizontal scale, and proof via benchmarking that it holds up.

### Week 9 — Persistence: Snapshotting (RDB-style) & Faster Recovery
- **Sub-tasks:**
  - Implement point-in-time snapshotting: serialize the full keyspace to disk (via `bincode` or a custom binary format) — this is your fast-recovery path, since replaying a huge AOF from scratch on every restart doesn't scale
  - Implement `BGSAVE`-equivalent: snapshot without blocking client traffic (fork-based copy-on-write like real Redis isn't idiomatic in Rust/Tokio — instead, consider a read-lock snapshot approach per shard, or a lightweight fork via `nix` crate if you want to go that deep)
  - Implement hybrid recovery on startup: load the latest snapshot, then replay only the AOF entries written *after* that snapshot
  - Add snapshot scheduling (time-based and write-count-based triggers, like Redis's `save` config)
- **Crates:** `bincode` or `rkyv` (for snapshot serialization — `rkyv` is worth exploring for zero-copy deserialization, good Rust learning detour), optionally `nix` if exploring fork-based snapshotting
- **Example tests to write:**
  ```rust
  #[test]
  fn snapshot_then_reload_reproduces_identical_keyspace() {
      let engine = engine_with_n_keys(50_000);
      let bytes = engine.snapshot();
      let restored = Engine::from_snapshot(&bytes);
      assert_eq!(engine.checksum(), restored.checksum());
  }

  #[test]
  fn snapshot_plus_incremental_aof_recovery_beats_full_aof_replay() {
      let (snapshot_startup_ms, full_replay_startup_ms) = bench_recovery_paths(1_000_000);
      assert!(snapshot_startup_ms < full_replay_startup_ms / 5);
  }
  ```
- **Deliverable / test:** recovery time benchmark: compare full-AOF-replay startup time vs. snapshot+incremental-AOF startup time at 1M keys; should be a dramatic difference

### Week 10 — Replication
- **Sub-tasks:**
  - Design a leader-follower model: follower connects to leader, requests full sync (leader sends a snapshot), then leader streams subsequent write commands to the follower in real time (this reuses your AOF command-log format nicely — replication IS streaming the AOF)
  - Implement `REPLICAOF`/`SLAVEOF` command to point a node at a leader
  - Handle replication lag and reconnection: follower tracks its replication offset, can resume from where it left off after a disconnect rather than requiring a full resync every time
  - Implement basic read routing: followers serve reads, reject writes (or proxy them, your choice — document the decision)
  - Test with 1 leader + 2 followers: write to leader, confirm eventual consistency on followers within a bounded time window
- **Crates:** builds on `tokio` networking primitives you already have
- **Example tests to write:**
  ```rust
  #[tokio::test]
  async fn write_on_leader_is_visible_on_follower_within_bound() {
      let leader = spawn_server().await;
      let follower = spawn_server().await;
      follower.client().replicaof(leader.addr()).await.unwrap();
      leader.client().set("k", "v").await.unwrap();
      assert!(eventually(Duration::from_millis(200), || follower.client().get("k") == Ok("v".into())).await);
  }

  #[tokio::test]
  async fn follower_resumes_from_offset_after_disconnect_without_full_resync() {
      // disconnect follower mid-stream, reconnect, assert leader sends only the delta, not a fresh snapshot
  }
  ```
- **Deliverable / test:** replication integration test: 3-node cluster in CI (or docker-compose), writes on leader visible on followers within N ms; kill-and-reconnect-follower test confirms resumable sync

### Week 11 — Clustering / Sharding Across Nodes
- **Sub-tasks:**
  - Decide scope honestly here: full Redis Cluster protocol (hash slots, gossip, resharding) is a multi-month project on its own — a reasonable production-grade v1 is **client-side or proxy-side sharding** with a fixed hash-slot map (16384 slots like Redis Cluster, statically assigned to N nodes)
  - Implement `CLUSTER KEYSLOT`, `CLUSTER SHARDS`/`CLUSTER NODES` (informational, so clients that support Cluster mode can route correctly)
  - Implement `MOVED` redirection responses so cluster-aware clients (most modern ones) route to the correct node automatically
  - Skip live resharding for v1 — document it as a known Phase-5 gap rather than under-building it
- **Crates:** none new; this is design + wiring work
- **Example tests to write:**
  ```rust
  #[test]
  fn keyslot_matches_the_real_redis_cluster_algorithm() {
      assert_eq!(cluster_keyslot(b"foo"), 12182); // known reference value from real Redis Cluster
  }

  #[tokio::test]
  async fn client_hitting_wrong_shard_receives_moved_and_the_right_client_succeeds() {
      let cluster = spawn_3_shard_cluster().await;
      let wrong_shard = cluster.shard_not_owning(b"foo");
      let err = wrong_shard.client().get("foo").await.unwrap_err();
      assert!(matches!(err, RedisError::Moved { .. }));
  }
  ```
- **Deliverable / test:** a 3-shard cluster where keys route deterministically by hash slot; a cluster-aware client (e.g. `redis-rs` with cluster support, or `ioredis` cluster mode) correctly finds keys across shards

### Week 12 — Observability & Benchmarking
- **Sub-tasks:**
  - Add Prometheus metrics: commands/sec, latency histograms per command, memory usage, connected clients, replication lag, expired-keys count
  - Flesh out `INFO` properly (real Redis's `INFO` output format, since tooling parses it) — sections for server, clients, memory, persistence, replication, stats
  - Add structured request logging (slow-log equivalent: log any command over a configurable latency threshold)
  - Run head-to-head benchmarks against real Redis using `redis-benchmark`: SET/GET throughput, pipelined vs. non-pipelined, various payload sizes — document results honestly, including where you're slower and why
  - Profile hot paths with `flamegraph`/`perf` and fix any embarrassing bottlenecks found (lock contention is the usual suspect)
- **Crates:** `metrics` + `metrics-exporter-prometheus`, `flamegraph` (dev-only, via `cargo-flamegraph`)
- **Example tests to write:**
  ```rust
  #[tokio::test]
  async fn set_command_latency_appears_in_prometheus_histogram() {
      let server = spawn_test_server().await;
      server.client().set("k", "v").await.unwrap();
      let metrics_text = fetch(&format!("{}/metrics", server.metrics_url())).await;
      assert!(metrics_text.contains(r#"command_latency_seconds{cmd="set"}"#));
  }
  ```
  Plus a benchmark script committed to the repo: `redis-benchmark -t set,get -n 100000 -p <port>` run against both servers, results diffed and recorded in the benchmark report.
- **Deliverable / test:** a benchmark report (markdown, checked into the repo) comparing your server vs. real Redis across several workloads; Grafana dashboard config (json) if you want to visualize the Prometheus metrics — ties nicely into the Grafana/Loki experience you've already got

---

## Phase 4 — Extended Protocol & Production Readiness (Weeks 13–16)
Goal: this is where the "layer your own protocol on top" plan you wanted lands, plus the access-control and polish needed before calling it production-ready for others.

### Week 13 — Custom Protocol Design
- **Sub-tasks:**
  - Now that the engine is proven, design what your protocol adds that RESP can't easily express: candidates — typed values (not everything-is-a-string), server-side schemas, richer error metadata, request multiplexing over a single connection (RESP is fundamentally request-response per connection; a binary framed protocol can multiplex many in-flight requests, similar to HTTP/2)
  - Pick a wire format: hand-rolled length-prefixed binary framing is a good learning exercise, or adopt something like a Protobuf/Cap'n Proto/FlatBuffers schema if you want strong cross-language codegen for future client libraries
  - Write the protocol spec as a doc before writing code (message types, framing, error model) — treat it like an RFC
- **Crates:** depends on format choice — `prost` (protobuf) or `capnp` if going that route; otherwise none, hand-roll framing over `tokio_util::codec`
- **Example tests to write:**
  ```rust
  #[test]
  fn typed_value_message_round_trips_through_encode_decode() {
      let msg = TypedValue::Int(42);
      let bytes = msg.encode();
      assert_eq!(TypedValue::decode(&bytes).unwrap(), msg);
  }
  ```
  Since Week 13 is primarily a design week, the main "test" is a spec review: walk the written protocol doc against 3-4 concrete example exchanges (e.g. "client sends a multiplexed GET+SET pair, here's the exact byte layout of both frames") and confirm they're unambiguous enough to implement from.
- **Deliverable / test:** a protocol spec document; a decision record on wire format choice with tradeoffs listed

### Week 14 — Custom Protocol Implementation
- **Sub-tasks:**
  - Implement the new protocol's decoder/encoder as a second `Framed` codec alongside RESP — the server should be able to listen on two ports (or use protocol detection on the first bytes of a connection) and route both into the *same* underlying engine and dispatcher
  - Add whatever new capabilities motivated the custom protocol in Week 13 (e.g., multiplexed requests, typed values) as new dispatcher paths
  - Write a minimal client library in Rust for your new protocol (proves the design works end-to-end); stub out what a second-language client (e.g. Go or TypeScript, given your stack) would need
- **Crates:** as chosen in Week 13
- **Example tests to write:**
  ```rust
  #[tokio::test]
  async fn resp_write_is_visible_to_a_read_over_the_custom_protocol() {
      let server = spawn_server_with_both_protocols().await;
      server.resp_client().set("k", "v").await.unwrap();
      assert_eq!(server.custom_client().get("k").await.unwrap(), "v");
  }

  #[tokio::test]
  async fn custom_protocol_correctly_multiplexes_concurrent_requests_on_one_connection() {
      let mut con = server.custom_client().await;
      let (r1, r2) = tokio::join!(con.get("a"), con.get("b"));
      assert!(r1.is_ok() && r2.is_ok());
  }
  ```
- **Deliverable / test:** integration test suite exercising the new protocol against the same engine, confirming RESP and your protocol see a consistent, shared keyspace when used concurrently

### Week 15 — Auth, ACLs & Multi-tenancy
- **Sub-tasks:**
  - Implement `AUTH` (password-based to start), then a basic ACL system: named users, permitted command categories, permitted key patterns (mirrors real Redis ACLs, which are well-documented to crib design from)
  - Add TLS support for client connections (`tokio-rustls`) — non-negotiable for anything calling itself production-grade
  - Consider namespacing/multi-tenancy: key-prefix isolation per tenant, or separate logical DBs (`SELECT`-style) — decide based on whether you see this serving multiple projects/teams or just being hardened for one
- **Crates:** `tokio-rustls`, `argon2` or `bcrypt` (password hashing, not plaintext comparison)
- **Example tests to write:**
  ```rust
  #[tokio::test]
  async fn acl_user_denied_command_outside_permitted_key_pattern() {
      let server = spawn_server().await;
      server.admin().acl_setuser("readonly-app", &["on", ">pw", "~app:*", "+get", "-set"]).await.unwrap();
      let con = server.connect_as("readonly-app", "pw").await.unwrap();
      assert!(con.get("app:1").await.is_ok());
      assert!(con.set("other:1", "x").await.is_err());
  }

  #[tokio::test]
  async fn plaintext_connection_is_rejected_when_tls_only_mode_is_enabled() {
      let server = spawn_server_with_tls_required().await;
      assert!(TcpStream::connect(server.addr()).await.is_ok()); // TCP connects
      // but the handshake / first command should fail without TLS
  }
  ```
- **Deliverable / test:** ACL test suite (user with restricted key-pattern access correctly denied out-of-scope commands); TLS handshake test with a real client

### Week 16 — Final Hardening, Documentation & Release
- **Sub-tasks:**
  - Chaos-test: random kill -9s during writes/replication/snapshotting in a loop overnight, confirm no data corruption ever surfaces
  - Config file support (TOML) covering everything that's been hardcoded or flag-based so far — persistence intervals, memory limits, replication settings, ACL bootstrap
  - Write real documentation: getting-started guide, config reference, command compatibility matrix (vs. real Redis), architecture doc pulling together the design decisions from every phase
  - Package for distribution: Docker image, systemd unit file example, versioned release on GitHub with binaries for common platforms (this is a nice use of GitHub Actions release workflows, given your existing GitHub familiarity)
  - Tag v1.0.0
- **Crates:** `figment` or `config` (layered config file + env var + CLI flag handling)
- **Example tests to write:**
  ```rust
  #[test]
  fn config_file_env_var_and_cli_flag_layer_with_correct_precedence() {
      let cfg = Config::load_from(["--maxmemory=200mb"], env_with("APP_PORT", "7000"), "config.toml");
      assert_eq!(cfg.maxmemory, "200mb"); // CLI wins over file
      assert_eq!(cfg.port, 7000); // env wins over file default
  }
  ```
  ```text
  # overnight chaos loop (shell pseudocode, run via CI nightly job)
  for i in $(seq 1 200); do
    start_server_with_load_generator
    sleep $((RANDOM % 30))
    kill -9 $SERVER_PID
    restart_server
    verify_data_integrity || fail "corruption detected on iteration $i"
  done
  ```
- **Deliverable / test:** overnight chaos-test log with zero corruption incidents; public-facing README and docs site; a tagged, versioned release with a Docker image that "just works" on `docker run`

---

## Cross-cutting notes
- **Testing philosophy throughout:** every week's deliverable includes tests, not just working code — for a data store, correctness bugs are the ones that quietly cost someone their data six months later. Treat the kill-and-recover and concurrency stress tests as seriously as the feature work itself.
- **Scope honesty:** Phase 3's clustering (Week 11) is intentionally scoped down from full Redis Cluster. Calling that out explicitly in your docs is more credible than quietly under-delivering on an unstated "full cluster support" claim.
- **Time estimate:** ~16 weeks at solid part-time pace alongside your day job (Sage). If some weeks need to stretch to two, the phase boundaries are natural pause points to reassess without losing coherence.
- **Where this could go next (Phase 5, unscoped):** live cluster resharding, Lua scripting (`EVAL`) support, pub/sub (`SUBSCRIBE`/`PUBLISH`), transactions (`MULTI`/`EXEC`), streams (`XADD` etc.) — all reasonable follow-ons once v1.0 is stable and you have a sense of what real usage actually demands.
