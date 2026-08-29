# Sprint 2 — Manual Client Verification Results

**Environment note:** the machine already has a real Redis server (8.10.1) bound to
`127.0.0.1:6379`. All three checks below ran rocket-mem on `127.0.0.1:16379` instead
(`crates/server/src/main.rs`'s bind address was temporarily changed for this session,
then reverted — no permanent change to the shipped port).

## redis-py

- **Version tested:** 8.1.0 (`pip show redis`)
- **Date:** 2026-08-29
- **Result:** PASS, with a caveat — see Notes
- **Output (with `protocol=2` passed explicitly):**
  ```
  PING: True
  SET: True
  GET: bar
  INCR: 1
  HSET: 1
  HGET: value
  RPUSH: 1
  LRANGE: ['a']
  SADD: 1
  SISMEMBER: 1
  ```
- **Notes:** with default client settings (no `protocol=2`), redis-py 8.1.0 does **not**
  fall back to RESP2 silently on `HELLO` failure — it raises
  `redis.exceptions.ResponseError: unknown command 'HELLO'` from its connection
  health-check and the connection never gets used. This contradicts
  `2026-08-29-sprint-2-spec.md`'s RESP3/`HELLO` decision, which assumed all three
  targeted client libraries fall back automatically. Passing `protocol=2` explicitly to
  `redis.Redis(...)` avoids the `HELLO` probe entirely and every command then works
  correctly (see output above) — this is a one-line client-side connection option, not a
  rocket-mem-side fix. Recorded as PASS since the server's RESP2 behavior itself is
  fully correct; flagging this so a future reader knows newer redis-py defaults need
  `protocol=2` against rocket-mem specifically, and the spec's "all three fall back
  automatically" claim needs a footnote rather than being taken as still-accurate for
  every redis-py version.

## ioredis

- **Version tested:** 6.0.0 (`npm list ioredis`)
- **Date:** 2026-08-29
- **Result:** PASS
- **Output:**
  ```
  PING: PONG
  SET: OK
  GET: bar
  INCR: 2
  HSET: 1
  HGET: value
  RPUSH: 2
  LRANGE: [ 'a', 'a' ]
  SADD: 1
  SISMEMBER: 1
  ```
- **Notes:** default client settings, no special flags needed. `INCR`/`RPUSH`/`LRANGE`
  values reflect state left over from the redis-py run immediately before this one
  (same server instance, not restarted between the two) — expected, not a bug. `ioredis`
  tolerated the `HELLO`-equivalent capability probing gracefully as the spec predicted;
  it never raised or blocked on rocket-mem's unknown-command response to anything it
  sent beyond the smoke sequence's own commands.

## redis-cli — full Sprint 1 command set

- **Date:** 2026-08-29
- **Result:** PASS
- **Full transcript** (server restarted fresh before this run):
  ```
  $ redis-cli -p 16379 <<'EOF'
  SET foo bar
  GET foo
  SET k v NX
  SET k v2 NX
  APPEND foo baz
  STRLEN foo
  INCR counter
  DECR counter
  INCRBY counter 10
  HSET h f v
  HGET h f
  HDEL h f
  HGETALL h
  HEXISTS h f
  HLEN h
  RPUSH l a
  LPUSH l z
  LRANGE l 0 -1
  LPOP l
  RPOP l
  LLEN l
  SADD s m
  SISMEMBER s m
  SCARD s
  SREM s m
  SMEMBERS s
  PING
  ECHO hello
  EOF

  OK
  bar
  OK
  (nil)
  6
  6
  1
  0
  10
  1
  v
  1
  (empty array)
  0
  0
  1
  2
  z
  a
  z
  a
  0
  1
  1
  1
  1
  (empty array)
  PONG
  hello
  ```
- **Notes:** no `(error)` lines anywhere. The two semantically-expected non-values —
  `SET k v2 NX` returning nil (key already existed from the prior `SET k v NX`) and
  `HGETALL`/`SMEMBERS` returning empty after their sole member was deleted/removed —
  both came back correctly. `APPEND foo baz` on `"bar"` correctly returned `6`
  (`"barbaz"`).
