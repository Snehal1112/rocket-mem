# Sprint 7 Close-Out Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the repo documents what Sprint 7 shipped — RMP's wire format, the new `ROCKET_MEM_RMP_ADDR` env var, the `rmp-client` crate, and the fact that it reaches the full command set — and the sprint plan records the sprint as done.

**Architecture:** documentation only. No source file under `crates/` is modified by this plan.

**Tech Stack:** none.

**Spec:** [`../../specs/2026-08-31-sprint-7-spec.md`](../../specs/2026-08-31-sprint-7-spec.md). Depends on plans 01–04 being complete, since this documents their result.

## Global Constraints

- **Known limits and stale claims go in the README, not in a follow-up issue** — this project's stated convention (every prior sprint's close-out did this).
- **A pre-existing inconsistency, found while writing this plan, must be corrected, not left in place:** `README.md`'s "Remaining sprints" sentence already says "(clustering, a custom protocol, ACLs/TLS) ... not started" even though the paragraph immediately above it (Sprint 6) says clustering is done — that sentence was never updated when Sprint 6 closed. It must now say only ACLs/TLS remain.
- **The Sprint 6 close-out's prediction about `ReplicationHandle`'s rename must be corrected, not carried forward.** `README.md` currently says the rename to `ServerState` was "deferred to Sprint 7, whose dual-protocol work already has to touch those signatures" — Sprint 7's actual design (see the spec's Architecture recap) reuses `ReplicationHandle` exactly as-is and never renames it, so that justification turned out to be false. State plainly that the rename is still deferred, without repeating a prediction this sprint disproved.

---

### Task 1: README — status, RMP usage, configuration, workspace layout

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: everything Plans 01–04 shipped.
- Produces: the repo's front-door description of Sprint 7.

- [ ] **Step 1: Add the Sprint 7 status paragraph**

Insert immediately after the Sprint 6 status paragraph's closing sentence ("...why `INFO`/`HELLO` moved out of `dispatch`).") and before the "Known limits, called out explicitly..." paragraph:

```markdown
**Sprint 7 (custom protocol) — done.** rocket-mem now speaks a second wire protocol of its own,
**RMP**, alongside RESP — both read and write the same shared keyspace, and a client can prove it
by writing over one and reading over the other. RMP's headline capability is the one thing RESP
structurally cannot do: request multiplexing. A client sends many requests on one connection
without waiting for each reply, tagging each with its own `request_id`; the server may answer them
in any order, and the client correlates each reply back to its request by that id rather than by
arrival order. RMP is a hand-rolled binary framing (magic bytes, version, a 16-byte envelope,
length-prefixed values) rather than Protobuf/Cap'n Proto, reachable on its own port
(`ROCKET_MEM_RMP_ADDR`, default `127.0.0.1:6380`) — see "Running the custom protocol (RMP)" below.
Crucially, RMP reaches rocket-mem's *entire* command set, including `INFO`, `CLUSTER`, `SAVE`,
`REPLICAOF`, and `SLOWLOG`, for free: its connection handler builds the same `Array`-of-`Bulk`
command shape RESP already builds and calls the identical, unmodified `dispatch_and_log` function
every RESP command goes through, so AOF logging, replica fan-out, cluster redirection, and the
read-only-replica gate all apply to an RMP write exactly as they do to a RESP one. A new
`rmp-client` crate is a minimal async Rust client (`connect`/`call`/`get`/`set`/`del`) proving the
whole design end-to-end, including a test that deliberately has the server answer two concurrent
requests out of order and confirms the client still resolves each to the right caller. See
`docs/superpowers/specs/2026-08-31-sprint-7-spec.md` for the full wire format (byte-exact worked
examples for a request, a response, and a multiplexed pair) and the connection concurrency model.
```

- [ ] **Step 2: Correct the stale `ReplicationHandle` rename prediction**

In the Sprint 6 known-limits paragraph, find:

```markdown
`ReplicationHandle` is now misnamed — it carries the snapshot path, AOF handle, cluster config,
slow log, and server counters — with the rename to `ServerState` deferred to Sprint 7, whose
dual-protocol work already has to touch those signatures.
```

Replace with:

```markdown
`ReplicationHandle` is still misnamed — it carries the snapshot path, AOF handle, cluster config,
slow log, and server counters. Sprint 7 turned out not to force this rename after all: its RMP
connection handler takes the exact same `Arc<ReplicationHandle>` every RESP connection already
does, unchanged. The rename remains deferred, with no forcing sprint currently scoped.
```

- [ ] **Step 2b: Add a known-limit clause for RMP connections and connection metrics**

A finding from Plan 02's task review, ruled on rather than fixed in-place (see
`../../../.superpowers/sdd/02-server-connection-handling-and-listener/progress.md`'s ruling):
RMP's connection handler never calls `ReplicationHandle::connection_opened`/`connection_closed`,
so RMP connections are invisible to `rocket_mem_connected_clients` and
`rocket_mem_connections_total`. This is a real, deliberate limitation for this sprint, not a
silent gap — record it in the same known-limits paragraph the Step 2 edit above lives in, appended
after the `ReplicationHandle` sentence (before the paragraph's closing "Remaining sprints" text
that Step 3 handles separately):

```markdown
RMP connections are not yet counted in `rocket_mem_connected_clients`/`rocket_mem_connections_total`
— those counters are only wired into RESP's connection lifecycle (`connection.rs`'s `ClientGuard`);
extending them to RMP is a small, contained follow-up (an equivalent guard in
`rmp_connection.rs`), not attempted this sprint to keep it scoped to the protocol itself.
```

- [ ] **Step 3: Fix the stale "Remaining sprints" sentence**

Find:

```markdown
Remaining sprints (clustering, a custom protocol, ACLs/TLS) are scoped in the
[sprint plan](docs/rocket-mem-sprint-plan.md) but not started.
```

Replace with:

```markdown
The remaining sprint (auth, ACLs, TLS & release) is scoped in the
[sprint plan](docs/rocket-mem-sprint-plan.md) but not started.
```

- [ ] **Step 4: Add the `ROCKET_MEM_RMP_ADDR` env var row**

In the environment-variable table, find the `ROCKET_MEM_ADDR` row:

```markdown
| `ROCKET_MEM_ADDR` | `127.0.0.1:6379` | TCP address to bind |
```

Add immediately after it:

```markdown
| `ROCKET_MEM_RMP_ADDR` | `127.0.0.1:6380` | TCP address the RMP (custom protocol) listener binds — always on, no opt-out |
```

- [ ] **Step 5: Add a "Running the custom protocol (RMP)" subsection**

Insert a new subsection after "### Running a cluster" ends (after the paragraph ending "...following a `-MOVED` is the client's job.") and before "### Observability":

```markdown
### Running the custom protocol (RMP)

Every node also listens for **RMP** on its own port, unconditionally. A client can send several
requests without waiting for each reply — each carries a `request_id` the matching response
echoes back, so replies may arrive in any order:

```bash
ROCKET_MEM_RMP_ADDR=127.0.0.1:6380 cargo run --release --bin rocket-mem
```

The `rmp-client` crate is a minimal async client proving the design end-to-end:

```rust
let client = rmp_client::RmpClient::connect("127.0.0.1:6380").await?;
client.set("foo", "bar").await?;
assert_eq!(client.get("foo").await?, Some(bytes::Bytes::from_static(b"bar")));
```

RMP reaches the same command set RESP does — see
[`docs/superpowers/specs/2026-08-31-sprint-7-spec.md`](docs/superpowers/specs/2026-08-31-sprint-7-spec.md)
for the wire format's exact byte layout and the multiplexing design.
```

- [ ] **Step 6: Update "Workspace layout" for the fifth crate**

Find:

```markdown
## Workspace layout

Four crates under `crates/`:

- **`common`** — shared `EngineError` enum (`WrongType`, `NotAnInteger`, `NoSuchKey`). No dependencies on the other crates.
- **`engine`** — the storage engine: `Value` enum, 16-shard `Store`, and one free function per command under `commands/`. Everything in "Status" above lives here.
- **`protocol`** — RESP wire format: the `Frame` type (RESP2 plus RESP3's `Map`) and `RespCodec`, encoding/decoding both including split-read reassembly.
- **`server`** — the binary (package name `rocket-mem`): Tokio TCP accept loop, per-connection task, command dispatcher, AOF writer/replayer, snapshotting, leader/follower replication, the active-expiry and fsync background loops, cluster hash-slot routing and `-MOVED` redirection, the Prometheus metrics endpoint, and the slow log.
```

Replace with:

```markdown
## Workspace layout

Five crates under `crates/`:

- **`common`** — shared `EngineError` enum (`WrongType`, `NotAnInteger`, `NoSuchKey`). No dependencies on the other crates.
- **`engine`** — the storage engine: `Value` enum, 16-shard `Store`, and one free function per command under `commands/`. Everything in "Status" above lives here.
- **`protocol`** — wire formats: RESP's `Frame` type (RESP2 plus RESP3's `Map`) and `RespCodec`, and RMP's envelope/value codec (`rmp` module) reusing the same `Frame` as its value model. Both codecs handle split-read reassembly.
- **`server`** — the binary (package name `rocket-mem`): Tokio TCP accept loops for both RESP and RMP, per-connection tasks, the shared command dispatcher every protocol calls, AOF writer/replayer, snapshotting, leader/follower replication, the active-expiry and fsync background loops, cluster hash-slot routing and `-MOVED` redirection, the Prometheus metrics endpoint, and the slow log.
- **`rmp-client`** — a minimal async Rust client for RMP (`connect`/`call`/`get`/`set`/`del`), proving the protocol end-to-end.
```

- [ ] **Step 7: Update the top-level intro sentence**

Find (line 5):

```markdown
A from-scratch, RESP-compatible (Redis wire protocol) in-memory data store written in Rust. The goal is a server real Redis clients (`redis-cli`, `redis-py`, `ioredis`, `go-redis`, ...) can talk to unmodified, built on a storage engine that stays protocol-agnostic so a custom binary protocol can be layered on top later without a rewrite.
```

Replace with:

```markdown
A from-scratch, RESP-compatible (Redis wire protocol) in-memory data store written in Rust. The goal is a server real Redis clients (`redis-cli`, `redis-py`, `ioredis`, `go-redis`, ...) can talk to unmodified, built on a storage engine that stays protocol-agnostic — proven out in Sprint 7, which layered **RMP**, a second binary protocol of rocket-mem's own, on top without touching the engine at all.
```

- [ ] **Step 8: Update the architecture diagram's forward-reference comment**

Find:

```
│  Protocol Layer (RESP2/RESP3, later:     │
│  a custom binary protocol)               │
```

Replace with:

```
│  Protocol Layer (RESP2/RESP3, RMP)       │
```

- [ ] **Step 9: Proofread and commit**

Run: `grep -n "custom binary protocol\|Sprint 7\|ROCKET_MEM_RMP_ADDR" README.md` and confirm every match reads correctly in context (no leftover "later:" phrasing, no duplicate status paragraphs).

```bash
git add README.md
git commit -m "docs: mark Sprint 7 complete in the README"
```

---

### Task 2: Sprint plan — mark Sprint 7 complete

**Files:**
- Modify: `docs/rocket-mem-sprint-plan.md` (Sprint 7 section, currently ending in an unchecked Definition of Done)

**Interfaces:**
- Consumes: nothing.
- Produces: an accurate sprint-tracking document, matching the `**Status:** ✅ Complete — ...` line every prior completed sprint has directly under its `## Sprint N — ...` heading.

- [ ] **Step 1: Add the Status line and check off the Definition of Done**

Find:

```markdown
## Sprint 7 — Custom protocol: design & implementation
**Maps to:** Weeks 13-14 | **Dates:** Day 85–98

**Sprint goal:** Your own protocol is live alongside RESP, both reading and writing the same shared keyspace.
```

Replace with:

```markdown
## Sprint 7 — Custom protocol: design & implementation
**Maps to:** Weeks 13-14 | **Dates:** Day 85–98

**Status:** ✅ Complete — full P0 scope shipped, plus the P1 client library. See
`docs/superpowers/specs/2026-08-31-sprint-7-spec.md` and
`docs/superpowers/plans/2026-08-31-sprint-7-plans/`.

**Sprint goal:** Your own protocol is live alongside RESP, both reading and writing the same shared keyspace.
```

Find the Sprint 7 Definition of Done block:

```markdown
### Definition of done
- [ ] Protocol spec doc committed
- [ ] Integration test proves a write via RESP is visible via the new protocol (and vice versa)
- [ ] Minimal client library can perform at least GET/SET-equivalent operations
```

Replace with:

```markdown
### Definition of done
- [x] Protocol spec doc committed
- [x] Integration test proves a write via RESP is visible via the new protocol (and vice versa)
- [x] Minimal client library can perform at least GET/SET-equivalent operations
```

- [ ] **Step 2: Commit**

```bash
git add docs/rocket-mem-sprint-plan.md
git commit -m "docs: mark Sprint 7 complete in the sprint plan"
```
