# Pub/Sub Plan 07: Integration Tests, Benchmarks & Docs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close out the pub/sub series — an end-to-end leader/follower delivery test beyond
Plan 06's unit-level `sync_once` coverage, the required before/after `scripts/benchmark.sh`
regression gate plus a first-time `PUBLISH` throughput reference number, and documentation.

**Architecture:** One new integration test in `crates/server/tests/replication.rs`, built on that
file's existing `spawn_node()` helper (real `TcpListener` + `rocket_mem::serve` + a real
`start_replicating` call — no mock streams, unlike Plan 06's `sync_once`-level test). A new
`scripts/benchmark-pubsub.sh`, modeled on `scripts/benchmark.sh`'s existing head-to-head
structure. Doc updates to `README.md` and `docs/command-compatibility.md`, following the exact
precedent the transactions series set in
[`docs/benchmarks/2026-09-11-post-transactions-final.md`](../../../benchmarks/2026-09-11-post-transactions-final.md).

**Tech Stack:** Rust (`tokio::net::TcpListener`, `tokio_util::codec::Framed`), bash,
`redis-benchmark`.

**Spec:** [../../specs/2026-09-11-pubsub-spec.md](../../specs/2026-09-11-pubsub-spec.md) — see its
"Testing strategy" and "Performance" sections.

**Global Constraints:** see
[01-frame-push-variant.md](01-frame-push-variant.md)'s `## Global Constraints` section.

---

### Task 1: End-to-end leader/follower pub/sub delivery test

**Files:**
- Modify: `crates/server/tests/replication.rs` (new test, using the existing `spawn_node()`
  helper at line 107 and the `RespCodec`/`Framed` pattern `crates/server/src/connection.rs`'s own
  tests already use for raw RESP connections)

**Interfaces:**
- Consumes: `spawn_node() -> (TempDir, Arc<Engine>, Arc<AofWriter>, Arc<ReplicationHandle>,
  String)` (existing, line 107); `ReplicationHandle::start_replicating(&self, host_port: String)`
  (existing).

- [ ] **Step 1: Write the failing test**

Add to `crates/server/tests/replication.rs`, near the existing
`one_leader_two_followers_propagates_writes_within_a_bounded_time_window` test:

```rust
#[tokio::test]
async fn a_followers_subscriber_receives_a_message_published_on_the_leader() {
    use futures_util::{SinkExt, StreamExt};
    use protocol::Frame;
    use protocol::codec::RespCodec;
    use tokio_util::codec::Framed;

    let (_leader_dir, _leader_engine, _leader_aof, _leader_replication, leader_addr) =
        spawn_node().await;
    let (_f_dir, _f_engine, _f_aof, f_replication, f_addr) = spawn_node().await;

    f_replication.start_replicating(leader_addr.clone());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Subscribe directly on the follower.
    let mut subscriber = Framed::new(
        tokio::net::TcpStream::connect(&f_addr).await.unwrap(),
        RespCodec::default(),
    );
    subscriber
        .send(Frame::Array(vec![
            Frame::Bulk(bytes::Bytes::from_static(b"SUBSCRIBE")),
            Frame::Bulk(bytes::Bytes::from_static(b"news")),
        ]))
        .await
        .unwrap();
    subscriber.next().await.unwrap().unwrap(); // the subscribe confirmation

    // Publish on the leader.
    let mut publisher = Framed::new(
        tokio::net::TcpStream::connect(&leader_addr).await.unwrap(),
        RespCodec::default(),
    );
    publisher
        .send(Frame::Array(vec![
            Frame::Bulk(bytes::Bytes::from_static(b"PUBLISH")),
            Frame::Bulk(bytes::Bytes::from_static(b"news")),
            Frame::Bulk(bytes::Bytes::from_static(b"hello")),
        ]))
        .await
        .unwrap();
    assert_eq!(
        publisher.next().await.unwrap().unwrap(),
        Frame::Integer(0), // the leader itself has no local subscriber on "news"
    );

    let delivered = tokio::time::timeout(std::time::Duration::from_secs(5), subscriber.next())
        .await
        .expect("timed out waiting for the replicated message")
        .unwrap()
        .unwrap();
    assert_eq!(
        delivered,
        Frame::Push(vec![
            Frame::Bulk(bytes::Bytes::from_static(b"message")),
            Frame::Bulk(bytes::Bytes::from_static(b"news")),
            Frame::Bulk(bytes::Bytes::from_static(b"hello")),
        ])
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --test replication a_followers_subscriber_receives_a_message_published_on_the_leader`
Expected: FAIL — before Plan 06, `sync_once` never delivered a replicated `PUBLISH` to the
follower's local registry, so `subscriber.next()` would hang until the 5-second timeout fires and
the test fails with "timed out waiting for the replicated message." If Plan 06 already landed
correctly, this test should actually pass immediately — in that case, treat this step as
confirmation the earlier plan's unit-level coverage generalizes to a real end-to-end path, not as
a sign something is wrong; still run it once to observe the actual outcome before moving on,
rather than assuming.

- [ ] **Step 3: Fix anything this test's broader harness reveals**

Since this test exercises the whole chain (`connection.rs`'s command-parsing, `dispatch_and_log`'s
`intercept_for_pubsub`, `replication.rs`'s broadcast, `sync_once`'s interception, and
`connection.rs`'s `tokio::select!` delivery on the follower side) together for the first time,
treat any failure here as a real integration bug in one of Plans 01-06's work, not a flaw in this
test — re-read the specific stage that failed (which frame arrived, on which side) before
patching. If it fails for a reason not already covered by an earlier plan's unit tests, add a
regression test at the unit level in the relevant earlier plan's file before fixing it here, so
future changes to that stage don't reintroduce the same gap silently.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem --test replication a_followers_subscriber_receives_a_message_published_on_the_leader`
Expected: PASS.

- [ ] **Step 5: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/server/tests/replication.rs
git commit -m "$(cat <<'EOF'
Add end-to-end leader-to-follower pub/sub delivery test

Exercises the whole chain for the first time together: a real
SUBSCRIBE on a follower, a real PUBLISH on its leader, replicated
over a live TCP connection and delivered through the follower's
own tokio::select! read loop -- beyond Plan 06's sync_once-level
unit coverage.
EOF
)"
```

---

### Task 2: Benchmark verification

**Files:**
- Create: `scripts/benchmark-pubsub.sh`
- Create: `docs/benchmarks/2026-09-11-post-pubsub-final.md`

**Interfaces:**
- Consumes: `scripts/benchmark.sh` (existing, unmodified); the gate rows and baseline means from
  [`docs/benchmarks/2026-09-11-post-transactions-final.md`](../../../benchmarks/2026-09-11-post-transactions-final.md)
  (this series' "before" state, since it's the most recent prior benchmark capture on `main`).

- [ ] **Step 1: Run the before/after `scripts/benchmark.sh` gate**

```bash
git status # confirm a clean tree before benchmarking -- an uncommitted change would contaminate the "after" numbers
./scripts/benchmark.sh
```

Run three times (matching this project's established 3-run-mean methodology — see
[`docs/benchmarks/2026-09-11-pre-transactions-baseline.md`](../../../benchmarks/2026-09-11-pre-transactions-baseline.md)'s
own "Harness" section for the exact convention: matched durability, `rocket-mem-shard-{a,b,c}`
systemd services confirmed inactive first). Compute the mean for `SET, 3B, no pipeline` and
`GET, 3B, no pipeline` across the three runs. Compare against
[`docs/benchmarks/2026-09-11-post-transactions-final.md`](../../../benchmarks/2026-09-11-post-transactions-final.md)'s
own means for those two rows (83,796.29 and 89,512.08 respectively) — this series' gate is the
same `<=2%` regression threshold, against that file's numbers as the "before" baseline, since it
is the most recent capture on `main` prior to this series.

If the gate fails (a regression beyond 2% on either of those two rows), stop and investigate
before proceeding to Step 2 — do not write a benchmark report documenting a regression as if it
were acceptable. The most likely suspect, per the spec's "Performance" section, is the
`session.push_rx.lock()` + `Option::is_none()` check Plan 05 Task 2 added to every iteration of
the read loop, even for a connection that never subscribes.

- [ ] **Step 2: Write `scripts/benchmark-pubsub.sh`**

Unlike the transactions series' `scripts/benchmark-transactions.sh` (which needed a custom Python
RESP client because `redis-benchmark`'s `-t` flag has no multi-command-transaction mode),
`redis-benchmark` supports an arbitrary custom command as trailing arguments — `PUBLISH` needs no
special handling. Base this script directly on `scripts/benchmark.sh`'s existing structure (same
ports, same matched-durability server startup, same cleanup trap):

```bash
#!/usr/bin/env bash
# PUBLISH throughput: redis-benchmark supports an arbitrary trailing command directly, unlike
# MULTI/EXEC (which needed scripts/benchmark-transactions.sh's custom RESP client because
# redis-benchmark's -t flag has no multi-command-transaction mode). No subscriber is attached
# here -- this measures PUBLISH's own dispatch overhead (registry lookup, zero matches, the
# replication broadcast), not delivery latency to a subscriber, which the two-connection
# integration test in this same plan already covers functionally.
set -euo pipefail

for bin in redis-server redis-benchmark; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "error: '$bin' is not on PATH. Install a Redis distribution first" >&2
    exit 1
  fi
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REDIS_PORT=7797
ROCKET_PORT=7798
N=100000

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

redis-server --port "$REDIS_PORT" --save '' --appendonly yes --appendfsync everysec \
  --dir "$WORK" >"$WORK/redis.log" 2>&1 &
REDIS_PID=$!

ROCKET_MEM_ADDR="127.0.0.1:$ROCKET_PORT" \
ROCKET_MEM_AOF_PATH="$WORK/rocket.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/rocket.snapshot" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:9189" \
ROCKET_MEM_RMP_ADDR="127.0.0.1:9190" \
  "$ROOT/target/release/rocket-mem" --config "$WORK/unused.toml" >"$WORK/rocket.log" 2>&1 &
ROCKET_PID=$!

sleep 1
redis-cli -p "$REDIS_PORT" ping >/dev/null
redis-cli -p "$ROCKET_PORT" ping >/dev/null

echo "--- PUBLISH throughput ($N requests, no attached subscriber) ---"
echo -n "redis-server: "
redis-benchmark -p "$REDIS_PORT" -n "$N" -q PUBLISH news hello
echo -n "rocket-mem: "
redis-benchmark -p "$ROCKET_PORT" -n "$N" -q PUBLISH news hello
```

Mark it executable: `chmod +x scripts/benchmark-pubsub.sh`.

- [ ] **Step 3: Run it and record results**

```bash
./scripts/benchmark-pubsub.sh
```

- [ ] **Step 4: Write `docs/benchmarks/2026-09-11-post-pubsub-final.md`**

Follow the exact structure of
[`docs/benchmarks/2026-09-11-post-transactions-final.md`](../../../benchmarks/2026-09-11-post-transactions-final.md):
a `## rocket-mem requests/sec (unchanged commands)` table with the three runs' `SET, 3B, no
pipeline`/`GET, 3B, no pipeline` means and the `vs. baseline` percentage against that file's own
83,796.29/89,512.08 figures, a `## Gate verdict` section (PASS/FAIL, with the reasoning), and a
`## PUBLISH throughput` section with the `redis-server`/`rocket-mem` requests/sec this task's
Step 3 produced — recorded as a first-time reference number, not gated against anything.

- [ ] **Step 5: Commit**

```bash
git add scripts/benchmark-pubsub.sh docs/benchmarks/2026-09-11-post-pubsub-final.md
git commit -m "$(cat <<'EOF'
Verify no regression and add a PUBLISH throughput number

scripts/benchmark-pubsub.sh reuses redis-benchmark directly --
unlike MULTI/EXEC, PUBLISH needs no custom RESP client, since
redis-benchmark already supports an arbitrary trailing command.
EOF
)"
```

---

### Task 3: Documentation

**Files:**
- Modify: `README.md` (command coverage table)
- Modify: `docs/command-compatibility.md` (coverage table, "Known divergences", "Commands not
  implemented")

**Interfaces:** none — pure documentation.

- [ ] **Step 1: Update `README.md`**

Find the command coverage table (the same one the transactions series added a `Transactions` row
to — search `grep -n "| Transactions |" README.md` to locate it) and add, in the same style:

```markdown
| Pub/Sub | `SUBSCRIBE`, `UNSUBSCRIBE`, `PSUBSCRIBE`, `PUNSUBSCRIBE`, `PUBLISH`, `PUBSUB` (`CHANNELS`/`NUMSUB`/`NUMPAT`) — single-node delivery only, no cluster-wide fanout |
```

- [ ] **Step 2: Update `docs/command-compatibility.md`'s coverage table**

Add a row to the `## Command coverage` table (`docs/command-compatibility.md:14-26`), after the
existing `Transactions` row:

```markdown
| Pub/Sub | `SUBSCRIBE`, `UNSUBSCRIBE`, `PSUBSCRIBE`, `PUNSUBSCRIBE`, `PUBLISH`, `PUBSUB CHANNELS`/`NUMSUB`/`NUMPAT` |
```

- [ ] **Step 3: Update `docs/command-compatibility.md`'s "Known divergences" section**

Add, after the existing `MULTI`/`EXEC` and `WATCH`/`UNWATCH` bullets (around line 100-108):

```markdown
- **Pub/sub delivery is single-node only, not cluster-wide.** `PUBLISH` only reaches subscribers
  connected to the same node it was called on -- no cross-shard forwarding, no gossip. Real
  Redis's cluster-wide pub/sub relies on its gossip protocol, which this project doesn't
  implement. See [the pub/sub spec](superpowers/specs/2026-09-11-pubsub-spec.md)'s "Cluster
  scope" section.
- **No sharded pub/sub (`SPUBLISH`/`SSUBSCRIBE`).** Not applicable without cluster-wide fanout.
```

- [ ] **Step 4: Remove `Transactions`/pub/sub families from "Commands not implemented"**

`docs/command-compatibility.md`'s `## Commands not implemented` section (around line 110-128)
still lists `MULTI`, `EXEC`, `DISCARD`, `WATCH`/`UNWATCH` under "Transactions" (stale since the
transactions series landed — confirm via `grep -n "Transactions:" docs/command-compatibility.md`
whether this was already fixed; if so, skip this step) and `SUBSCRIBE`, `UNSUBSCRIBE`, `PUBLISH`,
`PSUBSCRIBE` under "Pub/sub". Update the "Pub/sub" bullet to only list what remains unimplemented:

```markdown
- **Sharded pub/sub:** `SPUBLISH`, `SSUBSCRIBE`, `SUNSUBSCRIBE` (Redis 7's cluster-aware pub/sub
  variant) -- not applicable without cluster-wide fanout (see "Known divergences" above).
```

If the `Transactions:` bullet is still present and stale (listing commands this project now
implements), fix it the same way while you're here — remove `MULTI`/`EXEC`/`DISCARD` from it,
leaving only `WATCH`/`UNWATCH` as still-unimplemented, matching what the transactions series'
own Plan 04 should have already done; if it already reads correctly, leave it untouched.

- [ ] **Step 5: Run the full workspace CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

(No code changed in this task, but running the gates one final time confirms the series ends in a
clean, green state.)

- [ ] **Step 6: Commit**

```bash
git add README.md docs/command-compatibility.md
git commit -m "$(cat <<'EOF'
Document pub/sub support (Phase 6)

README and command-compatibility coverage tables, plus the
single-node-delivery and no-sharded-pub/sub divergences.
EOF
)"
```

## Next plan

None — this is the last plan in the `2026-09-11-pubsub-plans` series. The pub/sub feature (Phase
6 of the post-v1 roadmap) is complete once this plan's tasks are committed: `SUBSCRIBE`,
`UNSUBSCRIBE`, `PSUBSCRIBE`, `PUNSUBSCRIBE`, `PUBLISH`, and `PUBSUB CHANNELS`/`NUMSUB`/`NUMPAT`
are all implemented, tested at the unit and end-to-end level, benchmarked against the
`<=2%` regression gate, and documented.
