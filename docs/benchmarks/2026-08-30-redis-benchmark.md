# rocket-mem vs Redis — `redis-benchmark`, Sprint 6

**Run:** `scripts/benchmark.sh` (committed). **Date / host / versions:**

```
redis-server:  redis_version:8.10.1
rocket-mem:    redis_version:rocket-mem-0.1.2
host:          Linux 7.0.0-30-generic x86_64
date:          2026-08-30T12:43:42Z
```

## Setup

Both servers run with the same durability settings: `appendonly yes`, `appendfsync everysec`,
RDB/snapshot auto-save off. `redis-benchmark -t set,get -n 100000 -c 50` at two payload sizes
(3B and 1024B), each without pipelining and with `-P 16`. The real Redis keyspace is flushed
between cases; rocket-mem has no `FLUSHALL`, so its keyspace carries over — worth knowing when
reading the 1024B numbers, where its memory footprint is larger by the end of the run.

## Results (requests/second, higher is better)

| Workload | redis-server | rocket-mem | ratio (redis ÷ rocket) |
|---|---|---|---|
| SET, 3B, no pipeline | 112,739.57 | 99,800.40 | 1.13x |
| GET, 3B, no pipeline | 109,289.62 | 105,708.25 | 1.03x |
| SET, 3B, `-P 16` | 934,579.44 | 390,624.97 | 2.39x |
| GET, 3B, `-P 16` | 1,639,344.25 | 1,428,571.38 | 1.15x |
| SET, 1KB, no pipeline | 103,734.44 | 92,764.38 | 1.12x |
| GET, 1KB, no pipeline | 103,092.78 | 98,619.32 | 1.05x |
| SET, 1KB, `-P 16` | 450,450.44 | 245,700.25 | 1.83x |
| GET, 1KB, `-P 16` | 1,136,363.62 | 19,493.18 | 58.30x |

## Where we are slower, and why

Across six of the eight cases, rocket-mem is 1.03x–2.39x slower than redis-server — close enough
that the candidate explanations from the plan (per-command name allocation in the dispatcher, a
freshly-allocated `BytesMut` per reply, and the double-encode/channel-hop AOF path) are plausible
fits without needing a profile to believe them: the gap is modest and roughly proportional to how
much per-command dispatcher/encoding work each request does, and it widens for SET specifically
(1.13x–2.39x vs. GET's 1.03x–1.15x at the same payload/pipeline), consistent with SET paying the
extra AOF encode-and-channel-send that GET never touches. No flamegraph exists yet in this repo
(`2026-08-30-flamegraph-notes.md` is a later task), so none of this is confirmed by a profile —
it is inference from the shape of the numbers, not a verified root cause.

Pipelining amplifies the SET gap rather than closing it: at 3B, `-P 16` widens the SET ratio to
2.39x (from 1.13x unpipelined), and at 1KB it widens to 1.83x (from 1.12x). That cuts against a
naive "batching should help both engines equally" expectation and points at something SET-specific
that doesn't amortize with pipelining — plausibly the AOF channel-send per command, since batching
requests into one syscall doesn't reduce the number of AOF encodes or channel sends, only the
number of socket writes.

One result is not a modest gap: GET at 1KB payload with `-P 16` measured 19,493 req/s for
rocket-mem against 1,136,363 req/s for redis-server — a 58x difference, an order of magnitude
worse than every other row (which top out at 2.39x). This does not fit any of the plan's candidate
explanations proportionally — none of them predict a cliff specific to *pipelined, larger-payload
GET* while leaving pipelined 3B GET (1.15x) and unpipelined 1KB GET (1.05x) nearly at parity. The
raw benchmark trace for this case shows throughput pinned at a roughly constant ~19,000–20,800
req/s for the entire run (see the `GET: rps=...` lines for the `payload=1024B, pipeline=16`
rocket-mem case in `/tmp/rocket-mem-bench.txt`) with per-request avg latency staying flat around
0.2ms — i.e., this doesn't look like the server getting progressively slower, it looks like
something capping throughput at a fixed rate for that specific combination of payload size and
pipeline depth. This is flagged as an anomaly rather than explained: it needs the flamegraph
(the next task in this sprint) before asserting a cause, and it stands out enough from the rest of
the table that it should not be averaged in with the "modest, proportional" story above.

The Setup section's keyspace-carryover caveat is not the explanation here: the run's own `/metrics`
sample, captured at the end of the full run, shows `rocket_mem_keys 1` and
`rocket_mem_memory_used_bytes 1088` — confirming the keyspace never grew beyond a single key (both
`redis-benchmark` runs reused one key throughout, since `-r` was never passed to it). That rules
out keyspace carryover as the cause of this specific anomaly and strengthens the case that it
genuinely needs flamegraph profiling to explain.

## Where we are faster, if anywhere

None. rocket-mem was slower than redis-server in every one of the eight workloads measured, from
a small 1.03x gap (unpipelined 3B GET) up to the 58.30x gap on pipelined 1KB GET. There is no case
in this run where rocket-mem beat redis-server's throughput.

## What this does not measure

Single-node only; no cluster routing overhead is exercised (a `-MOVED` reply is cheaper than a
served command, so a cluster-mode benchmark would flatter the numbers). No concurrent-client
scaling curve. No latency percentiles beyond what `-q` reports — the `/metrics` histogram is the
place to read those.

## Effect of the Sprint 6 optimization

`perf(server): uppercase command names into a stack buffer instead of the heap` removed the
two-to-four per-command heap allocations described in
[`2026-08-30-flamegraph-notes.md`](2026-08-30-flamegraph-notes.md).

| Workload | before | after | change |
|---|---|---|---|
| SET, 3B, no pipeline | 99,800.40 | 105,596.62 | +5.80% |
| GET, 3B, no pipeline | 105,708.25 | 109,409.20 | +3.50% |
| SET, 3B, `-P 16` | 390,624.97 | 361,010.81 | -7.58% |
| GET, 3B, `-P 16` | 1,428,571.38 | 1,492,537.25 | +4.47% |

Both runs are single samples on one machine (the "before" figures are the ones already recorded
in the Results table above, from Task 2's run; the "after" figures come from a fresh
`scripts/benchmark.sh` run executed for this task, `date: 2026-08-30T13:17:45Z`, same host, same
build profile, same durability settings). Three of the four rows moved a few percent in the
faster direction (+3.5% to +5.8%) and one — SET at 3B with `-P 16`, the row where per-command
overhead should matter most under the plan's own reasoning — moved 7.6% in the *slower*
direction. That mixed sign is the honest signal here: on a single-sample, shared-machine
benchmark, a few percent of movement in either direction is well within ordinary run-to-run
noise, and this data does not show a clean, consistent throughput win from the allocation fix.
The allocations are provably gone from the code (that's a static fact, not a benchmark claim),
but this particular pair of runs does not demonstrate a measurable, repeatable improvement at
any of the four workloads — including the pipelined one where the fix's benefit was expected to
show up most clearly.

As an aside (not part of the table above, since the brief only asks for the 3B rows): the
1024B/`-P 16` GET anomaly flagged in Task 2 is still present and essentially unchanged —
19,493.18 req/s before vs. 19,542.70 req/s after, a 0.25% difference that is noise-level. That is
consistent with the flamegraph notes' conclusion that the anomaly has a separate cause from the
per-command allocations this task's fix addressed.

One more scope note: the allocations removed above are only the ones at the four sites this task
targeted (`dispatch`, `extract_write_command_name`, and `command_keys`, plus the buffer this fix
introduced). The observability wrapper added by an earlier Sprint 6 task, `dispatch_and_log`'s
`metric_label` function in `crates/server/src/dispatcher.rs`, still allocates 2-3 `String`s per
command via `to_ascii_lowercase()` and two `.clone()` calls feeding the `metrics` crate's
counter/histogram macros — that function was never touched by this optimization. It is a real
remaining opportunity for a future pass, e.g. a parallel lowercase-name lookup table keyed by the
same `KNOWN_COMMANDS` index, which would avoid the allocation entirely.
