# Benchmark Report & Flamegraph Pass Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a committed, reproducible head-to-head benchmark against real Redis — honest about where this server is slower and why — plus one profiled hot-path bottleneck actually fixed, with before/after numbers.

**Architecture:** `scripts/benchmark.sh` starts a real `redis-server` and a release-build `rocket-mem` side by side with matching durability settings and runs the identical `redis-benchmark` matrix against both. The results and their reading are committed under `docs/benchmarks/`. The profiling pass uses `cargo flamegraph` against the same load; the fix it validates is named in advance because it is visible by inspection: every command currently allocates its uppercased name on the heap two to four times.

**Tech Stack:** `redis-server` + `redis-benchmark` (system packages, not Cargo dependencies), `cargo-flamegraph` (a dev tool installed with `cargo install flamegraph`, deliberately **not** added to `Cargo.toml` — it is a binary, not a dependency).

**Spec:** ../../specs/2026-08-30-sprint-6-spec.md — "the benchmark is a committed script plus a committed report; the profiling pass fixes one named bottleneck" is authoritative for this plan. Depends on plans 01–06, since the thing being benchmarked is the finished server.

## Global Constraints

- **The benchmark is not a CI test.** `redis-server` is not installed on the CI runner, and throughput numbers from a shared CI machine would be noise gated as if it were signal. Nothing in this plan adds a test that requires either binary.
- **Both servers are configured for the same durability** (`appendonly yes`, `appendfsync everysec`), because `rocket-mem`'s binary always runs an AOF with an `EverySecond` policy (`crates/server/src/main.rs:21`) and comparing it against a Redis with persistence off would flatter it.
- **The report must state where this server is slower and why.** That is the DoD's own wording and the part a reader will judge; a table of numbers with no reading of them does not satisfy it.
- **The lock-contention rabbit hole stays closed.** Per the sprint plan's risk-table mitigation: if the flamegraph shows shard-lock contention, it is recorded in the notes and in `docs/design/sharding-decision.md` (which has been waiting for this data since Sprint 1) — it is *not* acted on this sprint.
- Numbers in the committed report must come from a real run on the machine that runs it. Do not fill the table from this plan.

---

### Task 1: the benchmark script

**Files:**
- Create: `scripts/benchmark.sh`

**Interfaces:**
- Consumes: the release binary built from plans 01–06.
- Produces: `scripts/benchmark.sh`, which writes its results to stdout in the exact shape Task 2's report records.

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Head-to-head redis-benchmark run: real Redis vs rocket-mem, identical workloads and
# identical durability settings. Writes a plain-text summary to stdout; the committed report
# in docs/benchmarks/ is assembled from this output.
set -euo pipefail

for bin in redis-server redis-benchmark; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "error: '$bin' is not on PATH. Install a Redis distribution first" >&2
    echo "       (Debian/Ubuntu: apt install redis-server redis-tools)" >&2
    exit 1
  fi
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REDIS_PORT=7777
ROCKET_PORT=7778

echo "Building rocket-mem in release mode..." >&2
cargo build --release --workspace --manifest-path "$ROOT/Cargo.toml" >&2

WORK="$(mktemp -d)"
REDIS_PID=""
ROCKET_PID=""
cleanup() {
  [ -n "$REDIS_PID" ] && kill "$REDIS_PID" 2>/dev/null || true
  [ -n "$ROCKET_PID" ] && kill "$ROCKET_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# Same durability on both sides: rocket-mem always runs an AOF with an everysec fsync policy,
# so benchmarking against a Redis with persistence off would flatter it.
redis-server --port "$REDIS_PORT" --save '' --appendonly yes --appendfsync everysec \
  --dir "$WORK" >"$WORK/redis.log" 2>&1 &
REDIS_PID=$!

ROCKET_MEM_ADDR="127.0.0.1:$ROCKET_PORT" \
ROCKET_MEM_AOF_PATH="$WORK/rocket.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/rocket.snapshot" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:9178" \
  "$ROOT/target/release/rocket-mem" >"$WORK/rocket.log" 2>&1 &
ROCKET_PID=$!

sleep 1
redis-cli -p "$REDIS_PORT" ping >/dev/null
redis-cli -p "$ROCKET_PORT" ping >/dev/null

echo "redis-server:  $(redis-cli -p "$REDIS_PORT" info server | grep -i '^redis_version' | tr -d '\r')"
echo "rocket-mem:    $(redis-cli -p "$ROCKET_PORT" info server | grep -i '^redis_version' | tr -d '\r')"
echo "host:          $(uname -srm)"
echo "date:          $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo

# -q prints one "COMMAND: N requests per second" line per tested command.
run_case() { # $1=label $2=port $3=payload-bytes $4=pipeline-depth
  echo "--- $1 (payload=${3}B, pipeline=${4}) ---"
  redis-benchmark -h 127.0.0.1 -p "$2" -t set,get -n 100000 -c 50 -d "$3" -P "$4" -q
  redis-cli -p "$2" flushall >/dev/null 2>&1 || true
}

for payload in 3 1024; do
  for pipeline in 1 16; do
    run_case "redis-server" "$REDIS_PORT" "$payload" "$pipeline"
    run_case "rocket-mem" "$ROCKET_PORT" "$payload" "$pipeline"
    echo
  done
done

echo "--- rocket-mem /metrics sample after the run ---"
curl -s "http://127.0.0.1:9178/metrics" | grep -E '^rocket_mem_(commands_total|keys|memory_used_bytes)' || true
```

Note `redis-cli -p "$ROCKET_PORT" flushall` will fail against this server (there is no `FLUSHALL`), which is why `run_case` tolerates a non-zero exit there; the real Redis side is genuinely reset between cases, and `rocket-mem`'s keyspace simply carries over, which is noted in the report as a difference in setup rather than hidden.

- [ ] **Step 2: Make it executable and smoke-test the guard path**

```bash
chmod +x scripts/benchmark.sh
PATH=/usr/bin:/bin scripts/benchmark.sh 2>&1 | head -3   # if redis isn't installed, this must
                                                          # print the install hint and exit 1
```

Expected: either the script runs (Redis installed), or it prints `error: 'redis-server' is not on PATH...` and exits 1 — never a confusing failure deeper in.

- [ ] **Step 3: Commit**

```bash
git add scripts/benchmark.sh
git commit -m "chore(bench): add a head-to-head redis-benchmark script"
```

---

### Task 2: the committed benchmark report

**Files:**
- Create: `docs/benchmarks/2026-08-30-redis-benchmark.md`

**Interfaces:**
- Consumes: `scripts/benchmark.sh` (Task 1).
- Produces: the sprint's second DoD item, evidenced. Its "before" numbers are the baseline Task 5 compares against.

- [ ] **Step 1: Run the benchmark and capture the output**

```bash
mkdir -p docs/benchmarks
scripts/benchmark.sh | tee /tmp/rocket-mem-bench.txt
```

If `redis-server` is not installed, install it first (`apt install redis-server redis-tools`, `brew install redis`, or equivalent) — this DoD item cannot be satisfied without a real Redis to compare against, and inventing numbers is worse than shipping nothing.

- [ ] **Step 2: Write the report from that output**

Create `docs/benchmarks/2026-08-30-redis-benchmark.md` with this structure, filling every number from `/tmp/rocket-mem-bench.txt` — no value in the table may be copied from this plan:

```markdown
# rocket-mem vs Redis — `redis-benchmark`, Sprint 6

**Run:** `scripts/benchmark.sh` (committed). **Date / host / versions:** <paste the header block the script printed>.

## Setup

Both servers run with the same durability settings: `appendonly yes`, `appendfsync everysec`,
RDB/snapshot auto-save off. `redis-benchmark -t set,get -n 100000 -c 50` at two payload sizes
(3B and 1024B), each without pipelining and with `-P 16`. The real Redis keyspace is flushed
between cases; rocket-mem has no `FLUSHALL`, so its keyspace carries over — worth knowing when
reading the 1024B numbers, where its memory footprint is larger by the end of the run.

## Results (requests/second, higher is better)

| Workload | redis-server | rocket-mem | ratio |
|---|---|---|---|
| SET, 3B, no pipeline | | | |
| GET, 3B, no pipeline | | | |
| SET, 3B, `-P 16` | | | |
| GET, 3B, `-P 16` | | | |
| SET, 1KB, no pipeline | | | |
| GET, 1KB, no pipeline | | | |
| SET, 1KB, `-P 16` | | | |
| GET, 1KB, `-P 16` | | | |

## Where we are slower, and why

<Write this section from the numbers, not from expectations. Candidate explanations to check
against the profile in `2026-08-30-flamegraph-notes.md` before asserting any of them:>

- **Per-command allocations in the dispatcher.** Every command allocates its uppercased name on
  the heap (`dispatch`, `extract_write_command_name`, the metrics wrapper, the cluster gate), and
  every write command clones its `Frame` for the AOF path. Real Redis parses into a reused
  argument vector and never allocates a command name at all.
- **Reply encoding.** `RespCodec::encode` builds a fresh `BytesMut` per reply; Redis writes into
  a per-client output buffer it reuses.
- **Durability path.** Every write is encoded a second time for the AOF and sent over a channel
  to the writer thread; Redis appends into an in-process buffer flushed once per event-loop
  iteration.
- **What we should be competitive on:** pipelined reads, where `connection.rs`'s `now_or_never`
  batching (Sprint 2) means one syscall per batch rather than per reply.

## Where we are faster, if anywhere

<Record honestly. If nothing, say so.>

## What this does not measure

Single-node only; no cluster routing overhead is exercised (a `-MOVED` reply is cheaper than a
served command, so a cluster-mode benchmark would flatter the numbers). No concurrent-client
scaling curve. No latency percentiles beyond what `-q` reports — the `/metrics` histogram is the
place to read those.
```

- [ ] **Step 3: Commit**

```bash
git add docs/benchmarks/2026-08-30-redis-benchmark.md
git commit -m "docs(bench): record the Sprint 6 head-to-head benchmark against real Redis"
```

---

### Task 3: the flamegraph pass

**Files:**
- Create: `docs/benchmarks/2026-08-30-flamegraph.svg`
- Create: `docs/benchmarks/2026-08-30-flamegraph-notes.md`

**Interfaces:**
- Consumes: the release binary and `scripts/benchmark.sh`.
- Produces: the evidence Task 4's optimization is judged against.

- [ ] **Step 1: Install the tooling and allow profiling**

```bash
cargo install flamegraph        # provides `cargo flamegraph`; a dev tool, never a Cargo dependency
# Linux only: perf needs permission to sample this process
sudo sysctl -w kernel.perf_event_paranoid=1
```

- [ ] **Step 2: Profile under the benchmark's load**

```bash
mkdir -p docs/benchmarks
ROCKET_MEM_ADDR=127.0.0.1:7778 \
ROCKET_MEM_AOF_PATH=/tmp/flame.aof \
ROCKET_MEM_SNAPSHOT_PATH=/tmp/flame.snapshot \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9179 \
  cargo flamegraph --release --bin rocket-mem \
  -o docs/benchmarks/2026-08-30-flamegraph.svg &
FLAME=$!
sleep 3
redis-benchmark -h 127.0.0.1 -p 7778 -t set,get -n 200000 -c 50 -d 3 -q
redis-benchmark -h 127.0.0.1 -p 7778 -t set,get -n 200000 -c 50 -d 3 -P 16 -q
kill -INT $FLAME    # SIGINT lets cargo-flamegraph finish writing the SVG
wait $FLAME || true
```

- [ ] **Step 3: Read the flamegraph and write the notes**

Create `docs/benchmarks/2026-08-30-flamegraph-notes.md`:

```markdown
# Flamegraph notes — Sprint 6

**Profile:** `2026-08-30-flamegraph.svg`, captured with `cargo flamegraph --release --bin
rocket-mem` under `redis-benchmark -t set,get -n 200000 -c 50` (both un-pipelined and `-P 16`).

## What the profile shows

<Read the SVG and record the widest frames under `dispatcher::dispatch_and_log` and under the
tokio reactor, with rough percentages. Name real frames from this profile; do not paraphrase the
expectations below as if they were findings.>

## The bottleneck this sprint fixes

Per-command heap allocation of the uppercased command name. Before this sprint's change the name
was allocated by `String::from_utf8_lossy(..).to_ascii_uppercase()` in four places on every
command: `dispatch` (`crates/server/src/dispatcher.rs:67`), `extract_write_command_name`
(`:933`), `command_name_upper` (the metrics wrapper), and `command_keys` (the cluster gate). All
four now share one stack-allocated `CommandName`; the measured effect is in
[`2026-08-30-redis-benchmark.md`](2026-08-30-redis-benchmark.md)'s "Effect of the Sprint 6
optimization" section.

## Recorded, not acted on

<If the profile shows shard-lock contention (`parking_lot::RwLock` frames under `Shard::get`/
`Shard::set`), record the share here and cross-reference `docs/design/sharding-decision.md`. Per
the sprint plan's risk table, a lock-free shard rewrite is explicitly out of scope this sprint —
this profile is the data that decision has been waiting for since Sprint 1, not a licence to act
on it now.>
```

- [ ] **Step 4: Commit**

```bash
git add docs/benchmarks/2026-08-30-flamegraph.svg docs/benchmarks/2026-08-30-flamegraph-notes.md
git commit -m "docs(bench): record the Sprint 6 flamegraph profile and its reading"
```

---

### Task 4: remove the per-command command-name allocations

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (`dispatch` at `:59-67`, `extract_write_command_name` at `:926-937`, `command_keys` and `command_name_upper` from plans 02 and 04)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub(crate) struct CommandName` with `fn as_str(&self) -> &str`, and `fn upper_name(raw: &[u8]) -> Option<CommandName>`; `extract_write_command_name` now returns `Option<CommandName>` and `command_name_upper` returns `Option<CommandName>`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
    #[test]
    fn upper_name_uppercases_ascii_into_a_stack_buffer() {
        assert_eq!(upper_name(b"get").unwrap().as_str(), "GET");
        assert_eq!(upper_name(b"SeT").unwrap().as_str(), "SET");
        assert_eq!(upper_name(b"ZINCRBY").unwrap().as_str(), "ZINCRBY");
        assert_eq!(upper_name(b"").unwrap().as_str(), "");
    }

    #[test]
    fn upper_name_rejects_names_that_cannot_be_a_command() {
        // longer than any real command name -- necessarily unknown, and handled on the cold path
        assert!(upper_name(&[b'a'; MAX_COMMAND_NAME_LEN + 1]).is_none());
        // non-ASCII cannot be uppercased byte-wise, and no command name contains it
        assert!(upper_name(&[0xff, 0xfe]).is_none());
    }

    #[test]
    fn an_over_long_command_name_still_gets_the_normal_unknown_command_error() {
        let engine = Engine::new();
        let long_name = vec![b'A'; MAX_COMMAND_NAME_LEN + 1];
        let reply = dispatch(
            &engine,
            Frame::Array(vec![Frame::Bulk(Bytes::from(long_name.clone()))]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            reply,
            Frame::Error(format!(
                "ERR unknown command '{}'",
                String::from_utf8_lossy(&long_name)
            ))
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::upper_name dispatcher::tests::an_over_long_command_name`
Expected: FAIL to compile with "cannot find function `upper_name`"

- [ ] **Step 3: Add `CommandName` and route every name through it**

```rust
// crates/server/src/dispatcher.rs — add near the top, below `frame_to_args` (:23)
/// No command this server answers is longer than 12 bytes (`SUNIONSTORE`, `SRANDMEMBER`); 32 is
/// generous headroom that still fits comfortably on the stack.
pub(crate) const MAX_COMMAND_NAME_LEN: usize = 32;

/// A command name uppercased into a fixed stack buffer. Exists to remove the two-to-four heap
/// allocations every single command used to pay for its own name -- `dispatch`,
/// `extract_write_command_name`, the metrics wrapper, and the cluster routing gate each did
/// `String::from_utf8_lossy(..).to_ascii_uppercase()` independently. See
/// ../../docs/benchmarks/2026-08-30-flamegraph-notes.md for the profile that motivated it.
pub(crate) struct CommandName {
    buf: [u8; MAX_COMMAND_NAME_LEN],
    len: usize,
}

impl CommandName {
    pub(crate) fn as_str(&self) -> &str {
        // `upper_name` accepts only ASCII, so this cannot fail.
        std::str::from_utf8(&self.buf[..self.len]).expect("upper_name accepts only ASCII input")
    }
}

/// Uppercases `raw` into a `CommandName`, or `None` if it cannot be a command name at all --
/// longer than `MAX_COMMAND_NAME_LEN`, or non-ASCII. Both cases are necessarily unknown
/// commands, and callers handle them on their cold path.
pub(crate) fn upper_name(raw: &[u8]) -> Option<CommandName> {
    if raw.len() > MAX_COMMAND_NAME_LEN || !raw.is_ascii() {
        return None;
    }
    let mut buf = [0u8; MAX_COMMAND_NAME_LEN];
    for (slot, byte) in buf.iter_mut().zip(raw) {
        *slot = byte.to_ascii_uppercase();
    }
    Some(CommandName {
        buf,
        len: raw.len(),
    })
}
```

```rust
// crates/server/src/dispatcher.rs — in `dispatch`, replace lines :67-70 (the `let name = ...`
// line, the `let rest = ...` line that follows it, and the `match name.as_str() {` opener) with:
    let Some(name) = upper_name(&args[0]) else {
        // Cold path only: a name too long or non-ASCII to be any command we know. The error text
        // is unchanged from before this optimization -- it echoes the client's own bytes.
        return Frame::Error(format!(
            "ERR unknown command '{}'",
            String::from_utf8_lossy(&args[0])
        ));
    };
    let rest = &args[1..];

    match name.as_str() {
```

and change `dispatch`'s final fall-through arm to use `name.as_str()`:

```rust
        _ => Frame::Error(format!("ERR unknown command '{}'", name.as_str())),
```

```rust
// crates/server/src/dispatcher.rs — `extract_write_command_name` (:926) now returns
// `Option<CommandName>`; only its last three lines change
fn extract_write_command_name(frame: &Frame) -> Option<CommandName> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name_bytes)) = items.first() else {
        return None;
    };
    let name = upper_name(name_bytes)?;
    crate::aof::WRITE_COMMANDS
        .contains(&name.as_str())
        .then_some(name)
}
```

```rust
// crates/server/src/dispatcher.rs — `command_name_upper` (from 04-prometheus-metrics.md) now
// returns `Option<CommandName>`
fn command_name_upper(frame: &Frame) -> Option<CommandName> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    upper_name(name)
}
```

```rust
// crates/server/src/dispatcher.rs — the `dispatch_and_log` wrapper's first lines become:
    let name = command_name_upper(&frame);
    let name = name.as_ref().map(|n| n.as_str()).unwrap_or("");
    let label = metric_label(name);
// ...and its slow-log call (from 06-slowlog.md) passes `name` directly:
    replication
        .slowlog
        .maybe_record(name, first_key, arg_count, elapsed);
```

```rust
// crates/server/src/dispatcher.rs — `command_keys` (from 02-cluster-commands-and-moved.md)
// replaces its `String::from_utf8_lossy(..).to_ascii_uppercase()` line with:
    let Some(name) = upper_name(name_bytes) else {
        return Vec::new(); // not a command name we know, so it has no keys to route
    };
// ...and its `match key_spec(&name)` becomes `match key_spec(name.as_str())`.
```

The `command_name_upper` tests added by `04-prometheus-metrics.md` change with the return type: `assert_eq!(command_name_upper(&cmd(&[b"get", b"k"])), "GET")` becomes `assert_eq!(command_name_upper(&cmd(&[b"get", b"k"])).unwrap().as_str(), "GET")`, and the two `""` cases become `assert!(command_name_upper(&Frame::Simple("nope".into())).is_none())` and `assert!(command_name_upper(&Frame::Array(vec![])).is_none())`.

- [ ] **Step 4: Run the whole suite to verify nothing changed but the allocations**

Run: `cargo test --workspace`
Expected: PASS, every test in the workspace — this is a pure optimization, so any behavioral test that changes is a bug in the change, not in the test

- [ ] **Step 5: Verify the lint gate**

Run: `cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "perf(server): uppercase command names into a stack buffer instead of the heap"
```

---

### Task 5: re-measure and record the effect

**Files:**
- Modify: `docs/benchmarks/2026-08-30-redis-benchmark.md`

**Interfaces:**
- Consumes: `scripts/benchmark.sh`, the Task 4 optimization.
- Produces: the before/after numbers the sprint's P1 profiling item is judged on.

- [ ] **Step 1: Re-run the benchmark on the optimized build**

```bash
scripts/benchmark.sh | tee /tmp/rocket-mem-bench-after.txt
```

- [ ] **Step 2: Append the comparison to the report**

Add this section to `docs/benchmarks/2026-08-30-redis-benchmark.md`, filling both columns from the two captured runs:

```markdown
## Effect of the Sprint 6 optimization

`perf(server): uppercase command names into a stack buffer instead of the heap` removed the
two-to-four per-command heap allocations described in
[`2026-08-30-flamegraph-notes.md`](2026-08-30-flamegraph-notes.md).

| Workload | before | after | change |
|---|---|---|---|
| SET, 3B, no pipeline | | | |
| GET, 3B, no pipeline | | | |
| SET, 3B, `-P 16` | | | |
| GET, 3B, `-P 16` | | | |

<Both runs are single samples on one machine. If the difference is inside run-to-run noise, say
exactly that — an honest "no measurable change at this workload, though the allocations are
provably gone" is a better entry here than a rounded-up improvement.>
```

- [ ] **Step 3: Commit**

```bash
git add docs/benchmarks/2026-08-30-redis-benchmark.md
git commit -m "docs(bench): record the before/after effect of the dispatcher allocation fix"
```
