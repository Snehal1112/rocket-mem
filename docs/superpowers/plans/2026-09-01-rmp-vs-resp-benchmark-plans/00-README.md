# RMP vs RESP Benchmark — Plan Index

Five plans, each with **at most three tasks**, executed in order. Every plan ends with a green
`cargo fmt`/`cargo clippy`/`cargo test` and at least one commit.

**Spec:** [`../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md`](../../specs/2026-09-01-rmp-vs-resp-benchmark-design.md)

| # | Plan | Tasks | Deliverable |
|---|---|---|---|
| 01 | [Crate foundation](01-crate-foundation.md) | 3 | `crates/bench` builds; `BenchError`, `Workload`, `Samples` |
| 02 | [RESP batch driver](02-resp-batch-driver.md) | 3 | `RespDriver` pipelines and verifies replies |
| 03 | [RMP drivers](03-rmp-drivers.md) | 3 | `Driver` trait, `RmpDriver`, `RmpWindowDriver` |
| 04 | [Sweep runner and CLI](04-sweep-runner-and-cli.md) | 3 | `rocket-mem-bench` runs all 54 cells |
| 05 | [Harness and report](05-harness-and-report.md) | 2 | `scripts/rmp-vs-resp.sh` + committed report |

## Dependency order

```
01 ──> 02 ──> 03 ──> 04 ──> 05
```

Strictly sequential. Task 1 of plan 03 refactors `RespDriver` from plan 02 to implement the
`Driver` trait, and plan 04's runner is generic over that trait, so nothing here parallelizes
cleanly.

## Two divergences from the spec, decided during planning

1. **`crates/bench` is a lib + bin, not a bin only.** CI runs
   `cargo clippy --workspace --all-targets -- -D warnings`, and `dead_code` is included in
   `warnings`. In a binary-only crate, every type would be dead code until the task that wires it
   into `main.rs`, forcing contrived `main.rs` churn in each early task. `pub` items in a **lib**
   target are never dead code, so each task can land a tested, complete unit without touching
   `main.rs`.

2. **The `Driver` trait is `pub` with RPITIT, not private with `async fn`.** Consequence of (1): a
   `pub(crate)` trait in a lib target is dead code. Making it `pub` would trigger the
   `async_fn_in_trait` lint the spec warns about, so it is written desugared as
   `fn run(...) -> impl Future<Output = ...> + Send` — the alternative the spec's own
   "three drivers, one trait" section already names. Same semantics, no lint.
