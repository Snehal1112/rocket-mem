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

**Not directly measurable from this run.** `redis-benchmark -q` prints one throughput line per
command (`requests per second` and `p50`) — it does not surface `max` latency or any other tail
percentile, and `scripts/benchmark.sh` does not pass `--latency-history` or drop `-q` to get the
non-quiet percentile breakdown that would show it. Nothing in this run's output can confirm or
deny whether the 213–495ms `max`-latency stalls documented in the manual sessions are gone. The
`p50` figures reported below are all sub-millisecond and unremarkable on both servers, which is
at least consistent with (not proof of) the stalls being fixed. **This is a real gap**: a
follow-up run with `--latency-history` (or `redis-benchmark` without `-q`, reading the full
percentile distribution it prints) is needed to directly verify the fix's effect on tail latency,
and is not something this task attempted.

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
  in the 08-30 report. That is a real reduction (58.30x → 43.10x, roughly 26% smaller), but it
  remains an order of magnitude worse than every other row (which top out at 2.84x), so it should
  still be read as a distinct, unexplained anomaly rather than folded into the "modest, roughly
  proportional" story the other rows tell. Whether the AOF fix is what narrowed it, versus
  ordinary run-to-run variance on a shared machine, is not something a single sample on each side
  of the fix can establish — that would need a controlled before/after pair on the same run, which
  this task did not do (the 08-30 numbers predate a great deal of other work, not just the AOF
  fix).

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
scaling curve. No latency percentiles beyond `p50`/throughput as reported by `-q` — see "Whether
the stall is confirmed gone" above: this is the specific, acknowledged gap in this run, not an
oversight being silently skipped. The `/metrics` histogram on the running server is one available
place to look at percentiles today; a `--latency-history` `redis-benchmark` run is the more direct
one for a future task.
