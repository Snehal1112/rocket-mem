# Replica Announce Address — Spec & Design

**Date:** 2026-09-10
**Status:** Approved
**Scope:** `crates/server` only — `config.rs`, `main.rs`, and docs. No engine, protocol, or wire-format change.
**Goal:** let a node control the address it advertises to its leader, so the value reported by
`INFO REPLICATION` is one a peer can actually reach.

## Problem

A follower tells its leader where to reach it, in the address argument of its `PSYNC <addr>`
frame. That value comes from exactly one place:

```rust
// crates/server/src/main.rs:297
.with_own_addr(config.addr.clone())
```

`config.addr` is the **plaintext** RESP listen address, unconditionally — never
`config.tls_resp_addr`, even when replication TLS is configured ten lines later at `main.rs:307`.

On a TLS deployment the result is a log line that contradicts itself:

```
INFO conn{conn_id=1 peer=192.168.1.12:36194 protocol=RESP tls=true}:repl{host_port=numericlabs.lxd:6479}:
     rocket_mem::connection: replica registered host_port=numericlabs.lxd:6479
```

The connection is TLS (`tls=true`). The address the replica announced is its plaintext port
(`6479`, not its `tls_resp_addr` of `16479`). Both facts are correct; together they are
misleading.

### Why this is worth fixing now rather than later

Today the announced address is **purely informational**. Every production `TcpStream::connect`
in the workspace was traced: the only one is `replication.rs:766` inside `connect_and_sync`,
and its target comes from a `REPLICAOF` command or the `replicaof` config key — never from
`ReplicaRegistry`. The announced value is only ever *displayed*, in three places:

1. `INFO REPLICATION`'s `slaveN:ip=…,port=…,state=online` lines (`dispatcher.rs:2027`),
2. the `repl` span's `host_port` field and the "replica pruned" event
   (`connection.rs:369`, `replication.rs:60`),
3. the startup banner's `slaveN <addr>` rows (`main.rs:435`).

That changes with failover.
[`2026-09-09-sentinel-failover-spec.md`](2026-09-09-sentinel-failover-spec.md) (lines 25–32)
has `rocket-sentinel` using `INFO REPLICATION`'s `slaveN:ip=…` lines as its **discovery
mechanism**, with each sentinel "probing every node" and issuing `REPLICAOF` for promotion. At
that point the announced address stops being a label and becomes a dial target. That spec
mentions TLS **zero times** — so a plaintext address announced by a TLS-only node would either
fail to connect or, worse, connect in the clear.

Fixing it while the field is still only displayed is cheap. Fixing it after something dials it
is a behaviour change to a live control plane.

## Rejected: derive the address from the cluster config

The obvious shortcut, and it does not work.

`cluster.conf` really does hold the TLS addresses in this deployment — `shard-a
numericlabs.lxd:16379`, matching `tls_resp_addr`, not the plaintext `6379`. The shard TOMLs say
why: *"a client following a MOVED redirect here has to reach the same kind of port on every
node."* And a node can read its own entry at runtime via
`replication.cluster()?.myself().addr` (`cluster.rs:190`).

Three reasons it is still the wrong source:

- **A replica's `cluster_node_id` names the node it replicates, not itself.** In this
  deployment `rocket-mem-shard-a-replica.toml` carries `cluster_node_id = "shard-a"` — the same
  id as its leader. `myself()` would therefore resolve to the leader's entry and the replica
  would announce **the leader's address**. That is worse than announcing the wrong protocol:
  it announces a different machine. (The replica TOMLs' own promotion runbook confirms this:
  step 3 requires editing `cluster.conf` to repoint `shard-a` at the replica's `16479` *before*
  the replica may serve as `shard-a`.)
- **Cluster config is optional for replication.** The two subsystems never reference each
  other's config or state — a node can be a full replica with no `cluster.conf` at all, and a
  cluster member can have no replication config at all. A fix that only works in cluster mode
  is not a fix.
- **`ClusterNode` has one `addr` and no notion of protocol** (`cluster.rs:54`). It is
  deliberately stored and echoed verbatim, because a `-MOVED` reply must name something the
  *client* can reach. It answers "where do clients find this slot owner", which is a different
  question from "where does a peer reach this replica".

## Rejected: default to `tls_resp_addr` when TLS is configured

A one-line change with the right instinct and the wrong semantics. It silently alters what
every existing follower reports the moment TLS is switched on, and it assumes the reachable
address is one of the two the node binds locally — false under NAT, container port mapping, or
a load balancer.

## Decision: an explicit `replica_announce_addr`

One new optional config field, participating in the existing figment layering (defaults < TOML
< `ROCKET_MEM_*` env < CLI) exactly like every other field:

```toml
# The address this node advertises to its leader in PSYNC, and which the leader reports in
# INFO REPLICATION. Defaults to `addr`. Set it when the address a peer must dial differs from
# the address this node binds -- a TLS deployment (announce `tls_resp_addr`), or NAT and
# container port mapping (announce the externally reachable host:port).
replica_announce_addr = "numericlabs.lxd:16479"
```

Wiring:

```rust
// crates/server/src/main.rs
.with_own_addr(
    config.replica_announce_addr.clone().unwrap_or_else(|| config.addr.clone()),
)
```

**Unset means today's behaviour byte for byte**, so no existing deployment or test changes.
This matters: `dispatcher.rs`'s `info_lists_each_connected_slaves_advertised_address` pins the
exact wire format `slave0:ip=127.0.0.1,port=6480,state=online\r\n`, and
`replication.rs`'s `sync_once_advertises_its_own_address_in_the_psync_frame_when_configured`
pins the raw `PSYNC` frame bytes. Both must keep passing untouched.

### Validate at startup, not at use

Follow the precedent of `validate_replicaof` and `validate_tls` (`config.rs`): reject a
malformed value before anything binds, rather than degrading at display time. Today a bad value
survives to `INFO REPLICATION`'s `split_addr`, which falls back to `("?", 0)` — an operator
sees `ip=?,port=0` and has nothing to grep for. A startup failure naming the field is strictly
more useful.

The check is a shape check (`host:port`, port parses as `u16`), not a reachability check. This
node cannot know whether a *peer* can reach an address, and pretending to check would be worse
than not checking.

### Warn when the announced address contradicts the transport

Defaulting is deliberately dumb, so the misconfiguration this spec exists to fix must be
*visible* rather than guessed at. At startup, when all three hold —

- this node is or will be a follower (`replicaof` set, or `REPLICAOF` issued later),
- a TLS listener is configured (`tls_resp_addr` or `tls_rmp_addr`),
- `replica_announce_addr` is unset,

— emit one `warn` naming the plaintext address being announced and the field to set. One line,
at startup, never per-command. This is the observability the just-merged logging series exists
to provide, used on the first real misconfiguration it can catch.

### Redaction and escaping

No new work, but worth recording as checked. The announced address is operator-supplied on the
follower, and **client-supplied on the leader** — it arrives in a `PSYNC` frame from the
network. It is already routed through `logging::escape_ident` at `connection.rs:369`, added by
the verbose-logging series' log-injection fix, so a value containing `\n` cannot forge a log
record. Any new call site rendering this field must do the same.

## Out of scope

- **Making anything dial the announced address.** This spec makes the value trustworthy; it
  does not add a consumer. The sentinel work is where that lands.
- **TLS for the sentinel control plane.** `2026-09-09-sentinel-failover-spec.md` needs a TLS
  decision before it dials anything. Named here so it is not forgotten; not decided here.
- **`ClusterNode` gaining a second address.** Cluster topology answers a client-facing
  question and stays as it is.
- **Auto-detecting the reachable address.** A node cannot know how peers route to it.
