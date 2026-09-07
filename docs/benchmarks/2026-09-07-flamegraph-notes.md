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
trusting the benchmark doc) was step 1 of this task's own analysis.

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
entry point, not a rocket-mem frame. This happens because `perf record --call-graph dwarf` samples
and unwinds from the interrupted context; a thread that's actually asleep in the kernel (as
opposed to running user code that gets NMI-sampled) doesn't have a live register/stack snapshot at
the point `perf` can walk DWARF CFI from — the unwind has nothing above the syscall to walk. This
is real and reproducible (checked with `--symbol-filter` and threshold 0, not just a display cutoff)
across both pipelined captures. It means direct call-graph attribution of the contended-Mutex
frame to one specific caller is **not possible from this kind of profile**, independent of the
`kptr_restrict` fix — a different tool (e.g. an eBPF-based off-CPU profiler, or `perf record -e
sched:sched_switch`) would be needed to see the sleeping side of a contended lock's call stack.

## The Mutex-contention question: attributed by strong structural + correlational evidence, not by direct call-graph

**Direct answer: still not attributable by call-graph** (see above), but the isolated,
per-phase captures this task's methodology change was specifically for make an indirect
attribution solid enough to act on.

Flat self-time for `std::sync::mutex::futex::Mutex::lock_contended`, isolated per phase (this is
the number the 2026-08-30 profile could only report as one blended 1.96% across all three phases):

| Capture | `lock_contended` self % | `AofWriter::append_encoded` | `AofWriter::send` | `SlowLog::maybe_record` | `ReplicaRegistry::broadcast` |
|---|---|---|---|---|---|
| unpipelined 3B | **0.14%** | 0.14% | 0.04% | 0.09% | 0.01% |
| pipelined 3B (`-P 16`) | **3.54%** | 0.10% | 0.17% | 0.20% | 0.09% |
| pipelined 1KB (`-P 16 -d 1024`) | **2.96%** | 0.06% | 0.04% | 0.07% | 0.09% |

Two things this table shows cleanly, that the blended prior profile couldn't:

1. **The Mutex is overwhelmingly a pipelining/throughput-rate effect, not a payload-size effect.**
   It jumps ~25x between unpipelined and pipelined at the *same* 3-byte payload (0.14% → 3.54%),
   but stays roughly the same order of magnitude between the two pipelined runs regardless of
   payload (3.54% at 3B vs 2.96% at 1KB). Whatever this Mutex guards, contention on it scales with
   *how fast commands arrive back-to-back*, not with how big they are.
2. **None of the three named candidates' own self-time scales anywhere near proportionally with
   `lock_contended`.** `SlowLog::maybe_record` and `ReplicaRegistry::broadcast` stay under 0.2%
   in every capture (and this benchmark has zero registered replicas, so `broadcast`'s lock is
   held only to iterate an empty `Vec` and return — a few nanoseconds). If either of those were
   the contended lock, their own self-time (the fast path *and* the contended path both run
   inside the same function) would need to scale similarly. It doesn't.

Cross-referencing the source settles which of the three candidates this has to be.
`crates/server/src/dispatcher.rs:2537`:

```rust
let _order_guard = write_name.as_ref().map(|_| aof.lock_for_ordering());
```

`AofWriter::lock_for_ordering()` (`crates/server/src/aof.rs:277`, backed by `self.order:
Mutex<()>`) is held across the *entire* "mutate the engine, then log it" section of
`dispatch_and_log` for every write command — not a quick push like `SlowLog`'s or
`ReplicaRegistry`'s, but a single global mutex serializing all concurrent writers across the
whole server for the encode-then-enqueue duration. `-P 16` doesn't change payload size; it changes
how many write commands per connection are in flight and get dispatched back-to-back with no
network round-trip between them, directly multiplying the *rate* at which 50 concurrent
connections' commands hit this one lock. That is exactly the variable the data above says drives
the contention. `SlowLog`'s and `ReplicaRegistry`'s locks are architecturally quick,
independently-scoped, per-call critical sections with no reason to scale with pipeline depth the
way a single serializing ordering-lock does.

**Verdict:** `AofWriter::lock_for_ordering` (the `order: Mutex<()>` field) is the far more likely
source of the contended `std::sync::Mutex`, on structural grounds (it is the only one of the three
candidates architected to serialize *all* concurrent writers into one critical section) and on
correlational grounds (contention tracks pipelining exactly as this lock's design predicts, while
none of the alternative candidates' own measured self-time scales similarly). This is not a
call-graph-confirmed attribution — that remains blocked by the futex-unwind limitation above — so
treat it as a strong, actionable lead for Phase 3, not a certainty proven by this profile alone.

## The 43x/58x pipelined-1KB GET anomaly

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

**The real finding is in the resolved call-graph, and it points at the socket write path — but
not the way "a distinct hot path" implies.** Following `entry_SYSCALL_64_after_hwframe` down with
`perf report -g graph,0.5,caller`, in **both** pipelined captures essentially the same two
syscalls dominate, at essentially the same *proportions*:

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
*entire* TCP/IP stack and back into the kernel's own receive path for every pipelined write,
costing double-digit percent of all CPU cycles sampled. This is real, resolved, and exactly the
"socket write path under large pipelined responses" the 08-30 notes hypothesized — but it is
**not distinctively larger for the 1KB run**. The proportion is nearly identical (11.07% vs
11.39%) between a workload running at 1.09M req/s and one running at 19.4K req/s — a 56x
difference in throughput with essentially no difference in *where the active CPU cycles go*.

That is the actual anomaly, restated precisely: **the bottleneck is not what the CPU is doing, it
is how much wall-clock time elapses per unit of that (roughly fixed-proportion) work.** A
CPU-cycle sampling profiler (`perf record -e cpu/cycles/P`, what `cargo flamegraph` uses by
default) only samples active, running cycles — it is structurally blind to time a thread spends
genuinely blocked/descheduled (e.g. backpressure from a full socket send buffer, or Nagle-driven
batching delaying a small final segment behind an unacked one). Weak but consistent supporting
evidence for this reading: sample *density* (samples per wall-clock second) is markedly lower in
the 1KB capture (~5,540 samples / 4.68s ≈ 1,180/s for pipelined-3b vs ~7,399 samples / 16.09s ≈
460/s for pipelined-1kb) — roughly 2.6x fewer active-CPU samples per second of wall time, in a
recording where 50 connections × 16 pipeline depth should be keeping every worker thread
maximally busy if the server weren't stalling. That drop is consistent with — not proof of —
threads spending real wall-clock time off-CPU (blocked) rather than computing.

One more data point narrows this further: the SET phase (client → server, 1KB *request* payload,
still `-P 16`) is only ~2.2x slower at 1KB than at 3B (96,665 vs 209,864 req/s) — nothing like the
56x GET gap. **The anomaly is specific to the server's write path for large pipelined *replies*,
not to large payloads in general** — ruling out a generic "big buffers are slow" explanation and
narrowing it specifically to the direction rocket-mem writes to the socket.

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
both were **discarded and re-captured cleanly** once the fix was in place (final sample durations:
8.8s, 4.7s, and 16.1s — all consistent with their actual `redis-benchmark` run times, confirmed via
`perf report --header-only`). All percentages and call-graphs in this document are from the clean,
tightly-scoped re-captures, not the diluted first attempts.

## Recorded, not acted on

Both findings above are diagnostic input for Phase 3's fix selection, not fixes themselves, per
this task's brief and the sprint's own sequencing (a TDD plan can't be written for a fix whose
target this task is the one establishing). Specifically queued for that follow-up work:

- Narrowing `AofWriter::lock_for_ordering`'s scope, or replacing the `Mutex<()>` ordering
  mechanism, informed by the correlational (not call-graph-direct) attribution above.
- Setting `TCP_NODELAY` on the RESP/RMP accept paths, informed by the source-confirmed absence
  and the profile's consistent (not proving, but non-contradicting) symptom shape.
- If pursued further, an off-CPU/wallclock profiling pass (not a CPU-cycle one) is the correct
  next tool for both open items — this profile's method has now been pushed about as far as a
  `cpu/cycles` sampling profiler can go for a wait-dominated bottleneck.
