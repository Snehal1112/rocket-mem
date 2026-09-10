# Plan 04: Documentation and spec alignment

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** make `replica_announce_addr` discoverable by an operator who does not read Rust, and
record the TLS gap this work exposed in the spec that will trip over it.

**Architecture:** No code. Plans 01–03 built the field, the wiring, and the warning; this plan
is the part that makes them usable. Documentation-only, so the three gates protect against
nothing here except an accidental stray edit under `crates/` — run them anyway.

**Spec:** [`../../specs/2026-09-10-replica-announce-addr-spec.md`](../../specs/2026-09-10-replica-announce-addr-spec.md)

## Global Constraints

Identical to [plan 01](01-config-field-and-validation.md#global-constraints); re-read that
section before starting. The two that bite hardest here:

- **No test may load the repo-root `rocket-mem.toml`.** It is a live deployment's
  credential-bearing config, not a template.
- **Comment style.** Short, full sentences ending in a punctuation mark. No emoji.

---

### Task 1: Document the field in the three prose docs

**Files:**
- Modify: `docs/config-reference.md` — the field table, plus a prose paragraph
- Modify: `README.md` — the Configuration table (~line 277, where `log_value_max_bytes` sits)
- Modify: `.claude/manual-testing.md` — the replication section (~line 601)

**Interfaces:**
- Consumes: the field's final shape from plans 01–03 — name, env var, CLI flag, default, and
  the validation error text. **Read those plans' committed code rather than this plan's
  description of it**; if they diverge, the code is right and this task documents what shipped.
- Produces: no code. Three docs an operator can grep.

- [ ] **Step 1: Add the row to `docs/config-reference.md`**

The table's columns are field | env var | CLI flag | default | description. Place the row
immediately after `replicaof_auth_password` (line ~34), because the three replication fields
belong together and a reader scanning for replication config should meet all of them at once.

```markdown
| `replica_announce_addr` | `ROCKET_MEM_REPLICA_ANNOUNCE_ADDR` | `--replica-announce-addr` | `addr` | The `host:port` this node advertises to its leader in `PSYNC`, and which the leader reports in `INFO REPLICATION`'s `slaveN:` lines. Defaults to `addr`. Set it when the address a peer must dial differs from the address this node binds — a TLS deployment, or NAT and container port mapping. |
```

Then add a prose paragraph after the validation prose that already covers `replicaof` (near
line 60). It must say three things, because each is a question an operator will otherwise ask:

1. Unset means `addr`, so nothing changes for a deployment that does not set it.
2. It is **not** validated for reachability — only shape (`host:port`, port parses as `u16`).
   This node cannot know how a peer routes to it.
3. Nothing dials this address *today*; it is reported by `INFO REPLICATION`, the `repl` span,
   and the startup banner. Say so plainly, and say that failover tooling is expected to dial
   it — an operator setting it needs to know whether it matters yet.

- [ ] **Step 2: Add the row to `README.md`'s Configuration table**

Same field, shorter description — the README's table is a summary and its rows are one line.
Match the surrounding rows' brevity; `log_value_max_bytes`'s row at line ~277 is the model.

- [ ] **Step 3: Document it in `.claude/manual-testing.md`**

The replication section (~line 601) already tells the reader to set
`replicaof_auth_username`/`replicaof_auth_password` when the leader has ACL users. Add the
announce address alongside it, with a worked TLS example — this is the file someone reads when
a replica is behaving strangely, and the symptom that brought us here appears here first:

```
INFO conn{... protocol=RESP tls=true}:repl{host_port=numericlabs.lxd:6479}: replica registered
```

Explain that the connection is TLS while the announced address is the plaintext port, that both
are accurate, and that `replica_announce_addr = "numericlabs.lxd:16479"` is what makes the
reported address match the transport.

- [ ] **Step 4: Verify**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three green, **test count unchanged** from plan 03's. A documentation task that
changes the test count has touched code it should not have.

Then confirm the docs agree with the code rather than with this plan:

```bash
rg -n "replica_announce_addr|REPLICA_ANNOUNCE_ADDR|--replica-announce-addr" docs/ README.md .claude/ crates/server/src/config.rs
```

Every spelling of the name must match `config.rs` exactly — a documented env var that differs
by one character from the real one is worse than no documentation, because it fails silently.

**Commit.**

---

### Task 2: Record the TLS gap in the sentinel-failover spec

**Files:**
- Modify: `docs/superpowers/specs/2026-09-09-sentinel-failover-spec.md`
- Modify: `rocket-mem-shard-{a,b,c}-replica.toml` — the commented example

**Interfaces:**
- Consumes: nothing. This is a note recording a gap found while building something else.
- Produces: no code.

**Why this task exists.** `2026-09-09-sentinel-failover-spec.md` (lines 25–32) has
`rocket-sentinel` using `INFO REPLICATION`'s `slaveN:ip=…` lines as its discovery mechanism,
with each sentinel "probing every node" and issuing `REPLICAOF` for promotion. Grepping that
spec for `tls` returns **zero matches**. So a sentinel would dial whatever address a replica
announced — and on a TLS-only node, a plaintext announced address means the probe either fails
or connects in the clear. That is a security-relevant gap in a spec that is still a draft,
which is the cheapest possible moment to record it.

- [ ] **Step 1: Add the note to the sentinel spec**

Add it to that spec's own deferred/open-questions section rather than inventing a new
top-level heading — match how that document already records things it has not decided. Verify
the current section structure before writing; **do not restructure the file**.

The note must state, without deciding anything:

- The sentinel design dials addresses that replicas announce about themselves.
- What is announced is now controlled by `replica_announce_addr`, defaulting to `addr` —
  cross-reference [`../../specs/2026-09-10-replica-announce-addr-spec.md`](../../specs/2026-09-10-replica-announce-addr-spec.md).
- The spec has no TLS decision for the sentinel control plane: whether probes speak TLS,
  whether they authenticate, and what a sentinel does when a replica announces an address whose
  protocol it cannot determine. An address string carries no protocol marker.
- This blocks nothing today, because nothing dials the announced address yet.

**Do not design the answer.** Naming an open question is the deliverable; deciding it needs its
own spec and is explicitly out of scope for this one.

- [ ] **Step 2: Add the commented example to the replica TOMLs**

All three `rocket-mem-shard-{a,b,c}-replica.toml` files are TLS replicas that announce their
plaintext address — the exact case this series exists for. Add a **commented-out** line near
`replicaof`, with a sentence saying what it does and why it is commented:

```toml
# The address this node announces to its leader, reported by the leader's INFO REPLICATION.
# Unset means `addr` above -- the plaintext port -- even though replication itself runs over
# TLS. Uncomment to announce the TLS address instead.
#replica_announce_addr = "numericlabs.lxd:16479"
```

Use each file's own `tls_resp_addr` value: `16479` for shard-a, `16480` for shard-b, `16481`
for shard-c. **Read each file to confirm** rather than assuming the pattern — these are live
deployment configs.

**Leave it commented.** These files drive a running deployment; uncommenting changes what three
live replicas report. That is the operator's call, not this plan's. (The same files recently
carried `cluster_config`/`cluster_node_id` accidentally left uncommented, which would have made
each replica claim its leader's slots — this plan does not repeat that mistake.)

- [ ] **Step 3: Verify**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: green, test count unchanged.

Then confirm the TOMLs still parse — a stray edit to a live config is the failure mode here:

```bash
rg -n "^replica_announce_addr" rocket-mem-shard-a-replica.toml rocket-mem-shard-b-replica.toml rocket-mem-shard-c-replica.toml
```

Expected: **no output.** Every added line must still be commented. If this prints anything, a
live replica's announced address has changed.

**Commit.**

---

## Next plan

None — this is the final plan in the series.

Deliberately left undone, and named here so it is not mistaken for an oversight:

- **The sentinel control plane's TLS and authentication design.** Recorded as an open question
  by Task 2; it needs its own spec.
- **Anything dialing the announced address.** This series makes the value trustworthy and
  documents it. Adding a consumer is the failover work's job.
- **A second address on `ClusterNode`.** Cluster topology answers a client-facing question —
  "where does a client find this slot owner" — which is not the same question as "where does a
  peer reach this replica". The spec's Rejected section records why deriving one from the other
  fails, chiefly that a replica's `cluster_node_id` names the node it replicates, so its own
  topology entry resolves to its leader's address.
