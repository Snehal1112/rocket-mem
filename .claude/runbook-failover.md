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
