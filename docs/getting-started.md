# Getting started

This page takes you from a fresh clone to an authenticated `redis-cli` session and a first RMP
round-trip. It assumes nothing about the rest of this project's docs — everything you need to
run the server is here, with pointers at the end for going deeper.

## Build

```bash
git clone https://github.com/Snehal1112/rocket-mem.git
cd rocket-mem
cargo build --release --bin rocket-mem
```

The binary lands at `target/release/rocket-mem`. Every example below either runs it directly or
through `cargo run --release --bin rocket-mem --`; the two are interchangeable.

## First run, no configuration at all

`rocket-mem` needs no configuration file and no environment variables to start:

```bash
target/release/rocket-mem
```

It binds three loopback listeners and prints each one:

```
Recovered state from ./dump.snapshot and ./appendonly.aof
Metrics on http://127.0.0.1:9121/metrics
RMP listening on 127.0.0.1:6380
Listening on 127.0.0.1:6379
```

- `127.0.0.1:6379` — RESP, the Redis wire protocol. Any Redis client can talk to it.
- `127.0.0.1:6380` — RMP, rocket-mem's own binary protocol.
- `127.0.0.1:9121` — the Prometheus `/metrics` endpoint, loopback-only because it is
  unauthenticated.

There is no authentication at this point: with no ACL users configured, every connection may run
every command. Configuration is additive — you opt into auth, TLS, clustering, and non-default
paths, and none of it is required to get a working server.

In another terminal:

```bash
redis-cli -p 6379 PING
redis-cli -p 6379 SET foo bar
redis-cli -p 6379 GET foo
```

Two files appear in the working directory as you write: `appendonly.aof` (every write, appended
and replayed on the next startup) and, once you run `SAVE`, `dump.snapshot`. Both paths are
configurable — see [`config-reference.md`](config-reference.md).

## A minimal `rocket-mem.toml`

Configuration comes from four layers, merged in this order: built-in defaults, then a TOML file,
then `ROCKET_MEM_*` environment variables, then CLI flags. A minimal file that turns on
authentication:

```toml
addr = "127.0.0.1:6379"
rmp_addr = "127.0.0.1:6380"

[[acl.users]]
username = "app"
password = "changeme"
enabled = true
rules = ["allcommands", "allkeys"]
```

Run it:

```bash
target/release/rocket-mem --config rocket-mem.toml
```

`--config` is optional: with no flag, `rocket-mem` picks up `rocket-mem.toml` from the current
directory if it exists, and starts from built-in defaults if it doesn't. Neither case is an error.

Defining even one ACL user switches authentication on for every connection. Only `AUTH` and
`HELLO` are reachable before authenticating; everything else gets
`NOAUTH Authentication required.`

## First `redis-cli` session

```bash
redis-cli -u 'redis://app:changeme@127.0.0.1:6379' --no-auth-warning PING
```

Or, authenticating explicitly inside an interactive session:

```
$ redis-cli -p 6379
127.0.0.1:6379> GET foo
(error) NOAUTH Authentication required.
127.0.0.1:6379> AUTH app changeme
OK
127.0.0.1:6379> SET foo bar
OK
127.0.0.1:6379> GET foo
"bar"
127.0.0.1:6379> ACL WHOAMI
"app"
```

`AUTH <password>` (the single-argument form) works too, but only if you have configured a user
literally named `default` — it authenticates against that user, so against the config above,
which defines `app` and no `default`, it returns
`WRONGPASS invalid username-password pair or user is disabled.` A RESP3 client can also send its
credentials inline as `HELLO 3 AUTH app changeme`.

Users defined in `rocket-mem.toml` are bootstrapped at startup; `ACL SETUSER` adds or edits them
at runtime. Runtime changes live in memory only and are lost on restart unless they are also
written into the TOML file. See [`command-compatibility.md`](command-compatibility.md) for the
full `ACL` command family and its rule-token vocabulary.

## First RMP session

RMP is rocket-mem's own binary protocol, on its own port, always listening. Its headline
capability is request multiplexing: a client sends many requests on one connection without
waiting for each reply, tagging each with a `request_id` the response echoes back, so replies may
arrive in any order. The `rmp-client` crate is a minimal async Rust client for it:

```rust
let client = rmp_client::RmpClient::connect("127.0.0.1:6380").await?;
client.set("foo", "bar").await?;
assert_eq!(client.get("foo").await?, Some(bytes::Bytes::from_static(b"bar")));
```

RESP and RMP share one keyspace — write over one, read it back over the other. If ACL users are
configured, an RMP connection authenticates the same way a RESP one does, by sending `AUTH` as
its first command.

## Enabling TLS

TLS listeners run *alongside* the plaintext ones, on their own addresses, and need a PEM
certificate chain and private key. Set `tls_resp_addr` and/or `tls_rmp_addr` plus
`tls_cert_path` and `tls_key_path` — a TLS address without a cert and key is a startup error,
not a listener that silently never binds.

For local testing, a self-signed certificate is enough:

```bash
openssl req -x509 -newkey rsa:2048 -keyout key.pem -out cert.pem \
  -days 3650 -nodes -subj "/CN=localhost"
```

This is for local testing only — get a real certificate from a CA for anything else.

```toml
tls_resp_addr = "127.0.0.1:6390"
tls_rmp_addr = "127.0.0.1:6391"
tls_cert_path = "cert.pem"
tls_key_path = "key.pem"
```

The server then prints `TLS listening on 127.0.0.1:6390` and
`RMP TLS listening on 127.0.0.1:6391` alongside its plaintext listeners. Connect with
`redis-cli --tls --cacert cert.pem -p 6390`. A plaintext client that connects to a TLS port
never gets a usable reply: the TCP connection is accepted, but the handshake fails and the
connection is dropped. `redis-cli -p 6390 PING` reports
`Protocol error, got "\x15" as reply type byte` — that `\x15` is a TLS alert record, not a RESP
reply.

See [`config-reference.md`](config-reference.md) for every `tls_*` field's env var and CLI flag
equivalents.

## Where to go next

- [`config-reference.md`](config-reference.md) — every configuration field: TOML key, environment
  variable, CLI flag, default, and the precedence rules between them.
- [`command-compatibility.md`](command-compatibility.md) — the full command table, the known
  divergences from real Redis, and what isn't implemented at all.
- [`architecture.md`](architecture.md) — the three-layer design, the concurrency model, and where
  each sprint's decisions are recorded.
- [`../README.md`](../README.md)'s "Running a cluster" section — hash-slot routing across multiple
  nodes, the topology file format, and `-MOVED` redirection.
