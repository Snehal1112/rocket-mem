# Sprint 6 Close-Out Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the repo documents what Sprint 6 shipped — clustering, observability, and the benchmark — including every new command, every new environment variable, and every known limit, and the sprint plan records the sprint as done.

**Architecture:** documentation only. No source file under `crates/` is modified by this plan; the one code-adjacent change is answering the question `docs/design/sharding-decision.md` has been holding open since Sprint 1 ("revisit once Sprint 6 benchmarking gives real contention data") with the profile plan 07 captured.

**Tech Stack:** none.

**Spec:** ../../specs/2026-08-30-sprint-6-spec.md. Depends on plans 01–07 being complete, since this documents their result.

## Global Constraints

- **Known limits go in the README, not in a follow-up issue.** This project's stated convention (Sprints 4 and 5 both did it) is that a gap called out explicitly is more credible than one quietly under-delivered. Sprint 6 has a lot of them: no gossip, no failover, no resharding, no forwarding, no `CLUSTER SLOTS`, 4-field slow-log entries, active-only `expired_keys`, no true replication-offset lag, and the `ReplicationHandle` naming debt.
- **Sprint 5's stale known-limits sentence must be corrected, not appended to.** `README.md:74-75` currently says "`HELLO`/`INFO` still hardcode `role: master`/a bare `# Server` section regardless of actual replica status" — that is no longer true after `05-info-and-hello-overhaul.md`, and leaving it would be worse than never having written it.
- Every number quoted in the README comes from the committed benchmark report, not from memory.

---

### Task 1: README — status, commands, configuration

**Files:**
- Modify: `README.md` (Status section ending `:82`; command table `:86-92`; env-var table `:105-116`; workspace layout `:120-125`; documentation list `:140-143`)

**Interfaces:**
- Consumes: everything plans 01–07 shipped.
- Produces: the repo's front-door description of Sprint 6.

- [ ] **Step 1: Correct Sprint 5's now-stale limitation sentence**

In `README.md:69-75`, the known-limits paragraph ends with "...; `HELLO`/`INFO` still hardcode `role: master`/a bare `# Server` section regardless of actual replica status." Replace that final clause with:

```markdown
(`HELLO` and `INFO` now report the real role — that Sprint 5 limitation was fixed in Sprint 6).
```

- [ ] **Step 2: Add the Sprint 6 status paragraph**

Insert after the Sprint 5 block (before the "Remaining sprints" line at `:81`):

```markdown
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
otherwise; **no live resharding and no failover** (slot ownership is fixed at process start;
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
```

- [ ] **Step 3: Extend the command-coverage table**

Add a row to the table at `README.md:86-92`:

```markdown
| Server/Cluster | `PING`, `ECHO`, `SELECT`, `COMMAND`, `HELLO`, `INFO [section]`, `SAVE`, `REPLICAOF`, `PSYNC`, `CLUSTER KEYSLOT`/`SHARDS`/`NODES`/`INFO`/`MYID`, `SLOWLOG GET`/`LEN`/`RESET` |
```

- [ ] **Step 4: Extend the environment-variable table**

Replace the "The server binary reads three environment variables at startup" line at `README.md:105` with "The server binary reads these environment variables at startup:", and add these rows to the table:

```markdown
| `ROCKET_MEM_CLUSTER_CONFIG` | unset | Path to the cluster topology file. Unset means cluster mode is off (no `-MOVED`, no `-CROSSSLOT`). Must be set together with `ROCKET_MEM_CLUSTER_NODE_ID` |
| `ROCKET_MEM_CLUSTER_NODE_ID` | unset | Which line of that file describes this process. Startup fails if it is missing or names an unknown id |
| `ROCKET_MEM_METRICS_ADDR` | `127.0.0.1:9121` | Where the Prometheus `/metrics` endpoint listens. Loopback by default because it is unauthenticated |
| `ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS` | `10000` (10ms) | Commands at or over this duration are recorded in the slow log. `0` disables it |
```

- [ ] **Step 5: Add the "Running a cluster" and "Observability" sections**

Insert both after the existing "Running with persistence and replication" section (which ends at `README.md:116`):

````markdown
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
````

- [ ] **Step 6: Update the workspace-layout and documentation lists**

In `README.md:125`, extend the `server` bullet's list of responsibilities with "cluster hash-slot routing and `-MOVED` redirection, the Prometheus metrics endpoint, and the slow log".

In the Documentation list (`README.md:140-143`), add:

```markdown
- [`docs/benchmarks/`](docs/benchmarks/) — the committed `redis-benchmark` head-to-head report and the flamegraph profiling notes.
```

- [ ] **Step 7: Commit**

```bash
git add README.md
git commit -m "docs: update README for Sprint 6 completion"
```

---

### Task 2: answer `sharding-decision.md`'s open question

**Files:**
- Modify: `docs/design/sharding-decision.md`

**Interfaces:**
- Consumes: `docs/benchmarks/2026-08-30-flamegraph-notes.md` (plan 07).
- Produces: a design doc that no longer defers to a sprint that has now happened.

- [ ] **Step 1: Replace the deferral with the finding**

The "Why 16 shards" section currently ends "Revisit once Sprint 6 (Week 12) benchmarking gives real contention data." Replace that sentence with what the flamegraph actually showed, quoting the recorded share from `docs/benchmarks/2026-08-30-flamegraph-notes.md` — for example, if lock frames were a small share:

```markdown
Sprint 6's flamegraph pass (see
[`../benchmarks/2026-08-30-flamegraph-notes.md`](../benchmarks/2026-08-30-flamegraph-notes.md))
is the contention data this section was waiting for: <state the measured share of time in shard
lock frames from that profile>. On that evidence, 16 shards stays — the escape hatch the
Architecture Decision Record documented (swapping each shard's internals for a lock-free
structure) is still open and still unexercised, and the sprint plan's own risk table calls acting
on it mid-sprint the rabbit hole to avoid.
```

- [ ] **Step 2: Add the cluster-slot disambiguation**

Append a new section, so nobody reading this doc later confuses the two mechanisms:

```markdown
## Not to be confused with cluster hash slots

Sprint 6 added a second, unrelated mechanism also called sharding: 16384 Redis-Cluster hash slots
(`CRC16(hash_tag(key)) % 16384`, `crates/server/src/cluster.rs`) that decide which *node* owns a
key. That is a placement decision across processes; this document is about lock striping *within*
one process. The two never interact — the `engine` crate knows nothing about slots, `Store`'s
16-shard routing is unchanged, and every node of a cluster is a complete server with its own
16 internal shards. The digits matching is a coincidence. See
[`../superpowers/specs/2026-08-30-sprint-6-spec.md`](../superpowers/specs/2026-08-30-sprint-6-spec.md).
```

- [ ] **Step 3: Commit**

```bash
git add docs/design/sharding-decision.md
git commit -m "docs(design): answer the shard-count question with Sprint 6 profiling data"
```

---

### Task 3: full verification and the sprint-plan tick

**Files:**
- Modify: `docs/rocket-mem-sprint-plan.md` (Sprint 6 section, `:243-282`)

**Interfaces:**
- Consumes: plans 01–07.
- Produces: the sprint recorded as complete.

- [ ] **Step 1: Verify the whole workspace one last time**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean/green. Do not proceed to Step 2 on a red run — the DoD's fourth line is exactly this command set.

- [ ] **Step 2: Confirm each DoD item against real evidence**

```bash
cargo test -p rocket-mem --test cluster    # DoD 1: 3-shard routing + MOVED
ls -l docs/benchmarks/2026-08-30-redis-benchmark.md   # DoD 2: report committed
cargo test -p rocket-mem --test metrics    # DoD 3: metrics scrape asserted end to end
```

Expected: both test binaries green, and the report present with real numbers in its tables (open it and check that no cell is still empty — an unfilled table does not satisfy DoD 2).

- [ ] **Step 3: Mark the sprint complete**

In `docs/rocket-mem-sprint-plan.md`, directly under the Sprint 6 heading's "**Sprint goal:**" line (`:246`), add the same status block Sprint 5 uses (`:202-204`):

```markdown
**Status:** ✅ Complete — full P0/P1/P2 scope shipped. See
`docs/superpowers/specs/2026-08-30-sprint-6-spec.md` and
`docs/superpowers/plans/2026-08-30-sprint-6-plans/`.
```

and tick the three Definition-of-done boxes at `:272-274`, changing `- [ ]` to `- [x]` on each:

```markdown
- [x] 3-shard cluster test passes: keys route by hash slot, cluster-aware client finds them via `MOVED`
- [x] Benchmark report committed to the repo, including where you're slower than real Redis and why
- [x] Prometheus metrics visible and scraping correctly
```

- [ ] **Step 4: Tick this sprint's plan checkboxes**

Every `- [ ]` step in `docs/superpowers/plans/2026-08-30-sprint-6-plans/01-*.md` through `07-*.md` that was actually executed becomes `- [x]`, matching how Sprint 5's plans were closed out (commit `02ccdfb`, "docs: tick Sprint 5 plans 01-06 completed steps"). Do not tick a step that was skipped — an honestly unticked step is the record that it was.

- [ ] **Step 5: Commit**

```bash
git add docs/rocket-mem-sprint-plan.md docs/superpowers/plans/2026-08-30-sprint-6-plans/
git commit -m "docs: mark Sprint 6 complete in the sprint plan"
```
