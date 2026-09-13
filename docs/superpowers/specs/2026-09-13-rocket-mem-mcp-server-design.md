# `rocket-mem-mcp` — Spec & Design

**Date:** 2026-09-13
**Status:** Approved
**Scope:** A new standalone crate, `rocket-mem-mcp`, living outside the Cargo workspace (own
`Cargo.toml`, not in root `Cargo.toml`'s `members`, not built or gated by
`.github/workflows/ci.yml`) — the same relationship `examples/job-queue-pubsub` and
`tools/review-agent` already have to this repo. No changes to any workspace crate
(`common`/`engine`/`protocol`/`rmp-client`/`server`).
**Goal:** Expose rocket-mem's command set as MCP (Model Context Protocol) tools, so an LLM agent
can read and write a running rocket-mem instance as part of an agent workflow.

## Problem

rocket-mem has no way for an MCP-speaking LLM agent to interact with it today. The only existing
non-Rust-internal client precedent is `examples/job-queue-pubsub` (Go, `go-redis`), which
demonstrates rocket-mem is a wire-compatible RESP2/RESP3 server but is a demo, not a reusable
integration. There is no MCP-related code anywhere in this repo (confirmed by a full-repo grep
prior to this spec).

## Decision: standalone crate, RESP2/RESP3 via `redis-rs`, one MCP tool per rocket-mem command

### Why standalone, not a 6th workspace crate

An MCP server is an agent-facing *consumer* of rocket-mem, not a wire protocol rocket-mem itself
serves — unlike `server`, which owns RESP/RMP. Pulling in `rmcp` (a pre-1.0, macro-heavy
dependency) plus `schemars`/`serde_json` into a Cargo workspace whose CI runs
`cargo clippy --workspace --all-targets -- -D warnings` (strict across every target, including
tests) would risk an unrelated future clippy lint on `rmcp`'s generated code blocking unrelated
PRs. `examples/` and `tools/review-agent` already establish the precedent of a rocket-mem-adjacent
tool living outside `members` and outside the CI gate.

### Why RESP2/RESP3 via `redis-rs`, not RMP via `rmp-client`

Either would work — `rmp-client` (already in this repo) would mean zero new protocol code, but it
ties this crate to a path-dependency on rocket-mem's own workspace, working against the
"standalone" decision above. `redis-rs` is the mature, independently-maintained Rust client for
the Redis wire protocol; rocket-mem's RESP2/RESP3 support (README.md's "Command coverage" table)
is wire-compatible with it, matching how `examples/job-queue-pubsub` already validates real-client
compatibility (with `go-redis` instead). This also means `rocket-mem-mcp` could point at any
Redis-wire-compatible server, not only rocket-mem, at no extra cost.

### Why one MCP tool per command

Each rocket-mem command becomes a distinct `#[tool]`-annotated function (`rmcp`'s macro pattern)
with a typed argument schema — e.g. `set(key: String, value: Bytes, ex: Option<u64>)` — rather
than a handful of generic per-type dispatch tools or a single raw passthrough. This gives the
calling agent real per-command schema validation and the best tool-selection accuracy, at the
cost of a large tool list (see "Tool surface" below and "Scale" under Out of scope).

### Security model: fully delegated to the target instance

No read-only mode, no command denylist, no auth layer of this crate's own. `rocket-mem-mcp`
connects to one already-running rocket-mem instance whose address and optional ACL
username/password are supplied via this crate's own config/CLI/env at startup (mirroring
rocket-mem's own layered config convention: defaults → file → env → CLI). Whatever that
connection is authorized to do via rocket-mem's own ACL (`crates/server/src/acl.rs`) is exactly
what the agent can do — this crate adds no restriction beyond what rocket-mem itself enforces. If
the target instance has ACLs configured, connect with a suitably-scoped user; if not (the
default — auth/TLS are off by default per `crates/server/src/config.rs`), the agent has the same
access any unauthenticated client would.

## Tool surface

One tool per command from README.md's "Command coverage" table, grouped into modules
(`tools/string.rs`, `tools/hash.rs`, `tools/list.rs`, `tools/set.rs`, `tools/sorted_set.rs`,
`tools/keys.rs`, `tools/server.rs`, `tools/acl.rs`, `tools/transaction.rs`, `tools/pubsub.rs`),
with three categories of exception:

- **Excluded — connection-lifecycle commands, not agent operations.** `HELLO` (protocol
  negotiation) and `AUTH` (credential handshake) are handled once, internally, when
  `rocket-mem-mcp` establishes its pooled connections at startup — never exposed as a per-call
  tool, the same way a human wouldn't re-authenticate before every `redis-cli` command.
- **Excluded — mechanically incompatible with a pooled request/response connection.** `PSYNC`
  hijacks the whole TCP connection into a one-way replication byte stream for the rest of its
  life (`docs/superpowers/specs/2026-09-11-pubsub-spec.md`'s own description of
  `connection.rs`'s `serve_replica`) — a pooled connection that received this would never be
  reusable again and would corrupt the pool. There is no tool-call shape this command can take.
- **Collapsed — `MULTI`/`EXEC`/`DISCARD` become one `run_transaction` tool.** See "Connection and
  session model" below for why.

Everything else in the table — including `REPLICAOF` (topology-changing) and the `ACL
SETUSER`/`DELUSER` family (privilege-changing) — becomes a tool as-is, consistent with the "full
read/write, no built-in restriction" decision above. These two in particular are flagged here
explicitly: an agent with tool access to this MCP server can repoint replication topology or
rewrite its own ACL grants. That is a deliberate consequence of the security model, not an
oversight.

This yields roughly 105 tools across all modules (String/Key ~30, Hash 12, List 11, Set 14, Sorted
Set 7, Server/Cluster/Slowlog ~17, ACL 5, one `run_transaction`, Pub/Sub 7). See "Scale" under Out
of scope for the one open question this raises.

## Connection and session model

Three different lifecycles, matched to what each command actually needs:

**Stateless commands** (everything except transactions and pub/sub): a shared connection pool
(`pool.rs`, built on `redis-rs`'s multiplexed connection support) is borrowed for the single
round-trip and returned immediately after. No state survives between calls. This is the large
majority of tools.

**Transactions — collapsed into `run_transaction(commands: [{name, args}])`.** Rather than
exposing `MULTI`/`EXEC`/`DISCARD` as three separate tool calls (which would require pinning one
rocket-mem connection to one MCP client session for the whole multi-call sequence — real
session-lifecycle state for a feature that doesn't need it), the whole batch arrives in one tool
call. The handler borrows one pooled connection, builds a `redis::pipe().atomic()` from the
command list, executes it as a single `MULTI...EXEC` round trip, and returns the ordered
per-command results (or the server's `EXECABORT`/`WRONGTYPE` error verbatim on failure). Since
rocket-mem's own transactions have writers-only isolation and no `WATCH`/`UNWATCH`
(README.md's "Command coverage" table), there is nothing about real rocket-mem transaction
semantics this loses — batching an atomic sequence into one call carries the same guarantees as
sending `MULTI`, the same commands, and `EXEC` back-to-back on a dedicated connection would.

**Pub/sub — the one place session state is unavoidable.** A standing subscription cannot be
honestly flattened into a single call the way a transaction can; a real subscribe/poll/unsubscribe
lifecycle needs somewhere to hold "what am I currently subscribed to" and "what arrived since I
last checked" across separate tool calls from the same agent. `session.rs` holds exactly this one
piece of state, keyed by the MCP session id `rmcp` (and, per the "both transports" decision, the
Streamable HTTP transport too) already assigns per client session:

- `subscribe(channels: [String])` — looks up or creates this session's `SubscriptionManager` entry
  (`subscription.rs`), opens a dedicated `redis-rs` pub/sub connection (separate from the shared
  pool — a pub/sub connection is not reusable for ordinary commands), and spawns a background task
  appending incoming messages to a bounded, oldest-dropped ring buffer for this session.
- `poll_messages()` — drains and returns everything buffered since the last call. Bounded and
  oldest-dropped rather than unbounded, so an agent that never polls can't leak memory the way
  rocket-mem's own `ReplicaRegistry`/pub/sub push channel deliberately accepts as a known,
  documented tradeoff (`docs/superpowers/specs/2026-09-11-pubsub-spec.md`'s "Out of scope" —
  unbounded there because it's a different failure mode, a live TCP connection, not an
  MCP polling client).
- `unsubscribe(channels: [String])` — stops the background task for those channels and closes the
  dedicated connection when no channels remain. Also invoked automatically on MCP session
  teardown, so a disconnected agent never leaves an orphaned subscription running.
- `publish(channel, message)` and the three `pubsub_*` introspection tools (`channels`, `numsub`,
  `numpat`) are stateless — ordinary pooled-connection commands, no session involvement.

No other command family needs session state; this confines the complexity to exactly the one
feature (pub/sub) that structurally requires it.

## Transport

Both `stdio` and Streamable HTTP from the start, both provided by `rmcp`'s transport layer over
the same tool implementation — the tool modules and connection/session logic are transport-
agnostic; only `main.rs`'s startup wiring differs (spawn a child process vs. bind an HTTP
listener). `stdio` is how Claude Code/Desktop launch MCP servers locally; Streamable HTTP lets
other agents/systems connect to a standing `rocket-mem-mcp` instance remotely (SSE is the older,
now-deprecated HTTP transport — not implemented).

## Error handling

Every rocket-mem error (`WRONGTYPE`, `NOAUTH`, `EXECABORT`, connection failures) surfaces to the
agent as an MCP tool error with the underlying message preserved verbatim — never swallowed or
generalized, since the agent needs the real reason to decide its next move (e.g. retry vs. give up
vs. try a different command). A distinct transport-level error class (connection-pool exhaustion,
a dropped rocket-mem connection) is used for "the server is unreachable, retry" so an agent can
tell that apart from "your specific command was rejected."

## Testing strategy

- Unit tests per tool module run against a real local rocket-mem instance — this project's own
  integration tests already follow that convention rather than mocking RESP, and a mock would
  hide exactly the kind of wire-level mismatch this crate exists to avoid.
- A `run_transaction` test asserting the wire trace genuinely shows `MULTI`/queued
  commands/`EXEC`, not a sequence of independent round trips — the failure mode a higher-level
  mock would hide entirely.
- A `run_transaction` test on a command list that triggers `EXECABORT` (e.g. an invalid command),
  asserting the tool surfaces that error rather than a partial result.
- A subscription lifecycle test: `subscribe` → publish from a second connection → `poll_messages`
  sees it → `unsubscribe` → a later publish no longer appears in a subsequent poll.
- A session-teardown test: disconnecting an MCP session with an active subscription results in
  that subscription's background task stopping (observable via the pooled connection no longer
  being held open, or via rocket-mem's own `PUBSUB NUMSUB` dropping to 0).
- `cargo fmt`/`cargo clippy` for this crate on its own (this crate is outside the workspace CI
  gate, but held to the same "must build and lint clean before any PR" bar as everything else per
  project convention).

## Out of scope (v1)

- **Tool-list scale.** ~105 tools in one MCP server is large; whether that materially hurts an
  agent's tool-selection accuracy or context budget in practice is an open question this spec
  does not resolve — worth watching once this is in use, not a reason to prune the surface
  preemptively given the explicit "everything rocket-mem supports" scope decision.
- **Multi-instance/cluster awareness.** This crate talks to one rocket-mem endpoint; it does not
  follow `-MOVED` redirects for a clustered deployment. A cluster-aware connection layer is a
  separable follow-up if needed.
- **RMP protocol support.** RESP-only for v1, matching the "why RESP2/RESP3" decision above.
- **Read-only/permission tiers within this crate.** Explicitly rejected by the security-model
  decision above — access control is entirely the target rocket-mem instance's own ACL, not this
  crate's job to duplicate or narrow.
