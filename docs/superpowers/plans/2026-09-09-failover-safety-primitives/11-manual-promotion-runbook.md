# Manual Promotion Runbook Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `.claude/runbook-failover.md`, the operator-followable procedure for a manual failover on this project's real 3-shard + 3-replica deployment — confirming a leader is actually dead (not just unreachable), picking the most-caught-up replica with the new offset fields, promoting it, re-pointing survivors, the cluster-mode topology surgery this deployment's clustered shape requires (hand-editing `cluster.conf` on every node and restarting every node), the failback hazard, and the split-brain warning. This is pure documentation — there is no code to write — but it is verified for real, against the real running deployment, not merely proofread.

**Architecture:** one Markdown file at `.claude/runbook-failover.md`, cross-referencing `.claude/manual-testing.md` (voice and existing sections) and `scripts/replication-health.sh` (plan 10's alerting probe, used to gather evidence in Section 1 but never to act). It is written in three passes: Task 1 covers the replication-only half of a failover (confirm, pick, promote, re-point), Task 2 covers the cluster-mode-specific half (topology surgery, failback hazard, split-brain), and Task 3 is a real, live dry run of the whole thing against the actual `numericlabs.lxd` 3-shard deployment, correcting anything the drill finds inaccurate.

**Tech Stack:** Markdown, `redis-cli` (with `--tls`/`--cacert`/`--user`/`--pass`, matching this deployment's TLS+ACL configuration), `systemctl --user` (the real supervision mechanism for the three leader shards in this deployment).

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md)

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) first, in full.** It is normative; where it and this plan disagree, it wins and the disagreement is a bug worth reporting before writing anything. This plan assumes chains A (01-06), B (07-09), and plan 10 (the health probe) have already landed.
- **This runbook is a human procedure, not automation.** It exists precisely because this project has no automatic failover, no quorum, and no live cluster-topology reload — see the spec's "Decision: v1 is not failover" section. Nothing in it should read as "and then a script does the rest."
- **Do not write aspirational instructions for features that don't exist.** `CLUSTER NODES` reports every configured node as connected regardless of whether it is actually reachable (see the spec's "zero health-awareness" finding, live-verified 2026-09-09) — the runbook must say so plainly rather than imply `CLUSTER NODES` can be trusted as a liveness signal. There is no live topology reload in `cluster.rs` — every cluster-mode step requires a full restart of every cluster member, with no shortcut.
- **Per `CLAUDE.md`, project documentation belongs in `.claude/` unless told otherwise.** `.gitignore` currently blanket-ignores `.claude/*` with a single explicit exception for `manual-testing.md` (`.gitignore:29-30`). This runbook needs its own exception line or it is silently untracked forever, no matter how many times it gets `git add`ed — Task 1 adds that line before it ever tries to commit the runbook itself.
- **Use the real deployment's real values, not placeholders.** TLS ports 16379/16380/16381 (leaders) and 16479/16480/16481 (replicas), ACL user `app` / password `changeme` (exactly what is checked into `rocket-mem.toml`/`rocket-mem-shard-*.toml`/`rocket-mem-shard-*-replica.toml` today), the CA path `/home/numericlabs/data/tls/root_ca-numericlabs.crt` (from the replicas' own `tls_ca_path`), and `systemctl --user restart rocket-mem-shard-{a,b,c}` for the three leaders — which are the only three nodes in this deployment under systemd today; the three replicas have no unit and are started by hand.
- **Comment style / prose style** (project `CLAUDE.md`): short, easy, full sentences ending in a punctuation mark. No emojis.
- Commit through the `1-git-commit` skill, this project's standing convention for Superpowers-driven commits — not a freeform `git commit -m`.
- The three CI gates below are run once per task as a repo-wide safety net even though this plan touches no Rust:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```

---

### Task 1: `.gitignore` exception, and Sections 1-4 of the runbook

**Files:**
- Modify: `.gitignore`
- Create: `.claude/runbook-failover.md`

**Interfaces:**
- Produces: `.claude/runbook-failover.md` — a title/intro block, a topology reference table for the real deployment, and Sections 1-4 ("Confirm the leader is actually dead", "Pick the most-caught-up replica", "Promote it: `REPLICAOF NO ONE`", "Re-point surviving followers"). Task 2 appends Sections 5-7 to this same file.
- Consumes: `scripts/replication-health.sh` (plan 10, referenced but never modified here); the `INFO REPLICATION` `slave_repl_offset`/`master_link_status` fields (chains A/B); the real topology in `cluster.conf`/`rocket-mem*.toml`.

- [ ] **Step 1: Add the `.gitignore` exception**

In `.gitignore`, the existing exception block currently reads:

```
.claude/*
!.claude/manual-testing.md
```

Change it to:

```
.claude/*
!.claude/manual-testing.md
!.claude/runbook-failover.md
```

- [ ] **Step 2: Verify the exception works before anything depends on it**

Run: `git status --porcelain .claude/ 2>&1; git check-ignore -v .claude/runbook-failover.md; echo "check-ignore exit=$?"`
Expected: the file does not exist yet, so `git status` shows nothing for it and `git check-ignore` reports it is not ignored — `check-ignore exit=1` (git's convention: exit 1 means "not ignored"). If it printed a match against `.claude/*` with exit 0, the exception line was not written correctly; fix it before proceeding.

- [ ] **Step 3: Write `.claude/runbook-failover.md` (Sections 1-4)**

Create `.claude/runbook-failover.md`:

```markdown
# rocket-mem Manual Failover Runbook

**Read `docs/superpowers/specs/2026-09-09-sentinel-failover-spec.md` first if you have not.**
rocket-mem has no automatic failover, no quorum, and no live cluster-topology reload. Every
step below is something a human runs by hand, in order, using judgment at the points marked
as such. **There is no "just run this script" version of this runbook, on purpose** —
`scripts/replication-health.sh` alerts on the conditions below but never acts on them; see
`docs/superpowers/plans/2026-09-09-failover-safety-primitives/10-replication-health-probe.md`.

This runbook assumes the real 3-shard + 3-replica deployment described in `cluster.conf`,
`rocket-mem.toml` / `rocket-mem-shard-{b,c}.toml`, and
`rocket-mem-shard-{a,b,c}-replica.toml` at the repo root, and cross-references
`.claude/manual-testing.md` throughout — read that file's "Replication" and "Cluster mode"
sections first if anything below is unfamiliar.

**Today's topology, for reference:**

| Node | Role | TLS RESP addr | Plaintext addr | systemd unit |
|---|---|---|---|---|
| shard-a | leader | numericlabs.lxd:16379 | numericlabs.lxd:6379 | `rocket-mem-shard-a.service` |
| shard-a-replica | follower of shard-a | numericlabs.lxd:16479 | numericlabs.lxd:6479 | none — started by hand (see below) |
| shard-b | leader | numericlabs.lxd:16380 | numericlabs.lxd:6380 | `rocket-mem-shard-b.service` |
| shard-b-replica | follower of shard-b | numericlabs.lxd:16480 | numericlabs.lxd:6480 | none — started by hand |
| shard-c | leader | numericlabs.lxd:16381 | numericlabs.lxd:6381 | `rocket-mem-shard-c.service` |
| shard-c-replica | follower of shard-c | numericlabs.lxd:16481 | numericlabs.lxd:6481 | none — started by hand |

ACL user `app` / password `changeme` (rotate this in a real deployment; it is what is
actually configured in the checked-in `.toml` files today) is required on every node. TLS
clients need `--tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt`. **The
three replicas have no systemd unit in this deployment today** — they are started by hand
per `.claude/manual-testing.md`'s config-driven-`replicaof` pattern
(`./target/release/rocket-mem --config rocket-mem-shard-a-replica.toml &`, or under
`nohup`/equivalent supervision of your choosing). Keep this in mind below: "restart" for a
leader means `systemctl --user restart`, but for a replica it means stopping the process
and re-running it by hand with whatever config it should come back with.

All three shards in this deployment happen to run on the same host (`numericlabs.lxd`),
distinguished only by port. **A genuine network partition is not a scenario this specific
deployment can experience** — "the leader is unreachable" here almost always means "the
process died or was killed," verifiable directly with `systemctl --user status`. Section 1
below still documents the general multi-host procedure, because a real production
deployment of this project would not share one host, and because the judgment call it
teaches ("unreachable" is not "dead") is the important part regardless of topology.

---

## 1. Confirm the leader is actually dead

**Do not promote a replica because you cannot reach the leader. Confirm the leader is
actually gone, not just unreachable from where you are standing.** This is the single most
important step in this runbook — skipping it is how you create split-brain (see Section 7).

- If you have direct access to the leader's host, check the process itself:
  `systemctl --user status rocket-mem-shard-a` (substitute the affected shard).
  `inactive (dead)` or the unit missing its PID entirely is authoritative — the process is
  gone.
- If you do not have host access, or in a real multi-host deployment, check from more than
  one independent vantage point — another shard's host, a different network path, not just
  your own workstation:
  ```bash
  redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt \
    -h numericlabs.lxd -p 16379 ping
  ```
  A `PONG` from anywhere means the leader is alive — stop, do not promote anything. Silence
  or a connection error from **every** vantage point you can reach is evidence, not proof.
- Check the replica's own view: it has been watching the leader continuously and is the one
  component in the system that already knows.
  ```bash
  redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt \
    --user app --pass changeme -h numericlabs.lxd -p 16479 info replication | grep master_link_status
  ```
  `master_link_status:down` for a *sustained* period (several of the follower's 1-second
  reconnect attempts, not one blip) corroborates the other checks. A single momentary
  `down` is not enough on its own — `sync_once`'s reconnect loop retries every second, so a
  genuinely transient hiccup self-heals before you finish reading this sentence.
- Run `scripts/replication-health.sh` from more than one vantage point if you can:
  ```bash
  scripts/replication-health.sh --nodes scripts/replication-health.nodes.example \
    --tls-ca /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme
  ```
  Consistent `unreachable`/`master_link_status=down` reports across vantage points is
  strong evidence. A report from one place is not — see Section 7.

**The split-brain trap:** if *you* cannot reach the leader but a client somewhere else
still can, the leader is not dead — it is partitioned, and it may still be accepting and
acknowledging writes from clients on its side of the partition. rocket-mem has no
`min-replicas-to-write` fencing unless it was explicitly configured
(`min_replicas_to_write > 0`; see `07-fencing-config.md`/`08-fencing-enforcement.md`), and
even with fencing on, it only stops the old leader once *it* notices its replica count
dropped — that is not instant, and it is not a substitute for this confirmation step. Do
not proceed past this section on "I can't reach it" alone.

---

## 2. Pick the most-caught-up replica

In today's topology, each shard has exactly one replica, so this section is mostly about
confirming that replica is usable, not choosing among candidates. Do the comparison anyway
if a shard is ever scaled to more than one replica.

```bash
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt \
  --user app --pass changeme -h numericlabs.lxd -p 16479 info replication
```

Look at `slave_repl_offset` (this replica's own applied position) and
`master_link_status`. With more than one candidate, pick the one with the **highest**
`slave_repl_offset` — more bytes of the write stream applied means more caught up. A
candidate whose link has been `down` for a long time is likely more stale than one that was
`up` right until the failure, even if their last-known offsets look close; when in doubt,
prefer the one that was `up` more recently.

**Write down the chosen replica's offset before you touch anything.** This number is the
exact boundary of what did not make it: any write the old leader accepted after this point,
and never got to broadcast before it died, is gone, silently, with no error and no log line
— this is precisely what "offset-less/ack-less replication" already told you was possible
(see the spec). The offset tells you *where* the line is; it does not mean there is no loss.

---

## 3. Promote it: `REPLICAOF NO ONE`

```bash
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt \
  --user app --pass changeme -h numericlabs.lxd -p 16479 replicaof no one
```

Verify:

```bash
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt \
  --user app --pass changeme -h numericlabs.lxd -p 16479 info replication | grep role
# -> role:master
```

This node is now a normal, writable, standalone rocket-mem instance. It knows nothing about
cluster mode yet — see Section 5 before routing real cluster traffic to it.

---

## 4. Re-point surviving followers

If the failed shard had more than one replica, point every *other* surviving replica at the
newly-promoted node instead of the dead leader:

```bash
redis-cli --tls --cacert <ca> --user app --pass <pass> -h <survivor> -p <port> \
  replicaof numericlabs.lxd 16479
```

Do this for every remaining replica of the failed shard before moving on — a replica left
pointed at the dead leader spins in `sync_once`'s 1-second reconnect loop forever, and will
not automatically discover the promotion.

**In today's topology this step has nothing to do**, because shard-a has exactly one
replica and it is the node just promoted in Section 3. This section exists for when a shard
gains a second replica; do not skip reading it just because it is currently a no-op.
```

- [ ] **Step 4: Verify the file's content and that it is actually trackable**

Run:
```bash
grep -c "^## " .claude/runbook-failover.md
grep -n "systemctl --user status rocket-mem-shard-a" .claude/runbook-failover.md
grep -n "slave_repl_offset" .claude/runbook-failover.md
grep -n "replicaof no one" .claude/runbook-failover.md
grep -n "16479" .claude/runbook-failover.md
git add -n .claude/runbook-failover.md .gitignore
```
Expected: `grep -c "^## "` reports `4` (Sections 1-4); each other `grep` reports at least one match; `git add -n` (dry run) lists both files as things it *would* add, proving the `.gitignore` exception took effect — if the runbook were still ignored, `git add -n` would silently print nothing for it.

- [ ] **Step 5: Commit**

```bash
git add .gitignore .claude/runbook-failover.md
```
Commit through the `1-git-commit` skill. Suggested subject: `Add the manual failover runbook: confirm, pick, promote, re-point`.

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green (unaffected by this task's doc-only changes).

---

### Task 2: Sections 5-7 — cluster-mode surgery, failback hazard, split-brain

**Files:**
- Modify: `.claude/runbook-failover.md`

**Interfaces:**
- Produces: Sections 5 ("Cluster mode: hand-edit cluster.conf on every node, and restart every node"), 6 ("Failback hazard"), 7 ("Split-brain") appended to the file Task 1 created.
- Consumes: the real `cluster.conf` topology; `ClusterConfig`'s parse-once-at-startup behavior and `cluster_redirect`'s unconditional trust in it (design contract §1.8, §2.6); `sync_once`'s unconditional full resync via `Engine::load_snapshot`/`Store::load_snapshot_entries` (design contract §1.5, and the spec's "Failback silently destroys the promoted node's writes" finding) — all cited as ground truth, not modified by this plan.

- [ ] **Step 1: Write the failing check**

This is a documentation task with no code to run red/green — the "failing" state is simply that Sections 5-7 do not exist yet.

Run: `grep -c "^## 5\.\|^## 6\.\|^## 7\." .claude/runbook-failover.md`
Expected: `0` — none of the three sections exist yet.

- [ ] **Step 2: Append Sections 5-7**

Append to `.claude/runbook-failover.md`:

```markdown

---

## 5. Cluster mode: hand-edit `cluster.conf` on every node, and restart every node

rocket-mem's cluster mode has no live topology-reload path: `ClusterConfig` is parsed once
at startup and, by its own doc comment, "never changes for the life of the process." A
promoted replica's writes stay unroutable cluster-wide until every node's `cluster.conf` is
edited and every node is restarted. There is no partial or gradual version of this step.

**5a. Edit `cluster.conf` identically on every cluster-member host.** This file lives at
`/home/numericlabs/data/rocket/rocket-mem/cluster.conf` (in this single-host deployment
there is only one copy to edit). Change the failed shard's line to the promoted node's
address:

Before:
```
shard-a numericlabs.lxd:16379 0     5460
shard-b numericlabs.lxd:16380 5461  10922
shard-c numericlabs.lxd:16381 10923 16383
```

After:
```
shard-a numericlabs.lxd:16479 0     5460
shard-b numericlabs.lxd:16380 5461  10922
shard-c numericlabs.lxd:16381 10923 16383
```

(`16479` is shard-a-replica's TLS RESP port — `cluster.conf` must advertise the same *kind*
of port every other node advertises, matching the TLS convention
`rocket-mem-shard-b.toml`/`rocket-mem-shard-c.toml`'s own comments already document.)

This file must be byte-for-byte identical on every node. A partial edit leaves nodes
disagreeing about who owns shard-a's slots, and nothing in this project detects that
disagreement — there is no `configEpoch`, no cluster bus, no gossip (see the design
contract, §2.6, and the spec's "cheapest honest first step" paragraph).

**5b. The promoted node was a replica, not a cluster member — give it cluster fields
before restarting it as one.** `rocket-mem-shard-a-replica.toml` has no
`cluster_config`/`cluster_node_id` (replicas in this deployment are deliberately not
cluster members). Left as-is, the promoted node will happily serve direct reads/writes for
shard-a's keys (nothing in rocket-mem enforces slot ownership on a node with no cluster
config at all), but it will not answer `CLUSTER NODES`/`CLUSTER SHARDS` correctly, and it
will not redirect a client that mistakenly sends it a shard-b or shard-c key. Stop the
promoted process and restart it with cluster fields added — either edit a copy of its
config:

```toml
# added to a copy of rocket-mem-shard-a-replica.toml; remove/comment out replicaof and its
# auth fields too -- this node is a promoted standalone leader now, not a follower.
cluster_config = "/home/numericlabs/data/rocket/rocket-mem/cluster.conf"
cluster_node_id = "shard-a"
```

or pass the equivalent CLI flags on its next start:

```bash
./target/release/rocket-mem --config rocket-mem-shard-a-replica.toml \
  --cluster-config /home/numericlabs/data/rocket/rocket-mem/cluster.conf \
  --cluster-node-id shard-a &
```

**5c. Restart the other cluster members so they pick up the edited `cluster.conf`:**

```bash
systemctl --user restart rocket-mem-shard-b rocket-mem-shard-c
```

**5d. Do NOT restart `rocket-mem-shard-a.service`.** That unit still describes the dead
leader's identity — `rocket-mem.toml`, bound to `numericlabs.lxd:16379`/`6379`. Restarting
it would put a second, stale process on the network simultaneously claiming to be part of
this cluster, right alongside the promoted node now actually serving as shard-a. If the
original host is only temporarily down and might come back on its own (this unit's
`WantedBy=default.target` means it auto-starts on the next login/boot), disable it until
you have deliberately decided what to do with it:

```bash
systemctl --user disable --now rocket-mem-shard-a
```

**5e. Verify from a surviving node:**

```bash
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt \
  -h numericlabs.lxd -p 16380 cluster nodes
# -> shard-a's slot range (0-5460) shown against numericlabs.lxd:16479, not :16379

redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt \
  --user app --pass changeme -h numericlabs.lxd -p 16479 set drill-marker post-promotion
# -> OK, direct write to the promoted node succeeds
```

`CLUSTER NODES`/`CLUSTER SHARDS` on this project report every configured node as
`connected`/`online` unconditionally (see the spec's "zero health-awareness" finding,
live-verified 2026-09-09) — a stale entry for the disabled old shard-a would look identical
to a healthy one if you had left its line unedited. The edit in 5a is what actually fixes
routing, not anything `CLUSTER NODES` tells you on its own.

---

## 6. Failback hazard — read this before you ever point anything back at a restored original leader

**Do not run `REPLICAOF numericlabs.lxd 16379` against the promoted node
(`numericlabs.lxd:16479`) once the original shard-a host comes back, expecting to "restore"
the old topology. This destroys every write the promoted node has accepted since
promotion, silently.**

The mechanism: `sync_once` performs a full resync unconditionally on every `REPLICAOF`
connect. It calls `Engine::load_snapshot`, and `Store::load_snapshot_entries` clears all 16
shards before loading the new leader's snapshot. There is no partial sync, no merge, no
conflict detection — the promoted node's entire keyspace is wiped and replaced with
whatever the restored old leader has. No error, no log line, no offset-mismatch warning:
none of that machinery exists in this project today.

If you must fail back, treat it as a second, deliberate migration — never a "revert":

1. Stop writes to the promoted node (application-level, or by disconnecting clients).
2. Decide which side's data should win. It is almost always the promoted node's — it has
   been the live, accepting-writes side since the incident, and the restored original
   leader's data is frozen at the moment it died.
3. If the promoted node's data should win: make the *restored old node* a replica of the
   *promoted node* (`REPLICAOF numericlabs.lxd 16479` run against the restored shard-a, not
   the other way around), then repeat Section 5's cluster.conf-edit-and-restart-every-node
   dance in reverse only once you deliberately decide to switch the "canonical" address
   back — and only if you ever decide to. There is no requirement to ever switch back; the
   promoted node can simply remain shard-a going forward.
4. If the old node's data should win for some reason (rare — this means discarding
   everything accepted since promotion, on purpose): that is exactly what re-pointing the
   promoted node at the restored leader does. Confirm this is really what you want, in
   writing, before running it — this project gives you no undo.

---

## 7. Split-brain — the old leader might not really be dead

This restates and sharpens Section 1's warning, because it is the failure mode this whole
runbook exists to avoid, not just note in passing.

A partitioned-but-still-alive old leader keeps accepting and "succeeding" writes from
clients on its side of the partition indefinitely. There is no quorum in this project, no
consensus, and (unless `min_replicas_to_write` was explicitly configured beforehand) no
self-fencing — the old leader has no way to know it has been abandoned. If you promote a
replica while the old leader might still be reachable by even one client, you now have two
nodes both claiming to own the same slot range, accepting divergent writes, with no
reconciliation mechanism in this project to ever merge them back. Whichever side loses the
eventual failback decision (Section 6) loses its divergent writes, permanently.

**The only real defense is Section 1's confirmation step, done properly, every time.** "I
can't reach it" is a symptom. "It is provably not running" (a `systemctl --user status` on
its own host showing `inactive (dead)`, or a corroborating loss of `PONG` from every
vantage point you have) is the bar for promoting anything.
```

- [ ] **Step 3: Verify**

Run:
```bash
grep -n "^## 5\.\|^## 6\.\|^## 7\." .claude/runbook-failover.md
grep -n "Store::load_snapshot_entries" .claude/runbook-failover.md
grep -n "clears all 16 shards" .claude/runbook-failover.md
grep -n "cluster_node_id = \"shard-a\"" .claude/runbook-failover.md
grep -n "systemctl --user disable --now rocket-mem-shard-a" .claude/runbook-failover.md
```
Expected: all five `grep`s find at least one match — Sections 5, 6, 7 exist, the exact failback mechanism is named, the exact fix for the promoted node's missing cluster fields is present, and the "do not restart the dead leader's unit" instruction is present.

- [ ] **Step 4: Commit**

```bash
git add .claude/runbook-failover.md
```
Commit through the `1-git-commit` skill. Suggested subject: `Extend the failover runbook with cluster-mode surgery and hazards`.

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

---

### Task 3: Live dry run against the real deployment

**Files:**
- Modify: `.claude/runbook-failover.md` (only if the drill finds an inaccuracy to correct; append a "Last verified" line regardless)

**Interfaces:**
- Consumes: the real running (or startable) 3-shard + 3-replica deployment on `numericlabs.lxd` — `rocket-mem-shard-{a,b,c}.service` via `systemctl --user`, and the three replica processes started by hand per Task 1's Section 0 note; `scripts/replication-health.sh` and `scripts/replication-health.nodes.example` (plan 10).
- Produces: nothing new for later plans — this is the acceptance test for the whole runbook. It doubles as a real demonstration of Section 6's failback hazard, using a harmless marker key instead of real data.

This task has no code to write, so there is no red/green cycle in the usual sense. The
"test" is the drill itself: every command below is real, run against the real deployment,
and its actual output is compared against what the runbook says will happen. Any mismatch
is a bug in the runbook (Sections 1-7), fixed in this task, in the same commit as the
verification note.

- [ ] **Step 1: Bring the deployment to a known-healthy baseline**

```bash
cd /home/numericlabs/data/rocket/rocket-mem
cargo build --release --workspace
systemctl --user start rocket-mem-shard-a rocket-mem-shard-b rocket-mem-shard-c
nohup ./target/release/rocket-mem --config rocket-mem-shard-a-replica.toml >/tmp/shard-a-replica.log 2>&1 &
nohup ./target/release/rocket-mem --config rocket-mem-shard-b-replica.toml >/tmp/shard-b-replica.log 2>&1 &
nohup ./target/release/rocket-mem --config rocket-mem-shard-c-replica.toml >/tmp/shard-c-replica.log 2>&1 &
sleep 2
ps aux | grep rocket-mem | grep -v grep | wc -l
```
Expected: `6` (three leaders under systemd, three replicas started by hand).

```bash
scripts/replication-health.sh --nodes scripts/replication-health.nodes.example \
  --tls-ca /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme
```
Expected: exit code `0`, six `OK` lines, summary `replication-health: 6 node(s) checked, 0 alert(s)`.

- [ ] **Step 2: Write a marker key and confirm it replicates (sets up Section 6's demonstration)**

```bash
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme \
  -h numericlabs.lxd -p 16379 set drill-marker before-failover
sleep 1
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme \
  -h numericlabs.lxd -p 16479 get drill-marker
```
Expected: `OK`, then `"before-failover"`.

- [ ] **Step 3: Kill shard-a and run Section 1's confirmation checks for real**

```bash
systemctl --user stop rocket-mem-shard-a
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt \
  -h numericlabs.lxd -p 16379 ping
```
Expected: connection refused, no `PONG`.

```bash
scripts/replication-health.sh --nodes scripts/replication-health.nodes.example \
  --tls-ca /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme
```
Expected (allow a few seconds and re-run if the follower hasn't yet noticed): exit code `1`,
an `ALERT` line for `shard-a` (`unreachable`), and an `ALERT` line for `shard-a-replica`
(`master_link_status=down`).

If the runbook's Section 1 language doesn't match this actual output, fix Section 1 now.

- [ ] **Step 4: Run Sections 2-3 for real**

```bash
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme \
  -h numericlabs.lxd -p 16479 info replication | grep -E 'slave_repl_offset|master_link_status'
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme \
  -h numericlabs.lxd -p 16479 replicaof no one
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme \
  -h numericlabs.lxd -p 16479 info replication | grep role
```
Expected: `role:master` after the promotion.

Write a second marker key directly to the promoted node, simulating a write accepted while
it was standalone — this is what Section 6's hazard demonstration destroys in Step 7 below:

```bash
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme \
  -h numericlabs.lxd -p 16479 set drill-marker-post-promotion lost-on-failback
```
Expected: `OK`.

Section 4 (re-point surviving followers) has nothing to do in this topology — confirm that
by noting there is no second replica of shard-a to re-point, and move on.

- [ ] **Step 5: Run Section 5 for real**

```bash
sed -i 's#shard-a numericlabs.lxd:16379#shard-a numericlabs.lxd:16479#' cluster.conf
cat cluster.conf
systemctl --user restart rocket-mem-shard-b rocket-mem-shard-c
systemctl --user disable --now rocket-mem-shard-a
kill %1 2>/dev/null || pkill -f "rocket-mem-shard-a-replica.toml"   # stop the promoted process
./target/release/rocket-mem --config rocket-mem-shard-a-replica.toml \
  --cluster-config /home/numericlabs/data/rocket/rocket-mem/cluster.conf \
  --cluster-node-id shard-a >/tmp/shard-a-promoted.log 2>&1 &
sleep 1
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt \
  -h numericlabs.lxd -p 16380 cluster nodes
```
Expected: shard-a's slot range (`0-5460`) shown against `numericlabs.lxd:16479`.

```bash
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme \
  -h numericlabs.lxd -p 16479 get drill-marker-post-promotion
```
Expected: `"lost-on-failback"` — still present, confirming the promoted node is fully
functional as a cluster member post-restart.

If any of Section 5's commands or expected outputs don't match, fix Section 5 now.

- [ ] **Step 6: Demonstrate Section 6's hazard for real, then restore the baseline**

```bash
# Bring the original shard-a back and demonstrate the failback hazard against the marker key
# written in Step 4, rather than against anything that matters.
sed -i 's#shard-a numericlabs.lxd:16479#shard-a numericlabs.lxd:16379#' cluster.conf   # restore
systemctl --user enable --now rocket-mem-shard-a
sleep 1
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme \
  -h numericlabs.lxd -p 16479 replicaof numericlabs.lxd 16379
sleep 1
redis-cli --tls --cacert /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme \
  -h numericlabs.lxd -p 16479 get drill-marker-post-promotion
```
Expected: `(nil)` — the marker key written while promoted is gone, exactly as Section 6
warns, wiped by the full resync `Store::load_snapshot_entries` performs. This is the
concrete proof that Section 6's warning is accurate, not speculative.

Finish restoring the baseline:

```bash
kill %1 2>/dev/null || pkill -f "rocket-mem-shard-a-replica.toml"   # stop the ad hoc process
systemctl --user restart rocket-mem-shard-b rocket-mem-shard-c
nohup ./target/release/rocket-mem --config rocket-mem-shard-a-replica.toml >/tmp/shard-a-replica.log 2>&1 &
sleep 2
scripts/replication-health.sh --nodes scripts/replication-health.nodes.example \
  --tls-ca /home/numericlabs/data/tls/root_ca-numericlabs.crt --user app --pass changeme
```
Expected: exit code `0`, six `OK` lines, `replication-health: 6 node(s) checked, 0 alert(s)`
— back to the Step 1 baseline. Delete the two marker keys if you want a clean keyspace
afterward; they are harmless either way.

- [ ] **Step 7: Record the verification and commit**

Append to `.claude/runbook-failover.md`, immediately after the title:

```markdown

*Last verified: 2026-09-10, full failover-and-restore drill against the numericlabs.lxd
3-shard deployment (Sections 1-6 exercised end to end, including a live demonstration of
Section 6's failback hazard using a throwaway marker key).*
```

If Steps 3-6 found any inaccuracy in Sections 1-7, fix it in this same commit and note what
changed in the commit message.

```bash
git add .claude/runbook-failover.md
```
Commit through the `1-git-commit` skill. Suggested subject: `Verify the failover runbook against the live 3-shard deployment`.

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

---

## Next plan

[`12-cluster-peer-liveness-prober.md`](12-cluster-peer-liveness-prober.md) — chain D (the spec's "cheapest honest first step" for clustered deployments): a background prober that makes `CLUSTER NODES`/`SHARDS`/`INFO` stop hardcoding `connected`/`online`/`ok`, so a future revision of this runbook's Section 1 can eventually cite a real signal instead of only `systemctl --user status` and cross-host `PING`.
