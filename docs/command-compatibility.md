# Command compatibility

`rocket-mem` speaks the Redis wire protocol (RESP2/RESP3), and — as of Sprint 7 — a second
protocol of its own, RMP. Both protocols route through the same command dispatcher, so
everything on this page applies equally to a `redis-cli`/`redis-py`/`ioredis`/`go-redis` client
and to an RMP client (see `docs/superpowers/specs/2026-08-31-sprint-7-spec.md` for the RMP wire
format, and `README.md`'s "Running the custom protocol (RMP)" section for how to reach it).

This page lists every command the dispatcher recognizes, calls out every known divergence from
real Redis behavior, and names notable real-Redis commands that have no counterpart here. For
environment variables and config-file options, see
[`docs/config-reference.md`](config-reference.md).

## Command coverage

| Type | Implemented |
|---|---|
| String/Key | `GET`, `SET` (`NX`/`XX`/`EX`/`PX`), `GETSET`, `GETRANGE`, `SETRANGE`, `APPEND`, `STRLEN`, `INCR`/`DECR`/`INCRBY`, `MSET`, `MGET`, `MSETNX`, `RENAME`, `RENAMENX`, `TYPE`, `RANDOMKEY`, `KEYS` (glob: `*`, `?`, `[abc]` only), `SCAN`, `DEL`/`EXISTS` (variadic), `EXPIRE`, `PEXPIRE`, `EXPIREAT`, `PEXPIREAT`, `TTL`, `PTTL`, `PERSIST`, `MEMORY USAGE`, `OBJECT ENCODING` |
| Hash | `HSET`, `HGET`, `HDEL`, `HEXISTS`, `HGETALL`, `HLEN`, `HINCRBY`, `HKEYS`, `HVALS`, `HMGET`, `HSETNX`, `HSCAN` |
| List | `LPUSH`, `RPUSH` (both variadic), `LPOP`, `RPOP`, `LRANGE`, `LLEN`, `LINDEX`, `LSET`, `LTRIM`, `LREM`, `LINSERT` |
| Set | `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`, `SINTER`, `SUNION`, `SDIFF`, `SINTERSTORE`, `SUNIONSTORE`, `SDIFFSTORE`, `SPOP`, `SRANDMEMBER` |
| Sorted Set | `ZADD`, `ZSCORE`, `ZREM`, `ZCARD`, `ZINCRBY`, `ZRANGE`, `ZRANK` |
| Server/Cluster | `PING`, `ECHO`, `DEBUG SLEEP`[^debug-sleep-cap], `SELECT`, `COMMAND`, `HELLO`, `INFO [section]`, `SAVE`, `REPLICAOF`, `PSYNC`, `CLUSTER KEYSLOT`/`SHARDS`/`NODES`/`INFO`/`MYID`, `SLOWLOG GET`/`LEN`/`RESET` |
| Auth/ACL | `AUTH` (both single-arg and `<user> <pass>` forms), `ACL SETUSER`/`DELUSER`/`WHOAMI`/`LIST`/`GETUSER` |

[^debug-sleep-cap]: `DEBUG SLEEP` is capped at a 10-second maximum duration; a longer request is rejected with an error rather than accepted and blocking a server thread indefinitely.

`KEYS`'s glob support is intentionally partial: no character ranges (`[a-z]`), negation
(`[^abc]`), or escaping. Active expiry sweeps one whole shard per 100ms tick rather than
sampling individual keys within a shard the way real Redis does — an accepted simplification,
not a bug. `OBJECT ENCODING` reports this engine's own type name (`string`/`list`/`hash`/`set`/
`zset` — exactly what `TYPE` returns, since both come from `Value::type_name()`), not real
Redis's actual internal encodings (`embstr`/`listpack`/etc.), which this engine doesn't
implement.

## Known divergences from real Redis

- **`KEYS`'s glob support is partial.** No character ranges (`[a-z]`), negation (`[^abc]`), or
  escaping — only `*`, `?`, and `[abc]`-style literal-character classes.
- **Active expiry sweeps a whole shard per tick, not per-key sampling.** One whole shard is
  swept every 100ms, rather than sampling individual keys within a shard the way real Redis
  does. This is an accepted simplification, not a bug.
- **`OBJECT ENCODING` reports this engine's own type names, not real Redis's internal
  encodings.** It returns `string`/`list`/`hash`/`set`/`zset` — exactly what `TYPE` returns,
  since both come from `Value::type_name()` — not real Redis's actual internal encodings
  (`embstr`/`listpack`/etc.), which this engine doesn't implement.
- **`SLOWLOG` entries carry 4 fields, not real Redis's 6.** Real Redis ≥4.0 emits 6 fields per
  entry (adding client address and client name). This implementation emits the original 4,
  because `dispatch_and_log` never learns the peer address — `handle_connection` has the
  `TcpStream` but passes only `client_id` down — and threading a `SocketAddr` through six call
  layers for two cosmetic fields wasn't judged worth it. Clients that index fields positionally
  read the same first four either way. Relatedly, a slow-log entry records the command name and
  its first argument rather than the full argument list, with real Redis's own
  `... (N more arguments)` marker standing in for the rest.
- **`INFO`'s `expired_keys` counts only active expiry, not passive.** It counts only *actively*
  expired keys (the background sweep's removal count); passive expiry — a read finding a key
  already dead — removes keys without counting them, since threading a counter into that path
  would touch the hottest read path in the project.
- **No partial replication resync — every (re)sync is a full resync.** A dropped follower
  connection always triggers a full resnapshot; there is no partial-resync/offset-resume
  support. Relatedly, there is no true replication-*offset* lag metric, because this
  full-resync-only design means no offsets exist —
  `rocket_mem_replication_last_apply_timestamp_seconds` is the substitute reported instead.
- **`DEBUG SLEEP` is capped at a 10-second maximum duration.** A longer request is rejected with
  an error rather than accepted and blocking a server thread indefinitely.
- **No `@category` ACL grants (`+@read`, `+@write`, ...).** Real Redis's category taxonomy is
  large and nothing here needs it yet; only explicit `+CMDNAME`/`-CMDNAME` grants plus
  `allcommands`/`nocommands` (equivalently spelled `+@all`/`-@all`) are accepted — any other
  `+@category`/`-@category` token is a syntax error.
- **ACL users are in-memory only.** A runtime `ACL SETUSER` is not persisted to the AOF or
  snapshot, and is lost on restart unless the same user is also declared in the `[[acl.users]]`
  bootstrap array of the TOML config (see [`docs/config-reference.md`](config-reference.md)).
  This mirrors real Redis's own `ACL SETUSER` behavior when `ACL SAVE`/`aclfile` isn't
  configured — this project has no `ACL SAVE` command and no `aclfile` equivalent beyond that
  bootstrap array.
- **ACL changes are leader-local, not replicated.** `ACL SETUSER`/`DELUSER` are not among the
  commands logged to the AOF or fanned out to replicas, so a follower's ACL state can diverge
  from its leader's unless both are started from the same bootstrap config.
- **Only `AUTH` and `HELLO` are reachable before authenticating.** This matches real Redis's own
  `CMD_NO_AUTH` set (`AUTH`, `HELLO`, `RESET`), minus `RESET`, which this project doesn't
  implement. Notably, `ACL` is **not** exempt — it is gated like any other command, subject to
  the same `NOAUTH`/`NOPERM` checks, so an unauthenticated client can't run `ACL SETUSER
  attacker on >x allcommands allkeys` and then authenticate as its own newly-created user.
- **`ACL LIST` output is not round-trippable.** `ACL LIST` renders a user's password as
  `#<hash>` (its stored Argon2 hash), matching real Redis's own rendering — but that `#<hash>`
  form is not accepted as an input token by `ACL SETUSER`; only `>password` (a plaintext
  password to hash) and `nopass` are accepted there. `ACL LIST`'s user ordering is also
  unspecified: users are stored in a hash map, so the order returned can vary between calls.

## Commands not implemented

The following real-Redis commands (and command families) have no counterpart in this project.
None of these are silent gaps — they're deliberately out of scope for the current sprint plan,
and Lua scripting, pub/sub, transactions, and streams are explicitly tracked as a future backlog
in [`docs/rocket-mem-sprint-plan.md`](rocket-mem-sprint-plan.md) (its retro entry scopes a
follow-on backlog covering exactly these four areas, plus live cluster resharding).

- **List/sorted-set extras:** `LPOS`, `LMPOP`, `ZMPOP`, and their blocking (`B*`) counterparts
  (`BLPOP`, `BRPOP`, `BLMPOP`, `BZMPOP`, `BZPOPMIN`/`BZPOPMAX`, etc.).
- **Key/object extras:** `COPY`, `OBJECT FREQ`, `OBJECT IDLETIME`, `WAIT`, `LOLWUT`.
- **Lua scripting:** `EVAL`, `EVALSHA`, `SCRIPT LOAD`/`EXISTS`/`FLUSH`.
- **Pub/sub:** `SUBSCRIBE`, `UNSUBSCRIBE`, `PUBLISH`, `PSUBSCRIBE`, and related commands.
- **Transactions:** `MULTI`, `EXEC`, `DISCARD`, `WATCH`/`UNWATCH`.
- **Streams:** `XADD` and the rest of the stream command family.
- **Cluster live operations:** `CLUSTER SETSLOT`, `MIGRATE`, `ASK`/`ASKING` — slot ownership is
  fixed at process start via a static config file, so there is no live resharding or failover.
  `CLUSTER SLOTS` is also not implemented (deprecated since Redis 7.0 in favor of
  `CLUSTER SHARDS`, which is implemented).
