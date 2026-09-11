# Post-Pub/Sub Throughput — Final Result

**Date:** 2026-09-11
**Commit:** `c4ac4a4`
**Purpose:** the series-final measurement for the pub/sub plan series, gating the spec's
requirement that the non-pub/sub hot path not regress, against
[the post-transactions capture](2026-09-11-post-transactions-final.md) — the most recent
benchmark capture on `main` prior to this series.

**Harness:** `scripts/benchmark.sh`, three consecutive runs, matched durability
(`--appendonly yes --appendfsync everysec` on both servers).

**Contamination check:** `rocket-mem-shard-{a,b,c}` systemd services confirmed inactive, and no
stray `rocket-mem`/`redis-server` processes were bound to the benchmark ports (7777/7778) before
this capture — the system's own `redis-server` instance runs on port 6379 and is unrelated.

## rocket-mem requests/sec (unchanged commands)

| Workload | Run 1 | Run 2 | Run 3 | Mean | vs. baseline |
|---|---|---|---|---|---|
| SET, 3B, no pipeline | 89,285.71 | 85,397.09 | 82,576.38 | 85,753.06 | +2.34% |
| GET, 3B, no pipeline | 97,943.19 | 95,057.03 | 94,250.71 | 95,750.31 | +6.97% |

`vs. baseline` = `(mean - baseline_mean) / baseline_mean * 100`, computed only for the two gated
rows against [the post-transactions capture](2026-09-11-post-transactions-final.md)'s own means
(83,796.29 and 89,512.08 respectively) — the most recent prior capture on `main`.

## Gate verdict

**PASS.** Both gated rows are *above* their baseline mean (+2.34% for `SET, 3B, no pipeline`,
+6.97% for `GET, 3B, no pipeline`), not below it — the opposite of a regression, so the `<=2%`
regression gate is satisfied regardless of how the percentages themselves are read. This is the
result this gate exists to produce: the `session.push_rx.lock()` + `Option::is_none()` check
Plan 05 Task 2 added to every iteration of the RESP read loop — even for a connection that never
subscribes — did not measurably cost anything on this hot path. As with the transactions series'
own gate capture, these two rows are known to be noisy run-to-run (the pre-transactions baseline's
three runs alone swung ~12%-27% peak-to-peak on these same rows), so the improvement itself is not
claimed as a real effect of this series either — it is simply evidence of no regression.

## PUBLISH throughput

From `scripts/benchmark-pubsub.sh` (100,000 `PUBLISH news hello` requests, matched durability, no
subscriber attached — this measures `PUBLISH`'s own dispatch overhead, i.e. registry lookup, zero
subscriber matches, and the replication broadcast, not delivery latency to a subscriber, which the
two-connection end-to-end integration test elsewhere in this plan series already covers
functionally):

- redis-server: 74,906.37 requests/sec
- rocket-mem: 92,421.44 requests/sec

Recorded as a first-time reference number, not gated against anything — there is no prior
rocket-mem `PUBLISH` throughput to compare it to. rocket-mem outpaces redis-server here; given this
project's own already-documented general throughput gap against Redis on other commands (shared
atomic clock, no `TCP_NODELAY`, dispatcher overhead — specced separately, unresolved as of
2026-09-08), this is more likely explained by `PUBLISH` being a cheap no-match dispatch on both
sides plus this run's own machine jitter (visible in the redis-server ramp-up within this same run)
than by a genuine rocket-mem advantage; a future series that touches this path again should treat
this file as its own "before" baseline rather than assume the gap holds.
