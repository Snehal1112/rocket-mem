# Post-Transactions Throughput — Final Result

**Date:** 2026-09-11
**Commit:** `68f0b70`
**Purpose:** the series-final measurement against
[the pre-transactions baseline](2026-09-11-pre-transactions-baseline.md), gating the spec's
requirement that the non-transaction hot path not regress.

## rocket-mem requests/sec (unchanged commands)

| Workload | Run 1 | Run 2 | Run 3 | Mean | vs. baseline |
|---|---|---|---|---|---|
| SET, 3B, no pipeline | 87,336.24 | 83,472.46 | 80,580.17 | 83,796.29 | +15.51% |
| GET, 3B, no pipeline | 95,419.85 | 87,719.30 | 85,397.09 | 89,512.08 | +18.06% |
| SET, 3B, pipeline=16 | 757,575.75 | 709,219.88 | 689,655.19 | 718,816.94 | (context only) |
| GET, 3B, pipeline=16 | 1,162,790.62 | 1,123,595.50 | 925,925.88 | 1,070,770.67 | (context only) |
| SET, 1KB, no pipeline | 78,802.20 | 77,279.75 | 81,037.28 | 79,039.74 | (context only) |
| GET, 1KB, no pipeline | 88,261.25 | 85,763.29 | 84,104.29 | 86,042.94 | (context only) |
| SET, 1KB, pipeline=16 | 523,560.22 | 469,483.56 | 431,034.50 | 474,692.76 | (context only) |
| GET, 1KB, pipeline=16 | 662,251.69 | 625,000.00 | 657,894.75 | 648,382.15 | (context only) |

`vs. baseline` = `(mean - baseline_mean) / baseline_mean * 100`, computed only for the two gated
rows against [the pre-transactions baseline](2026-09-11-pre-transactions-baseline.md)'s means
(72,545.96 and 75,816.30 respectively).

## Gate verdict

**PASS.** Both gated rows are *above* their baseline mean (+15.51% for `SET, 3B, no pipeline`,
+18.06% for `GET, 3B, no pipeline`), not below it — the opposite of a regression. This is well
outside the noise band either baseline capture's own run-to-run jitter would explain on its own
(the pre-series baseline's three runs alone swung ~12%-27% peak-to-peak on these same two rows;
see that file's Gate section), so the improvement itself isn't claimed as a real effect of this
series — it's simply evidence that the `session.in_transaction.load(Ordering::Relaxed)` fast-path
check added to every dispatched command (Plan 01) did not measurably cost anything, which is what
this gate exists to verify.

## MULTI/EXEC transaction throughput

From `scripts/benchmark-transactions.sh` (20,000 two-command transactions — `MULTI`, one `SET`,
`EXEC` — matched durability). `redis-benchmark`'s `-t` has no multi-command-transaction mode, and
`redis-cli --pipe`'s completion-detection handshake does not work against rocket-mem at all
(confirmed separately: it fails identically for plain, non-transactional `SET` piped the same
way — a real, pre-existing, unrelated gap, not a transactions bug). The script instead uses a
small stdlib-only Python client that sends the whole stream over a raw socket and counts RESP
reply frames directly.

- redis-server: 447,449.70 transactions/sec (20,000 transactions in 0.044698s)
- rocket-mem: 142,922.02 transactions/sec (20,000 transactions in 0.139936s)

Recorded as a first-time reference number, not gated against anything — there is no prior
rocket-mem `MULTI`/`EXEC` throughput to compare it to. rocket-mem trails real Redis by roughly
3.1x here, consistent with this project's own already-documented, unrelated general throughput
gap against Redis (shared atomic clock, no `TCP_NODELAY`, dispatcher overhead — specced
separately, unresolved as of 2026-09-08) rather than anything specific to transaction handling; a
future series that touches this path again should treat this file as its own "before" baseline.

## Known limitation surfaced during this benchmark (unrelated to transactions)

`redis-cli --pipe` cannot be used against rocket-mem at all today — it fails with a protocol
decode error partway through its own completion-detection handshake, reproduced with plain `SET`
commands and no `MULTI`/`EXEC` involved. Not investigated further here (out of scope for this
spec); worth a follow-up issue if bulk-loading via `redis-cli --pipe` is a workflow this project
wants to support.
