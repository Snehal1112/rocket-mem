# Benchmark Smoke Test Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** confirm rocket-mem doesn't panic or deadlock under `redis-benchmark`'s concurrent load — a light smoke test, not a performance benchmark. Real throughput comparison against Redis is explicitly Sprint 6 / Week 12 scope (`../rocket-mem-production-plan.md`), not this sprint's.

**Architecture:** no production code — this is a manual run of the real Redis project's own `redis-benchmark` CLI tool against a running rocket-mem instance, with the result recorded in the repo.

**Tech Stack:** `redis-benchmark` (ships with the real Redis project — install via your OS package manager or build Redis from source; it's a client tool, not a rocket-mem dependency).

**Depends on:** `04-tcp-listener.md` must be complete (needs a working TCP server to point the benchmark at). This item is independent of `05`–`07` — it can run any time after networking exists.

**Priority note:** this is Sprint 2's P2 item, per `../rocket-mem-sprint-plan.md` — the first thing to cut if the sprint runs long. Don't let it block anything else finishing.

---

### Task 1: Run `redis-benchmark` against a running rocket-mem instance

**Files:**
- Create: `docs/sprint-2-plans/benchmark-smoke-test-results.md`

- [ ] **Step 1: Start the server in release mode** (debug-mode timings aren't meaningful even for a smoke test — the pass/fail bar here is "no panic/deadlock," not speed, but a debug build's occasional GC-like pauses from unoptimized allocation can look deceptively like contention)

Run: `cargo run --release --bin rocket-mem`

- [ ] **Step 2: Run the smoke test**

```bash
redis-benchmark -h 127.0.0.1 -p 6379 -t set,get -n 100000 -c 50 -q
```

`-t set,get` limits it to commands rocket-mem actually implements (an unscoped `redis-benchmark` run tries the full real-Redis command set, most of which doesn't exist yet and would just fill the output with expected errors, not signal). `-c 50` gives real concurrent-connection pressure across the sharded engine — this is what actually exercises the `RwLock`-per-shard design under load, not raw single-connection throughput. `-q` keeps output to one summary line per command.

Expected: the command completes (doesn't hang), the server process is still running afterward (check with `redis-cli -p 6379 ping` immediately after), and no panic output appeared in the server's terminal.

- [ ] **Step 3: If it hangs or panics, capture the failure before fixing anything**

```bash
# if the server process is unresponsive but still running, get a backtrace of every thread:
RUST_BACKTRACE=full cargo run --release --bin rocket-mem 2>&1 | tee server.log
# re-run the benchmark in another terminal, then inspect server.log for the panic/hang location
```

Fix the root cause (most likely a lock ordering issue causing a deadlock between two shards, or an unwrap on a real error path this test happened to trigger) before considering this task done — a smoke test that finds a real bug and gets shrugged off defeats its own purpose.

- [ ] **Step 4: Record the result**

```markdown
<!-- docs/sprint-2-plans/benchmark-smoke-test-results.md -->
# Sprint 2 — Benchmark Smoke Test Result

**Date:** <today's date>
**Command:** `redis-benchmark -h 127.0.0.1 -p 6379 -t set,get -n 100000 -c 50 -q`
**Result:** <PASS — completed without panic/deadlock, server responsive afterward / FAIL — see notes>
**Raw output:**
```
<paste the actual redis-benchmark output here>
```
**Notes:** this is a smoke test only — no throughput comparison against real Redis is implied or claimed here. That comparison is explicitly Sprint 6 (Week 12) scope.
```

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`docs/sprint-2-plans/benchmark-smoke-test-results.md` — do not compose the commit message
freeform. Suggested subject: `docs: record Sprint 2 redis-benchmark smoke test result`.
