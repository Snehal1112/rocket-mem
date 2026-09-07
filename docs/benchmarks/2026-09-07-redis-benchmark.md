# rocket-mem vs Redis — `redis-benchmark`, post-AOF-blocking-fix baseline

**Run:** `scripts/benchmark.sh` (committed, unmodified). **Date / host / versions:**

```
redis-server:  redis_version:8.10.1
rocket-mem:    redis_version:rocket-mem-0.1.3
host:          Linux 7.0.0-30-generic x86_64
date:          2026-09-07T12:16:35Z
```

## Setup

Both servers run with the same durability settings: `appendonly yes`, `appendfsync everysec`,
RDB/snapshot auto-save off. `redis-benchmark -t set,get -n 100000 -c 50` at two payload sizes
(3B and 1024B), each without pipelining and with `-P 16`. The real Redis keyspace is flushed
between cases; rocket-mem has no `FLUSHALL`, so its keyspace carries over — the run's own
`/metrics` sample, taken at the end, shows `rocket_mem_keys 1` and
`rocket_mem_memory_used_bytes 1088`, confirming the keyspace never grew past a single key (no
`-r` flag was passed, so every `redis-benchmark` invocation reused one key throughout).

This run is **post-AOF-blocking-fix**: `fix: stop AofWriter from blocking Tokio worker threads`
(`6e1d10b`) and `perf: keep block_in_place off the AOF hot path` (`76c25ef`) are both in this
worktree's history ahead of this run. It is also **matched-durability on both sides** —
`appendonly yes`, `appendfsync everysec` set explicitly on `redis-server` by `scripts/benchmark.sh`
itself, not left at its default (`no`). That matters because the informal manual
`redis-benchmark` sessions run earlier today (2026-09-07, documented in
`docs/superpowers/specs/2026-09-07-redis-parity-perf-design.md`) that motivated the AOF fix had
`redis-server`'s `appendonly` left at `no` for the last several runs — an unmatched-durability
comparison the design spec itself flagged as not apples-to-apples — and *those* runs showed
rocket-mem's `SET`/`GET` `max` latency spiking to 213–495ms (against redis-server's 4–7ms max on
the same runs), roughly 1 in 1,000–2,000 requests stalling severely while the rest were unaffected.
`docs/benchmarks/2026-08-30-redis-benchmark.md`, the prior committed report, already used matched
durability for its throughput numbers, so this run is not a durability-fairness fix relative to
that report specifically — it is a fresh run under the same fair methodology, after the stall-bug
fix, to see whether the fix changed anything and whether the 08-30 gaps persist.

## Whether the stall is confirmed gone

**Not directly measurable from the `scripts/benchmark.sh` run below.** `redis-benchmark -q` prints
one throughput line per command (`requests per second` and `p50`) — it does not surface `max`
latency or any other tail percentile, and `scripts/benchmark.sh` does not pass `--latency-history`
or drop `-q` to get the non-quiet percentile breakdown that would show it. Nothing in that run's
output can confirm or deny whether the 213–495ms `max`-latency stalls documented in the manual
sessions are gone. The `p50` figures reported below are all sub-millisecond and unremarkable on
both servers, which is at least consistent with (not proof of) the stalls being fixed.

That gap has since been closed by a dedicated follow-up run without `-q` — see
"Tail-latency verification" below, which **directly confirms the stalls are gone**.

## Tail-latency verification: the stall is confirmed gone

**Verdict: confirmed by direct measurement.** rocket-mem's `max` latency on the exact workload
that used to stall is now **2.0–4.2ms**, in the same range as the `redis-server` 4–7ms max
recorded alongside those stalls — not the 213–495ms of the pre-fix manual sessions. That is a
~50–240x reduction in worst-case latency, and it holds for both `SET` and `GET`.

**Method.** A freshly built `target/release/rocket-mem` (same binary as above), started with the
same env vars `scripts/benchmark.sh` uses (`ROCKET_MEM_AOF_PATH` / `ROCKET_MEM_SNAPSHOT_PATH` in a
fresh `mktemp -d`, `ROCKET_MEM_METRICS_ADDR` on 9178) so durability matches the rest of this
report — the server always runs an AOF with `FsyncPolicy::EverySecond` (`main.rs:155`), i.e.
`appendonly yes` / `appendfsync everysec`. The AOF file measured 4,500,000 bytes after each run,
confirming the writes really went through the AOF path. Then, **without `-q`** so the full
percentile distribution is printed:

```
redis-benchmark -h 127.0.0.1 -p 7778 -t set,get -n 100000 -c 50 -d 3
```

That is the 3B-payload, no-pipelining, `-c 50` case the pre-fix manual sessions stalled on.
`redis-server` was not re-run here: the question is specifically whether rocket-mem's *own* tail
collapsed, and the pre-fix rocket-mem numbers (213–495ms) are what this is measured against.

### Run 1 — full `redis-benchmark` output

```
====== SET ======
  100000 requests completed in 1.28 seconds
  50 parallel clients
  3 bytes payload
  keep alive: 1
  multi-thread: no

Latency by percentile distribution:
0.000% <= 0.071 milliseconds (cumulative count 1)
50.000% <= 0.303 milliseconds (cumulative count 53520)
75.000% <= 0.399 milliseconds (cumulative count 75668)
87.500% <= 0.455 milliseconds (cumulative count 88221)
93.750% <= 0.495 milliseconds (cumulative count 93899)
96.875% <= 0.591 milliseconds (cumulative count 96918)
98.438% <= 0.719 milliseconds (cumulative count 98449)
99.219% <= 0.847 milliseconds (cumulative count 99258)
99.609% <= 0.903 milliseconds (cumulative count 99624)
99.805% <= 0.967 milliseconds (cumulative count 99808)
99.902% <= 1.007 milliseconds (cumulative count 99909)
99.951% <= 1.031 milliseconds (cumulative count 99953)
99.976% <= 1.151 milliseconds (cumulative count 99976)
99.988% <= 1.503 milliseconds (cumulative count 99988)
99.994% <= 1.735 milliseconds (cumulative count 99994)
99.997% <= 1.767 milliseconds (cumulative count 99998)
99.998% <= 1.775 milliseconds (cumulative count 99999)
99.999% <= 2.039 milliseconds (cumulative count 100000)
100.000% <= 2.039 milliseconds (cumulative count 100000)

Cumulative distribution of latencies:
0.006% <= 0.103 milliseconds (cumulative count 6)
0.451% <= 0.207 milliseconds (cumulative count 451)
53.520% <= 0.303 milliseconds (cumulative count 53520)
76.839% <= 0.407 milliseconds (cumulative count 76839)
94.330% <= 0.503 milliseconds (cumulative count 94330)
97.300% <= 0.607 milliseconds (cumulative count 97300)
98.363% <= 0.703 milliseconds (cumulative count 98363)
98.984% <= 0.807 milliseconds (cumulative count 98984)
99.624% <= 0.903 milliseconds (cumulative count 99624)
99.909% <= 1.007 milliseconds (cumulative count 99909)
99.974% <= 1.103 milliseconds (cumulative count 99974)
99.979% <= 1.207 milliseconds (cumulative count 99979)
99.981% <= 1.303 milliseconds (cumulative count 99981)
99.983% <= 1.407 milliseconds (cumulative count 99983)
99.988% <= 1.503 milliseconds (cumulative count 99988)
99.990% <= 1.607 milliseconds (cumulative count 99990)
99.993% <= 1.703 milliseconds (cumulative count 99993)
99.999% <= 1.807 milliseconds (cumulative count 99999)
100.000% <= 2.103 milliseconds (cumulative count 100000)

Summary:
  throughput summary: 78064.01 requests per second
  latency summary (msec):
          avg       min       p50       p95       p99       max
        0.332     0.064     0.303     0.527     0.815     2.039

====== GET ======
  100000 requests completed in 0.95 seconds
  50 parallel clients
  3 bytes payload
  keep alive: 1
  multi-thread: no

Latency by percentile distribution:
0.000% <= 0.079 milliseconds (cumulative count 1)
50.000% <= 0.239 milliseconds (cumulative count 56665)
75.000% <= 0.255 milliseconds (cumulative count 81990)
87.500% <= 0.271 milliseconds (cumulative count 89392)
93.750% <= 0.295 milliseconds (cumulative count 94421)
96.875% <= 0.319 milliseconds (cumulative count 97061)
98.438% <= 0.343 milliseconds (cumulative count 98557)
99.219% <= 0.383 milliseconds (cumulative count 99270)
99.609% <= 0.439 milliseconds (cumulative count 99620)
99.805% <= 0.495 milliseconds (cumulative count 99815)
99.902% <= 0.943 milliseconds (cumulative count 99904)
99.951% <= 1.887 milliseconds (cumulative count 99952)
99.976% <= 2.143 milliseconds (cumulative count 99979)
99.988% <= 2.151 milliseconds (cumulative count 99990)
99.994% <= 2.159 milliseconds (cumulative count 99997)
99.998% <= 2.167 milliseconds (cumulative count 99999)
99.999% <= 2.607 milliseconds (cumulative count 100000)
100.000% <= 2.607 milliseconds (cumulative count 100000)

Cumulative distribution of latencies:
0.007% <= 0.103 milliseconds (cumulative count 7)
3.539% <= 0.207 milliseconds (cumulative count 3539)
95.658% <= 0.303 milliseconds (cumulative count 95658)
99.462% <= 0.407 milliseconds (cumulative count 99462)
99.817% <= 0.503 milliseconds (cumulative count 99817)
99.833% <= 0.607 milliseconds (cumulative count 99833)
99.847% <= 0.703 milliseconds (cumulative count 99847)
99.880% <= 0.807 milliseconds (cumulative count 99880)
99.894% <= 0.903 milliseconds (cumulative count 99894)
99.907% <= 1.007 milliseconds (cumulative count 99907)
99.910% <= 1.103 milliseconds (cumulative count 99910)
99.913% <= 1.207 milliseconds (cumulative count 99913)
99.916% <= 1.303 milliseconds (cumulative count 99916)
99.917% <= 1.407 milliseconds (cumulative count 99917)
99.918% <= 1.503 milliseconds (cumulative count 99918)
99.920% <= 1.607 milliseconds (cumulative count 99920)
99.922% <= 1.703 milliseconds (cumulative count 99922)
99.925% <= 1.807 milliseconds (cumulative count 99925)
99.959% <= 1.903 milliseconds (cumulative count 99959)
99.971% <= 2.007 milliseconds (cumulative count 99971)
100.000% <= 3.103 milliseconds (cumulative count 100000)

Summary:
  throughput summary: 105485.23 requests per second
  latency summary (msec):
          avg       min       p50       p95       p99       max
        0.245     0.072     0.239     0.303     0.359     2.607
```

### Run 2 — independent repetition (fresh server, fresh AOF)

```
====== SET ======
  latency summary (msec):
          avg       min       p50       p95       p99       max
        0.353     0.080     0.319     0.535     0.871     4.191

====== GET ======
  latency summary (msec):
          avg       min       p50       p95       p99       max
        0.248     0.072     0.239     0.319     0.367     1.975
```

### Reading it

| | pre-fix manual session | post-fix, run 1 | post-fix, run 2 | `redis-server` (same manual session) |
|---|---:|---:|---:|---:|
| SET `max` | 213–495ms | 2.039ms | 4.191ms | 4–7ms |
| GET `max` | 213–495ms | 2.607ms | 1.975ms | 4–7ms |

The pre-fix stalls hit roughly 1 request in 1,000–2,000, so a 100,000-request run would have
contained 50–100 of them; a single stalled request is enough to set `max`. Two independent
100,000-request runs per command with `max` at 2.0–4.2ms is therefore a positive result, not an
absence of evidence — the stalls are not merely rarer, the whole tail is gone. The percentile
ladders confirm the shape as well as the endpoint: `p99.999` is 2.039ms (SET) and 2.607ms (GET),
so there is no long thin tail hiding under `max` either. What remains is ordinary scheduler/fsync
jitter of the same magnitude real Redis shows.

Two honest limits on this: the pre-fix baseline it is compared against came from the manual
sessions recorded in `docs/superpowers/specs/2026-09-07-redis-parity-perf-design.md`, not from a
controlled before/after pair executed back-to-back on the same machine state; and this is the 3B,
unpipelined case only — the pipelined and 1KB cases were not re-measured for tail latency. Neither
weakens the conclusion for the workload the bug was reported on, which is what this section set out
to verify.

## Results (requests/second, higher is better)

| Workload | redis-server | rocket-mem | ratio (redis ÷ rocket) |
|---|---|---|---|
| SET, 3B, no pipeline | 77,881.62 | 87,719.30 | **0.89x (rocket faster)** |
| GET, 3B, no pipeline | 103,519.66 | 99,206.34 | 1.04x |
| SET, 3B, `-P 16` | 943,396.25 | 332,225.91 | 2.84x |
| GET, 3B, `-P 16` | 1,587,301.50 | 1,351,351.38 | 1.17x |
| SET, 1KB, no pipeline | 98,716.68 | 72,463.77 | 1.36x |
| GET, 1KB, no pipeline | 94,786.73 | 86,655.11 | 1.09x |
| SET, 1KB, `-P 16` | 454,545.47 | 264,550.28 | 1.72x |
| GET, 1KB, `-P 16` | 840,336.12 | 19,496.98 | 43.10x |

Full raw `redis-benchmark` trace (including the interim `rps=...` progress lines) is preserved at
`/tmp/rocket-mem-baseline-2026-09-07.txt` on the machine this run was executed on. It is also
quoted in full in the local task report at
`.superpowers/sdd/2026-09-07-perf-baseline-and-profiling-plan/task-1-report.md` — note that path
is gitignored (`.superpowers/sdd/.gitignore`) and won't exist in a fresh clone; both this and the
`/tmp` path are ephemeral, machine-local artifacts, not committed references.

## Where we are slower, and why

Seven of eight rows are still slower than redis-server, from a small 1.04x (unpipelined 3B GET) up
to the still-enormous 43.10x on pipelined 1KB GET. Compared to the 2026-08-30 report's numbers,
the picture is mixed rather than uniformly better or worse:

- **Unpipelined 3B/1KB and pipelined 3B GET are roughly where they were** (1.04x–1.17x here vs.
  1.03x–1.15x before) — noise-level movement, no material change.
- **Unpipelined SET at 1KB widened** from 1.12x to 1.36x. The pipelined SET rows moved in
  opposite directions: 3B `-P 16` widened from 2.39x to 2.84x, while 1KB `-P 16` narrowed slightly
  from 1.83x to 1.72x. SET's gap is not shrinking with the AOF fix in any consistent way; if
  anything the 3B pipelined case got worse. This is consistent with the
  08-30 report's own framing that SET's extra AOF encode/channel-send cost, not the blocking-fsync
  bug specifically, is the likely driver of SET's gap — the blocking-fsync fix targeted a
  different symptom (tail-latency stalls) than SET's steady-state throughput cost.
- **The pipelined 1KB GET anomaly is still present and still an outlier**: 43.10x here vs. 58.30x
  in the 08-30 report, roughly 26% smaller. **That ratio move is not a rocket-mem-side
  improvement.** rocket-mem's own absolute figure on this case is unchanged — 19,493.18 req/s on
  08-30 vs. 19,496.98 req/s here, a ~0.02% difference, i.e. flat within measurement noise (and a
  third session, the flamegraph capture in `2026-09-07-flamegraph-notes.md`, lands at 19,417
  req/s on the same case). The entire movement comes from the *other* side of the ratio:
  redis-server's own reference number dropped 26% between the two sessions, 1,136,363.62 →
  840,336.12 req/s. Nothing about rocket-mem got faster here, and no effect of the AOF fix on this
  row should be inferred from the smaller ratio. The anomaly also remains an order of magnitude worse than
  every other row (which top out at 2.84x), so it should still be read as a distinct anomaly
  rather than folded into the "modest, roughly proportional" story the other rows tell.

No flamegraph was taken as part of this task (that is Task 2's job); everything above is inference
from the shape of the numbers, same caveat as the prior report.

## Where we are faster, if anywhere

**Yes, one row**: unpipelined SET at 3B payload, rocket-mem does 87,719 req/s against
redis-server's 77,881 req/s — rocket-mem is faster here by a factor of about 1.13x (ratio 0.89x
redis-to-rocket). This is new relative to the 08-30 report, where rocket-mem was slower on every
one of the eight rows (including this exact case, 1.13x slower then). Whether this reflects the
AOF fix removing wasted work on the SET path, or ordinary machine-load noise on an unpipelined,
already-close-to-parity case (both this run and the prior one park in the 90-115K req/s range for
this row), is not something one sample can distinguish — worth watching in a future run rather
than treating as a settled win.

## What this does not measure

Single-node only; no cluster routing overhead is exercised (a `-MOVED` reply is cheaper than a
served command, so a cluster-mode benchmark would flatter the numbers). No concurrent-client
scaling curve. The head-to-head table itself carries no latency percentiles beyond `p50`, since
`redis-benchmark -q` reports nothing else; tail latency is covered separately by the
"Tail-latency verification" section above, which measured rocket-mem's full percentile
distribution but only on the 3B, unpipelined case and only for rocket-mem (no `redis-server` side,
no pipelined or 1KB case). The `/metrics` histogram on the running server is another place to look
at percentiles; a `--latency-history` run would give a time series rather than the single
end-of-run distribution used above.
