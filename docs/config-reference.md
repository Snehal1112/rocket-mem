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
| `cluster_config` | `ROCKET_MEM_CLUSTER_CONFIG` | `--cluster-config` | unset | Path to the cluster topology file. Requires `cluster_node_id` to also be set; unset means standalone (non-cluster) mode. |
| `cluster_node_id` | `ROCKET_MEM_CLUSTER_NODE_ID` | `--cluster-node-id` | unset | This node's id within `cluster_config`'s topology. Requires `cluster_config` to also be set. |
| `tls_resp_addr` | `ROCKET_MEM_TLS_RESP_ADDR` | `--tls-resp-addr` | unset | TCP address for a TLS-wrapped RESP listener, run alongside the plaintext one at `addr`. Unset means no TLS RESP listener. Setting this requires `tls_cert_path` and `tls_key_path` — see the TLS note below. |
| `tls_rmp_addr` | `ROCKET_MEM_TLS_RMP_ADDR` | `--tls-rmp-addr` | unset | TCP address for a TLS-wrapped RMP listener, run alongside the plaintext one at `rmp_addr`. Unset means no TLS RMP listener. Setting this requires `tls_cert_path` and `tls_key_path` — see the TLS note below. |
| `tls_cert_path` | `ROCKET_MEM_TLS_CERT_PATH` | `--tls-cert-path` | unset | Path to a PEM certificate chain, shared by both TLS listeners. |
| `tls_key_path` | `ROCKET_MEM_TLS_KEY_PATH` | `--tls-key-path` | unset | Path to a PEM private key, shared by both TLS listeners. |
| `[[acl.users]]` | *(file-only — no flat env var for an array)* | *(file-only)* | empty | Bootstrap ACL users, loaded once at startup. See "The `[[acl.users]]` array" below. |

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
