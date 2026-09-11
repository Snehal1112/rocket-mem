# Configuration reference

`rocket-mem` reads its configuration from four layers, merged in this order (later layers
win):

```
built-in defaults  <  TOML file  <  ROCKET_MEM_* environment variables  <  CLI flags
```

A field left unset at a given layer simply falls through to the next one down — there's no
need to fully specify a TOML file or repeat every environment variable; set only what you
want to change from the default.

## Fields

| TOML key | Env var | CLI flag | Default | Meaning |
|---|---|---|---|---|
| `addr` | `ROCKET_MEM_ADDR` | `--addr` | `127.0.0.1:6379` | TCP address the RESP listener binds to. |
| `rmp_addr` | `ROCKET_MEM_RMP_ADDR` | `--rmp-addr` | `127.0.0.1:6380` | TCP address the RMP (rocket-mem's own protocol) listener binds to. |
| `metrics_addr` | `ROCKET_MEM_METRICS_ADDR` | `--metrics-addr` | `127.0.0.1:9121` | TCP address the Prometheus `/metrics` endpoint binds to. |
| `aof_path` | `ROCKET_MEM_AOF_PATH` | `--aof-path` | `./appendonly.aof` | Path to the append-only file used for write-durability and crash recovery. |
| `snapshot_path` | `ROCKET_MEM_SNAPSHOT_PATH` | `--snapshot-path` | `./dump.snapshot` | Path to the point-in-time snapshot file written by `SAVE` and loaded on startup. |
| `slowlog_threshold_micros` | `ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS` | `--slowlog-threshold-micros` | `10000` | Minimum command duration, in microseconds, that gets logged to the slow log. `0` disables the slow log entirely. |
| `log_level` | `ROCKET_MEM_LOG_LEVEL` | `--log-level` | `info` | Log level filter passed to `tracing`'s `EnvFilter`, e.g. `info`, `debug`, `rocket_mem=debug,warn`. Same syntax as `RUST_LOG`, which — when set — always overrides this field. |
| `log_value_max_bytes` | `ROCKET_MEM_LOG_VALUE_MAX_BYTES` | `--log-value-max-bytes` | `128` | Maximum bytes of a value or command argument rendered into a `trace`-level log line before truncation. Only consulted at `trace`. |
| `cluster_config` | `ROCKET_MEM_CLUSTER_CONFIG` | `--cluster-config` | unset | Path to the cluster topology file. Requires `cluster_node_id` to also be set; unset means standalone (non-cluster) mode. |
| `cluster_node_id` | `ROCKET_MEM_CLUSTER_NODE_ID` | `--cluster-node-id` | unset | This node's id within `cluster_config`'s topology. Requires `cluster_config` to also be set. |
| `cluster_probe_interval_secs` | `ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS` | `--cluster-probe-interval-secs` | `1` | How often, in seconds, this node probes every other node in `cluster_config`'s topology for liveness (a TCP connect plus a `PING`). Cluster mode only — a standalone node has no peers and never starts a prober. Must be at least 1. |
| `cluster_node_timeout_secs` | `ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS` | `--cluster-node-timeout-secs` | `15` | How long, in seconds, a peer may go without answering a probe before this node reports it failed in `CLUSTER NODES`, `CLUSTER SHARDS`, and `CLUSTER INFO`. Reporting only — see the note below. Must be at least 1. |
| `tls_resp_addr` | `ROCKET_MEM_TLS_RESP_ADDR` | `--tls-resp-addr` | unset | TCP address for a TLS-wrapped RESP listener, run alongside the plaintext one at `addr`. Unset means no TLS RESP listener. Setting this requires `tls_cert_path` and `tls_key_path` — see the TLS note below. |
| `tls_rmp_addr` | `ROCKET_MEM_TLS_RMP_ADDR` | `--tls-rmp-addr` | unset | TCP address for a TLS-wrapped RMP listener, run alongside the plaintext one at `rmp_addr`. Unset means no TLS RMP listener. Setting this requires `tls_cert_path` and `tls_key_path` — see the TLS note below. |
| `tls_cert_path` | `ROCKET_MEM_TLS_CERT_PATH` | `--tls-cert-path` | unset | Path to a PEM certificate chain, shared by both TLS listeners. |
| `tls_key_path` | `ROCKET_MEM_TLS_KEY_PATH` | `--tls-key-path` | unset | Path to a PEM private key, shared by both TLS listeners. |
| `tls_ca_path` | `ROCKET_MEM_TLS_CA_PATH` | `--tls-ca-path` | unset | Path to a PEM root CA certificate used by follower replication to verify leader TLS certificate. |
| `replicaof` | `ROCKET_MEM_REPLICAOF` | `--replicaof` | unset | `host:port` of a leader to auto-connect to as a follower on every startup. Unset means standalone (or purely live-`REPLICAOF`-driven) operation. |
| `replicaof_auth_username` | `ROCKET_MEM_REPLICAOF_AUTH_USERNAME` | `--replicaof-auth-username` | unset | Username sent in `AUTH` before `PSYNC`, when `replicaof`'s leader has ACL users configured. Must be set together with `replicaof_auth_password`, or neither. |
| `replicaof_auth_password` | `ROCKET_MEM_REPLICAOF_AUTH_PASSWORD` | `--replicaof-auth-password` | unset | Password sent in `AUTH` before `PSYNC`. Plaintext in the TOML file, same as `[[acl.users]]`'s own `password` field. |
| `replica_announce_addr` | `ROCKET_MEM_REPLICA_ANNOUNCE_ADDR` | `--replica-announce-addr` | `addr` | The `host:port` this node advertises to its leader in `PSYNC`, and which the leader reports in `INFO REPLICATION`'s `slaveN:` lines. Defaults to `addr`. Set it when the address a peer must dial differs from the address this node binds — a TLS deployment, or NAT and container port mapping. |
| `min_replicas_to_write` | `ROCKET_MEM_MIN_REPLICAS_TO_WRITE` | `--min-replicas-to-write` | `0` | Minimum number of replicas that must have acknowledged within `min_replicas_max_lag_secs` for this leader node to accept write commands (`0` disables write fencing). |
| `min_replicas_max_lag_secs` | `ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS` | `--min-replicas-max-lag-secs` | `10` | Maximum lag in seconds for a replica's last ack to qualify as "good" for `min_replicas_to_write`. |
| `[[acl.users]]` | *(file-only — no flat env var for an array)* | *(file-only)* | empty | Bootstrap ACL users, loaded once at startup. See "The `[[acl.users]]` array" below. |

> **`trace` writes your data to disk.** At `trace`, rocket-mem logs command arguments and
> value contents, so a trace-level log file is a plaintext copy of the dataset and every
> mutation applied to it, capped per value by `log_value_max_bytes`. Credentials are always
> redacted (`AUTH`, `HELLO ... AUTH`, `ACL SETUSER`, `ACL GETUSER`, `REPLICAOF ... AUTH`), but
> ordinary values are not. Treat a trace log with the same retention and access controls as
> the data itself.

`--config <path>` is a fifth, special-cased CLI flag: it names which TOML file gets merged
into the layers above, so it isn't itself one of the layered fields. If `--config` is
omitted, `rocket-mem` looks for `rocket-mem.toml` in the current directory and merges it if
present; if that file doesn't exist either, `rocket-mem` starts from built-in defaults with
no TOML layer at all — that's not an error.

### TLS requires both a cert and a key

If `tls_resp_addr` or `tls_rmp_addr` is set, `tls_cert_path` and `tls_key_path` must both
also be set. `rocket-mem` checks this at startup, before binding either TLS listener, and
aborts immediately with a clear error if the cert or key path is missing — it will not
silently start up with that TLS listener simply never bound. This check runs regardless of
which layer supplied the TLS address (TOML, env var, or CLI flag).

### `replicaof`'s auth pair is all-or-nothing; the target itself is not validated

If `replicaof_auth_username` or `replicaof_auth_password` is set, both must be — `rocket-mem`
checks this at startup, before any listener binds, and aborts immediately if only one is set.
`replicaof` itself (the `host:port` target) is **not** validated at startup: a bad host or an
unreachable leader is only discoverable by actually attempting the connection, so it fails
soft — the node starts normally, and its background reconnect loop retries once a second
forever, exactly as it would for a leader that later becomes unreachable. This mirrors the
live `REPLICAOF` command's existing behavior; see `.claude/manual-testing.md`'s "Replication
(`REPLICAOF`)" section.

### Cluster peer health is reported, never acted on

In cluster mode, `rocket-mem` probes every other node in the topology every
`cluster_probe_interval_secs` and reports any peer that has not answered within
`cluster_node_timeout_secs` as `master,fail?`/`disconnected` in `CLUSTER NODES`, `health: failed`
in `CLUSTER SHARDS`, and `cluster_state:fail` with a non-zero `cluster_slots_pfail` in
`CLUSTER INFO`. Two Prometheus gauges, `rocket_mem_cluster_peers_reachable` and
`rocket_mem_cluster_peers_unreachable`, carry the same information, and each state change is
logged once — once per change, not once per probe.

Each probe round is also a real TCP connection to the peer, whose own `connection.rs` accept
loop would otherwise log a `connection accepted`/`connection closed` pair at `info` every
round regardless of this section's "once per change" behavior — rocket-mem recognizes its own
probe traffic and logs that pair at `debug` instead, so a healthy, unchanging cluster stays
quiet at the `info` default. See `crates/server/src/cluster_health.rs`'s `PROBE_MARKER`.
"Connection accepted" now logs on the connection's first frame rather than at TCP accept, so
an idle-holding connection that never sends anything (an L4 health checker, a connection-pool
pre-open) produces no accept line until it does — or never, if it never sends anything;
`rocket_mem_connected_clients` remains the immediate signal for a live connection count.

`cluster_slots_fail` stays `0` even then, and that is correct rather than a bug. Redis's *pfail*
(`fail?`) means one node suspects a peer; *fail* means a majority agreed over the cluster bus.
`rocket-mem` has no cluster bus and no quorum, so a suspicion here can never be promoted — nothing
can ever agree — and the counter has no value it could honestly take but zero. Read
`cluster_slots_pfail` and `cluster_state`; `cluster_slots_fail` is structurally always zero.

That is the entire feature. **Nothing is promoted and no routing changes.** A slot's owner stays
its configured owner while it is dead, so clients keep getting `-MOVED` to a dead address until an
operator intervenes: picking a different owner is a topology decision this project has no
mechanism to agree on (there is no cluster bus, and `cluster_current_epoch` is pinned to `0`).
Recovering write access to a dead shard's slots still means hand-editing `cluster.conf` on every
node and restarting every node.

`cluster_state:fail` here is a report, not a mode — unlike real Redis, this node keeps serving its
own slots and keeps redirecting for everyone else's. That is a deliberate wire-compatibility
divergence: a cluster-aware client that checks `cluster_state` before sending commands may treat
this node as unusable when it is still serving normally.

Both timers are rejected at startup if set to `0`: a zero probe interval is not a valid timer at
all, and a zero node timeout would report every peer failed permanently. If you want faster
detection, lower `cluster_node_timeout_secs` — but keep it comfortably above
`cluster_probe_interval_secs`, or a single slow round will flap a healthy peer.

### `replica_announce_addr` is shape-checked, not reachability-checked

Left unset, a node announces `addr` to its leader — today's behavior for every deployment
that predates this field. Set `replica_announce_addr` when the address a peer must dial
differs from the address this node binds: a TLS deployment (announce `tls_resp_addr`, since
`addr` is the plaintext port), or NAT and container port mapping (announce the externally
reachable `host:port`).

Like `replicaof`, only the *shape* is validated at startup — `host:port`, with a non-empty
host and a port that parses as a `u16` — never whether a peer can actually reach it. This
node cannot know how a peer routes to it, so a startup check on that would be meaningless.

Nothing dials this address today: it is reported by `INFO REPLICATION`'s `slaveN:` lines,
the `repl` tracing span's `host_port` field, and the startup banner's `slaveN` rows. Future
failover tooling (see `docs/superpowers/specs/2026-09-09-sentinel-failover-spec.md`) is
expected to dial it to reach a follower directly.

### Replica fencing (`min_replicas_to_write`) requires a nonzero lag window

`min_replicas_to_write` makes this node refuse client writes with a `NOREPLICAS` error unless at
least that many replicas have sent a `REPLCONF ACK` within the last `min_replicas_max_lag_secs`
seconds. It is disabled by
default (`0`) — every deployment that doesn't set it is completely unaffected, and every write is
accepted regardless of replica state, exactly as before this field existed.

Setting `min_replicas_to_write` above `0` while leaving `min_replicas_max_lag_secs` at `0` is
rejected at startup: no replica's ack could ever be recent enough to satisfy a zero-second window,
so every write would be refused forever — a permanent outage spelled as a config typo, not a real
deployment intent. `rocket-mem` checks this before any listener binds and aborts immediately if it
sees that combination, the same way it aborts on a broken `tls_*` or `replicaof_auth_*` pairing.

This gate applies only to this node's own client-originated writes. A replica already rejects
client writes with `READONLY` regardless of `min_replicas_to_write` — fencing and read-only mode
are independent gates, and `READONLY` always wins on a replica.

Note that fencing counts a replica as good from the *recency of its ack*, which says the replica is
keeping up with the stream. It is not a guarantee that any particular write reached that replica
before the write was acknowledged to the client.

### Malformed values fail startup, not silently

Values are parsed at load time, and a value that doesn't parse into its field's type is a
hard startup failure, not a fallback to the default. For example,
`ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS=abc` (a non-numeric string for a numeric field) makes
config loading return an error, and `rocket-mem` exits immediately with that error printed
to stderr rather than silently running with the default `10000`. The same applies to a
malformed value in the TOML file. If you set a config value at all, make sure it parses.

## Precedence, worked example

Given this `rocket-mem.toml`:

```toml
addr = "127.0.0.1:1111"
slowlog_threshold_micros = 2000
rmp_addr = "127.0.0.1:1234"
```

...this environment:

```bash
export ROCKET_MEM_ADDR=127.0.0.1:2222
export ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS=3000
```

...and this invocation:

```bash
rocket-mem --config rocket-mem.toml --addr 127.0.0.1:4444
```

the resulting configuration is:

- `addr` = `127.0.0.1:4444` — the `--addr` flag wins over everything below it.
- `slowlog_threshold_micros` = `3000` — no CLI flag set it, so the env var (which beat the
  file's `2000`) wins.
- `rmp_addr` = `127.0.0.1:1234` — no CLI flag or env var touched it, so the file's value
  reaches the final config, beating the built-in default of `127.0.0.1:6380`.
- Every other field — `metrics_addr`, `aof_path`, `snapshot_path`, and so on — falls all the
  way through to its built-in default, since none of the three layers above mentioned it.

A CLI flag that isn't passed at all does not override anything: only flags explicitly given
on the command line participate in the merge, so running `rocket-mem` with no flags never
clobbers a TOML or env-var value with an implicit default.

## The `[[acl.users]]` array

`rocket-mem.toml` can bootstrap one or more ACL users at startup via a repeated
`[[acl.users]]` table. Each entry has this shape:

```toml
[[acl.users]]
username = "readonly-app"
password = "hunter2"
enabled = true
rules = ["~app:*", "+get", "-set"]
```

| Field | Type | Default | Meaning |
|---|---|---|---|
| `username` | string | *(required)* | The user's name, as passed to `AUTH`. |
| `password` | string, optional | unset (`nopass`) | Plaintext in the TOML file — hashed once at startup, and only ever kept as that hash: the plaintext is never written to the AOF, the snapshot, the slow log, or any error message. Omitting it means `nopass`: the user authenticates with any password, or none at all. |
| `enabled` | boolean | `true` | Whether the user can authenticate at all. A `false` entry is loaded but rejected at `AUTH` time. |
| `rules` | array of strings | `[]` (empty) | Access-control rule tokens, applied left to right, same-vocabulary as `ACL SETUSER`'s tokens (`allcommands`/`nocommands`, `allkeys`, `+CMD`/`-CMD` to allow/deny one command, `~pattern` to allow a key glob). Later rules override earlier ones for anything they overlap — e.g. `["allcommands", "-flushall"]` grants every command except `FLUSHALL`. An empty `rules` list denies every command and key until rules are added (via `ACL SETUSER` at runtime, or a longer list here). |

A minimal example with two users — one full-access, one restricted to a key prefix and a
single command:

```toml
[[acl.users]]
username = "admin"
password = "hunter2"
enabled = true
rules = ["allcommands", "allkeys"]

[[acl.users]]
username = "readonly"
rules = ["~app:*", "+get"]
```

The second user above has no `password` field, so it authenticates with `nopass`; it can
only run `GET` and only against keys matching `app:*` — every other command and key is
denied. See [`docs/command-compatibility.md`](command-compatibility.md) for `ACL SETUSER`'s
full token vocabulary and the runtime `ACL` command family.

Note that bootstrap ACL users configured here live only in memory at runtime: a user added
later via `ACL SETUSER` is not persisted, and is lost on restart unless it's also added to
this array.

## Backward compatibility

Every `ROCKET_MEM_*` environment variable this project read before config layering was
added still works identically today — the env var names and their effect are unchanged.
Config layering (the TOML file and CLI flags) is purely additive: a deployment that only
ever set `ROCKET_MEM_*` environment variables needs no changes to keep working exactly as
before.
