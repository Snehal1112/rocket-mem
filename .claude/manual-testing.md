# Manual testing guide

How to run `rocket-mem` by hand and exercise its features from the command line.

Configuration comes from four layers, merged in this order (later wins): built-in defaults, a
TOML file, `ROCKET_MEM_*` environment variables, then CLI flags. None of it is required — the
binary starts with no configuration at all. The env vars below are enough for everything in this
guide except cluster mode's topology file; see "Configuration layering" for the TOML and
CLI-flag routes, which arrived in Sprint 8 and are purely additive (every env var that worked
before still works identically).

Build once: `cargo build --release --workspace`, binary at `./target/release/rocket-mem`.

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `ROCKET_MEM_ADDR` | `127.0.0.1:6379` | TCP address to bind for RESP clients |
| `ROCKET_MEM_AOF_PATH` | `./appendonly.aof` | Append-only file — replayed on startup if present |
| `ROCKET_MEM_SNAPSHOT_PATH` | `./dump.snapshot` | Snapshot file — loaded on startup if present, written by `SAVE` |
| `ROCKET_MEM_METRICS_ADDR` | `127.0.0.1:9121` | Prometheus `/metrics` endpoint. **Every node defaults to the same port** — running more than one node on one machine requires giving each a distinct value, or the second/third node crashes with `AddrInUse` even though its RESP port is free. This bit me during Sprint 6 testing; it's a real gap, not documented as a known limit yet. |
| `ROCKET_MEM_RMP_ADDR` | `127.0.0.1:6380` | RMP — rocket-mem's own binary protocol listener. Same multi-node collision `ROCKET_MEM_METRICS_ADDR` has, above — every node defaults to the same port, so a second/third node on one machine needs a distinct value or it crashes with `AddrInUse`. |
| `ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS` | `10000` (10ms) | Commands at/over this duration go in the slow log. `0` disables it |
| `ROCKET_MEM_CLUSTER_CONFIG` | unset | Path to the cluster topology file (see below). Cluster mode is off unless this AND the next var are both set |
| `ROCKET_MEM_CLUSTER_NODE_ID` | unset | Which line of the topology file is this process |
| `ROCKET_MEM_TLS_RESP_ADDR` | unset | TLS-wrapped RESP listener, run *alongside* the plaintext one — not instead of it. Unset means no TLS RESP listener |
| `ROCKET_MEM_TLS_RMP_ADDR` | unset | TLS-wrapped RMP listener, same alongside-not-instead behavior |
| `ROCKET_MEM_TLS_CERT_PATH` | unset | PEM certificate chain, shared by both TLS listeners. Required if either TLS address is set — startup fails otherwise |
| `ROCKET_MEM_TLS_KEY_PATH` | unset | PEM private key, shared by both TLS listeners. Required if either TLS address is set — startup fails otherwise |

Never both cluster vars set → standalone mode, byte-for-byte the same behavior as if cluster code
didn't exist.

There is no env var for ACL users: the `[[acl.users]]` bootstrap array is file-only, because a
flat `ROCKET_MEM_*` variable can't express an array of tables. See "ACL and authentication".
`docs/config-reference.md` has the full field list with TOML/CLI equivalents.

## Configuration layering (TOML file + CLI flags)

Sprint 8 added a `rocket-mem.toml` file and CLI flags on top of the pre-existing `ROCKET_MEM_*`
env vars. Four layers merge, later wins: built-in defaults < TOML file < `ROCKET_MEM_*` env vars
< CLI flags. `docs/config-reference.md` has the full field table; this section is about proving
the layering behaves as documented.

`rocket-mem --help` lists every flag:

```
--config <CONFIG>                                      path to a TOML file
--addr <ADDR>                                          [default: 127.0.0.1:6379]
--rmp-addr <RMP_ADDR>                                  [default: 127.0.0.1:6380]
--metrics-addr <METRICS_ADDR>                          [default: 127.0.0.1:9121]
--aof-path <AOF_PATH>                                  [default: ./appendonly.aof]
--snapshot-path <SNAPSHOT_PATH>                        [default: ./dump.snapshot]
--slowlog-threshold-micros <SLOWLOG_THRESHOLD_MICROS>  [default: 10000]
--cluster-config <CLUSTER_CONFIG>
--cluster-node-id <CLUSTER_NODE_ID>
--tls-resp-addr <TLS_RESP_ADDR>
--tls-rmp-addr <TLS_RMP_ADDR>
--tls-cert-path <TLS_CERT_PATH>
--tls-key-path <TLS_KEY_PATH>
```

`--config` is special: it names which TOML file gets merged, so it isn't itself a layered field.
Every other flag defaults to "unset" rather than the value shown above, so an unset flag doesn't
clobber a lower layer.

### `--config <path>` and auto-pickup

```bash
cat > /tmp/rm-cfg/my-config.toml <<'EOF'
addr = "127.0.0.1:6440"
rmp_addr = "127.0.0.1:6441"
metrics_addr = "127.0.0.1:9240"
aof_path = "/tmp/rm-cfg.aof"
snapshot_path = "/tmp/rm-cfg.snap"
EOF

./target/release/rocket-mem --config /tmp/rm-cfg/my-config.toml &
redis-cli -p 6440 ping            # -> PONG, bound to the TOML's addr
kill %1
```

With no `--config` flag, a `rocket-mem.toml` in the current working directory is picked up
automatically. With neither, startup falls through to env/defaults — not an error:

```bash
cd /tmp/rm-cfg && cp my-config.toml rocket-mem.toml
./target/release/rocket-mem &     # no --config, still picks up ./rocket-mem.toml
redis-cli -p 6440 ping            # -> PONG
kill %1

mkdir /tmp/no-toml-here && cd /tmp/no-toml-here
ROCKET_MEM_ADDR=127.0.0.1:6442 ROCKET_MEM_METRICS_ADDR=127.0.0.1:9240 \
  ./target/release/rocket-mem &
redis-cli -p 6442 ping            # -> PONG; a missing TOML is not a startup failure
kill %1
```

A `--config` naming a file that doesn't exist is also *not* an error — it silently skips the TOML
layer. A typo'd `--config` path therefore fails open rather than loud, which is worth knowing if
a deployment expects it to be load-bearing.

### Precedence, proven by which port actually gets bound

The startup banner's `Listening on <addr>` line is ground truth. Each run below sets `addr` at one
more layer than the last:

```bash
./target/release/rocket-mem &                                     # TOML only
sleep 0.5 && kill %1                                              # -> Listening on 127.0.0.1:6440

ROCKET_MEM_ADDR=127.0.0.1:6442 ./target/release/rocket-mem &      # TOML + env
sleep 0.5 && kill %1                                              # -> 127.0.0.1:6442, env beats file

ROCKET_MEM_ADDR=127.0.0.1:6442 ./target/release/rocket-mem --addr 127.0.0.1:6443 &
sleep 0.5 && kill %1                                              # -> 127.0.0.1:6443, CLI beats env
```

That third run passed only `--addr`, and its other two listeners still came up on the TOML's
values, not the built-in defaults:

```
Metrics on http://127.0.0.1:9240/metrics    # from the TOML, untouched by --addr
RMP listening on 127.0.0.1:6441             # from the TOML, untouched by --addr
Listening on 127.0.0.1:6443                 # from --addr
```

If an unpassed flag serialized as null instead of being omitted from the merge, those two would
have reset to `127.0.0.1:6380`/`127.0.0.1:9121`. They don't.

### Malformed values fail startup hard

A value that doesn't parse aborts with exit 1 before anything binds — it does not fall back to the
default. True for both the env and TOML layers:

```bash
ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS=abc ./target/release/rocket-mem
# Error: Custom { kind: InvalidInput, error: "config error: invalid type: found string \"abc\",
#   expected u64 for key \"SLOWLOG_THRESHOLD_MICROS\" in `ROCKET_MEM_` environment variable(s)" }
```

That wrapper is `std::io::Error`'s `Debug` output, not a hand-written message — `main.rs` wraps the
`figment` error and lets `?` propagate it to the default printer. The useful part (field, expected
type, source layer) is inside it, just noisier than it should be.

## Standalone mode

```bash
ROCKET_MEM_ADDR=127.0.0.1:6399 \
ROCKET_MEM_AOF_PATH=/tmp/rm-standalone.aof \
ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-standalone.snapshot \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9199 \
  ./target/release/rocket-mem &
```

```bash
redis-cli -p 6399 set foo bar
redis-cli -p 6399 get foo

redis-cli -p 6399 info server          # real uptime, os, version — not a stub
redis-cli -p 6399 info replication     # role:master
redis-cli -p 6399 hello 3              # role field in the map reply

curl -s http://127.0.0.1:9199/metrics | grep rocket_mem_commands_total

redis-cli -p 6399 slowlog len
redis-cli -p 6399 slowlog get
redis-cli -p 6399 slowlog reset

kill %1   # stop it
```

## ACL and authentication

ACL users are the one feature that needs a config file: `[[acl.users]]` is a TOML array, and no
flat `ROCKET_MEM_*` variable can express an array of tables.

**Defining even one user turns authentication on for the whole server.** There is no
`requirepass`-style switch — an empty user list means no auth at all, and the first user, however
narrow, makes every connection start out unauthenticated.

```bash
cat > /tmp/rm-acl.toml <<'EOF'
addr = "127.0.0.1:6410"
rmp_addr = "127.0.0.1:6411"
metrics_addr = "127.0.0.1:9210"

[[acl.users]]
username = "admin"
password = "adminpw"
enabled = true
rules = ["allcommands", "allkeys"]

[[acl.users]]
username = "app"
password = "apppw"
enabled = true
rules = ["~app:*", "+get"]

[[acl.users]]
username = "retired"
password = "retiredpw"
enabled = false
rules = ["allcommands", "allkeys"]
EOF

./target/release/rocket-mem --config /tmp/rm-acl.toml \
  --aof-path /tmp/rm-acl.aof --snapshot-path /tmp/rm-acl.snap &
```

### The gate

Nothing is reachable before `AUTH` — not even `PING`. `ACL` is deliberately not exempt, so an
anonymous client cannot bootstrap itself an admin account and then log into it.

```bash
redis-cli -p 6410 ping                   # -> NOAUTH Authentication required.
redis-cli -p 6410 acl whoami             # -> NOAUTH Authentication required.

# The privilege-escalation attempt the gate exists to stop.
redis-cli -p 6410 acl setuser attacker on '>x' allcommands allkeys
                                         # -> NOAUTH Authentication required.

redis-cli -p 6410 auth admin adminpw     # -> OK
redis-cli -p 6410 auth admin nope        # -> WRONGPASS invalid username-password pair or user is disabled.
redis-cli -p 6410 auth retired retiredpw # -> WRONGPASS ... — same message for a DISABLED user.
redis-cli -p 6410 auth ghost x           # -> WRONGPASS ... — and for an unknown username.
```

One message covers wrong password, disabled user, and unknown username on purpose, and all three
pay the same argon2 cost, so neither the reply nor the latency reveals which usernames exist.

`AUTH` and `HELLO` are the only commands that pass unauthenticated, because RESP3 clients send
credentials inline. A *bare* `HELLO` is still refused:

```bash
redis-cli -p 6410 hello 3                      # -> NOAUTH Authentication required.
redis-cli -p 6410 hello 3 auth admin adminpw   # -> full map reply: server, version, proto 3, ...
```

### Auth state is per-connection

`redis-cli -p 6410 auth ...` authenticates a connection that then immediately closes, so the next
invocation starts over. Pipe into one `redis-cli` to keep a single connection, or use
`--user`/`--pass`:

```bash
redis-cli -p 6410 auth admin adminpw     # -> OK
redis-cli -p 6410 ping                   # -> NOAUTH ... — a NEW connection, not authenticated.

printf 'auth admin adminpw\nping\nset k1 v1\nget k1\n' | redis-cli -p 6410
                                         # -> OK / PONG / OK / v1 — one connection, auth sticks.

redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning ping   # -> PONG
```

### The two NOPERM messages

The distinction matters when debugging. User `app` has `rules = ["~app:*", "+get"]`:

```bash
redis-cli -p 6410 --user app --pass apppw --no-auth-warning get app:1
# -> "hello" — granted command, key matches the pattern.

redis-cli -p 6410 --user app --pass apppw --no-auth-warning get other:1
# -> NOPERM no permissions to access a key
#    Command IS granted; the key falls outside every ~pattern.

redis-cli -p 6410 --user app --pass apppw --no-auth-warning ping
# -> NOPERM this user has no permissions to run this command
#    The command itself was never granted.
```

Read them as: "access a key" = your `~pattern` is too narrow; "run this command" = you need a
`+cmd`. Note that `PING` takes no keys and is still refused — a command grant is required even for
keyless commands. Every key of a multi-key command must match, too: `MGET app:1 other:1` is denied
outright rather than partially served.

### Runtime `ACL` commands

`ACL SETUSER` takes effect on the next command, with no restart and no reconnect — including on
connections already open and already authenticated.

```bash
redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl whoami    # -> "admin"

redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl list
# -> one line per user, in ACL SETUSER vocabulary, password as its argon2 hash:
#    user app on #$argon2id$v=19$m=19456,t=2,p=1$MFKf...$CUEK... ~app:* +get
#    user admin on #$argon2id$...$... +@all ~*
#    user retired off #$argon2id$...$... +@all ~*
#    Order is HashMap iteration order — it changes between runs, don't script against it.

redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl getuser app
# -> flags: on / passwords: $argon2id$... / commands: "+get" / keys: "~app:*"
redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl getuser nobody
# -> (empty) — nil, not an error.

redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl deluser app
# -> (integer) 1 — count actually removed; a second call returns 0.
```

Revocation reaches live connections. Delete a user from a second terminal ~1s into this:

```bash
{ echo "auth app apppw"; echo "get app:1"; sleep 2.5; echo "get app:1"; } | redis-cli -p 6410
# -> OK / hello / NOAUTH Authentication required.   <- same connection, after the DELUSER landed
```

`ACL SETUSER` is **incremental** — it merges into the existing user and rules only ever append.
Revoke by adding the negative token and expect the list to grow:

```bash
redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl setuser ro on '>ropw' '~app:*' +get
redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl setuser ro +set
redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl setuser ro -set
redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl getuser ro
# -> commands: "+get +set -set" — a replay log, not a summary. Last rule wins.
```

There is no way to reset a user's rules short of `ACL DELUSER` and recreating them.

### TOML fields vs `ACL SETUSER` tokens

The two vocabularies overlap only for *rule* tokens. Login state and password are TOML **fields**
in the file, and **tokens** on the command line. Mixing them up fails loudly in both directions.

| Concept | `rocket-mem.toml` | `ACL SETUSER` |
|---|---|---|
| Enabled | `enabled = true` / `false` | `on` / `off` |
| Password | `password = "pw"` | `>pw` |
| No password | omit `password` | `nopass` |
| Rules | `rules = ["allkeys", "+get"]` | trailing `allkeys +get` args |

```bash
# TOML-side: on/off/>pw inside `rules` is a hard startup failure, not a warning.
#   rules = ["on", ">secret123", ...]
# -> Error: Custom { kind: InvalidInput, error: "acl bootstrap: ERR syntax error at 'on'" }
#    exit 1, before any listener binds. A misplaced password token is redacted in that error
#    as '<password token>' rather than echoed to stderr/journald.

# Command-side: TOML field syntax is a syntax error, and the user is NOT created.
redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl setuser tmp1 enabled=true
# -> ERR syntax error at 'enabled=true'
redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl setuser tmp1 on secret123
# -> ERR syntax error at 'secret123' — a bare password needs the '>' prefix.
redis-cli -p 6410 --user admin --pass adminpw --no-auth-warning acl getuser tmp1
# -> (empty) — SETUSER parses every token before applying any, so a bad one leaves nothing behind.
```

Rule tokens are identical in both places: `allcommands`/`+@all`, `nocommands`/`-@all`,
`allkeys`/`~*`, `+cmd`, `-cmd`, `~pattern`. Keywords are case-insensitive; patterns and passwords
are not. No other `@category` exists — `+@read` is `ERR syntax error at '+@read'`.

### Gotchas

**`KEYS` and `SCAN` ignore key patterns.** Their argument is a glob, not a key, so the ACL key
check sees a keyless command and every key name in the store comes back regardless of `~pattern`.
Values stay protected; names do not. This is a real gap, not a documented limit:

```bash
# user `scoped` has rules = ["allcommands", "~app:*"], store holds app:1, app:2, secret:1
redis-cli -p 6410 --user scoped --pass scopedpw --no-auth-warning get secret:1
# -> NOPERM no permissions to access a key
redis-cli -p 6410 --user scoped --pass scopedpw --no-auth-warning keys '*'
# -> app:1 / app:2 / secret:1     <- leaked, despite ~app:*
redis-cli -p 6410 --user scoped --pass scopedpw --no-auth-warning scan 7
# -> 8 / secret:1                 <- SCAN leaks it too
```

**On a server with no ACL configured, any anonymous client can create the first user and lock
everyone else out.** Bootstrap your admin in the TOML file before the port is reachable by
anything you don't trust. (A *failed* `SETUSER` does not arm the gate, so a malformed command
can't brick an open server by accident.)

**`+acl` is equivalent to full admin.** There is no per-subcommand granularity — a user granted
`+acl` can run `ACL SETUSER` on itself and escalate to `allcommands allkeys`.

**Deleting every user locks the server until restart.** The "auth is on" flag is sticky: it's set
the first time any user is configured and never cleared. Emptying the table doesn't turn auth back
off, it just leaves nobody to authenticate as, and there is no recovery command.

**`ACL SETUSER` is not persisted.** Runtime users live in memory only — never in the AOF or
snapshot. A restart rebuilds the table from `[[acl.users]]` alone, so runtime users are silently
gone while the data they guarded survives. ACL changes are also leader-local: they are not
replicated, so a follower's user table can diverge from its leader's.

**`/metrics` has no authentication of its own**, ACL or not — bind it to loopback or firewall it.
Failed logins and NOPERM refusals do increment `rocket_mem_command_errors_total{cmd="auth"|...}`,
which is a usable alerting hook.

**`AUTH` costs ~20ms** (argon2 verification), which clears the default 10ms slow-log threshold, so
essentially every `AUTH` lands in `SLOWLOG GET`. Its arguments are redacted there as
`... (2 more arguments)`, so the password does not leak. Nothing ACL-related reaches durable state
either — no plaintext password and no `AUTH`/`ACL` command ever reaches the AOF or snapshot.

**Not implemented:** `RESET` (so a connection cannot drop its identity short of reconnecting),
`ACL HELP`, `ACL CAT`, and `ACL GETUSER` selectors.

```bash
kill %1
```

## TLS

TLS listeners run *alongside* the plaintext ones on their own addresses — they never replace them.
Four settings control it, each available as a TOML key, a `ROCKET_MEM_TLS_*` env var, or a
`--tls-*` flag. Server-auth only, no mutual TLS: the server never asks the client for a
certificate.

First make a certificate. Self-signed, **local testing only** — it has no trust chain anyone else
will accept, so never point a real deployment at it:

```bash
mkdir -p /tmp/rm-tls && cd /tmp/rm-tls
openssl req -x509 -newkey rsa:2048 \
  -keyout key.pem -out cert.pem -days 3650 -nodes -subj "/CN=localhost"
# -nodes leaves the key unencrypted; the server has no way to prompt for a passphrase.
```

```bash
ROCKET_MEM_ADDR=127.0.0.1:6420 ROCKET_MEM_RMP_ADDR=127.0.0.1:6421 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9220 \
ROCKET_MEM_AOF_PATH=/tmp/rm-tls/rm.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-tls/rm.snap \
ROCKET_MEM_TLS_RESP_ADDR=127.0.0.1:6430 ROCKET_MEM_TLS_RMP_ADDR=127.0.0.1:6431 \
ROCKET_MEM_TLS_CERT_PATH=/tmp/rm-tls/cert.pem ROCKET_MEM_TLS_KEY_PATH=/tmp/rm-tls/key.pem \
  ./target/release/rocket-mem &
```

All five listeners come up side by side — plaintext and TLS both:

```
Metrics on http://127.0.0.1:9220/metrics
RMP listening on 127.0.0.1:6421
TLS listening on 127.0.0.1:6430
RMP TLS listening on 127.0.0.1:6431
Listening on 127.0.0.1:6420
```

```bash
redis-cli --tls --cacert /tmp/rm-tls/cert.pem -p 6430 ping        # -> PONG
redis-cli --tls --cacert /tmp/rm-tls/cert.pem -p 6430 set both 1  # -> OK
redis-cli -p 6420 incr both                                       # -> 2, plaintext port, same engine
redis-cli --tls --cacert /tmp/rm-tls/cert.pem -p 6430 get both    # -> "2", one store behind both
redis-cli --tls --cacert /tmp/rm-tls/cert.pem -p 6430 -3 ping     # -> PONG, RESP3 works over TLS too
redis-cli --tls --insecure -p 6430 ping                           # -> PONG, skips verification entirely
redis-cli --tls -p 6430 ping   # -> SSL_connect failed: certificate verify failed (no --cacert)
```

The RMP TLS port has no CLI client — `rmp-client` speaks plaintext RMP only. Confirm that listener
with `openssl` instead:

```bash
echo | openssl s_client -connect 127.0.0.1:6431 -CAfile /tmp/rm-tls/cert.pem -servername localhost \
  2>&1 | grep -E 'Verify return code|Protocol  :'
#     Protocol  : TLSv1.3
#     Verify return code: 0 (ok)
```

### A TLS address without a cert/key is a startup error

Not a silently-unbound listener — the process exits 1:

```bash
ROCKET_MEM_TLS_RESP_ADDR=127.0.0.1:6430 ./target/release/rocket-mem
# Error: Custom { kind: InvalidInput, error: "tls_resp_addr is set but tls_cert_path/tls_key_path
#   is not -- TLS requires both" }   exit=1

ROCKET_MEM_TLS_CERT_PATH=/tmp/rm-tls/missing.pem ...
# Error: Os { code: 2, kind: NotFound, ... }   It doesn't say WHICH path was missing. Check both.

ROCKET_MEM_TLS_CERT_PATH=key.pem ROCKET_MEM_TLS_KEY_PATH=cert.pem ...
# Error: Custom { kind: InvalidData, error: "no certificate found in cert file" }   (swapped pair)
```

These abort *after* the metrics and plaintext RMP listeners are already bound and printed, so the
error scrolls past those two success lines. The plaintext `Listening on ...` line never appears.

### What a plaintext client gets on the TLS port

The TCP connect succeeds — the port is open and accepting — then the handshake fails and the
server drops the connection:

```bash
redis-cli -p 6430 ping
# Error: Protocol error, got "\x15" as reply type byte     (exit 1)
```

That `\x15` is byte 21, a TLS alert record: the server *does* answer, it just answers in TLS, and
`redis-cli` tries to read it as a RESP type byte. It is **not** silence — an older version of these
docs claimed "no reply at all", which sends you hunting for a hung connection that isn't there.

### Gotchas

| Gotcha | What happens |
|---|---|
| `tls_cert_path`/`tls_key_path` resolve relative to the server's **working directory**, not to the config file. | A relative path works from the cert's directory and dies with a bare `NotFound` from anywhere else. Use absolute paths unless you control the cwd. |
| No config-time check that the TLS and plaintext addresses differ. | Setting `tls_resp_addr` to the same port as `addr` binds TLS first, then dies on the plaintext one with `AddrInUse` — no hint the two settings collided. |
| No hostname check. | `redis-cli --tls --cacert cert.pem -p 6430` connects fine against a `CN=localhost` cert while addressing `127.0.0.1`. Don't read a passing connection as proof the name matched. |
| Handshake timeout is 10s, hardcoded. | A client that opens the TCP connection then says nothing is dropped after 10 seconds. Not configurable. |

```bash
kill %1
```

## RMP (rocket-mem's own binary protocol)

RMP listens unconditionally on its own port alongside RESP — there is no flag to turn it off. It
is not reachable with `redis-cli`; the only client is the `rmp-client` crate in this workspace.
Its headline feature over RESP is request multiplexing: a caller fires several requests on one
connection without waiting for each reply, correlating replies by `request_id` rather than by
arrival order.

```bash
ROCKET_MEM_ADDR=127.0.0.1:6450 ROCKET_MEM_RMP_ADDR=127.0.0.1:6451 \
ROCKET_MEM_AOF_PATH=/tmp/rm-rmp.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-rmp.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9250 \
  ./target/release/rocket-mem &
# -> RMP listening on 127.0.0.1:6451   (its own banner line, printed unconditionally)
```

`rmp-client` is library-only. To poke at RMP by hand, drop a throwaway example under
`crates/rmp-client/examples/`, run it, then delete it — don't leave it in the tree:

```rust
// crates/rmp-client/examples/scratch.rs
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = rmp_client::RmpClient::connect("127.0.0.1:6451").await?;
    client.set("foo", "bar").await?;
    assert_eq!(client.get("foo").await?, Some(bytes::Bytes::from_static(b"bar")));
    println!("round-trip ok: foo -> bar");
    Ok(())
}
```

```bash
cargo run -p rmp-client --example scratch
# -> round-trip ok: foo -> bar
rm crates/rmp-client/examples/scratch.rs   # don't commit it
```

RESP and RMP share one keyspace — same `Engine`, same shards, no sync involved:

```bash
redis-cli -p 6450 get foo                # -> "bar", written over RMP above
redis-cli -p 6450 set fromresp viaresp   # -> OK
# client.get("fromresp").await           -> Some(b"viaresp"), read back over RMP
```

RMP's handler builds the same `Array`-of-`Bulk` shape RESP does and calls the identical
`dispatch_and_log`, so it reaches nearly the whole command set via `client.call(vec![...])` —
`INFO`, `SAVE` (`Simple("OK")`), `SLOWLOG GET` (`Array([])`), `CLUSTER`, `REPLICAOF` all work, with
AOF logging, replica fan-out, and the read-only-replica gate applying exactly as over RESP.

`PSYNC` is the one genuine exception: RESP intercepts it in `connection.rs` above
`dispatch_and_log` for its raw-socket takeover, and RMP's handler has no equivalent, so it falls
through — `PSYNC over RMP -> Error("ERR unknown command 'PSYNC'")`. (`HELLO` is *not* an
exception; it succeeds over RMP as a stateless no-op, since RMP has no per-connection negotiation
state to persist.)

### Concurrency — read this before relying on ordering

Each RMP request is handled on its own freshly spawned Tokio task. The read loop decodes a
request, spawns a task, and immediately decodes the next one without awaiting the reply. Commands
sent back-to-back on **one** RMP connection can therefore *execute* out of order, not just reply
out of order — a real difference from RESP, which processes one connection's commands strictly in
send order. A client needing command B to observe command A's effect must await A's reply before
sending B.

Each connection caps in-flight requests at 256; pipelining past that applies ordinary TCP
backpressure (the read loop pauses) rather than spawning unbounded tasks.

```bash
kill %1
```

## Replication (`REPLICAOF`)

`REPLICAOF <host> <port>` turns the CURRENT node into a read-only follower of the node at
`<host>:<port>`: it fetches a full snapshot, then streams every subsequent write live.
`REPLICAOF NO ONE` promotes it back to normal read-write operation. `REPLICAOF` itself is always
a live command sent to an already-running node — but the *initial* connect now has a
config-file equivalent, below.

As of the `replicaof` config field, the *initial* connect can now be config-driven instead —
useful for a follower that should resume following its leader automatically after a restart
(previously a restarted follower silently came back as standalone until `REPLICAOF` was
reissued by hand — see `docs/superpowers/specs/2026-08-30-sprint-5-spec.md`'s "footgun" note).
This only covers startup: a runtime `REPLICAOF`/`REPLICAOF NO ONE` change is still not
persisted back into the file.

```bash
cat > /tmp/rm-replicaof-config.toml <<'EOF'
addr = "127.0.0.1:6401"
aof_path = "/tmp/rm-follower.aof"
snapshot_path = "/tmp/rm-follower.snap"
metrics_addr = "127.0.0.1:9201"
rmp_addr = "127.0.0.1:6481"
replicaof = "127.0.0.1:6400"
# replicaof_auth_username/replicaof_auth_password if the leader has ACL users configured
EOF
./target/release/rocket-mem --config /tmp/rm-replicaof-config.toml &
# no `redis-cli replicaof` needed -- it already links up on its own
redis-cli -p 6401 info replication      # role:slave, master_link_status:up
```

```bash
# leader
ROCKET_MEM_ADDR=127.0.0.1:6400 ROCKET_MEM_AOF_PATH=/tmp/rm-leader.aof \
ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-leader.snap ROCKET_MEM_METRICS_ADDR=127.0.0.1:9200 \
ROCKET_MEM_RMP_ADDR=127.0.0.1:6480 \
  ./target/release/rocket-mem &

# follower
ROCKET_MEM_ADDR=127.0.0.1:6401 ROCKET_MEM_AOF_PATH=/tmp/rm-follower.aof \
ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-follower.snap ROCKET_MEM_METRICS_ADDR=127.0.0.1:9201 \
ROCKET_MEM_RMP_ADDR=127.0.0.1:6481 \
  ./target/release/rocket-mem &

redis-cli -p 6401 replicaof 127.0.0.1 6400
redis-cli -p 6400 set k v
sleep 0.2
redis-cli -p 6401 get k                 # -> "v", replicated
redis-cli -p 6401 set nope x            # -> READONLY error, followers reject client writes
redis-cli -p 6401 info replication      # role:slave, master_host, master_link_status:up

redis-cli -p 6401 replicaof no one      # promote back to standalone

kill %1 %2
```

### `replica_announce_addr`: what a TLS follower tells its leader

A follower's `PSYNC` frame carries an address for the leader to advertise back out in `INFO
REPLICATION` and to log in the `repl` tracing span. By default that's `addr` -- the plaintext
RESP listen address -- even when replication itself runs over TLS. If you see a log line like
this and it looks wrong, it isn't a bug, it's this default:

```
INFO conn{... protocol=RESP tls=true}:repl{host_port=numericlabs.lxd:6479}: replica registered
```

`tls=true` (the connection) and `host_port=numericlabs.lxd:6479` (the plaintext port) are both
accurate -- they're just answering different questions. The follower connected over TLS, but it
told its leader to reach it back at its plaintext port, because nothing said otherwise.

Set `replica_announce_addr` to the follower's own TLS address to make the two agree:

```toml
addr = "numericlabs.lxd:6479"
tls_resp_addr = "numericlabs.lxd:16479"
replicaof = "numericlabs.lxd:16379"
replica_announce_addr = "numericlabs.lxd:16479"
```

With that set, the leader's `INFO REPLICATION` and the `repl` span both report `16479`, matching
the TLS transport the connection actually used. Leaving it unset on a TLS follower now also
prints one `WARN` at startup naming the plaintext address it's about to announce -- see
`docs/superpowers/specs/2026-09-10-replica-announce-addr-spec.md`.

## Cluster mode

Needs a topology file — the one real "config file" this project has. Plain text,
`<node-id> <host:port> <first-slot> <last-slot>` per line, must cover all 16384 slots exactly
once (a gap or overlap is a startup error).

```bash
cat > /tmp/cluster.conf <<'EOF'
shard-a 127.0.0.1:7001 0     5460
shard-b 127.0.0.1:7002 5461  10922
shard-c 127.0.0.1:7003 10923 16383
EOF
```

Start all three — **remember the distinct metrics port and RMP port per node**:

```bash
ROCKET_MEM_ADDR=127.0.0.1:7001 ROCKET_MEM_AOF_PATH=/tmp/rm-a.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-a.snap \
ROCKET_MEM_CLUSTER_CONFIG=/tmp/cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-a \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9121 ROCKET_MEM_RMP_ADDR=127.0.0.1:6480 \
  ./target/release/rocket-mem &

ROCKET_MEM_ADDR=127.0.0.1:7002 ROCKET_MEM_AOF_PATH=/tmp/rm-b.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-b.snap \
ROCKET_MEM_CLUSTER_CONFIG=/tmp/cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-b \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9122 ROCKET_MEM_RMP_ADDR=127.0.0.1:6481 \
  ./target/release/rocket-mem &

ROCKET_MEM_ADDR=127.0.0.1:7003 ROCKET_MEM_AOF_PATH=/tmp/rm-c.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-c.snap \
ROCKET_MEM_CLUSTER_CONFIG=/tmp/cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-c \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9123 ROCKET_MEM_RMP_ADDR=127.0.0.1:6482 \
  ./target/release/rocket-mem &
```

```bash
redis-cli -p 7001 cluster keyslot foo    # -> 12182, owned by shard-c
redis-cli -p 7001 cluster shards
redis-cli -p 7001 cluster nodes

redis-cli -p 7001 set foo bar            # wrong node -> MOVED 12182 127.0.0.1:7003, no write happens
redis-cli -p 7003 set foo bar            # right node -> OK
redis-cli -p 7003 get foo                # -> "bar"

redis-cli -p 7001 mset hello 1 foo 2     # different slots -> CROSSSLOT error

redis-cli -p 7001 cluster keyslot '{user1000}.name'   # same slot as...
redis-cli -p 7001 cluster keyslot '{user1000}.city'   # ...this, via the hash tag

kill %1 %2 %3
```

## Cleanup

Background jobs (`&`) started in one shell session don't survive across separate tool
invocations/terminals. Find and kill strays with:

```bash
ps aux | grep rocket-mem | grep -v grep
ss -tlnp | grep -E ':(6379|6399|6400|6401|641[01]|642[01]|643[01]|644[0-3]|645[01]|7001|7002|7003|9121|9122|9123|9199|9200|9201|9210|9220|9240|9250)\b'
```

Kill strays **by PID** from that `ss` output, not with `pkill -f rocket-mem` — a broad pattern
kill also takes out any other server you or a parallel session has running, including the
leader/follower pair `scripts/chaos.sh` manages. That bit us while writing this guide.
