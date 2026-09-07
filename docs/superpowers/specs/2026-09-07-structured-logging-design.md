# Structured Logging via `tracing` — Spec & Design

**Date:** 2026-09-07
**Status:** Approved
**Scope:** `crates/server` only (`main.rs`, `connection.rs`, `rmp_connection.rs`, `dispatcher.rs`, `aof.rs`, `replication.rs`, `metrics.rs`, `config.rs`). `engine`, `protocol`, `common`, and `rmp-client` are untouched — none of them print anything today.
**Goal:** replace rocket-mem's ad-hoc `println!`/`eprintln!` calls with leveled, structured logging (in the spirit of Go's `logrus`), and close the "silent connection close" observability gap discovered during live debugging on 2026-09-06/07 (RedisInsight and rocketvault connections failing with zero server-side log output).

## Problem

`crates/server/src/*.rs` has 11 `eprintln!` call sites (error/warning-shaped events: AOF fsync/encode/append failures, replication disconnects, RMP decode errors, a metrics-recorder-install failure) and, as of the previous session, 7 `println!` statements forming the startup banner (`main.rs`). None of the 11 `eprintln!` sites carry a level, a timestamp, or structured fields — they're plain interpolated strings to stderr. Worse, several genuinely load-bearing events have **no output at all**: a client's connection getting dropped due to a decode error, a TLS handshake failure, or even a normal connection accept. This is not hypothetical — diagnosing why RedisInsight and rocketvault couldn't connect to rocket-mem earlier in this project's life required packet captures and manual `redis-cli` probing specifically because the server logged nothing when it silently closed a socket.

`tracing = "0.1"` is already declared in `[workspace.dependencies]` (`Cargo.toml`) but is not a dependency of any crate — it was anticipated but never wired in.

## Decision: `tracing` + `tracing-subscriber`, `crates/server` only

`tracing` is the de facto standard structured-logging/instrumentation crate in the Rust ecosystem (the closest analogue to `logrus`'s leveled + structured-field model). `tracing-subscriber`'s built-in `fmt` layer already produces `logrus`-shaped output with zero extra formatting crates: a timestamp, a colored level, the emitting module (`target`), the message, and any structured fields as trailing `key=value` pairs — auto-color-detected via the same "is this a tty" logic the startup banner's `paint()` helper already uses.

```toml
# workspace Cargo.toml -- new workspace dependency
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

```toml
# crates/server/Cargo.toml -- new direct dependencies
tracing.workspace = true
tracing-subscriber.workspace = true
```

`engine`/`protocol`/`common` stay dependency-light and untouched, matching the project's existing "engine stays protocol-agnostic, minimal deps" convention (`CLAUDE.md`).

## Decision: level resolution — `RUST_LOG` wins, `log_level` config is the fallback

A new `Config` field:

```rust
/// Log level filter, e.g. "info", "debug", "rocket_mem=debug,warn". Same syntax as `RUST_LOG`.
/// Overridden by the `RUST_LOG` env var when it's set, per tracing's own convention -- this
/// field is the *default* for a deployment that doesn't set RUST_LOG, not a competing source of
/// truth.
pub log_level: String,
```
default: `"info"`. It participates in the existing figment layering (defaults < toml < `ROCKET_MEM_*` env < CLI flags) automatically, the same as every other string field — no special-casing needed. A `--log-level` CLI flag and `ROCKET_MEM_LOG_LEVEL` env var fall out of the existing `clap`/figment wiring for free.

Subscriber initialization (`main.rs`, immediately after `config` loads successfully — a config-load failure itself still goes to a plain `eprintln!`, since no level is known yet at that point):

```rust
let filter = tracing_subscriber::EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.log_level));
tracing_subscriber::fmt().with_env_filter(filter).init();
```

`with_target(true)` (the default) stays on: each line shows its emitting module (e.g. `rocket_mem::aof`). A multi-module server benefits more from knowing which subsystem logged something than from `logrus`'s exact visual minimalism — directly informed by today's debugging session, where "which layer failed" was the whole question.

## Decision: the startup banner stays separate

The banner (`main.rs`'s boxed storage/ACL/cluster/listeners summary, added the previous session) stays exactly as-is — plain `println!`, not routed through `tracing`. It's one-shot human-facing terminal UX (the same design choice Redis and Postgres make with their own startup banners), not part of the ongoing operational log stream. Mixing the two would make the banner's carefully aligned formatting fight with log-line prefixes for no benefit.

## Decision: conversion scope

### A — mechanical: existing `eprintln!` → `tracing`

Every existing call site keeps its current trigger condition and control flow — only the output mechanism changes. The error value moves from string-interpolated into a structured field (`error = %e`), and the level is chosen per Redis/tracing convention (`error!` for durability-affecting failures, `warn!` for a retried/recovered condition):

| File:line | Current | New |
|---|---|---|
| `connection.rs:71` | `eprintln!("aof fsync failed: {e}")` | `tracing::error!(error = %e, "aof fsync failed")` |
| `rmp_connection.rs:128` | `eprintln!("rmp decode error: {e}")` | `tracing::warn!(error = %e, "rmp decode error")` |
| `dispatcher.rs:2606` | `eprintln!("aof encode failed: {e}")` | `tracing::error!(error = %e, "aof encode failed")` |
| `dispatcher.rs:2616` | `eprintln!("aof append failed: {e}")` | `tracing::error!(error = %e, "aof append failed")` |
| `aof.rs:110` | `eprintln!("aof append failed: {e}")` | `tracing::error!(error = %e, "aof append failed")` |
| `aof.rs:478-483` | `eprintln!("snapshot at {} names an AOF offset ({offset}) past the AOF's actual length ({len}) -- discarding the snapshot and replaying the full AOF from byte 0 instead", snapshot_path.display())` | `tracing::warn!(snapshot_path = %snapshot_path.display(), offset, aof_len = len, "snapshot offset past end of AOF; discarding snapshot and replaying full AOF")` |
| `aof.rs:491-494` | `eprintln!("snapshot at {} is unreadable ({e}); falling back to full AOF replay", snapshot_path.display())` | `tracing::warn!(snapshot_path = %snapshot_path.display(), error = %e, "snapshot unreadable; falling back to full AOF replay")` |
| `replication.rs:433` | `eprintln!("replication: connection to {host_port} closed; reconnecting")` | `tracing::warn!(host_port = %host_port, "replication connection closed, reconnecting")` |
| `replication.rs:434` | `eprintln!("replication: lost connection to {host_port}: {e}; reconnecting")` | `tracing::warn!(host_port = %host_port, error = %e, "replication connection lost, reconnecting")` |
| `replication.rs:565` | `eprintln!("replication: applying a replicated command failed: {e}")` | `tracing::error!(error = %e, "failed to apply replicated command")` |
| `metrics.rs:33-35` | `eprintln!("metrics: a global recorder was already installed; metrics may be incomplete")` | `tracing::warn!("global metrics recorder already installed; metrics may be incomplete")` |

### B — new: connection lifecycle logging (closes the "silent close" gap)

Added at the points identified during today's debugging session (`connection.rs`'s `handle_connection`/`serve_tls`, `rmp_connection.rs`'s equivalent):

- **Connection accepted** (`info!`): `peer`, `protocol` (`"resp"`/`"rmp"`), `tls` (bool).
- **Connection closed: decode error** (`warn!`): `peer`, `error` — this is the exact case that silently dropped RedisInsight/rocketvault connections with zero prior output.
- **TLS handshake failed** (`warn!`): `peer`, `error` — distinct from a handshake timeout.
- **TLS handshake timed out** (`warn!`): `peer`.
- Plain EOF / client-initiated clean close stays unlogged at `warn`/`error` (that's normal lifecycle, not a problem) — at most a `debug!` if useful during implementation, not required.

## Out of scope

- Per-command / hot-path debug tracing (e.g. `tracing::debug!` on every dispatched command). Not requested, and this is a performance-sensitive path across 16 shards under load — worth a dedicated design (sampling, `#[instrument(skip(...))]` cost) if wanted later.
- JSON output format / runtime-switchable formatter. Declined explicitly in favor of a single logrus-style text format.
- Any change to `engine`, `protocol`, `common`, or `rmp-client`.

## Testing

This change adds logging calls alongside existing logic without altering control flow or return values, so it does not warrant heavyweight log-capture test infrastructure (e.g. `tracing-mock`) at every call site — that would be testing observability plumbing, not behavior, and every existing test in the workspace must keep passing completely unchanged as the acceptance bar for the mechanical conversion (Part A) and the config field (default `"info"`, layering).

One new unit test *is* warranted because it's real logic, not a print statement: level-resolution precedence — `RUST_LOG`, when set, wins over `log_level`; `log_level`'s configured value is used when `RUST_LOG` is unset. This can be tested directly against `EnvFilter`'s parsing/precedence without spinning up the full subscriber or a server instance.

Part B (new connection-lifecycle logs) is verified manually against a live server for this round (matching how the previous session's startup-banner change was verified: build clean, clippy clean, fmt clean, then a real run inspected by eye) rather than via automated log-capture assertions, consistent with the "don't over-test side-effecting log statements" reasoning above.
