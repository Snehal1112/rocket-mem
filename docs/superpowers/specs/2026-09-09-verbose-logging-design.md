# Verbose Activity Logging — Spec & Design

**Date:** 2026-09-09
**Status:** Approved
**Scope:** all five workspace crates — `server`, `engine`, `protocol` (new `tracing` dependencies), plus `common` and `rmp-client` left untouched.
**Goal:** log every meaningful activity rocket-mem performs, at levels an operator can dial from "quiet production" to "full firehose", so a live server can be debugged and audited without packet captures or a rebuild.

## Problem

The previous round ([2026-09-07 structured logging](2026-09-07-structured-logging-design.md)) wired `tracing` into `crates/server` and closed the "silent connection close" gap. It deliberately stopped short, and its "Out of scope" section names exactly what this spec picks up:

> Per-command / hot-path debug tracing (e.g. `tracing::debug!` on every dispatched command). Not requested, and this is a performance-sensitive path across 16 shards under load — worth a dedicated design (sampling, `#[instrument(skip(...))]` cost) if wanted later.

Today's coverage is 22 call sites across 5 files, all in `crates/server`:

| Level | Count | Where |
|---|---|---|
| `error!` | 6 | AOF fsync/encode/append, replication apply |
| `warn!` | 13 | TLS handshake, decode errors, replication reconnect, snapshot fallback |
| `info!` | 3 | startup, connection accepted (RESP and RMP) |
| `debug!` / `trace!` | 0 | — |
| `#[instrument]` spans | 0 | — |

`dispatcher.rs` has no logging beyond two AOF error paths — in a 10,158-line file through which every command flows. `acl.rs`, `cluster.rs`, `slowlog.rs`, and `tls.rs` have none at all, and the `engine`, `protocol`, and `rmp-client` crates have none.

The practical consequence: at `debug` or `trace`, rocket-mem today emits nothing extra. An operator who sets `RUST_LOG=trace` on a misbehaving node learns nothing they did not already know at `info`.

## Decision: `tracing` extends to `engine` and `protocol`

```toml
# crates/engine/Cargo.toml, crates/protocol/Cargo.toml -- new dependency
tracing.workspace = true
```

The previous spec kept these crates dependency-free on the reasoning that they should stay protocol-agnostic and minimal. That reasoning does not actually apply to `tracing`: it is a facade crate with no runtime of its own, and with no subscriber installed its macros compile to a level check that is never true. Instrumenting the engine leaves it just as ignorant of RESP and RMP as before.

`CLAUDE.md`'s "Workspace layout" section is updated to record logging as the one cross-cutting dependency the engine and protocol crates are permitted, so a future reader does not treat this as an erosion of the boundary.

`common` and `rmp-client` stay untouched — neither has an activity worth logging.

## Decision: runtime filtering, gated on a benchmark

All call sites ship in the normal release build. Disabled ones cost a relaxed atomic load and a branch; no Cargo feature, no `release_max_level_*`. This is what makes `RUST_LOG=trace` work against a stock binary on a running server, which is the whole point of the exercise — a feature gate would require a rebuild and restart, destroying the state being investigated.

The cost is not assumed to be free. [`docs/benchmarks/2026-08-30-redis-benchmark.md`](../../benchmarks/2026-08-30-redis-benchmark.md), [`docs/benchmarks/2026-09-07-flamegraph-notes.md`](../../benchmarks/2026-09-07-flamegraph-notes.md), and [the throughput-parity spec](2026-09-07-throughput-parity-design.md) already record a `redis-benchmark` throughput gap in which dispatcher overhead is a named contributor, so each implementation plan carries a benchmark gate:

- A `scripts/benchmark.sh` baseline is captured **before** any instrumentation lands and recorded in plan 01.
- Each subsequent plan re-runs `scripts/benchmark.sh` at the default `info` level.
- **Acceptance: ≤2% throughput regression against the baseline.** A plan exceeding it is not done.
- `debug` and `trace` throughput is measured and documented, not gated. A large regression at those levels is expected and correct.

`scripts/benchmark.sh` is used rather than a hand-rolled `redis-benchmark` invocation because rocket-mem cannot disable its AOF; a manual run against a stock Redis compares mismatched durability settings and flatters Redis.

### Hot-path guardrails

- No `format!` outside a log macro's argument list.
- All fields passed with the `%` (Display) or `?` (Debug) sigil so formatting is lazy and never runs when the level is disabled.
- `Bytes` is never logged via `Debug` on the hot path — it renders byte-by-byte.
- No new atomic counters. The `cmd` span reuses the `client_id: u64` already threaded through `dispatch_and_log`.

## Decision: level taxonomy

The rule that keeps levels predictable: **`info` = milestones, `debug` = what happened, `trace` = what the bytes were.**

| Level | Meaning | Volume |
|---|---|---|
| `error` | Durability or correctness failure needing operator action: AOF append/fsync/encode failure, snapshot write failure, replication apply failure | rare |
| `warn` | Recovered, retried, or client-caused anomaly: TLS handshake failure/timeout, decode error, replication reconnect, snapshot fallback, ACL denial, eviction under memory pressure, slowlog hit | occasional |
| `info` | Lifecycle milestones, safe to leave on in production: startup, listener bound, connection accept/close, AOF rewrite, snapshot save/load, replica register/prune, `REPLICAOF` transitions | per-connection |
| `debug` | Per-command activity and subsystem sub-steps: one line per dispatched command, AOF offsets, active-expire cycle results, cluster routing decisions, PSYNC handshake steps | per-request |
| `trace` | Argument and value contents (capped), engine shard routing and byte deltas, codec frame decode and split-read reassembly, replication stream offsets | per-operation |

The production default remains `log_level = "info"`.

## Decision: three spans, not per-function instrumentation

Approach considered and rejected: `#[instrument]` on every public function across all crates. It is mechanically complete, but it opens a span per `Engine` facade call on a path already identified as a throughput bottleneck, and every annotation needs `skip(...)`/`fields(...)` anyway to stop `Bytes` arguments being formatted. It is the option most likely to fail the 2% gate.

Instead, exactly three spans carry the correlation, and everything nested inherits their fields for free:

| Span | Site | Fields |
|---|---|---|
| `conn` | `#[instrument(skip_all, ...)]` on `connection.rs`'s `handle_connection` and `rmp_connection.rs`'s `handle_connection` | `conn_id`, `peer`, `protocol`, `tls` |
| `cmd` | opened inside `dispatcher.rs`'s `dispatch_and_log` | `cmd`, `key`, `argc` |
| `repl` | replication client loop, and `connection.rs`'s `serve_replica` | `host_port` |

`dispatch_and_log` is the right home for the `cmd` span because it is already the single choke point every command passes through, and it already computes `name`, `first_key`, `arg_count`, and `elapsed` for the metrics and slowlog paths. The span's fields are values that exist at that point regardless — the span adds correlation, not computation.

### Field vocabulary

One fixed set of field names across every crate and subsystem, so a single `grep` follows an activity end to end:

`conn_id`, `peer`, `cmd`, `key`, `argc`, `user`, `error`, `elapsed_us`, `shard`, `offset`, `bytes`, `commands_served`.

`commands_served` was added during planning: the connection-closed event needs to report how much work a connection did before it went away, and none of the other names carried that meaning. Any further addition goes through the same route — extend this list rather than inventing a synonym at a call site, since the value of a fixed vocabulary is entirely in its being exhaustive.

## Decision: log content — values at `trace`, secrets never

Keys and command names are logged at `debug`. Argument and value **contents** are logged at `trace`, truncated to a configurable cap. Credentials are hard-redacted at every level, including inside spans.

This is a deliberate trade: a `trace`-level log file is a complete plaintext copy of the dataset and every mutation applied to it. That is the point — it is what makes a value-shaped bug (wrong encoding, unexpected size, truncation) diagnosable from logs alone — but the retention and access-control burden it creates falls on whoever operates the server. `docs/config-reference.md` and `README.md` state this explicitly next to the `log_level` documentation.

### New module: `crates/server/src/logging.rs`

Two pure functions, which are real logic and therefore genuinely worth testing:

- **`redact_args(cmd, args)`** — yields `<redacted>` in place of the entire argument list for `AUTH`, `HELLO` when it carries an `AUTH` clause, `ACL SETUSER` / `ACL GETUSER`, and `REPLICAOF` when it carries an `AUTH` clause. Applied both at `cmd`-span creation and at every `trace`-level argument site. `dispatcher.rs` already special-cases `AUTH` and `HELLO` in `command_key_and_arity`, and `key_spec`'s comment at `dispatcher.rs:1423` already names the `AUTH` plaintext password and the `ACL SETUSER` rule token as values that must not be treated as ordinary key bytes — so the command-shape knowledge this needs already exists in the file.

  **`REPLICAOF` was missed in this spec's first draft** and added on 2026-09-09 after a task review caught it. Its six-token form, `REPLICAOF <host> <port> AUTH <username> <password>` (parsed at `dispatcher.rs:1548-1560`, password at `items[5]`, exercised by the existing test at `dispatcher.rs:9292`), carries a plaintext password through the same `dispatch_and_log` entry point every other command uses. Without an arm it would have been rendered verbatim at `trace`. Recorded here rather than quietly fixed because it is the instructive case: the audit point is only as good as the command list it was built from, and that list came from reasoning about which commands *sound* credential-shaped rather than from reading the dispatcher's actual argument parsing. Any future command that accepts a secret must be added here at the same time it is added to the dispatcher.

  `CONFIG` is deliberately **not** on the redaction list: rocket-mem's `CONFIG` surface exposes no credential parameter (there is no `requirepass` or `masterauth`), so redacting it today would guard nothing. `redact_args` is written as a per-command match so adding one is a single arm if that changes.
- **`fmt_value(bytes, cap)`** — lossy-UTF8 rendering with non-printable bytes escaped, truncated at `cap` with a trailing `…(N more)` marker.

**Redaction policy lives only in `crates/server`.** `engine` and `protocol` log key names and byte *lengths*, never value contents. There is therefore exactly one crate to audit when asking whether a secret can reach the log.

## Event catalogue

Every existing call site keeps its current level and trigger condition; this table is purely additive.

| Subsystem | File(s) | New events (level) |
|---|---|---|
| Startup | `server/main.rs` | resolved config summary (info); listener bound, per protocol (info); shutdown (info) — **deferred**, see below |
| Connection | `server/connection.rs`, `server/rmp_connection.rs` | `HELLO`/RESP3 protocol upgrade (debug); clean close and EOF with duration + command count (info) |
| Dispatch | `server/dispatcher.rs` | per-command `cmd`/`key`/`argc`/`elapsed_us`/reply kind (debug); full arguments, capped and redacted (trace); unknown command, WRONGTYPE, and arity errors (debug) |
| ACL | `server/acl.rs` | auth success with `user` (info); auth failure with `user` + `peer`, never the secret (warn); permission denied with `user`/`cmd`/`key` (warn); `SETUSER`/`DELUSER` (info) |
| Engine | `engine/engine.rs`, `engine/shard.rs`, `engine/store.rs` | shard routing, `key` → `shard` (trace); mutation byte delta (trace); per-key TTL expiry (trace); active-expire cycle key count (debug); eviction with `key` + bytes freed + reason (warn) |
| Protocol | `protocol/codec.rs`, `protocol/rmp.rs` | frame decoded, kind + length (trace); split-read reassembly (trace); protocol error (warn) |
| AOF | `server/aof.rs` | append `offset`/`bytes` (trace); fsync (debug); rewrite start/finish with generation + size (info); recovery replay summary — commands, bytes, duration (info) |
| Snapshot | `engine/snapshot.rs`, `server/aof.rs` | save start/finish with path + bytes + duration (info); load (info) |
| Replication | `server/replication.rs` | PSYNC handshake steps (debug); replica register/prune with addr (info); offset progress (trace); per-command apply (debug) |
| Cluster | `server/cluster.rs`, `server/dispatcher.rs` | topology loaded (info); MOVED redirect with `key`/slot/target node (debug) |
| Slowlog | `server/slowlog.rs` | entry recorded, `cmd`/`key`/`elapsed_us` (warn — the threshold is operator-set, so crossing it is by definition notable) |
| Metrics | `server/metrics.rs` | scrape served (trace) |

**Startup's `shutdown (info)` event is deferred, not dropped.** There is no reachable code
path to log it from: `rocket_mem::serve` (`server/connection.rs`) is an unconditional `loop`
with no `break`, so it never returns to `main`, and the crate installs no `tokio::signal`
handler anywhere — a `SIGTERM`/`SIGKILL` therefore ends the process before any Rust code,
`tracing` included, would run. Emitting the event first requires adding real signal handling,
which is a graceful-shutdown feature in its own right and well outside a logging change's
scope. Recorded here rather than quietly deleted from the row, so the gap stays visible and
so no future contributor bolts a shutdown subsystem onto a logging-scoped plan to justify one
log line. The same note sits at the call site in `server/main.rs`, above the final `serve`
call.

## Configuration

One new field, which participates in the existing figment layering (defaults < TOML < `ROCKET_MEM_*` env < CLI) exactly like every other field, so `ROCKET_MEM_LOG_VALUE_MAX_BYTES` and `--log-value-max-bytes` fall out of the existing `clap`/figment wiring for free:

```rust
/// Maximum bytes of a value or argument rendered into a `trace`-level log line before
/// truncation. Only consulted at `trace`; lower it to keep trace logs readable, raise it to
/// see whole values. See `logging::fmt_value`.
pub log_value_max_bytes: u64,   // default: 128
```

`log_level` is unchanged, as is its resolution order — `RUST_LOG` wins, `log_level` is the fallback (`config::resolve_log_filter_directive`).

Both fields are documented in `docs/config-reference.md` and `README.md`'s config table, with the `trace`-level data-exposure warning stated alongside them.

## Testing

The reasoning from the previous spec still holds: log statements added alongside unchanged control flow do not warrant log-capture assertions at every call site, and **every existing test in the workspace must pass unchanged** as the acceptance bar.

Three things here *are* real logic and are tested:

1. **`redact_args`** — unit tests asserting `AUTH`, `HELLO ... AUTH`, `ACL SETUSER`, `ACL GETUSER`, and `REPLICAOF ... AUTH` argument lists never render their secret, at any level. This list must stay identical to `is_sensitive`'s match arms; when they drift, the arms are the authority and this line is the bug.
2. **`fmt_value`** — unit tests for the cap boundary, the `…(N more)` marker, and binary-safe escaping of non-printable bytes — including Unicode `Zl`/`Zp` (U+2028/U+2029), which `char::is_control()` does *not* cover.
3. **Level separation** — one integration test asserting that a subscriber at `info` emits no per-command lines while the same workload at `debug` does. This guards against a level regression silently enabling the firehose in production, which is the failure mode with the worst consequences here.

Everything else is verified as the previous round was: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, then a live server run inspected by eye at each of `info`, `debug`, and `trace`.

## Out of scope

- **JSON output format and log shipping.** The single logrus-style text format is retained.
- **Log file rotation and file destinations.** Logs continue to go to stderr; redirection is the operator's job.
- **Runtime-reloadable level / `CONFIG SET loglevel`.** A `tracing_subscriber::reload` layer mirroring Redis's own `CONFIG SET loglevel` was considered and deferred: it trades the filter's plain atomic load for an `RwLock` read at every callsite check, and adds command surface with its own ACL and replication implications. Worth its own spec once this round's benchmark numbers show what the instrumentation actually costs.
- **OpenTelemetry / distributed trace export.**
- **Sampling.** Rejected for now: at `debug` the per-command line is the thing being asked for, and sampling would make the log lie about what happened. Revisit only if the benchmark gate proves unmeetable.
- **`common` and `rmp-client`.** Neither performs an activity worth logging.
