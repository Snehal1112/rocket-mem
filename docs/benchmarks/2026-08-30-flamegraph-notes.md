# Flamegraph notes — Sprint 6

**Profile:** `2026-08-30-flamegraph.svg`, captured with `cargo flamegraph --release --bin
rocket-mem` (`perf record -F 997 --call-graph dwarf,64000 -g`) under three back-to-back
`redis-benchmark -t set,get -n 200000 -c 50` runs against the same server process, all inside one
continuous recording:

1. `redis-benchmark -h 127.0.0.1 -p 7778 -t set,get -n 200000 -c 50 -d 3 -q` (un-pipelined)
2. `redis-benchmark -h 127.0.0.1 -p 7778 -t set,get -n 200000 -c 50 -d 3 -P 16 -q`
3. `redis-benchmark -h 127.0.0.1 -p 7778 -t set,get -n 200000 -c 50 -d 1024 -P 16 -q` — added
   specifically to try to catch the 58x GET anomaly Task 2 found at this payload/pipeline
   combination (19,493 vs Redis's 1,136,363 req/s; see `2026-08-30-redis-benchmark.md`'s "Where we
   are slower, and why"). This run reproduced the anomaly live: 19,440 req/s for GET, matching
   Task 2's number almost exactly.

## What the profile shows

**Caveat up front:** `--call-graph dwarf` unwinding is badly degraded in this environment. Most
sampled stacks in the SVG bottom out in long runs of `[unknown]` frames — either genuine kernel
addresses (`0xffffffff...`) that can't be named because `kernel.perf_event_paranoid=0` was set but
`/proc/sys/kernel/kptr_restrict=1` still blocks kernel symbol resolution for a non-root profiler
(confirmed via `perf report`'s own warning: "Kernel address maps... were restricted"), or DWARF
unwind chains that jump straight from a kernel/library address to `std::rt::lang_start_internal`/
`main`, skipping every real frame in between. I did not have instructions to `sudo` to fix
`kptr_restrict`, so this is recorded as an environment limitation, not something I worked around.
Because of this, the flame graph's *inclusive/width* percentages for `rocket-mem`'s own call
chains are not trustworthy from the SVG shape alone.

**Widest frames actually present in the SVG** (`grep -oP '<title>[^<]+' ... | sort -t'(' -k2 -rn`):

| Frame | Samples | % |
|---|---|---|
| `all` (root) | 24,205,932,245 | 100% |
| `tokio-rt-worker` (thread root) | 20,099,132,410 | 83.03% |
| `aof-writer` (thread root) | 4,093,070,636 | 16.91% |
| `rocket-mem` (thread root, accept loop) | 13,726,879 | 0.06% |
| `mio::net::tcp::listener::TcpListener::accept` | 7,285,633 | 0.03% |
| `accept4` | 7,285,633 | 0.03% |
| `once_cell::imp::OnceCell<T>::initialize::{{closure}}` → `__clock_gettime` → `[[vdso]]` | 3,321,429 | 0.01% |

Everything else in the SVG under `tokio-rt-worker` and `aof-writer` is `[unknown]`, in slowly
shrinking chains (83.03% → 79.12% → 76.90% → ... → 0.02%) consistent with one long unresolved
kernel call path per thread (epoll/netstack processing for `tokio-rt-worker`, write/fsync syscalls
for `aof-writer`), not a broken recording — `perf script -i perf.data` on the raw samples confirms
these are genuine `0xffffffff...` kernel addresses, not corrupted data.

**Frames under `dispatcher::dispatch_and_log` / `dispatch_and_log_inner` and the tokio
reactor** — not visible as SVG *widths* for the reason above, but resolvable as flat **self**-time
via `perf report -i perf.data --stdio --sort=overhead,symbol -g none` against the same
`perf.data` (leaf-IP symbol lookup doesn't need stack unwinding, so it isn't affected by the DWARF
issue):

| Symbol | Self % |
|---|---|
| `std::sys::sync::mutex::futex::Mutex::lock_contended` | 1.96% |
| `_int_free` | 1.76% |
| `__memmove_avx512_unaligned_erms` | 1.20% |
| `rocket_mem::dispatcher::dispatch_and_log` | 1.03% |
| `malloc` | 1.00% |
| `cfree@GLIBC_2.2.5` | 0.89% |
| `_int_malloc` | 0.77% |
| `protocol::codec::parse_frame` | 0.75% |
| `<protocol::codec::RespCodec as Encoder<Frame>>::encode` | 0.67% |
| `rocket_mem::dispatcher::dispatch` | 0.59% |
| `<bytes::bytes_mut::BytesMut as BufMut>::put_slice` | 0.56% |
| `tokio::runtime::context::scoped::Scoped<T>::set` | 0.48% |
| `rocket_mem::dispatcher::dispatch_and_log_inner` | 0.48% |
| `rocket_mem::connection::handle_connection::{{closure}}` | 0.47% |
| `<core::hash::sip::Hasher<S> as Hasher>::write` | 0.41% |
| `core::hash::BuildHasher::hash_one` | 0.31% |
| `tokio_util::util::poll_buf::poll_read_buf` | 0.28% |
| `bytes::bytes_mut::BytesMut::reserve_inner` | 0.28% |
| `rocket_mem::dispatcher::extract_write_command_name` | 0.28% |
| `tokio::runtime::scheduler::multi_thread::queue::Steal<T>::steal_into` | 0.28% |
| `engine::store::Store::shard_for` | 0.21% |
| `rocket_mem::slowlog::SlowLog::maybe_record` | 0.18% |
| `engine::shard::Shard::get` | 0.16% |
| `engine::shard::Shard::set` | 0.14% |
| `tokio::runtime::io::registration::Registration::poll_ready` | 0.18% |
| `tokio::runtime::io::driver::Driver::turn` | 0.15% |
| `epoll_wait` | 0.14% |
| `mio::poll::Poll::poll` | 0.10% |
| `rocket_mem::aof::encode_frame` | 0.08% |
| `rocket_mem::aof::AofWriter::append_encoded` | 0.07% |
| `rocket_mem::replication::ReplicaRegistry::broadcast` | 0.06% |
| `parking_lot::raw_rwlock::RawRwLock::lock_shared_slow` | 0.01% |

Between `dispatch_and_log` (1.03%), `dispatch` (0.59%), and `dispatch_and_log_inner` (0.48%), the
dispatcher path accounts for roughly 2.1% of total self CPU time — this is the pre-optimization
picture the sprint's fix (one stack-allocated `CommandName` instead of four
`String::from_utf8_lossy(..).to_ascii_uppercase()` allocations) is aimed at.

**On the 58x GET anomaly (1024B, pipeline 16):** the anomaly reproduced live during capture
(19,440 req/s, matching Task 2's 19,493), but because all three `redis-benchmark` phases share one
continuous `perf.data` recording, the SVG/flat-report percentages above are aggregated across all
three phases — I could not cleanly attribute samples to just the `-d 1024 -P 16` window without
timestamp-splitting the recording, which the DWARF-unwind issue would have made unreliable anyway.
What I can say from the resolved frames: no new, distinct rocket-mem function shows up as a hot
path unique to this run — the same dispatcher/codec/Shard functions dominate resolved user time at
similar orders of magnitude as in the smaller-payload runs. The buffer-copy and allocation-related
frames (`__memmove_avx512_unaligned_erms` 1.20%, `BytesMut::reserve_inner` 0.28%,
`BytesMut::put_slice` 0.56%, `malloc`/`free` family combined ~4.8%) are plausible contributors to a
payload-size-sensitive slowdown, since larger GET replies mean bigger per-response buffer growth
and copies, but I can't isolate this to the `-d 1024` phase specifically from this recording. The
dominant cost by far, in all three phases, remains unresolved in-kernel time (83.03% for the
`tokio-rt-worker` thread) — given `-P 16` pipelines 16 GETs per read/write syscall pair, the
anomaly's root cause is at least as likely to be in socket buffer / TCP write-path behavior under
large pipelined responses as in anything visible in rocket-mem's own resolved code. This profile
does not give a definitive answer to the anomaly; it rules out a single obvious new rocket-mem hot
function as the cause, and points back at the kernel-unresolved I/O path as the place a proper
answer would live (would need `kptr_restrict=0` or a frame-pointer build to chase further, both
out of scope for this task).

## The bottleneck this sprint fixes

Per-command heap allocation of the uppercased command name. As of this profile, the name is
allocated by `String::from_utf8_lossy(..).to_ascii_uppercase()` in four places on every
command: `dispatch` (`crates/server/src/dispatcher.rs:67`), `extract_write_command_name`
(defined at `crates/server/src/dispatcher.rs:907`, allocating at `:914`), `command_name_upper`
(the metrics wrapper), and `command_keys` (the cluster gate). This sprint's fix — replacing all
four with one stack-allocated `CommandName` — has not landed yet as of this profile (that's Task 4
of this same plan); once it lands, Task 5 will add an "Effect of the Sprint 6 optimization"
section to [`2026-08-30-redis-benchmark.md`](2026-08-30-redis-benchmark.md) measuring it. The flat
self-time table above shows `dispatch_and_log` (1.03%), `dispatch` (0.59%),
`dispatch_and_log_inner` (0.48%), and `extract_write_command_name` (0.28%) together at ~2.4% of
self CPU time in this pre-optimization build — this profile is the *before* picture, not a
before/after comparison; the before/after delta will live in the benchmark numbers once Task 4's
fix is measured, not in this single profile.

## Recorded, not acted on

**Shard-lock contention:** `parking_lot::raw_rwlock::RawRwLock::lock_shared_slow` — the contended
(slow-path) branch of the shard lock — appears at only 0.01% self time, under both `Shard::get`
(0.16%) and `Shard::set` (0.14%) combined. At `-c 50` concurrent clients across 16 shards, shard
contention is real but small: consistent with `docs/design/sharding-decision.md`'s reasoning for
16 shards over `DefaultHasher` being adequate at this concurrency. A lock-free shard rewrite is
explicitly out of scope this sprint per the sprint plan's risk table — this profile is the data
that decision has been waiting for since Sprint 1 (it says "don't bother yet, not this sprint"),
not a licence to act on it now.

**A second, unexpected contention point:** `std::sys::sync::mutex::futex::Mutex::lock_contended`
is the single largest *named* self-time frame in the whole profile at 1.96% — notably larger than
the parking_lot shard-lock contention. This is a plain `std::sync::Mutex`, not the shard's
`parking_lot::RwLock`, so it isn't the lock `sharding-decision.md` is about. The codebase has three
candidates on the per-command hot path that use `std::sync::Mutex`: `AofWriter::lock_for_ordering`
(`crates/server/src/aof.rs:164`, held to order every AOF-writing command), `SlowLog`'s `entries`
Mutex (`crates/server/src/slowlog.rs:4`, touched by `SlowLog::maybe_record` on every command), and
`ReplicaRegistry::senders` (`crates/server/src/replication.rs:24`, touched on writes/broadcasts).
The degraded call-graph in this profile doesn't let me attribute the 1.96% to one of the three
specifically. Recording it here rather than guessing further or changing any locking — that's
follow-up profiling work, not something to act on from this data alone.
