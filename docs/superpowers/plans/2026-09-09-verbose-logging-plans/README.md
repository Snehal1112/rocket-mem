# Verbose Activity Logging — Implementation Plans

Twenty-one TDD implementation plans delivering [the verbose logging spec](../../specs/2026-09-09-verbose-logging-design.md).

**Execute them in order.** Each plan ends with a `## Next plan` section naming its successor, so a worker can follow the chain without consulting this index. Every plan holds **at most 3 tasks**, and every task ends with an independently testable deliverable and its own commit.

[Plan 01](01-baseline-and-dependencies.md) carries the **Global Constraints** section that all twenty-one share — fmt/clippy/test gates, the ≤2% throughput ceiling, lazy field sigils, no `Bytes` via `Debug`, no new hot-path atomics, and redaction confined to `crates/server`. Read it before starting any plan, not just the first.

## Order

| # | Plan | Tasks | Delivers |
|---|---|---|---|
| 01 | [Baseline & dependencies](01-baseline-and-dependencies.md) | 3 | Pre-instrumentation benchmark baseline; `tracing` into `engine` + `protocol`; `CLAUDE.md` note |
| 02 | [`logging` module & `fmt_value`](02-logging-module-fmt-value.md) | 2 | `crates/server/src/logging.rs`; capped, control-byte-escaped value rendering |
| 03 | [Credential redaction](03-logging-module-redaction.md) | 2 | `is_sensitive`, `redact_args` — the credential guard |
| 04 | [`log_value_max_bytes` config](04-config-log-value-max-bytes.md) | 2 | The `cap` field through all four figment layers, plus its docs rows |
| 05 | [RESP connection span](05-resp-connection-span.md) | 2 | `conn` span on `connection.rs`; connection-closed event |
| 06 | [RMP connection span](06-rmp-connection-span.md) | 2 | The same for `rmp_connection.rs` |
| 07 | [Command span](07-command-span.md) | 3 | `cmd` span in `dispatch_and_log`; per-command `debug!` line; benchmark gate |
| 08 | [Command trace args](08-command-trace-args.md) | 3 | Redacted `trace!` argument rendering; `cap` threading; benchmark gate |
| 09 | [Level-separation test](09-level-separation-test.md) | 3 | `tests/logging.rs` capture harness; proof `info` stays quiet and `debug` does not |
| 10 | [Dispatch error events](10-dispatch-error-events.md) | 3 | Unknown-command, WRONGTYPE, and arity `debug!` events |
| 11 | [ACL events](11-acl-events.md) | 3 | Auth success/failure, NOPERM, `HELLO` upgrade, `SETUSER`/`DELUSER` |
| 12 | [Engine shard routing](12-engine-shard-routing-trace.md) | 3 | Shard routing and byte-delta traces; benchmark gate |
| 13 | [Engine expiry & eviction](13-engine-expiry-and-eviction.md) | 3 | TTL expiry, active-expire cycle counts, eviction events |
| 14 | [Protocol codec events](14-protocol-codec-events.md) | 3 | Frame decode, split-read reassembly, protocol errors |
| 15 | [AOF events](15-aof-events.md) | 3 | Append/fsync, rewrite start/finish, recovery replay summary; benchmark gate |
| 16 | [Snapshot events](16-snapshot-events.md) | 2 | Save and load milestones |
| 17 | [Replication events](17-replication-events.md) | 3 | `repl` span; PSYNC handshake; replica register/prune; offset progress |
| 18 | [Cluster events](18-cluster-events.md) | 2 | Topology loaded; MOVED redirects |
| 19 | [Slowlog & metrics events](19-slowlog-and-metrics-events.md) | 2 | Slowlog entries; metrics scrapes |
| 20 | [Startup & listener events](20-startup-and-listener-events.md) | 3 | Resolved config summary; per-listener bind events |
| 22 | [Log field consistency sweep](22-log-field-consistency-sweep.md) | 3 | Key-spec-aware `key`; `key_field` relocation; one `protocol` rendering; duplicated span fields |
| 23 | [Deferred cleanups & decisions](23-deferred-cleanups-and-decisions.md) | 3 | Silent recovery error paths; two small cleanups; record decisions in the spec |
| 21 | [Docs & final verification](21-docs-and-final-verification.md) | 3 | Logging documentation; full sweep; spec-coverage audit |

**Execution order is 01 → 20, then 22 → 23 → 21.** Plans 22 and 23 were written after plan
20's review, once the items each earlier review had deliberately deferred outgrew what plan
21's three tasks could absorb under this series' three-task-per-plan cap. They run before
plan 21 so that its verification sweep and spec-coverage audit see the finished state. The
table is in execution order; the file numbers are not contiguous, and that is intentional.

## Two test harnesses, deliberately

Rust compiles each file under `tests/` as its own crate, so a helper there cannot be shared with a `#[cfg(test)] mod tests` inside `src/`. The series therefore has two, and they cannot be merged:

- **`capture_at`** in `crates/server/tests/logging.rs` (plan 09) — for integration tests.
- **`logging::test_support::CapturedLogs`** in `crates/server/src/logging.rs` (introduced by plan 17, imported by 18 and 19) — for unit tests.

## Deliberately deferred

Named in the spec's "Out of scope" section and **not** covered by any plan here: JSON output and log shipping, log file rotation, runtime-reloadable levels (`CONFIG SET loglevel`), OpenTelemetry export, and sampling. The reload layer in particular is worth its own spec once plan 21's cumulative benchmark numbers show what this instrumentation actually costs.
