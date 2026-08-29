# RESP3 Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** prove `HELLO 3` negotiation actually persists connection-wide (not just for `HELLO`'s own reply), then confirm real RESP3-aware clients (`redis-py` with `protocol=3`, `ioredis`'s RESP3-by-default behavior) now connect without the `protocol=2` workaround `phase-1-retro.md` flagged.

**Architecture:** no new production code — Task 1 adds one automated regression test; Task 2 is manual verification against real client libraries, recorded in `client-verification-results.md`.

**Tech Stack:** `redis-py` and `ioredis`, already installed in this session's scratchpad from Sprint 2's `07-manual-client-verification.md` (a venv at
`/tmp/claude-1000/-home-numericlabs-data-rocket-rocket-mem/c6ad3a9b-ca6a-4de3-89aa-5f6b2ad905c0/scratchpad/venv`,
a `node_modules/ioredis` at
`/tmp/claude-1000/-home-numericlabs-data-rocket-rocket-mem/c6ad3a9b-ca6a-4de3-89aa-5f6b2ad905c0/scratchpad/node-verify`
— if this session's scratchpad no longer exists, reinstall with
`pip install redis` / `npm install ioredis` exactly as `07-manual-client-verification.md` did).

**Spec:** `../../specs/2026-08-29-resp3-design.md` — the Testing section's manual-verification
requirements are authoritative.

**Depends on:** `02-dispatch-and-connection-wiring.md` must be complete.

## Global Constraints

- This plan does **not** add a `_\r\n` decode arm to `RespCodec::decode()` — Task 1's
  regression test reads raw bytes off a `TcpStream` (like
  `crates/server/tests/integration.rs`'s existing
  `malformed_resp_input_gets_a_graceful_disconnect_not_a_crash` test already does) rather
  than decoding through `RespCodec`, precisely to keep `01-frame-map-and-stateful-codec.md`'s
  "`decode()` needs zero changes" constraint intact. `RespCodec` is only ever used
  client-side in this project's own test harness to *drive* the server (encoding
  commands); it never needs to *decode* RESP3-only reply types, since real clients bring
  their own decoder.
- If this machine still has a real Redis server bound to `127.0.0.1:6379` (true as of
  Sprint 2's manual verification), bind the temporary rocket-mem instance used for manual
  verification to an alternate port (`127.0.0.1:16379`, as `client-verification-results.md`
  already did) instead — temporarily edit `crates/server/src/main.rs`'s bind address for
  the verification session, then revert it before committing, exactly as
  `07-manual-client-verification.md`'s execution did.

---

### Task 1: Regression test — `HELLO 3` negotiation persists for the rest of the connection

**Files:**
- Modify: `crates/server/tests/integration.rs`

**Interfaces:**
- Consumes: `spawn_test_server()` (already defined in this file by
  `06-integration-test-harness.md`'s Task 3).

- [ ] **Step 1: Write the failing test**

```rust
// add to crates/server/tests/integration.rs
#[tokio::test]
async fn hello_3_negotiation_persists_for_the_rest_of_the_connection() {
    let url = spawn_test_server().await;
    let addr = url.strip_prefix("redis://").unwrap();
    let mut raw = TcpStream::connect(addr).await.unwrap();

    // HELLO 3 — the reply must be a native RESP3 map (`%7\r\n...`), not a RESP2-emulated
    // flattened array, since hello_reply() always returns 7 key/value pairs.
    raw.write_all(b"*2\r\n$5\r\nHELLO\r\n$1\r\n3\r\n")
        .await
        .unwrap();
    let mut buf = vec![0u8; 512];
    let n = raw.read(&mut buf).await.unwrap();
    let reply = &buf[..n];
    assert!(
        reply.starts_with(b"%7\r\n"),
        "expected a native RESP3 map reply to HELLO 3, got {:?}",
        String::from_utf8_lossy(reply)
    );

    // GET on a missing key must now come back RESP3-encoded (`_\r\n`), not RESP2's
    // `$-1\r\n` — proving the negotiated protocol persists beyond HELLO's own reply.
    raw.write_all(b"*2\r\n$3\r\nGET\r\n$7\r\nmissing\r\n")
        .await
        .unwrap();
    let mut buf2 = [0u8; 16];
    let n2 = raw.read(&mut buf2).await.unwrap();
    assert_eq!(
        &buf2[..n2],
        b"_\r\n",
        "expected RESP3 null after HELLO 3 was negotiated, got {:?}",
        &buf2[..n2]
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rocket-mem --test integration hello_3_negotiation_persists_for_the_rest_of_the_connection`
Expected: FAIL if `02-dispatch-and-connection-wiring.md` was implemented as specified,
this should actually PASS immediately — its purpose is a regression guard, not new
production behavior. Treat any failure here as a bug in `02`'s implementation
(most likely: the `framed.codec_mut().protocol = protocol;` sync line missing or placed
after `send()` instead of before it), not a reason to weaken this test.

- [ ] **Step 3: If it failed, fix `connection.rs`**

The contract to restore: `handle_connection` must set `framed.codec_mut().protocol =
protocol` **before** calling `framed.send(response)` for every response, not just
`HELLO`'s — otherwise `HELLO 3`'s own reply would be RESP3-encoded but every later
command's reply would silently stay RESP2-encoded.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rocket-mem --test integration`
Expected: PASS, 5/5 (4 existing + 1 new)

- [ ] **Step 5: Run the full workspace check**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean.

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/tests/integration.rs` — do not compose the commit message freeform.
Suggested subject: `test(server): prove HELLO 3 negotiation persists connection-wide`.

---

### Task 2: Manual verification against `redis-py` and `ioredis`

**Files:**
- Modify: `docs/superpowers/plans/2026-08-29-sprint-2-plans/client-verification-results.md`

- [ ] **Step 1: Start the server**

Temporarily edit `crates/server/src/main.rs`'s bind address to `127.0.0.1:16379` if a
real Redis instance already occupies `6379` on this machine (check with
`redis-cli -p 6379 ping` — if it replies `PONG`, use the alternate port). Then:

```bash
cargo build --release --bin rocket-mem
./target/release/rocket-mem &
```

- [ ] **Step 2: Verify `redis-py` connects with `protocol=3` — no workaround needed**

```bash
/tmp/claude-1000/-home-numericlabs-data-rocket-rocket-mem/c6ad3a9b-ca6a-4de3-89aa-5f6b2ad905c0/scratchpad/venv/bin/python3 -c "
import redis
r = redis.Redis(host='127.0.0.1', port=16379, decode_responses=True, protocol=3)
print('PING:', r.ping())
print('SET:', r.set('foo', 'bar'))
print('GET:', r.get('foo'))
print('INCR:', r.incr('counter'))
print('HSET:', r.hset('h', 'field', 'value'))
print('HGET:', r.hget('h', 'field'))
print('RPUSH:', r.rpush('l', 'a'))
print('LRANGE:', r.lrange('l', 0, -1))
print('SADD:', r.sadd('s', 'member'))
print('SISMEMBER:', r.sismember('s', 'member'))
"
```

Expected: every line prints a sensible value, no exception — and critically, this now
works with `protocol=3` and **no** `protocol=2` fallback, unlike
`client-verification-results.md`'s Sprint 2 finding (redis-py 8.1.0's default health
check hard-failing on `HELLO`). Adjust the port in the command above if you used `6379`.

- [ ] **Step 3: Verify `ioredis` connects with RESP3 (its default) — no fallback needed**

```bash
cd /tmp/claude-1000/-home-numericlabs-data-rocket-rocket-mem/c6ad3a9b-ca6a-4de3-89aa-5f6b2ad905c0/scratchpad/node-verify
node -e "
const Redis = require('ioredis');
const r = new Redis({ port: 16379, host: '127.0.0.1', protocol: 3 });
(async () => {
  console.log('PING:', await r.ping());
  console.log('SET:', await r.set('foo2', 'bar2'));
  console.log('GET:', await r.get('foo2'));
  console.log('INCR:', await r.incr('counter2'));
  console.log('HSET:', await r.hset('h2', 'field', 'value'));
  console.log('HGET:', await r.hget('h2', 'field'));
  console.log('RPUSH:', await r.rpush('l2', 'a'));
  console.log('LRANGE:', await r.lrange('l2', 0, -1));
  console.log('SADD:', await r.sadd('s2', 'member'));
  console.log('SISMEMBER:', await r.sismember('s2', 'member'));
  r.disconnect();
})();
"
```

`ioredis` already defaults to `protocol: 3` (sends `HELLO 3` on connect, per its own
README's "RESP3 Protocol" section) — Sprint 2's manual verification worked *despite* this
because rocket-mem's `HELLO` error triggered ioredis's documented fallback-to-RESP2 path.
Passing `protocol: 3` explicitly here just makes that intent visible in the command; the
real change under test is that rocket-mem no longer forces the fallback path at all.

Expected: every line prints a sensible value, no exception, and no fallback occurs.

- [ ] **Step 4: Stop the server and revert the port if changed**

```bash
kill %1  # or the appropriate job/PID for the backgrounded rocket-mem process
```

If `main.rs`'s bind address was changed in Step 1, revert it back to `127.0.0.1:6379` and
confirm `git diff crates/server/src/main.rs` is empty before continuing.

- [ ] **Step 5: Record the results**

Append a new section to the existing
`docs/superpowers/plans/2026-08-29-sprint-2-plans/client-verification-results.md`:

```markdown

## RESP3 follow-up (2026-08-29-resp3-design.md)

- **redis-py 8.1.0, `protocol=3`:** PASS — connects and runs the full smoke sequence with
  no `protocol=2` workaround needed, resolving the caveat recorded above.
- **ioredis 6.0.0, RESP3 (its default):** PASS — connects and runs the full smoke
  sequence natively in RESP3 mode; Sprint 2's earlier PASS relied on ioredis's documented
  RESP2 fallback, which is no longer exercised now that `HELLO` succeeds.
- **Automated regression coverage:** `hello_3_negotiation_persists_for_the_rest_of_the_connection`
  in `crates/server/tests/integration.rs` proves the negotiated protocol persists beyond
  `HELLO`'s own reply, at the byte level.
```

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`docs/superpowers/plans/2026-08-29-sprint-2-plans/client-verification-results.md` — do
not compose the commit message freeform. Suggested subject:
`docs: record RESP3 manual verification results (redis-py, ioredis)`.
