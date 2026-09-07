# Flamegraph notes — perf baseline & profiling (2026-09-07)

**Profiles:** three separate, isolated `perf record` captures, one per workload, each against a
freshly started server process (fresh AOF/snapshot paths, fresh port), each stopped with `SIGINT`
immediately after its `redis-benchmark` run completed. Unlike the 2026-08-30 profile (one
continuous recording spanning all three phases), each capture here is self-contained — samples in
`2026-09-07-flamegraph-unpipelined-3b.svg` are *only* from the unpipelined 3B run, etc.

1. `2026-09-07-flamegraph-unpipelined-3b.svg` / `perf-unpipelined-3b.data` —
   `redis-benchmark -h 127.0.0.1 -p 7778 -t set,get -n 200000 -c 50 -d 3 -q`
   (SET 71,556 req/s, GET 83,438 req/s)
2. `2026-09-07-flamegraph-pipelined-3b.svg` / `perf-pipelined-3b.data` — same, `-P 16` added
   (SET 209,864 req/s, GET 1,092,896 req/s)
3. `2026-09-07-flamegraph-pipelined-1kb.svg` / `perf-pipelined-1kb.data` — `-d 1024 -P 16`
   (SET 96,665 req/s, GET **19,417 req/s**)

All captures: `cargo flamegraph --release --bin rocket-mem --deterministic` (`perf record -F 997
--call-graph dwarf,64000 -g`), `kernel.kptr_restrict=0`, `kernel.perf_event_paranoid=-1`
(confirmed via `cat /proc/sys/kernel/kptr_restrict` → `0` and `.../perf_event_paranoid` → `-1`
before starting). `perf.data` files themselves are ~330–570MB each (not committed — analysis
below is from `perf report`/`perf script` run directly against them from `/tmp/rocket-mem-fg/`);
only the rendered SVGs are checked in.

**Reproduction check:** pipelined-1KB GET measured at 19,417 req/s here, matching today's fresh
baseline (`2026-09-07-redis-benchmark.md`: 19,496.98 req/s, 43.10x slower than Redis) and the
2026-08-30 profile's in-band reproduction (19,440 req/s) almost exactly. The anomaly is real,
reproduces reliably across three separate measurement sessions, and confirming it live (not just
trusting the benchmark doc) was step 1 of this task's own analysis. This also rebuts a "maybe
`redis-benchmark` itself is the bottleneck" reading: the same client, on the identical `-d 1024 -P
16` GET workload, drives real Redis to 840,336.12 req/s in the same benchmark doc — the client is
plainly not what's capping this run at ~19,400.

**Three different ratios appear below; they measure different things, not the same anomaly three
ways.** 43.10x is today's `2026-09-07-redis-benchmark.md` figure — real Redis vs. rocket-mem,
unprofiled, same day. 58.30x is the older `2026-08-30-redis-benchmark.md` figure for the same
comparison, cited only for trend context. 56x (below, in the anomaly section) is a different
denominator entirely — it never involves Redis; it's this task's own pipelined-3B-GET vs.
pipelined-1KB-GET ratio, both rocket-mem, both from these profiled captures.

**A note on structure:** this file's section headings deliberately diverge from
`2026-08-30-flamegraph-notes.md`'s — that profile was one continuous recording analyzed as a
single artifact, so its headings ("What the profile shows," "The bottleneck this sprint fixes")
fit a single-profile narrative. This task produced three isolated recordings answering two
specific standing questions (the Mutex, the anomaly), so the structure here follows those
questions instead. Only "Recorded, not acted on" is carried over verbatim, since that heading's
purpose (this doc feeds Phase 3's fix selection, not fixes anything itself) is unchanged.

## Kernel symbols: confirmed resolved this time

The 2026-08-30 profile's defining problem — `kptr_restrict=1` blocking kernel symbol names, and
DWARF unwinding leaving huge `[unknown]`/`0xffffffff...` chains — is gone. Every flat `perf
report --sort=overhead,symbol -g none` below is full of real, named kernel functions:
`entry_SYSRETQ_unsafe_stack`, `ipt_do_table`, `nf_hook_slow`, `tcp_v4_rcv`, `tcp_rcv_established`,
`tcp_sendmsg_locked`, `dequeue_entity`, `native_queued_spin_lock_slowpath`,
`nf_conntrack_tcp_packet`, `aa_inet_msg_perm` (AppArmor), and so on — no unresolved chains, no
`perf report` "kernel address maps restricted" warning. Caller-mode call-graphs
(`-g graph,N,caller`) also resolve real multi-frame kernel call chains now (see the anomaly
section below for a 150+-line real chain from a `writev()` syscall down through the whole TCP/IP
stack and back). This half of the prior profile's "badly degraded" caveat is resolved.

One genuinely new, unrelated observation this resolution surfaces: a non-trivial chunk of self
time goes to netfilter/conntrack/iptables/AppArmor processing on every packet (`ipt_do_table`
1.17–2.67%, `nf_hook_slow` ~1%, `nf_conntrack_tcp_packet`, `iptable_mangle_hook`,
`aa_inet_msg_perm`, `resolve_normal_ct`) — this is host/container network-namespace overhead
(iptables rules + conntrack + AppArmor mediation on the loopback path), not rocket-mem or even
generic Linux TCP/IP cost. Recorded because it's now visible and non-trivial; not something this
task's scope covers acting on.

**A second, narrower unwind limitation remains, and is not the same issue as `kptr_restrict`.**
Samples where a thread is genuinely parked in `futex_wait` (blocked on a contended
`std::sync::Mutex`) do not unwind into user space *at all* — not "unresolved," entirely absent.
`perf script`'s raw per-sample frame list for these ends right at
`entry_SYSCALL_64_after_hwframe`/`do_syscall_64`/`schedule`, with zero frames above the syscall
boundary — no `lock_contended`, no caller, nothing. Confirmed directly:

```
$ perf report -i perf-pipelined-3b.data --stdio -g graph,0,caller \
    --symbol-filter='std::sys::sync::mutex::futex::Mutex::lock_contended'
     3.51%     3.51%  tokio-rt-worker  rocket-mem     [.] std::sys::sync::mutex::futex::Mutex::lock_contended
            |
             --0.08%--entry_SYSCALL_64
```

Only 0.08 of the 3.51% self-time resolves to *any* caller, and that caller is the kernel syscall
entry point, not a rocket-mem frame. (This run's `--symbol-filter` self-time, 3.51%, differs
trivially from the 3.54% the flat `--sort=overhead,symbol -g none` report gives for the same
symbol on the same `perf.data` in the table below — the two `perf report` modes recompute
percentages against slightly different internal event-count bases; both are the same underlying
data, not a discrepancy worth chasing further.) This happens because `perf record --call-graph dwarf` samples
and unwinds from the interrupted context; a thread that's actually asleep in the kernel (as
opposed to running user code that gets NMI-sampled) doesn't have a live register/stack snapshot at
the point `perf` can walk DWARF CFI from — the unwind has nothing above the syscall to walk. This
is real and reproducible (checked with `--symbol-filter` and threshold 0, not just a display cutoff)
across both pipelined captures. It means direct call-graph attribution of the contended-Mutex
frame to one specific caller is **not possible from this kind of profile**, independent of the
`kptr_restrict` fix — a different tool (e.g. an eBPF-based off-CPU profiler, or `perf record -e
sched:sched_switch`) would be needed to see the sleeping side of a contended lock's call stack.

## The Mutex-contention question: decided by source-level elimination, not by call-graph or by self-time scaling

**Direct call-graph attribution: still not possible** (see above — the futex-unwind limitation
means no sample resolves a caller). But the source code itself eliminates two of the three named
candidates outright, which is a stronger and simpler argument than anything a scaling comparison
could give:

**`SlowLog` is eliminated: its mutex is structurally almost never even reached on this benchmark.**
`SlowLog::maybe_record` (`crates/server/src/slowlog.rs:69`) returns *before* touching
`self.state.lock()` unless `elapsed >= self.threshold`:

```rust
if self.threshold.is_zero() || elapsed < self.threshold {
    return;
}
```

The default threshold is 10,000µs / 10ms (`crates/server/src/config.rs:44`,
`slowlog_threshold_micros: 10_000`), and nothing in this benchmark run overrides it. Every `p50`
this task measured, across all three captures and both commands, is 0.167–4.687ms — all comfortably
under the 10ms bar (none even reaches half of it) — so for the overwhelming majority of commands
the function returns on the first line, before the mutex is ever locked. This isn't "less contended
than the others"; it's structurally unreachable as *the* contended lock at these latencies, full
stop.

**`ReplicaRegistry` is eliminated: its lock is nested under the order lock, never independently
raced.** `crates/server/src/dispatcher.rs:2537` binds `_order_guard = aof.lock_for_ordering()` near
the top of `dispatch_and_log_inner` (the function `dispatch_and_log`, `:2433`, wraps for metrics/
timing — the guard itself lives in the inner function, not the outer wrapper); the function's only
calls into `ReplicaRegistry` — `registry.broadcast(...)` at `:2622` — happen inside that same
function body, and `_order_guard` isn't dropped until the function returns at `:2634`.
`replication.rs`'s `register()` (the other
write path into `senders`) documents the identical constraint in its own doc comment: "Called only
from `serve_replica`... while it still holds `AofWriter::lock_for_ordering()`." So every writer
that could possibly reach `senders.lock()` on a write path has *already* serialized on
`_order_guard` first — two concurrent writers can never race each other for `senders`, because
whichever one is inside `dispatch_and_log_inner` already holds the order lock alone. Any
`lock_contended` sample attributed to `senders` would be a symptom *downstream of* the order lock's
own serialization, not an independent source of contention.

(An earlier version of this section argued from each candidate's own self-time scaling with
pipelining — e.g. that `SlowLog::maybe_record`'s self-time staying flat across captures proved it
wasn't contended. That argument doesn't hold: `lock_contended` is its own, separate,
not-inlined profiler symbol — blocked/waiting time inside a `.lock()` call is counted there, in
the callee, not folded into the caller's self-time. This is consistent with, not in tension with,
the futex-unwind finding above: those samples don't unwind to *any* caller, which is exactly why a
caller's self-time can't be used as a proxy for how much it contends its own locks. The source-level
elimination above replaces that argument rather than supplementing it.)

By elimination, `AofWriter::lock_for_ordering()` (`crates/server/src/aof.rs:277`, backed by
`self.order: Mutex<()>`) is the only one of the three candidates a second concurrent writer can
actually race on this benchmark's write path — it's acquired near the top of
`dispatch_and_log_inner`, before either of the other two locks comes into play, so multiple worker
threads dispatching write commands concurrently genuinely contend on this one acquisition.

For reference, isolating `lock_contended`'s flat self-time per phase (impossible from the 2026-08-30
blended profile, which could only report one number, 1.96%, across all three phases at once) does
show it rising sharply under pipelining:

| Capture | `lock_contended` self % | `AofWriter::append_encoded` | `AofWriter::send` | `SlowLog::maybe_record` | `ReplicaRegistry::broadcast` |
|---|---|---|---|---|---|
| unpipelined 3B | **0.14%** | 0.14% | 0.04% | 0.09% | 0.01% |
| pipelined 3B (`-P 16`) | **3.54%** | 0.10% | 0.17% | 0.20% | 0.09% |
| pipelined 1KB (`-P 16 -d 1024`) | **2.96%** | 0.06% | 0.04% | 0.07% | 0.09% |

This is included for completeness, not as the basis of the attribution above, and it does not
hold up cleanly as a scaling argument on its own: SET throughput only rises 2.9x from unpipelined
to pipelined-3B (71,556 → 209,864 req/s) while `lock_contended` rises ~25x (0.14% → 3.54%) —
non-linear — and SET throughput *drops* from pipelined-3B to pipelined-1KB (209,864 → 96,665
req/s) while `lock_contended` barely moves (3.54% → 2.96%). Per the correction above, none of the
other columns (including `broadcast`'s own 9x jump, 0.01% → 0.09%, the largest relative move of
the three) can be read as evidence of relative contention-proneness either way, since self-time in
a lock's caller doesn't capture that lock's blocked time. The table is left here as raw data for
anyone following up, not re-interpreted into a proof this document doesn't need — the elimination
argument above is the decisive one.

**Verdict:** `AofWriter::lock_for_ordering` (the `order: Mutex<()>` field) is the contended Mutex,
established by eliminating the other two candidates at the source level — a decisive argument, not
a correlational one — though still not confirmed by direct call-graph evidence, which remains
blocked by the futex-unwind limitation documented above. Phase 3 can treat this as settled enough
to act on.

## The 43x/58x-vs-Redis, 56x-internal pipelined-1KB GET anomaly

**Reproduced:** 19,417 req/s GET in this capture, consistent with both the 43.10x figure in
today's `2026-09-07-redis-benchmark.md` and the 08-30 profile's in-band 19,440 req/s.

**No new, distinct rocket-mem hot function appears.** Comparing pipelined-3b vs pipelined-1kb flat
self-time, the same rocket-mem functions dominate in both (`dispatch_and_log`, `parse_frame`,
`RespCodec::encode`, `extract_write_command_name`, `BufMut::put_slice`), at similar orders of
magnitude. The allocation/copy family shifts composition with payload size as expected
(`__memmove_avx512_unaligned_erms` 0.99% → 2.03%, `kernel_init_pages` 0.32% → 2.07%, consistent
with bigger reply buffers needing more copies and occasionally fresh page allocation) but this
alone is nowhere near large enough to explain a 43–58x slowdown, and the malloc/free family's
*total* self-time is actually similar between the two runs (~9% either way, just redistributed).
This rules out "one new expensive function" as the explanation, same conclusion the 08-30 profile
reached — but now from clean, isolated data instead of a blended, unresolved one.

**The strongest evidence: within the single pipelined-1KB capture, SET and GET are cleanly
time-separable, and GET does markedly less sampled CPU work per request than SET while taking
markedly longer per request in wall-clock time — the opposite of what proportional CPU cost would
predict.** `redis-benchmark -t set,get` runs SET to completion, then GET, inside one recording;
`perf script -i perf-pipelined-1kb.data -F time,comm` (bucketed at 0.05–0.2s resolution, not
guessed) shows a sharp density cliff at the transition — SET's sample rate holds in the
hundreds-per-100ms range through absolute timestamp `525494.49`, then drops from 167 samples in a
50ms bucket to 5 in the next. That boundary, cross-checked against each phase's own known request
count and measured throughput (200,000 ÷ 96,665.05 req/s ≈ 2.07s expected for SET; 200,000 ÷
19,417.47 req/s ≈ 10.30s expected for GET — both within ~3% of the timestamp-derived durations
below), is reliable enough to filter on directly:

```
$ perf script -i perf-pipelined-1kb.data --time 525492.358,525494.49 | grep -c 'cpu/cycles/P:'
3972   # SET phase: 200,000 commands, 2.132s (525492.358 -> 525494.49)
$ perf script -i perf-pipelined-1kb.data --time 525494.49,525504.5 | grep -c 'cpu/cycles/P:'
1064   # GET phase: 200,000 commands, 9.956s (525494.49 -> 525504.446, last sample)
```

Per request, that's:

| Phase | Samples | Requests | Wall time | Samples/request | Wall-clock/request |
|---|---|---|---|---|---|
| SET | 3,972 | 200,000 | 2.132s | 0.0199 | 10.66µs |
| GET | 1,064 | 200,000 | 9.956s | 0.0053 | 49.78µs |

GET does **~3.7x fewer** sampled CPU cycles per request than SET (0.0199 → 0.0053) — cheaper work,
as expected for a read-only lookup-and-reply versus a write that also touches the AOF/order-lock
path — yet each GET request takes **~4.7x longer** in wall-clock time (10.66µs → 49.78µs). Less
work, more time: that inversion (a combined ~17.4x gap between the two ratios) is a direct,
non-circular statement that GET-phase wall-clock time here is not proportional to GET-phase CPU
work, measured entirely within one capture, same server process, same client, same connections —
nothing is being normalized across two different recordings, which directly closes the
phase-blending gap noted below for the secondary evidence. Since `perf record -e cpu/cycles/P`
(what `cargo flamegraph` uses) only samples active, running cycles, it is structurally blind to
time a thread spends genuinely blocked/descheduled (e.g. backpressure from a full socket send
buffer, or Nagle-driven batching delaying a small final segment behind an unacked one) — exactly
the kind of gap this per-request inversion points at.

This also lets the payload-size claim be checked directly rather than by throughput ratio alone:
pipelined-3B's own GET phase, at its measured 1,092,896.12 req/s, computes to an estimated 0.183s
for the same 200,000 requests — far too short a window to cleanly time-slice apart from SET the
way pipelined-1KB's was (its whole SET+GET burst is compressed into ~1.2s with no visible density
transition at any bucket resolution tried down to 50ms, unlike pipelined-1KB's sharp cliff), so
this number is computed from measured throughput, not isolated by timestamp the way the numbers
above are. Still, 9.956s (pipelined-1KB's real, isolated GET-phase duration) against 0.183s
(pipelined-3B's computed one) for the identical 200,000-request GET workload is a ~54.4x wall-clock
gap — consistent with, and an independent cross-check on, the 56.28x throughput ratio already
reported (19,417.47 vs 1,092,896.12 req/s) from a different angle.

**Secondary, corroborating color: the resolved call-graph's active-work composition looks nearly
identical between the two runs.** Following `entry_SYSCALL_64_after_hwframe` down with `perf
report -g graph,0.5,caller`, in **both** pipelined captures the same two syscalls dominate, at
close to the same *proportions*:

| | pipelined 3B | pipelined 1KB |
|---|---|---|
| Total syscall overhead (`entry_SYSCALL_64_after_hwframe` → ...) | 36.84% | 36.74% |
| — of which `__x64_sys_futex` (mutex wait/wake) | 20.16% | 16.91% |
| — of which `__x64_sys_writev` (the GET reply write) | 11.07% | 11.39% |

The `writev` branch resolves to a full, real call chain now (previously invisible):
`__x64_sys_writev → do_writev → vfs_writev → do_iter_readv_writev → sock_write_iter →
inet_sendmsg → tcp_sendmsg → tcp_sendmsg_locked → tcp_push → __tcp_push_pending_frames →
tcp_write_xmit → __tcp_transmit_skb → ip_queue_xmit → ip_local_out → ip_output →
ip_finish_output2 → neigh_hh_output → __dev_queue_xmit → do_softirq → net_rx_action →
__napi_poll → process_backlog → __netif_receive_skb → ip_rcv → ip_local_deliver →
tcp_v4_rcv → tcp_v4_do_rcv → tcp_rcv_established` — a genuine loopback round trip through the
*entire* TCP/IP stack and back into the kernel's own receive path for every pipelined write. This
is real and resolved, and it is exactly the "socket write path under large pipelined responses"
the 08-30 notes hypothesized. But treat "11.07% vs 11.39%, nearly identical" as weak, corroborating
color, not as the headline finding — it's close to tautological under the off-CPU-wait hypothesis
above: if the extra time genuinely is idle/blocked and idle/blocked time emits no cycle samples at
all, then of course the *proportions among the samples that do exist* look similar between the two
runs — that's what "the missing time doesn't show up as CPU work" predicts, not an independent
confirmation of it. The sample-count/wall-time comparison above is the non-circular version of
this same observation and should be weighted accordingly.

**A real limitation, still present in the secondary evidence above (not in the primary evidence
this time):** the `writev`/futex proportion table's two columns are each computed over their
capture's *whole* recording — SET phase and GET phase blended together — because that specific
`perf report -g graph,0.5,caller` pass wasn't time-sliced the way the primary SET-vs-GET comparison
above was. The order lock is SET-only (`dispatch_and_log_inner`'s `_order_guard` only binds for
write commands) and this anomaly is GET-only, so comparing that whole-capture aggregate (11.07% vs
11.39% writev proportion, blending both phases) against a GET-phase-only throughput ratio is still
a normalization mismatch for that specific table — the two numbers aren't measuring matched slices
of the same work. Re-running that `-g graph,0.5,caller` pass with the same `--time` boundaries used
above would close this the same way; not done here since the table was always secondary,
corroborating color (see above) and the primary evidence no longer depends on it.

**The SET-phase comparison, corrected.** The profiled captures' own SET numbers (96,665 req/s at
1KB vs 209,864 req/s at 3B, both `-P 16`) look like a ~2.2x gap, but those numbers are themselves
distorted by profiling overhead, and — as the next paragraph shows — that distortion is *not*
uniform across workloads, so it's not safe to compare two profiled numbers against each other
directly here. Against the same-day **unprofiled** baseline in `2026-09-07-redis-benchmark.md`
(SET 1KB `-P 16`: 264,550.28 req/s; SET 3B `-P 16`: 332,225.91 req/s), the real gap is **~1.26x**
— a modest, unremarkable difference, nothing like the 56x GET gap. This still supports the same
conclusion (the anomaly is specific to the server's large-pipelined-*reply* write path, not to
large payloads generally — SET pushes the 1KB payload the *other* direction, client→server, and
barely slows down) but the corrected number is the one to cite, not the profiled one.

**Profiling overhead itself is heterogeneous across workloads, which is worth flagging on its own.**
Computing unprofiled-baseline ÷ profiled-capture per workload this task profiled:

| Workload | Unprofiled (req/s) | Profiled (req/s) | Overhead ratio |
|---|---|---|---|
| SET, 3B, no pipeline | 87,719.30 | 71,556.35 | 1.23x |
| GET, 3B, no pipeline | 99,206.34 | 83,437.62 | 1.19x |
| SET, 3B, `-P 16` | 332,225.91 | 209,863.59 | 1.58x |
| GET, 3B, `-P 16` | 1,351,351.38 | 1,092,896.12 | 1.24x |
| SET, 1KB, `-P 16` | 264,550.28 | 96,665.05 | **2.74x** |
| GET, 1KB, `-P 16` | 19,496.98 | 19,417.47 | **1.00x** |

Five of six workloads cluster in a 1.19x–1.58x "profiling tax" band — unsurprising, `perf record
-F 997 --call-graph dwarf` isn't free. Two don't: SET-1KB-`P16` is far more distorted than
anything else (2.74x), while GET-1KB-`P16` — the anomaly itself — shows **no measurable profiling
distortion at all** (1.00x). What specifically drives SET-1KB-`P16`'s outsized distortion is not
established by this data, and it is *not* simply "more concurrent writers hitting
`lock_for_ordering`": SET-3B-`P16` puts more SET commands/sec through that exact same lock than
SET-1KB-`P16` does (209,863.59 vs 96,665.05 req/s profiled; 332,225.91 vs 264,550.28 req/s
unprofiled — SET-3B-`P16` is the higher-rate case either way) yet shows *less* profiling distortion
(1.58x vs 2.74x) — the opposite of what a "more lock traffic → more distortion" story predicts.
Recorded as an open, contradicted-by-its-own-neighbor question, not a claimed cause. GET-1KB-`P16`'s
1.00x is more straightforward: if the bottleneck were CPU-bound rocket-mem work, adding a sampling
profiler's overhead to that work should slow it down like it does everywhere else; a bottleneck
that's already dominated by kernel-side blocking/backpressure has little additional CPU work for
the profiler to tax — a minor data point for the off-CPU-wait reading.

**This profile corroborates, rather than contradicts, both candidate causes raised for
cross-validation:**
- **Missing `TCP_NODELAY`:** confirmed by source inspection (`grep -rn "nodelay\|NODELAY" crates/server/src/` returns nothing — no connection path sets it). A CPU profile cannot directly prove a Nagle-driven stall (that cost is off-CPU wait time by definition), but the *category* of symptom this profile shows — near-identical on-CPU work distribution paired with a huge wall-clock gap, isolated to the reply-write direction — is exactly what a missing-`TCP_NODELAY` bottleneck would look like under CPU-only sampling: invisible in "where do cycles go" and visible only in "how long did it take." Not proof, but consistent, and the profile found nothing to contradict it.
- **The shared `AtomicU64` recency clock** (`engine::store`/`engine::shard`): `Store::shard_for`/`Shard::get`/`Shard::set` self-time is small throughout (0.05–0.35% across all three captures) and does rise somewhat under concurrency, but the compiler almost certainly inlines the recency-tick touch into `Shard::get`/`set` at `-O3`, so this profile cannot isolate that specific atomic store's cost from the rest of those functions' work — nowhere near the magnitude of the Mutex or socket-write findings either way. (Note: `metrics_util::registry::recency::Generational<AtomicU64>` frames that also appear in these reports are the *Prometheus metrics registry's* unrelated recency tracking, not the engine's own per-entry recency clock — a naming coincidence worth flagging so it isn't misread as evidence either way.) This profile neither confirms nor rules out the recency-clock hypothesis; `perf annotate` at the instruction level, not this task, would be the way to check it directly.

## Methodology note (self-review)

The brief's script has a bash job-control bug: `cmd | tee file &` backgrounds the *pipeline*, so
`FLAME_PID=$!` captures `tee`'s PID, not `cargo flamegraph`'s — `kill -INT "$FLAME_PID"` silently
signals the wrong process and the capture never stops on its own. Worked around by redirecting to
a log file instead of piping (`... > log 2>&1 &`) and locating the actual `perf record` process
via `pgrep -x perf` before signaling it. The first attempts at the unpipelined-3b and pipelined-3b
captures hit this bug and sat idle for several minutes before being manually recovered mid-task;
both were **discarded and re-captured cleanly** once the fix was in place (final sample-bounded
spans — the interval between each capture's first and last recorded sample, not total process
runtime — were 8.8s, 4.7s, and 16.1s respectively, confirmed via `perf report --header-only`, and
each is in the right order of magnitude for its `redis-benchmark` run's actual duration). All
percentages and call-graphs in this document are from the clean, tightly-scoped re-captures, not
the diluted first attempts.

## Recorded, not acted on

Both findings above are diagnostic input for Phase 3's fix selection, not fixes themselves, per
this task's brief and the sprint's own sequencing (a TDD plan can't be written for a fix whose
target this task is the one establishing). Specifically queued for that follow-up work:

- Narrowing `AofWriter::lock_for_ordering`'s scope, or replacing the `Mutex<()>` ordering
  mechanism, informed by the source-level-elimination (not call-graph-direct) attribution above.
- Setting `TCP_NODELAY` on the RESP/RMP accept paths, informed by the source-confirmed absence
  and the profile's consistent (not proving, but non-contradicting) symptom shape.
- If pursued further, an off-CPU/wallclock profiling pass (not a CPU-cycle one) is the correct
  next tool for both open items — this profile's method has now been pushed about as far as a
  `cpu/cycles` sampling profiler can go for a wait-dominated bottleneck.
