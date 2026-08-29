# Sprint 2 — Benchmark Smoke Test Result

**Date:** 2026-08-29
**Command:** `redis-benchmark -h 127.0.0.1 -p 16379 -t set,get -n 100000 -c 50 -q`
**Environment note:** run against `127.0.0.1:16379` instead of the default `6379`, since
this machine already has a real Redis server (8.10.1) bound to `6379`.
`crates/server/src/main.rs`'s bind address was temporarily changed for this run, then
reverted — no permanent change to the shipped port.
**Result:** PASS — completed without panic/deadlock, server responsive afterward
**Raw output:**
```
WARNING: Could not fetch server CONFIG
SET: 96061.48 requests per second, p50=0.263 msec
GET: 90661.83 requests per second, p50=0.271 msec
```
(The `WARNING: Could not fetch server CONFIG` line is expected — `redis-benchmark`
probes `CONFIG GET` on startup, which rocket-mem doesn't implement yet; it's a
non-fatal informational probe, not a test failure.)

`redis-cli -p 16379 ping` returned `PONG` both before and after the benchmark run, and
the server process's own log showed no panic output — just the startup "Listening on"
line.

**Notes:** this is a smoke test only — no throughput comparison against real Redis is
implied or claimed here. That comparison is explicitly Sprint 6 (Week 12) scope.
