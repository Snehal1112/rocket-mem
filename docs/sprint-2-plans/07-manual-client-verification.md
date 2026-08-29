# Manual Client Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** confirm real, unmodified client libraries in at least two non-Rust languages can connect to rocket-mem and run a basic workload — the actual claim Sprint 2 makes ("real Redis clients... work against your server unmodified"), which `06-integration-test-harness.md`'s `redis-rs` tests can't fully prove since `redis-rs` is Rust and was written *for* this project's tests.

**Architecture:** this is a manual verification pass, not new production code — there is no TDD cycle here. The deliverable is a results record committed to the repo, per the production plan's Week 4 sub-task ("manual checklist: run the same SET/GET/HSET smoke sequence via `redis-py` and `ioredis`, record results").

**Tech Stack:** `redis-py` (Python), `ioredis` (Node.js) — the two libraries the sprint plan names explicitly. `go-redis` is a stretch goal if time allows; the P1 line item's bar is "at least 2 non-Rust client libraries," which `redis-py` + `ioredis` already satisfies.

**Depends on:** `06-integration-test-harness.md` must be complete (the server needs the full command set wired before this is a meaningful test).

---

### Task 1: Run the smoke sequence against `redis-py`

**Files:**
- Create: `docs/sprint-2-plans/client-verification-results.md`

- [ ] **Step 1: Start the server**

Run: `cargo run --bin rocket-mem` (leave it running in a terminal; note the port it prints)

- [ ] **Step 2: Install and run `redis-py` against it**

```bash
pip install redis
python3 -c "
import redis
r = redis.Redis(host='127.0.0.1', port=6379, decode_responses=True)
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

Expected: every line prints a sensible value (`True`, `'OK'`, `'bar'`, `1`, `1`, `'value'`, `1`, `['a']`, `1`, `True`) with no exceptions raised. `redis-py` sends `HELLO` on connect by default in recent versions — confirm it falls back to RESP2 silently per `00-sprint-2-spec.md`'s decision rather than raising a connection error. If it does raise, that decision needs revisiting before this task can pass, not this checklist.

- [ ] **Step 3: Record the result**

```markdown
<!-- docs/sprint-2-plans/client-verification-results.md -->
# Sprint 2 — Manual Client Verification Results

## redis-py
- **Version tested:** <fill in the version `pip show redis` reports>
- **Date:** <today's date>
- **Result:** <PASS/FAIL, plus exact output or error text>
- **Notes:** <anything unexpected — e.g. did HELLO fall back cleanly>
```

- [ ] **Step 4: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`docs/sprint-2-plans/client-verification-results.md` — do not compose the commit message
freeform. Suggested subject: `docs: record redis-py manual verification result`.

---

### Task 2: Run the smoke sequence against `ioredis`

**Files:**
- Modify: `docs/sprint-2-plans/client-verification-results.md`

- [ ] **Step 1: Install and run `ioredis` against the still-running server**

```bash
npm init -y --silent && npm install ioredis --silent
node -e "
const Redis = require('ioredis');
const r = new Redis({ port: 6379, host: '127.0.0.1' });
(async () => {
  console.log('PING:', await r.ping());
  console.log('SET:', await r.set('foo', 'bar'));
  console.log('GET:', await r.get('foo'));
  console.log('INCR:', await r.incr('counter'));
  console.log('HSET:', await r.hset('h', 'field', 'value'));
  console.log('HGET:', await r.hget('h', 'field'));
  console.log('RPUSH:', await r.rpush('l', 'a'));
  console.log('LRANGE:', await r.lrange('l', 0, -1));
  console.log('SADD:', await r.sadd('s', 'member'));
  console.log('SISMEMBER:', await r.sismember('s', 'member'));
  r.disconnect();
})();
"
```

Expected: every line prints a sensible value with no unhandled promise rejections. `ioredis` also probes capabilities on connect (it may send `INFO` and/or `CLIENT` commands beyond `HELLO`) — confirm none of those cause it to refuse the connection; if `ioredis` sends a command rocket-mem doesn't recognize yet, the RESP error response should be enough for `ioredis` to proceed (it treats unrecognized-command errors from optional startup probes as non-fatal) rather than a hard requirement to implement that command this sprint.

- [ ] **Step 2: Record the result**

```markdown
<!-- append to docs/sprint-2-plans/client-verification-results.md -->

## ioredis
- **Version tested:** <fill in the version `npm list ioredis` reports>
- **Date:** <today's date>
- **Result:** <PASS/FAIL, plus exact output or error text>
- **Notes:** <any startup commands ioredis sent that rocket-mem doesn't implement, and whether ioredis tolerated the error>
```

- [ ] **Step 3: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`docs/sprint-2-plans/client-verification-results.md` — do not compose the commit message
freeform. Suggested subject: `docs: record ioredis manual verification result`.

---

### Task 3: `redis-cli` full command-set pass (the sprint's headline DoD item)

**Files:**
- Modify: `docs/sprint-2-plans/client-verification-results.md`

- [ ] **Step 1: Run every Sprint 1 command via `redis-cli` against the still-running server**

```bash
redis-cli -p 6379 <<'EOF'
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
```

Expected: every command returns a sensible reply, no `(error)` lines except where one is semantically correct (e.g. the second `SET k v NX` returning `(nil)` because the key already exists from the first).

- [ ] **Step 2: Record the result**

```markdown
<!-- append to docs/sprint-2-plans/client-verification-results.md -->

## redis-cli — full Sprint 1 command set
- **Date:** <today's date>
- **Result:** <PASS/FAIL>
- **Full transcript:** <paste the actual redis-cli output here>
```

- [ ] **Step 3: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`docs/sprint-2-plans/client-verification-results.md` — do not compose the commit message
freeform. Suggested subject: `docs: record redis-cli full command-set verification`.

---

### Task 4: Phase 1 retro note

Sprint 2's Definition of Done in `../rocket-mem-sprint-plan.md` includes "Phase 1 retro note added to the repo (per the master plan's Week 4 task)." Phase 1 (`../rocket-mem-production-plan.md`, Weeks 1–4) spans both Sprint 1 and Sprint 2 — this retro covers the whole phase, not just this sprint, mirroring the format Sprint 1's own retro used (one line on what took longer than estimated, one line on what to adjust) but scaled to a phase-level writeup per the master plan's Week 4 sub-task: "what surprised you, what's technical debt to revisit in Phase 3."

**Files:**
- Create: `docs/phase-1-retro.md`

- [ ] **Step 1: Write the retro**

Cover, concretely (not generically — cite real commits/files, not vague impressions):
- What shipped vs. what was planned across Sprints 1–2 (any P0/P1 slippage, what got cut as P2)
- Where actual effort diverged from the sprint plan's hour estimates, and in which direction
- Real bugs the test suite caught during implementation (Sprint 1 had two — the phantom-key `lpop`/`rpop`/`srem` bug and the swallowed-WRONGTYPE bug, both in `docs/sprint-1-plans/known-issues`-style detail; record Sprint 2's equivalents here, if any — the arg-count panic gap flagged in `03-command-dispatcher.md` and closed in `06-integration-test-harness.md`'s Task 1 is one candidate)
- Technical debt explicitly deferred rather than accidentally skipped: `SET EX/PX` (Sprint 4), multi-value `RPUSH`/`LPUSH` (Sprint 3), RESP3/`HELLO` (not planned, a deliberate divergence)
- Anything from `00-sprint-2-spec.md`'s design decisions (the lib+bin crate split, in-process integration testing, the `pub`-everywhere CI gotcha fix) that did or didn't hold up once actually implemented

- [ ] **Step 2: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`docs/phase-1-retro.md` — do not compose the commit message freeform. Suggested subject:
`docs: add Phase 1 (Weeks 1-4 / Sprints 1-2) retro`.
