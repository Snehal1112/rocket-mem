# Harness and Report Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A committed, repeatable harness script, and the benchmark report it produces.

**Architecture:** `scripts/rmp-vs-resp.sh` mirrors the shape of the existing `scripts/benchmark.sh` — build release, start one server in a temp directory, wait for both ports, run the sweep, sample `/metrics`, tear down via an `EXIT` trap. The report is assembled from its output.

**Tech Stack:** Bash, `redis-cli` (liveness check only), `curl`, the `rocket-mem-bench` binary from plan 04.

**Spec:** [`../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md`](../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md)

**Depends on:** [`04-sweep-runner-and-cli.md`](04-sweep-runner-and-cli.md).

## Global Constraints

- **One server process serves both protocols.** Do not start two. Both listeners share one engine, one config, and one AOF, which is what makes the comparison fair without matching anything by hand.
- **AOF stays on with an `everysec` fsync policy**, matching `scripts/benchmark.sh` and real deployment. It is identical for both protocols so it cannot bias the comparison.
- **The report states what it does not measure.** Single-sample, one machine; per the Sprint 6 report's own conclusion, a few percent either way is noise.
- **Comment style:** short, plain full sentences ending in a punctuation mark. No emojis.

---

### Task 1: The harness script

**Files:**
- Create: `scripts/rmp-vs-resp.sh`

**Interfaces:**
- Consumes: `target/release/rocket-mem`, `target/release/rocket-mem-bench`.
- Produces: a plain-text sweep summary on stdout, suitable for pasting into the report.

- [ ] **Step 1: Write the script**

Create `scripts/rmp-vs-resp.sh`:

```bash
#!/usr/bin/env bash
# Sweeps in-flight depth for RESP pipelining against RMP multiplexing, both against ONE
# rocket-mem process so the two protocols share an engine, a config, and an AOF. Writes a
# plain-text summary to stdout; the committed report in docs/benchmarks/ is assembled from it.
set -euo pipefail

if ! command -v redis-cli >/dev/null 2>&1; then
  echo "error: 'redis-cli' is not on PATH -- it is used only as a liveness check" >&2
  echo "       (Debian/Ubuntu: apt install redis-tools)" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RESP_PORT=7779
RMP_PORT=7780
METRICS_PORT=9179

OPS="${OPS:-200000}"
WARMUP="${WARMUP:-10000}"
KEYS="${KEYS:-1000}"
VALUE_LEN="${VALUE_LEN:-64}"
DEPTHS="${DEPTHS:-1,2,4,8,16,32,64,128,256}"

echo "Building rocket-mem and the bench tool in release mode..." >&2
cargo build --release --workspace --manifest-path "$ROOT/Cargo.toml" >&2

WORK="$(mktemp -d)"
SERVER_PID=""
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# One process, both listeners. AOF on with an everysec fsync, matching scripts/benchmark.sh
# and real deployment. It applies identically to both protocols, so it cannot bias the result.
ROCKET_MEM_ADDR="127.0.0.1:$RESP_PORT" \
ROCKET_MEM_RMP_ADDR="127.0.0.1:$RMP_PORT" \
ROCKET_MEM_AOF_PATH="$WORK/rocket.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/rocket.snapshot" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:$METRICS_PORT" \
  "$ROOT/target/release/rocket-mem" >"$WORK/rocket.log" 2>&1 &
SERVER_PID=$!

# Wait for the RESP port rather than sleeping a fixed amount, so a slow machine does not
# produce a spurious connection failure.
for _ in $(seq 1 50); do
  if redis-cli -p "$RESP_PORT" ping >/dev/null 2>&1; then break; fi
  sleep 0.2
done
redis-cli -p "$RESP_PORT" ping >/dev/null

echo "rocket-mem:    $(redis-cli -p "$RESP_PORT" info server | grep -i '^redis_version' | tr -d '\r')"
echo "host:          $(uname -srm)"
echo "cores:         $(nproc 2>/dev/null || echo unknown)"
echo "date:          $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "ops/cell:      $OPS (warmup $WARMUP)"
echo "keyspace:      $KEYS keys x ${VALUE_LEN}B"
echo "depths:        $DEPTHS"
echo

"$ROOT/target/release/rocket-mem-bench" \
  --resp-addr "127.0.0.1:$RESP_PORT" \
  --rmp-addr "127.0.0.1:$RMP_PORT" \
  --ops "$OPS" \
  --warmup "$WARMUP" \
  --keys "$KEYS" \
  --value-len "$VALUE_LEN" \
  --depths "$DEPTHS"

echo
echo "--- /metrics sample after the run ---"
# rocket_mem_keys should equal the seeded key count. If it is 1, the keyspace collapsed and
# every request was contending on one shard, which invalidates the whole sweep.
curl -s "http://127.0.0.1:$METRICS_PORT/metrics" \
  | grep -E '^rocket_mem_(commands_total|keys|memory_used_bytes)' || true
```

- [ ] **Step 2: Make it executable and check it matches the existing script's conventions**

```bash
chmod +x scripts/rmp-vs-resp.sh
head -5 scripts/benchmark.sh
```
Expected: both start with `#!/usr/bin/env bash` and `set -euo pipefail`.

- [ ] **Step 3: Run a fast smoke pass**

```bash
OPS=2000 WARMUP=100 KEYS=100 DEPTHS=1,8 ./scripts/rmp-vs-resp.sh
```
Expected: provenance lines, a header, 12 rows, then a `/metrics` block showing `rocket_mem_keys 100`.

If `rocket_mem_keys` reads `1`, seeding did not work and the sweep is invalid — stop and fix plan 04's `seed` before continuing.

- [ ] **Step 4: Verify the trap cleans up**

```bash
ps aux | grep -c '[r]ocket-mem'
ls /tmp | grep -c tmp || true
```
Expected: no `rocket-mem` process survives the script.

- [ ] **Step 5: Commit**

```bash
git add scripts/rmp-vs-resp.sh
git commit -m "$(cat <<'EOF'
feat(bench): add the RMP-vs-RESP harness script

Starts one rocket-mem with both listeners and runs the depth sweep
against it. One process is the point: both protocols then share an
engine, a config, and an AOF, so nothing has to be matched by hand the
way the two-server scripts/benchmark.sh does.

Waits for the RESP port to answer instead of sleeping a fixed interval,
so a slow machine does not fail with a spurious connection error.

Samples /metrics at the end specifically to confirm rocket_mem_keys
matches the seeded count. If it reads 1, the keyspace collapsed and every
request was contending on a single shard, which would invalidate the
whole sweep -- the failure mode the Sprint 6 run hit without noticing.
EOF
)"
```

---

### Task 2: Run the sweep and write the report

**Files:**
- Create: `docs/benchmarks/2026-09-01-rmp-vs-resp.md`

**Interfaces:**
- Consumes: the output of `scripts/rmp-vs-resp.sh`.
- Produces: the committed report.

- [ ] **Step 1: Run the full sweep and capture the output**

```bash
./scripts/rmp-vs-resp.sh 2>/dev/null | tee /tmp/rmp-vs-resp-run.txt
```
Expected: 54 rows. This takes a while — 54 cells × 210,000 operations. Do not run anything else heavy on the machine while it runs.

- [ ] **Step 2: Check the depth-1 sanity gate before reading anything else**

Compare the three depth-1 rows per command. `RMP-batch` and `RMP-window` must agree within a few percent, since at depth 1 there is no batch and no window.

If they diverge materially, **stop**: that is a harness bug, not a finding, and every other row is suspect. Re-check plan 03's window-priming logic.

- [ ] **Step 3: Write the report**

Create `docs/benchmarks/2026-09-01-rmp-vs-resp.md` with these sections, filled from the captured run:

1. **Provenance** — the version/host/cores/date/ops/keyspace/depths block, verbatim from the script.
2. **Setup** — one server, both listeners, AOF `everysec`, one connection, 1,000 keys × 64B. Link the spec.
3. **Results** — the full 54-row table: driver, command, depth, ops/sec, p50, p99, latency unit. Keep the latency unit column; do not merge round and per-op latencies.
4. **Did the predictions hold?** — answer each of the spec's two predictions explicitly:
   - *At depth 1, RMP is slightly slower than RESP.* Quote the numbers. If RMP won at depth 1, say so plainly — the spec says that falsifies the model.
   - *RMP pulls ahead as depth rises.* Give the crossover depth, or state that there is none.
5. **The three comparisons**, each with its own subsection and its question restated:
   - RESP-batch vs RMP-batch — does the server's execution model help?
   - RMP-batch vs RMP-window — what does the fire-N-wait-all barrier cost?
   - RESP-batch vs RMP-window — each protocol at its best. **Label this one as not client-pattern-controlled**, since it mixes two variables on purpose.
6. **What this does not measure** — TLS, auth/ACL, cluster routing, multi-connection scaling, other command families, other payload sizes, RESP3, replication lag. Note explicitly that connections are pinned at 1, which maximises the visible RMP advantage, so these numbers do not predict the many-connection deployment case.
7. **Noise** — single sample, one machine. State the same caveat the Sprint 6 report reached: a few percent either way is not a real difference.

Report what the run actually produced. If RMP loses across the board, that is the finding — write it plainly, the way `2026-08-30-redis-benchmark.md`'s "Where we are faster, if anywhere: None." section does.

- [ ] **Step 4: Cross-check the report against the raw output**

Re-read every number in the table against `/tmp/rmp-vs-resp-run.txt`. A transcription error in a benchmark report is indistinguishable from a fabricated result.

- [ ] **Step 5: Commit**

```bash
git add docs/benchmarks/2026-09-01-rmp-vs-resp.md
git commit -m "$(cat <<'EOF'
docs(benchmarks): add the RMP-vs-RESP depth sweep results

Reports 54 cells: three drivers across GET and SET at in-flight depths
1 through 256, all on one connection against one server process.

Answers the spec's two predictions explicitly and keeps the three
comparisons separate, since only two of them hold the client's issuing
pattern constant. The RESP-batch vs RMP-window pairing mixes two
variables deliberately and is labelled as such.

Round and per-op latencies stay in separate columns. At depth N a round
covers N operations' work, so merging them would make the window driver
look N times better for a purely definitional reason.
EOF
)"
```

---

## Definition of Done

- [ ] `scripts/rmp-vs-resp.sh` is executable, committed, and leaves no stray processes or temp directories.
- [ ] The `/metrics` sample confirms `rocket_mem_keys` equals the seeded key count, not 1.
- [ ] The depth-1 sanity gate passed before any results were interpreted.
- [ ] `docs/benchmarks/2026-09-01-rmp-vs-resp.md` contains all seven sections, with every number cross-checked against the raw run.
- [ ] Two commits, one per task.
