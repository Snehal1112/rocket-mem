# Pre-Transactions Throughput Baseline

**Date:** 2026-09-11
**Commit:** `f790218`
**Purpose:** the reference point for the <=2% regression gate this series' Plan 04 defines, per
[the MULTI/EXEC transactions spec](../superpowers/specs/2026-09-10-multi-exec-transactions-spec.md)'s
"Performance" section. Captured from the commit immediately before this series' first commit
(`ad8dfb4`), via a temporary nested `git worktree` — never on the branch this plan itself
executes on.

**Harness:** `scripts/benchmark.sh`, three consecutive runs, matched durability
(`--appendonly yes --appendfsync everysec` on both servers).

**Contamination check:** `rocket-mem-shard-{a,b,c}` systemd services were confirmed inactive
before this capture — no contaminating local processes.

## rocket-mem requests/sec

| Workload | Run 1 | Run 2 | Run 3 | Mean |
|---|---|---|---|---|
| SET, 3B, no pipeline | 69,979.01 | 64,808.82 | 82,850.04 | 72,545.96 |
| GET, 3B, no pipeline | 53,821.31 | 88,809.95 | 84,817.64 | 75,816.30 |
| SET, 3B, pipeline=16 | 653,594.81 | 740,740.69 | 621,118.00 | 671,817.83 |
| GET, 3B, pipeline=16 | 847,457.62 | 1,111,111.12 | 1,086,956.50 | 1,015,175.08 |
| SET, 1KB, no pipeline | 83,752.09 | 62,853.55 | 73,529.41 | 73,378.35 |
| GET, 1KB, no pipeline | 97,181.73 | 72,939.46 | 87,719.30 | 85,946.83 |
| SET, 1KB, pipeline=16 | 411,522.62 | 315,457.41 | 454,545.47 | 393,841.83 |
| GET, 1KB, pipeline=16 | 787,401.56 | 719,424.44 | 602,409.69 | 703,078.56 |

## Gate

**The <=2% gate applies ONLY to `SET, 3B, no pipeline` and `GET, 3B, no pipeline`** — the two
rows this repo's own prior benchmark series (`2026-09-09-pre-logging-baseline.md`) found have the
tightest run-to-run jitter (0.8%-7.6%, versus 8.8%-22.2% on every other row). This capture's own
three runs show the same pattern: the 3B-no-pipeline rows swing roughly 12%-27% peak-to-peak here
(a noisier machine/run than the September 9 capture, but still visibly tighter than the
pipeline=16 rows' wider swings below), while the pipeline=16 and 1KB rows swing far more —
`SET, 1KB, no pipeline` alone ranges from 62,853.55 to 83,752.09, a ~33% spread. Task 2's
post-series numbers for `SET, 3B, no pipeline` and `GET, 3B, no pipeline`, measured the same way,
must be within 2% of these means (72,545.96 and 75,816.30 respectively). The other six rows are
recorded as context only.
