# Doc Sample Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** plans 01-14 in this folder change `INFO REPLICATION`'s wire format (offsets, per-replica
`offset=`/`lag=`) and the three `CLUSTER *` reply builders (real `connected`/`disconnected`,
`master,fail?`, `cluster_state:fail`). `docs/qa-playbook.md`, `README.md`, and (per Task 3's sweep)
no other tracked doc currently contain verbatim sample output and explicit known-limitation prose
that assumed the *old* format and the *old* "everything always reports healthy" behavior. Nothing
in this folder owns fixing that, and none of it is caught by CI — the docs would simply become
false the moment plans 01-14 land. This plan makes every stale sample and claim match the new
behavior, in a form a `grep`/`rg` command can verify mechanically.

**Architecture:** this is a documentation-only plan — no source file changes, no new tests. Each
task edits committed Markdown, using the exact final wire formats fixed by
[`01-leader-replication-offset.md`](01-leader-replication-offset.md),
[`03-follower-replication-offset.md`](03-follower-replication-offset.md),
[`05-replica-ack-tracking.md`](05-replica-ack-tracking.md), and
[`13-cluster-health-replies.md`](13-cluster-health-replies.md) as ground truth, cross-checked
against the design contract's §2.4 (`INFO` key table) and §2.6 (pfail/fail ruling, the
wire-compatibility divergence). Verification is a `rg` command with a stated expected result, not a
test run — there is no red/green cycle for prose.

**Out of scope, explicitly:** `docs/qa-playbook.html` and `docs/manual.html` are committed HTML
exports (`git log -1 --format=%ad` on each: `docs/manual.html` is dated 2026-09-01, `docs/
qa-playbook.html` 2026-09-02 — both already predate several sprints' worth of source changes, let
alone this folder's). A repo-wide search (`grep -rn "qa-playbook.html\|manual.html" --include="*.sh"
--include="*.yml" --include="Makefile" --include="*.toml" .`) found no generator script, build
rule, or CI step that produces either file — they appear to have been exported by hand once and
committed. Regenerating or hand-patching them is out of scope for this plan: there is no documented
process to regenerate them correctly, and hand-patching a stale export would create a second,
divergent copy of the same content this plan is fixing in the `.md` source. If they need to stay in
sync going forward, that is a separate, new piece of tooling work, not a doc-sample fix.

**`.claude/manual-testing.md` checked and found clean.** It already gained its own new sections from
plans 09 (`## Replica fencing (min-replicas-to-write)`) and 14 (the cluster-health timers and
kill-a-shard walkthrough), written against the *new* format from the start. A search for verbatim
`INFO`/`CLUSTER` sample blocks elsewhere in the file (`grep -n "role:master\|role:slave\|CLUSTER
NODES\|CLUSTER INFO\|CLUSTER SHARDS" .claude/manual-testing.md`) turns up only inline comments after
a command — e.g. `redis-cli -p 6399 info replication     # role:master` and `redis-cli -p 6401 info
replication      # role:slave, master_link_status:up` — never a fenced block of full output, and
never a claim that no offset field exists. Those comments stay true under the new format (a leader
genuinely does report `role:master`; nothing there asserts *only* `role:master` and nothing else).
No location in this file needs a content change; Task 3's sweep includes it only to prove that by
running its `rg` check, not by editing it.

**Tech Stack:** none — Markdown edits and `rg`/`grep` verification only.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md)

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) in full before editing anything.** §2.4
  fixes the exact `INFO REPLICATION` key names and the per-replica `slaveN:...offset={ack},lag=
  {secs}` format; §2.6 fixes the `CLUSTER *` state mapping and is explicit that
  `cluster_slots_fail` stays `0` forever and `cluster_state:fail` is report-only (routing is
  unchanged).
- **This is a documentation plan. Do not fake a TDD cycle.** There is no failing test to write.
  Each step below is: quote the exact current text, replace it with the exact corrected text, then
  run a stated `rg`/`grep` command and confirm it returns the stated result. That command *is* this
  plan's red/green check.
- **No placeholders.** Every edit in this plan already shows the real current text (verified against
  the file on disk as of 2026-09-10) and the real replacement text. If a line number drifts before
  this plan is executed, re-find the anchor text with `rg` rather than trusting the stated line
  number — every edit below is anchored on a quoted string for exactly this reason.
- **Preserve "no cluster bus or gossip."** It stays true after plans 12-14 land: the prober added by
  plan 12 is point-to-point liveness probing between this node and each configured peer, not gossip
  — there is still no inter-node agreement on anything, which is exactly why `cluster_slots_fail` is
  structurally always `0` (contract §2.6). Only the *consequence* — "every node always reports
  connected, `cluster_state` is always `ok`" — is what plans 12-14 make false. Do not delete the
  "no cluster bus or gossip" claim; correct only the consequence, and add the wire-compatibility
  divergence (`cluster_state:fail` is report-only; `-MOVED`/`cluster_redirect` keep routing to the
  configured owner even when it is known-dead).
- **Comment style** (project `CLAUDE.md`): short, easy, full sentences ending in a punctuation mark.
  No emojis.
- Two known-limitation claims turned up during verification that were **not** in the original scope
  handed to this plan, and both are fixed here because they are the same class of problem:
  - `docs/qa-playbook.md`'s known-limitations table, the "Replication lag metric" row (originally at
    line 4982) — falsified by chain A (plans 01, 03, 05) and by plan 09's
    `rocket_mem_good_replicas` gauge.
  - `README.md`'s "Full resync only" bullet (originally lines 376-377) — its second sentence ("there
    are no replication offsets, and therefore no true replication-lag metric") is falsified by the
    same chain; its first sentence ("full resync only", no partial resync) **stays true** per
    contract §2.1 and must not be deleted.
- `ReplicationHandle`/`ReplicaRegistry` metric names used in this plan's corrected text are limited
  to what plans 01-14 actually implement: `rocket_mem_master_repl_offset`,
  `rocket_mem_slave_repl_offset`, `rocket_mem_good_replicas`,
  `rocket_mem_writes_rejected_no_replicas_total`, `rocket_mem_cluster_peers_reachable`,
  `rocket_mem_cluster_peers_unreachable`. **Do not** write `rocket_mem_replica_min_ack_offset` into
  any doc text — it is named in the design contract's §2.4 metric table but is not actually added by
  any of plans 01-14 (verified by `grep -rn "replica_min_ack_offset" docs/superpowers/plans/
  2026-09-09-failover-safety-primitives/*.md`, which matches only `00-design-contract.md` itself).
  Documenting a metric that does not exist would just be a new instance of the exact problem this
  plan exists to fix.

---

### Task 1: `docs/qa-playbook.md` — `INFO replication` samples and the stale lag-metric limitation

**Files:**
- Modify: `docs/qa-playbook.md`

**Interfaces:**
- Consumes: the final `INFO REPLICATION` format fixed by plan 01 Task 3 Step 3 (leader:
  `role:master`/`master_repl_offset`/`connected_slaves`/per-replica `slaveN:` line, in that order —
  plan 05 Task 3 Step 4 is the plan that puts `master_repl_offset` directly after `role:master`,
  ahead of `connected_slaves`, superseding plan 01's original placement after the slave lines) and
  by plan 03 Task 2 Step 3 (follower: `role:slave`, optional `master_host`/`master_port`,
  `master_link_status`, then `slave_repl_offset`/`master_repl_offset` — the same value under both
  keys), and plan 05 Task 3 Step 4's per-replica `slave{i}:ip={ip},port={port},state=online,
  offset={ack},lag={secs}` line (`lag=-1` for a replica that has never acked).
- Produces: four corrected `INFO replication` sample blocks (SMOKE-09, REPL-04, REPL-05, OBS-02)
  and one corrected known-limitations table row.

- [ ] **Step 1: SMOKE-09 — leader with no replicas ever connected**

Current text (the whole `**Expected:**` block):

```
**Expected:**
```
# Replication
role:master
connected_slaves:0
```
```

Replace with:

```
**Expected:**
```
# Replication
role:master
master_repl_offset:0
connected_slaves:0
```
```

`master_repl_offset:0` because SMOKE-01's server has taken no writes by this point in the sequence
(SMOKE-02 through SMOKE-08 exercise `GET`/`INFO server`/etc., not `SET`); a byte counter that only
advances on writes is honestly `0` here.

- [ ] **Step 2: REPL-04 — both sides of an attached leader/follower pair**

Current text (the whole `**Expected:**` block):

```
**Expected:**
```
# Replication
role:master
connected_slaves:1

# Replication
role:slave
master_host:127.0.0.1
master_port:6560
master_link_status:up
```
```

Replace with:

```
**Expected:**
```
# Replication
role:master
master_repl_offset:<n>
connected_slaves:1
slave0:ip=127.0.0.1,port=6562,state=online,offset=<n>,lag=<n>

# Replication
role:slave
master_host:127.0.0.1
master_port:6560
master_link_status:up
slave_repl_offset:<n>
master_repl_offset:<n>
```
```

`<n>` rather than a literal number, matching this file's own existing convention for a value that
is real but not worth pinning exactly (see SMOKE-08's `process_id:<pid>` and
`uptime_in_seconds:<n>` a few sections up) — `master_repl_offset` here is the summed encoded byte
length of every write REPL-01/02 already issued (`preexisting`, `foo`, `livekey`), a real, derivable
number that is not useful to hardcode in a QA doc that will drift the moment a command in an earlier
step changes. `slave0`'s `port=6562` is the follower's own advertised `ROCKET_MEM_ADDR`, not the
connection's ephemeral source port (contract §2.4's `INFO` key table; `ReplicationHandle::own_addr`'s
doc comment). The follower's ack should already have landed by REPL-04 — plan 06 sends the first
`REPLCONF ACK` immediately on attach, not after waiting out the full one-second interval — so
`offset`/`lag` here are real values, not the `offset=0,lag=-1` "never acked" sentinel.

- [ ] **Step 3: REPL-05 — `REPLICAOF NO ONE` promotes back to a fresh master**

Current text (the whole `**Expected:**` block):

```
**Expected:**
```
OK
# Replication
role:master
connected_slaves:0
OK
yes
```
```

Replace with:

```
**Expected:**
```
OK
# Replication
role:master
master_repl_offset:0
connected_slaves:0
OK
yes
```
```

`master_repl_offset:0`, not `<n>`: `master_repl_offset` only advances inside the leader's own
broadcast fan-out loop (plan 01 Task 2), which a node never runs while it is a replica — the counter
this node accumulated as a *follower* is `slave_repl_offset`, a different field, not reported at all
once `role:master` takes over. A node that has just been promoted and has taken no writes yet as a
master genuinely reports `master_repl_offset:0`.

- [ ] **Step 4: OBS-02 — same leader-with-no-replicas case as SMOKE-09**

Current text (the whole `**Expected:**` block, including the trailing blank line before the closing
fence):

```
**Expected:**
```
# Replication
role:master
connected_slaves:0

```
```

Replace with:

```
**Expected:**
```
# Replication
role:master
master_repl_offset:0
connected_slaves:0

```
```

- [ ] **Step 5: the "Replication lag metric" known-limitations row**

Current text (one table row):

```
| Replication lag metric | No true replication-offset lag metric. Reported metric `rocket_mem_replication_last_apply_timestamp_seconds` measures apply time, not offset distance. | Full-resync-only design means no offsets exist. Timestamp is the honest substitute for offset-based lag. |
```

Replace with:

```
| Replication lag metric | Superseded: replication offsets now exist. `INFO REPLICATION` reports `master_repl_offset`/`slave_repl_offset` on both roles, and each `slaveN:` line carries `offset=<n>,lag=<secs>` (`lag=-1` for a replica that has never acked). Prometheus exports `rocket_mem_master_repl_offset`, `rocket_mem_slave_repl_offset`, and `rocket_mem_good_replicas`. `rocket_mem_replication_last_apply_timestamp_seconds` still exists alongside them as a coarser wall-clock signal. | Added by the failover-safety-primitives work (`docs/superpowers/plans/2026-09-09-failover-safety-primitives/`, chain A — see `00-design-contract.md` §2.1-§2.4). |
```

- [ ] **Step 6: verify**

Run:
```bash
rg -n -A1 '^role:master$' docs/qa-playbook.md
```
Expected: every `role:master` match's context line (the line immediately after it) now starts with
`master_repl_offset:` — never `connected_slaves:` directly. Concretely, this must return **zero
hits**:
```bash
rg -n -A1 '^role:master$' docs/qa-playbook.md | rg 'connected_slaves'
```
(the old shape, `role:master` directly followed by `connected_slaves:`, is gone from every sample).

And confirm the new keys are actually present:
```bash
rg -c 'master_repl_offset:' docs/qa-playbook.md
rg -c 'slave_repl_offset:' docs/qa-playbook.md
rg -n 'No true replication-offset lag metric' docs/qa-playbook.md
```
Expected: the first two report a count of at least `4` and `1` respectively (one `master_repl_offset`
per corrected sample, plus the OBS-02/REPL-05/SMOKE-09 duplicates, plus REPL-04's follower side; one
`slave_repl_offset` from REPL-04's follower side); the third returns **no output** — the stale
claim is gone.

- [ ] **Step 7: commit**

```bash
git add docs/qa-playbook.md
git commit -m "$(cat <<'EOF'
Reconcile qa-playbook.md's INFO replication samples with the new offset fields

SMOKE-09, REPL-04, REPL-05, and OBS-02's expected INFO replication
output predates plans 01/03/05 in this folder, which add
master_repl_offset/slave_repl_offset and per-replica offset=/lag=
fields. Also corrects the "Replication lag metric" known-limitations
row, which claimed no true offset-based lag metric exists -- it now
does.
EOF
)"
```

---

### Task 2: `docs/qa-playbook.md` — the cluster sample and its two falsified known-limitation claims

**Files:**
- Modify: `docs/qa-playbook.md`

**Interfaces:**
- Consumes: plan 13's `cluster_nodes_text`/`cluster_shards_reply`/`cluster_info_text` (real
  `connected`/`disconnected`, `master,fail?`, `cluster_state:ok`/`fail`, `cluster_slots_pfail`,
  `cluster_slots_fail` always `0`) and contract §2.6's pfail/fail ruling and wire-compatibility
  divergence.
- Produces: an updated CLUSTER-03 note explaining that `connected`/`cluster_state:ok` are now live
  probe results (not hardcoded literals), a corrected CLUSTER-06 known-limits note, and a corrected
  "Cluster gossip" table row.

- [ ] **Step 1: CLUSTER-03's notes — `connected`/`cluster_state:ok` are now live, not hardcoded**

Current text:

```
**Notes:** `foo` hashes to slot 12182, owned by shard-c — used as the MOVED example in
CLUSTER-04. `CLUSTER NODES`'s `@17101` cluster-bus port suffix is advertised by convention only;
nothing is ever bound there (no cluster bus exists — see CLUSTER-06).
```

Replace with:

```
**Notes:** `foo` hashes to slot 12182, owned by shard-c — used as the MOVED example in
CLUSTER-04. `CLUSTER NODES`'s `@17101` cluster-bus port suffix is advertised by convention only;
nothing is ever bound there (no cluster bus exists — see CLUSTER-06). `connected` and
`cluster_state:ok` above are live liveness-probe results, not hardcoded literals: each node probes
every other configured peer once per `cluster_probe_interval_secs` (default 1s) and reports
`disconnected`/`master,fail?`/`cluster_state:fail` once a peer misses `cluster_node_timeout_secs`
(default 15s) of probes. See CLUSTER-06 for what a genuinely dead peer looks like here.
```

- [ ] **Step 2: CLUSTER-06's known-limits note**

Current text:

```
**Notes — known limits to expect, not bugs:** no cluster bus or gossip — every node always
reports every configured node `connected` and `cluster_state:ok` regardless of whether the other
processes are even running, because the topology is a static file, not a live membership
protocol; no live resharding or failover — `CLUSTER SETSLOT`, `MIGRATE`, `ASK`/`ASKING` do not
exist as commands at all; no request forwarding — a `MOVED` reply is final, the client must
reconnect itself, this server never proxies a request to another shard on the client's behalf;
`CLUSTER SLOTS` is not implemented (deprecated upstream since Redis 7.0 in favor of
`CLUSTER SHARDS`, which is implemented — see CLUSTER-03).
```

Replace with:

```
**Notes — known limits to expect, not bugs:** no cluster bus and no gossip — nodes never agree
with each other on anything, so `cluster_slots_fail` is structurally always `0` and one node's
suspicion of a dead peer can never be promoted to an agreed failure; each node does, however,
probe its peers directly (`cluster_probe_interval_secs`/`cluster_node_timeout_secs`), so
`CLUSTER NODES` reports a peer that stops answering as `disconnected`/`master,fail?` and
`cluster_state` flips to `fail`, purely on this node's own observation — see CLUSTER-03's notes.
`cluster_state:fail` is report-only here: unlike real Redis, this node keeps serving its own slots
and a `MOVED` reply still points at the configured (possibly dead) owner, because picking a
different owner is a topology decision nothing here can agree on; no live resharding or failover —
`CLUSTER SETSLOT`, `MIGRATE`, `ASK`/`ASKING` do not exist as commands at all; no request
forwarding — a `MOVED` reply is final, the client must reconnect itself, this server never proxies
a request to another shard on the client's behalf; `CLUSTER SLOTS` is not implemented (deprecated
upstream since Redis 7.0 in favor of `CLUSTER SHARDS`, which is implemented — see CLUSTER-03).
```

- [ ] **Step 3: the "Cluster gossip" known-limitations table row**

Current text (one table row):

```
| Cluster gossip | No cluster bus and no gossip. Nodes never talk to each other. Every configured node reports as `connected` and `cluster_state` is always `ok`. | Static config file design: cluster membership is fixed at process start, not dynamic. Honest answers would require inter-node communication, which is out of scope. |
```

Replace with:

```
| Cluster gossip | No cluster bus and no gossip: nodes never agree with each other on anything, so `cluster_slots_fail` is structurally always `0`. Each node does directly probe its peers, though, so `CLUSTER NODES`/`SHARDS`/`INFO` report `disconnected`/`master,fail?`/`cluster_state:fail` based on that node's own observation — see CLUSTER-06's notes. `cluster_state:fail` is report-only: this node keeps serving its own slots and `cluster_redirect` still points at the configured owner even when it is known-dead. | Static config file design: cluster membership is fixed at process start, not dynamic. A per-node liveness probe (added by the failover-safety-primitives work, `docs/superpowers/plans/2026-09-09-failover-safety-primitives/`) makes reporting honest without adding a cluster bus, quorum, or automatic failover. |
```

- [ ] **Step 4: verify**

```bash
rg -n 'reports every configured node .connected. and .cluster_state:ok. regardless' docs/qa-playbook.md
rg -n 'Every configured node reports as .connected. and .cluster_state. is always .ok' docs/qa-playbook.md
```
Expected: **zero hits**, both commands — the two falsified claims are gone.

```bash
rg -c 'cluster_slots_fail. is structurally always .0.|structurally always .0.' docs/qa-playbook.md
rg -n 'master,fail\?' docs/qa-playbook.md
```
Expected: the first reports at least `2` (CLUSTER-06's note and the table row); the second returns
at least one hit — the new pfail flag is now documented somewhere in the file.

- [ ] **Step 5: commit**

```bash
git add docs/qa-playbook.md
git commit -m "$(cat <<'EOF'
Reconcile qa-playbook.md's cluster notes with the new peer prober

CLUSTER-06's known-limits note and the "Cluster gossip" table row
claimed every configured node always reports connected and
cluster_state is always ok. Plan 13 in this folder makes that false:
each node now directly probes its peers and reports a dead one as
disconnected/master,fail?/cluster_state:fail. "No cluster bus or
gossip" stays true -- there is still no inter-node agreement, which
is why cluster_slots_fail stays structurally 0 -- only the
consequence changes. CLUSTER-03's notes gain a pointer explaining the
sample's connected/cluster_state:ok are now live probe results, not
hardcoded literals.
EOF
)"
```

---

### Task 3: `README.md`'s cluster caveat, plus a repo-wide sweep for anything else stale

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: the same plan 13 / contract §2.6 ground truth as Task 2, plus contract §2.1 for why
  "full resync only" itself must **not** be deleted even though the offset claim beside it must be.
- Produces: a corrected "No cluster bus or gossip" bullet, a corrected "Full resync only" bullet, and
  a repo-wide `rg` sweep proving no other tracked doc still asserts either removed limitation.

- [ ] **Step 1: the cluster-gossip bullet**

Current text (`README.md`, in the `## Limitations` section):

```
- **No cluster bus or gossip.** Nodes never talk to each other, so `CLUSTER NODES` reports every
  configured node as connected and `cluster_state` is always `ok`.
```

Replace with:

```
- **No cluster bus or gossip.** Nodes never agree with each other on anything, so
  `cluster_slots_fail` is structurally always `0` and one node's suspicion of a dead peer can
  never be promoted to an agreed failure. Each node does directly probe its peers, though, so
  `CLUSTER NODES` reports a peer that stops answering as `disconnected`/`master,fail?` and
  `cluster_state` flips to `fail`, based purely on that node's own observation.
  `cluster_state:fail` is report-only: this node keeps serving its own slots and a `-MOVED` reply
  still points at the configured (possibly dead) owner, since picking a different owner is a
  topology decision nothing here can agree on.
```

- [ ] **Step 2: the full-resync/lag bullet**

Current text:

```
- **Full resync only.** A dropped follower connection triggers a complete resnapshot; there are
  no replication offsets, and therefore no true replication-lag metric.
```

Replace with:

```
- **Full resync only.** A dropped follower connection triggers a complete resnapshot; there is no
  partial resync and no cross-restart offset persistence. Replication offsets do exist, though —
  `INFO REPLICATION` reports `master_repl_offset`/`slave_repl_offset` plus each replica's acked
  offset and lag, and Prometheus exports `rocket_mem_master_repl_offset`,
  `rocket_mem_slave_repl_offset`, and `rocket_mem_good_replicas`.
```

"Full resync only" itself is preserved verbatim — contract §2.1 is explicit that the offset is
process-local with no partial-resync capability, and adding cross-restart persistence would falsely
imply one. Only the second sentence's claim that no offsets exist at all is removed.

- [ ] **Step 3: repo-wide sweep — prove no other tracked doc still asserts either removed limitation**

Run, from the repo root:

```bash
git grep -n -E "reports every configured node|every configured node reports as|no replication offsets, and therefore no true replication-lag|No true replication-offset lag metric" -- '*.md' ':!docs/superpowers/'
```

Expected: **no output.** (`docs/superpowers/` is excluded because it holds this folder's own plans
and the sprint-6 planning history that documented the *pre-prober* behavior truthfully at the time
it was written — those are point-in-time records of an already-executed sprint, not claims about
today's shipped system, and rewriting history is not this plan's job.)

Also confirm the two files this plan actually touches carry the corrected language:

```bash
git grep -c "cluster_slots_fail. is structurally always .0.\|structurally always .0." README.md docs/qa-playbook.md
```

Expected: both files report a count of at least `1`.

Finally, confirm `.claude/manual-testing.md` has nothing to fix (see this plan's Architecture
section for why):

```bash
grep -n "role:master\|role:slave\|CLUSTER NODES\|CLUSTER INFO\|CLUSTER SHARDS" .claude/manual-testing.md
```

Expected: only inline `# comment` annotations after a `redis-cli` command line — no fenced block of
full `INFO`/`CLUSTER` output, and no line asserting that no offset field or health field exists. If
this ever turns up a real sample block (e.g. after a future plan adds one), fix it in this same
task before committing.

- [ ] **Step 4: commit**

```bash
git add README.md
git commit -m "$(cat <<'EOF'
Reconcile README's cluster and replication-lag limitations

"No cluster bus or gossip" incorrectly implied every node always
reports connected and cluster_state:ok -- plan 13 in this folder adds
a peer prober that makes that false. The claim itself (no inter-node
agreement, so cluster_slots_fail is structurally always 0) is
preserved; only the stale consequence is corrected, plus the
wire-compatibility divergence (cluster_state:fail is report-only).
"Full resync only" similarly claimed no replication offsets exist at
all -- chain A (plans 01/03/05) adds them. The full-resync claim
itself stays true and is preserved verbatim.

A repo-wide sweep (git grep, excluding docs/superpowers/'s own
point-in-time planning history) confirms no other tracked doc still
asserts either removed limitation.
EOF
)"
```

---

## Next plan

**None — plan 15 is the last plan in this folder**, and with it every chain (A: 01-06, B: 07-09,
C: 10-11, D: 12-14) plus the documentation debt those chains created is closed out.

What deliberately remains unbuilt, and must stay that way until someone re-scopes the spec:

- **Automatic promotion.** No plan in this folder promotes a replica. The spec's central finding is
  that automatic failover on this project's replication would silently discard acknowledged writes
  — strictly worse than today's honest "no failover." Offsets (chain A) and `min-replicas-to-write`
  fencing (chain B) are prerequisites that make a *correct* promotion designable later; they are not
  a promotion.
- **Automated cluster-topology reconciliation.** Nothing updates any node's topology when a replica
  is promoted, manually or otherwise. `ClusterConfig` is still parsed once at startup and never
  mutated. Restoring cluster-wide write access after any promotion is still: hand-edit
  `cluster.conf` on every node, restart every node.
- **Automatic client-redirect on failover**, and **embedded consensus / Raft**.

All four are named in the spec's
[Non-goals](../../specs/2026-09-09-sentinel-failover-spec.md#non-goals-for-v1-and-likely-much-later)
section and in the design contract's §0. Anyone extending the work in this folder toward any of the
four should re-run the scoping first, not treat this plan chain as a foundation to build them on
directly.
