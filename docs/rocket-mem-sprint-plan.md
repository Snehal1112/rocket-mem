# rocket-mem: Sprint Plan
### 8 sprints × 2 weeks, solo, ~20+ hrs/week

This maps the 16-week phase plan (`rocket-mem-production-plan.md`) onto 2-week sprints. Each sprint pairs two consecutive weeks from that plan — see that document for full task detail, crate choices, and example tests; this document adds capacity planning, priority (P0/P1/P2), risks, and a definition of done per sprint.

**Capacity baseline:** ~20-25 hrs/week available → ~40-50 hrs per 2-week sprint. Following standard sprint-planning practice, each sprint is scoped to ~75-85% of that (32-40 hrs), leaving buffer for day-job interrupts, debugging overrun, and life. P2 items are the first things to cut if a sprint runs long — don't feel behind if they slip.

---

## Sprint 1 — Engine core & core data types
**Maps to:** Weeks 1-2 of the master plan | **Dates:** Day 1–14

**Status:** ✅ Complete — full P0/P1 scope shipped, no cuts. See `docs/phase-1-retro.md`.

**Sprint goal:** A tested, sharded in-memory engine correctly implementing Strings, Hashes, Lists, and Sets — no networking yet.

### Capacity
| | Available | Planned | Notes |
|---|---|---|---|
| You | ~40-50 hrs | ~39 hrs (≈85%) | First sprint — expect setup friction to eat into buffer |

### Sprint backlog
| Priority | Item | Estimate | Dependencies |
|---|---|---|---|
| P0 | Workspace scaffold (`engine`/`protocol`/`server`/`common`) + `Value` enum | 4 hrs | None |
| P0 | Sharded keyspace design + implementation (16 shards, `RwLock`) | 6 hrs | Workspace scaffold |
| P0 | `get`/`set`/`del`/`exists`/`keys` against engine directly | 4 hrs | Sharded keyspace |
| P0 | String commands: `SET`(flags)/`GET`/`APPEND`/`STRLEN`/`INCR` family | 8 hrs | Core engine |
| P0 | Hash/List/Set commands (`HSET`/`HGET`/`LPUSH`/`SADD` families) | 8 hrs | Core engine |
| P1 | `WRONGTYPE` error handling + full error-path test matrix | 6 hrs | Command implementations |
| P1 | CI skeleton (`cargo test`/`clippy -D warnings`/`fmt --check`) | 3 hrs | Workspace scaffold |
| P2 | Design doc: sharding decision and rationale | 2 hrs | Sharded keyspace |

**Planned load:** ~39 hrs of ~40-50 available (≈85%)

### Risks
| Risk | Impact | Mitigation |
|---|---|---|
| Sharding design gets bikeshedded | Delays everything downstream, since it's load-bearing | Timebox the design doc to 2 hrs; a reasonable 16-shard `RwLock` design is fine to start, it can be swapped later (see Week 12's escape hatch) |
| Rust ownership fights eat the week | New patterns (shared state across async boundaries) take longer than expected | If stuck >2 hrs on one compiler error, drop to a simpler locking strategy first, optimize later |

### Definition of done
- [x] `cargo test` green, `cargo clippy -- -D warnings` clean
- [x] Every P0 command has a passing test, including at least one wrong-type/missing-key case (`wrongtype_matrix_tests.rs`, `missing_key_semantics_tests.rs`)
- [x] Sharding design doc committed (even if brief) — `docs/design/sharding-decision.md`
- [x] Code pushed; CI runs on push

### Key dates
| Day | Event |
|---|---|
| 1 | Sprint start |
| 7 | Mid-sprint check-in — confirm sharded engine compiles and passes basic tests before starting command work |
| 14 | Sprint end / self-demo — run the full command set against the engine directly, no networking |
| 14 | Retro — note anything that took longer than estimated, adjust Sprint 2 if needed |

---

## Sprint 2 — RESP protocol, networking & client compatibility
**Maps to:** Weeks 3-4 | **Dates:** Day 15–28

**Status:** ✅ Complete — full P0/P1 scope shipped, plus the P2 benchmark smoke test. RESP3/`HELLO` negotiation was also added beyond original scope. See `docs/phase-1-retro.md` and `docs/superpowers/plans/2026-08-29-sprint-2-plans/{client-verification-results.md,benchmark-smoke-test-results.md}`.

**Sprint goal:** Real Redis clients (redis-cli plus 2+ language libraries) can connect over TCP and run the full Sprint 1 command set.

### Capacity
| | Available | Planned | Notes |
|---|---|---|---|
| You | ~40-50 hrs | ~37 hrs (≈80%) | |

### Sprint backlog
| Priority | Item | Estimate | Dependencies |
|---|---|---|---|
| P0 | RESP2 parser/serializer (Simple/Error/Integer/Bulk/Array) | 6 hrs | Sprint 1 engine |
| P0 | `Decoder`/`Framed` handling for split/partial TCP reads | 4 hrs | RESP parser |
| P0 | Command dispatcher (parse → validate → call engine → serialize) | 4 hrs | RESP parser, Sprint 1 |
| P0 | Tokio `TcpListener`, task-per-connection accept loop | 4 hrs | Dispatcher |
| P0 | `PING`/`ECHO`/`SELECT`/`COMMAND`/`INFO` stubs | 4 hrs | Dispatcher |
| P1 | Integration test harness (spawn server, drive via `redis-rs`) | 6 hrs | TCP listener |
| P1 | Manual verification against redis-py, ioredis, go-redis | 6 hrs | TCP listener |
| P2 | Light `redis-benchmark` smoke test (no panics/deadlocks) | 3 hrs | TCP listener |

**Planned load:** ~37 hrs of ~40-50 available (≈80%)

### Risks
| Risk | Impact | Mitigation |
|---|---|---|
| RESP3/`HELLO` negotiation from modern clients isn't handled | Client libraries fail to connect even though your parser is correct | Decide explicitly this sprint: support RESP3, or reject `HELLO` and force RESP2 — don't discover this mid-debug — **resolved:** initially rejected per spec, later implemented (RESP3 negotiation shipped in follow-up commits); `redis-py` 8.1.0 needs `protocol=2` passed explicitly since it doesn't fall back silently on `HELLO` failure |
| Partial-read framing bugs are subtle | Works with `redis-cli`, silently breaks under pipelining | Write the split-write test (P0 item) before declaring networking done, not after — **resolved:** split/partial-read tests in `crates/protocol/src/codec.rs` cover header- and multi-read-boundary cases |

### Definition of done
- [x] `redis-cli` runs every Sprint 1 command correctly over real TCP
- [x] At least 2 non-Rust client libraries connect and run a basic workload (redis-py, ioredis)
- [x] Split/malformed-input integration tests pass in CI
- [x] Phase 1 retro note added to the repo (per the master plan's Week 4 task) — `docs/phase-1-retro.md`

### Key dates
| Day | Event |
|---|---|
| 15 | Sprint start |
| 21 | Mid-sprint check-in — RESP parser + dispatcher should compile and pass unit tests before wiring TCP |
| 28 | Sprint end / self-demo — live `redis-cli` session against your server |
| 28 | Retro |

---

## Sprint 3 — Full command set: keys, collections & sorted sets
**Maps to:** Weeks 5-6 | **Dates:** Day 29–42

**Status:** ✅ Complete — full P0/P1/P2 scope shipped. See `docs/superpowers/specs/2026-08-29-sprint-3-spec.md` and `docs/superpowers/plans/2026-08-29-sprint-3-plans/`.

**Sprint goal:** Broad command coverage, including a working sorted-set implementation and a concurrency-safe `SCAN`.

### Capacity
| | Available | Planned | Notes |
|---|---|---|---|
| You | ~40-50 hrs | ~37 hrs (≈80%) | |

### Sprint backlog
| Priority | Item | Estimate | Dependencies |
|---|---|---|---|
| P0 | String/key commands (`GETSET`/`MSET`/`MGET`/`EXPIRE` family/`RENAME`) | 8 hrs | Sprint 2 |
| P0 | `SCAN` with cursor-based iteration, concurrency stress test | 8 hrs | Sharded engine |
| P0 | Sorted set structure (`BTreeMap` + `ordered-float`) + `ZADD`/`ZRANGE`/`ZSCORE`/`ZRANK` | 10 hrs | Sprint 1 engine |
| P1 | Remaining List/Hash/Set commands (`LINSERT`, `HINCRBY`, `SINTER`/`SUNION`/`SDIFF`) | 8 hrs | Sprint 1 commands |
| P2 | Glob pattern matching polish for `KEYS` | 3 hrs | None |

**Planned load:** ~37 hrs of ~40-50 available (≈80%)

### Risks
| Risk | Impact | Mitigation |
|---|---|---|
| `SCAN` under concurrent writes misses/double-counts keys | Silent correctness bug, hard to catch later | Write the concurrent-write stress test as part of the feature, not as an afterthought — it's already a P0 line item above |
| Sorted set performance is poor at scale | Only surfaces under load, easy to miss in unit tests | Do a rough 10k/100k-member timing check before calling it done, even informally |

### Definition of done
- [x] Command coverage table in the repo README updated
- [x] `SCAN` concurrency stress test passes
- [x] Sorted set operations covered by tests including score-ordering edge cases

### Key dates
| Day | Event |
|---|---|
| 29 | Sprint start |
| 35 | Mid-sprint check-in |
| 42 | Sprint end / self-demo |
| 42 | Retro |

---

## Sprint 4 — Expiry, eviction & AOF persistence
**Maps to:** Weeks 7-8 | **Dates:** Day 43–56

**Sprint goal:** Data survives a `kill -9` and restart; memory stays bounded under a configured ceiling.

**Status:** ✅ Complete — full P0/P1 scope shipped, plus the P2 `MEMORY`/`OBJECT` stubs. See
`docs/superpowers/specs/2026-08-30-sprint-4-spec.md` and
`docs/superpowers/plans/2026-08-30-sprint-4-plans/`.

### Capacity
| | Available | Planned | Notes |
|---|---|---|---|
| You | ~40-50 hrs | ~37 hrs (≈80%) | Durability sprint — treat testing time as non-negotiable, not the first thing cut |

### Sprint backlog
| Priority | Item | Estimate | Dependencies |
|---|---|---|---|
| P0 | Active + passive TTL expiry | 8 hrs | Sprint 1 engine |
| P0 | AOF writer with configurable `fsync` policy | 8 hrs | Command dispatcher |
| P0 | AOF replay on startup + corrupt-tail recovery | 6 hrs | AOF writer |
| P1 | LRU eviction + `MAXMEMORY` | 8 hrs | TTL expiry |
| P1 | Kill-and-recover test suite (the durability proof) | 5 hrs | AOF replay |
| P2 | `MEMORY USAGE` / `OBJECT ENCODING` stubs | 2 hrs | None |

**Planned load:** ~37 hrs of ~40-50 available (≈80%)

### Risks
| Risk | Impact | Mitigation |
|---|---|---|
| Crash-mid-write corrupts AOF in ways your test doesn't cover | Data loss on real crashes despite tests passing | Test multiple crash points (mid-line, mid-fsync, mid-rotation), not just one |
| Eviction and expiry interact badly under concurrent load | Deadlock or memory ceiling breach | Run the eviction stress test with concurrent writers active, not in isolation |

### Definition of done
- [x] `kill -9` + restart test passes in CI with all keys intact
- [x] Corrupt-tail AOF recovery test passes without panicking
- [x] TTL correctness suite covers both active and passive expiry paths independently

### Key dates
| Day | Event |
|---|---|
| 43 | Sprint start |
| 49 | Mid-sprint check-in — confirm AOF writer works before building replay logic on top of it |
| 56 | Sprint end / self-demo — kill the server mid-load, restart, show data intact |
| 56 | Retro |

---

## Sprint 5 — Snapshotting & replication
**Maps to:** Weeks 9-10 | **Dates:** Day 57–70

**Sprint goal:** A follower stays in sync with a leader in real time; startup time drops sharply via snapshot + incremental AOF.

**Status:** ✅ Complete — full P0/P1 scope shipped. See
`docs/superpowers/specs/2026-08-30-sprint-5-spec.md` and
`docs/superpowers/plans/2026-08-30-sprint-5-plans/`.

### Capacity
| | Available | Planned | Notes |
|---|---|---|---|
| You | ~40-50 hrs | ~37 hrs (≈80%) | |

### Sprint backlog
| Priority | Item | Estimate | Dependencies |
|---|---|---|---|
| P0 | Snapshot serialization (`bincode`/`rkyv`) | 8 hrs | Sprint 4 AOF |
| P0 | Hybrid recovery (latest snapshot + AOF tail replay) | 6 hrs | Snapshot serialization |
| P0 | Leader→follower streaming replication | 10 hrs | AOF command-log format |
| P1 | `REPLICAOF` command + reconnect/resume from offset | 8 hrs | Streaming replication |
| P2 | `BGSAVE`-equivalent non-blocking snapshot | 5 hrs | Snapshot serialization |

**Planned load:** ~37 hrs of ~40-50 available (≈80%)

### Risks
| Risk | Impact | Mitigation |
|---|---|---|
| Replication resume-after-disconnect is more complex than expected | P1 item slips, forces a full-resync-only fallback | Full resync on every reconnect is an acceptable fallback for this sprint — resumable sync can slip to Sprint 6 buffer if needed |
| Snapshot format changes later break compatibility | Annoying but not fatal this early | Not worth solving now — note it as a known v1 limitation |

### Definition of done
- [x] Recovery time benchmark (snapshot+AOF vs full AOF replay) recorded, showing clear improvement
- [x] 1 leader + 2 follower integration test passes, writes visible within a bounded time window
- [x] Kill-and-reconnect-follower test passes (even if it falls back to full resync)

### Key dates
| Day | Event |
|---|---|
| 57 | Sprint start |
| 63 | Mid-sprint check-in |
| 70 | Sprint end / self-demo — 3-node replication set, live write propagation |
| 70 | Retro |

---

## Sprint 6 — Clustering & observability
**Maps to:** Weeks 11-12 | **Dates:** Day 71–84

**Sprint goal:** Keys route deterministically across a 3-shard cluster; a benchmark report shows throughput in the same ballpark as real Redis.

### Capacity
| | Available | Planned | Notes |
|---|---|---|---|
| You | ~40-50 hrs | ~37 hrs (≈80%) | |

### Sprint backlog
| Priority | Item | Estimate | Dependencies |
|---|---|---|---|
| P0 | Hash-slot keyspace design + `CLUSTER KEYSLOT` | 6 hrs | Sprint 5 |
| P0 | `MOVED` redirection for cluster-aware clients | 6 hrs | Hash-slot design |
| P0 | Prometheus metrics + full `INFO` output | 8 hrs | Existing command set |
| P1 | `redis-benchmark` head-to-head report vs. real Redis | 8 hrs | Metrics |
| P1 | Flamegraph profiling pass, fix worst hot-path bottleneck | 6 hrs | Benchmark results |
| P2 | Slow-log equivalent | 3 hrs | Metrics |

**Planned load:** ~37 hrs of ~40-50 available (≈80%)

### Risks
| Risk | Impact | Mitigation |
|---|---|---|
| Benchmarks reveal lock contention as the real bottleneck | Tempting to rabbit-hole into a lock-free rewrite mid-sprint | This is exactly the documented escape hatch — note it, don't act on it until this sprint's other P0s are done |
| Full Redis Cluster protocol scope-creeps in | Multi-month feature disguised as a sprint item | Stick to the static hash-slot assignment already scoped — live resharding stays out of v1, as decided in the master plan |

### Definition of done
- [ ] 3-shard cluster test passes: keys route by hash slot, cluster-aware client finds them via `MOVED`
- [ ] Benchmark report committed to the repo, including where you're slower than real Redis and why
- [ ] Prometheus metrics visible and scraping correctly

### Key dates
| Day | Event |
|---|---|
| 71 | Sprint start |
| 77 | Mid-sprint check-in |
| 84 | Sprint end / self-demo — benchmark report walkthrough |
| 84 | Retro — this is the natural point to decide whether Sprint 7+ needs rescoping based on what benchmarking found |

---

## Sprint 7 — Custom protocol: design & implementation
**Maps to:** Weeks 13-14 | **Dates:** Day 85–98

**Sprint goal:** Your own protocol is live alongside RESP, both reading and writing the same shared keyspace.

### Capacity
| | Available | Planned | Notes |
|---|---|---|---|
| You | ~40-50 hrs | ~37 hrs (≈80%) | |

### Sprint backlog
| Priority | Item | Estimate | Dependencies |
|---|---|---|---|
| P0 | Protocol spec doc + wire-format decision (binary framing vs. protobuf/Cap'n Proto) | 6 hrs | Sprint 6 |
| P0 | Codec implementation (decoder/encoder) | 10 hrs | Spec doc |
| P0 | Dual-protocol dispatcher wiring (RESP + new protocol → same engine) | 8 hrs | Codec |
| P1 | Minimal Rust client library for the new protocol | 8 hrs | Codec |
| P2 | Second-language client stub (Go or TS) | 5 hrs | Rust client library |

**Planned load:** ~37 hrs of ~40-50 available (≈80%)

### Risks
| Risk | Impact | Mitigation |
|---|---|---|
| Wire-format bikeshedding | Whole sprint spent designing instead of building | Timebox the spec doc to 6 hrs as scoped; a hand-rolled binary framing is a perfectly good v1 choice, don't wait for the "perfect" format |
| Dual-protocol wiring reveals the engine wasn't as protocol-agnostic as assumed | Forces changes back in the engine layer | This is a good signal, not a failure — the Architecture Decision Record exists precisely so this stays a contained fix, not a rewrite |

### Definition of done
- [ ] Protocol spec doc committed
- [ ] Integration test proves a write via RESP is visible via the new protocol (and vice versa)
- [ ] Minimal client library can perform at least GET/SET-equivalent operations

### Key dates
| Day | Event |
|---|---|
| 85 | Sprint start |
| 91 | Mid-sprint check-in — spec doc should be done and codec underway |
| 98 | Sprint end / self-demo — same keyspace, two protocols, live |
| 98 | Retro |

---

## Sprint 8 — Auth, ACLs, TLS & release
**Maps to:** Weeks 15-16 | **Dates:** Day 99–112

**Sprint goal:** v1.0.0 tagged — authenticated, TLS-capable, chaos-tested, documented, and shipped as a Docker image.

### Capacity
| | Available | Planned | Notes |
|---|---|---|---|
| You | ~40-50 hrs | ~39 hrs (≈85%) | Last sprint — release-quality bar, less room to defer |

### Sprint backlog
| Priority | Item | Estimate | Dependencies |
|---|---|---|---|
| P0 | `AUTH` + ACL system (users, command categories, key patterns) | 8 hrs | Sprint 7 |
| P0 | TLS via `tokio-rustls` | 6 hrs | Networking layer |
| P0 | Overnight chaos test (random `kill -9` loop) | 6 hrs | Sprints 4-5 durability work |
| P1 | Config file layering (TOML, via `figment`) | 6 hrs | None |
| P1 | Documentation (getting-started, command matrix, architecture doc) | 8 hrs | Whole project |
| P2 | Docker image + GitHub Actions release workflow | 5 hrs | Config layering |

**Planned load:** ~39 hrs of ~40-50 available (≈85%)

### Risks
| Risk | Impact | Mitigation |
|---|---|---|
| Chaos test finds a real corruption bug late | Blocks the release | Run the chaos test starting Day 1 of this sprint, not Day 10 — it needs runway to surface issues and for you to fix them |
| Docs get cut for time | Ships without onboarding material | If something has to slip to a "Phase 5" backlog, let it be the Docker/release-workflow P2, not the docs P1 |

### Definition of done
- [ ] Overnight chaos test log shows zero corruption incidents
- [ ] ACL and TLS test suites pass
- [ ] README, config reference, and command-compatibility matrix are complete
- [ ] `v1.0.0` tagged; Docker image builds and runs via `docker run`

### Key dates
| Day | Event |
|---|---|
| 99 | Sprint start — kick off the overnight chaos test immediately |
| 105 | Mid-sprint check-in |
| 112 | Sprint end / release — tag `v1.0.0` |
| 112 | Retro — also the natural point to scope a "Phase 5" backlog (Lua scripting, pub/sub, transactions, streams, live resharding) |

---

## Notes on running this solo
- **No standup needed, but keep the mid-sprint check-in.** It's the one ritual worth keeping even solo — a forced moment to ask "is the P0 list still realistic" before the second week runs out.
- **Carryover:** if a sprint's P0 items don't finish, they become next sprint's first P0 items, and that sprint's original P1s become stretch. Don't silently re-scope without noticing — that's how a plan drifts.
- **The retro is short but real:** one line on what took longer than estimated, one line on what to adjust. That data makes Sprint N+1's estimates better than Sprint N's guesses.
