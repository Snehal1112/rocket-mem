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

This yields roughly 103 tools across all modules (String/Key ~30, Hash 11, List 11, Set 13, Sorted
Set 7, Server/Cluster/Slowlog ~17, ACL 5, one `run_transaction`, Pub/Sub 7 — Hash/Set corrected
down from an initial 12/14 estimate once `HSCAN`/`SSCAN` were confirmed unimplemented, see
"Command semantics reference" below). See "Scale" under Out
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

## Command semantics reference (verified 2026-09-13)

Ground truth for Plans 2-13, verified against this project's actual engine/dispatcher code (not
assumed real-Redis behavior) via a decomposed research pass before writing each plan. Corrects
two tool-count estimates from "Tool surface" above: **Hash is 11, not 12** (`HSCAN` is not
implemented); **Set is 13, not 14** (`SSCAN` is not implemented) — both confirmed absent from
`crates/engine/src/commands/{hash,set}.rs` and from `docs/command-compatibility.md`'s own command
lists.

### Plan 2 — String/Key rest, and Plan 13's `Bytes`/`EX` widening

`SET` already passes its value as raw `Bytes` at the dispatcher level
(`crates/server/src/dispatcher.rs:331`) and already supports `EX`/`PX` — Plan 13's widening is
mostly a **tool-schema** change (accept bytes in the MCP parameter, not just UTF-8 strings), not
an engine change. Real deviations worth stating in each tool's description (agents will otherwise
assume real-Redis behavior): `KEYS`'s glob support is partial; `OBJECT ENCODING` returns this
engine's type name (not real encodings) and errors "no such key" on a missing key (unlike
`TTL`/`PTTL`, which return `-2`); `TTL`/`PTTL` floor at 1 for a sub-second remaining TTL; `MGET`
never errors on WRONGTYPE (returns `None` for that key instead); `SETRANGE` with an empty value is
a total no-op, never creates the key; `RENAME`/`RENAMENX` preserve the source's TTL on the
destination, error `NoSuchKey` if the source is missing, and `RENAMENX` returns `false` (not an
error) if the destination already exists. Variadic (array-shaped tool params needed): `MSET`/
`MSETNX` (pairs), `MGET`/`DEL`/`EXISTS` (keys). `SCAN` is the one cursor-based command in this
family — its tool needs a `cursor` input and must return the `next_cursor` the engine gives back,
not a single fire-and-forget call like `KEYS`.

### Plan 3 — Hash (11 tools)

`HSET`/`HSETNX` are **single field/value pair at the engine level**
(`crates/engine/src/commands/hash.rs:8-13,167-172`) — the dispatcher loops per pair to give real
Redis's variadic multi-pair `HSET` on the wire. The MCP tool needs the same choice: accept one
pair (matching the engine call 1:1) or accept an array of pairs and loop internally like the
dispatcher does. `HDEL`/`HMGET` are already variadic at the engine level. `HINCRBY` raises two
error types beyond WRONGTYPE: `NotAnInteger`, `IncrementOverflow`.

### Plan 4 — List (11 tools)

All 11 map cleanly to fixed-parameter tools. `LPUSH`/`RPUSH` take a variadic values array.
`LINSERT`'s before/after is a plain `bool` at the engine level, not `BEFORE`/`AFTER` keywords.
`LSET` distinguishes `NoSuchKey` (key missing entirely) from `IndexOutOfRange` (bad index on an
existing list) — worth two distinct, separately-worded tool errors rather than collapsing both to
one message.

### Plan 5 — Set (13 tools)

`SADD`/`SREM` take variadic members; `SINTER`/`SUNION`/`SDIFF` and their `*STORE` variants take a
**variadic key list** (not a fixed two keys) plus, for the `*STORE` variants, a destination key.
`SPOP` and `SRANDMEMBER` do **not** support an optional `count` at the engine level — single-member
only, despite real Redis's `[count]` form; don't add a `count` parameter the engine will ignore.

### Plan 6 — Sorted Set (7 tools)

`ZADD` confirmed single-pair only (`score: f64, member: Bytes`), no `NX`/`XX`/`GT`/`LT`/`CH`/
`INCR` — matches README exactly. `ZRANGE`'s `start`/`stop` are `i64` with real-Redis
negative-index semantics and **both ends inclusive** (`crates/engine/src/commands/sorted_set.rs`
normalizes internally as `norm(stop) + 1`) — state this explicitly in the tool description, since
"inclusive stop" is the one detail an implementer coming from typical array-slicing conventions
would get wrong. Neither `ZRANGE` nor `ZRANK` has a `WITHSCORES`/`WITHSCORE` variant — members/rank
only. Scores are `f64` throughout; the tool's JSON schema uses a plain `number`.

### Plan 7 — Server/Cluster/Slowlog admin (~17 tools)

Reply shapes vary more here than any other family, and three items need real design attention
rather than a flat pass-through:
- **`INFO [section]`** replies with a single flat text blob in real Redis's `# Section\r\nkey:
  value\r\n...` format (`crates/server/src/dispatcher.rs:2085-2296`) — the tool should return that
  text as-is (or explicitly own parsing it into JSON), not assume a structured reply.
- **`CLUSTER SHARDS`** is genuinely nested and variable-shaped (`dispatcher.rs:1925-1978`): an
  array of shards, each alternating `"slots"→[start,end]` and `"nodes"→[array of field/value
  pairs]`. This is the one reply in the whole remaining surface that needs a real JSON schema
  designed for it, not a generic "array of strings" guess.
- **`COMMAND`/`COMMAND INFO`** replies with a nested array per command
  (`[name, arity, [flags], first_key, last_key, step]`, `dispatcher.rs:1514`) — also needs a
  proper nested schema.

Two operational-risk flags worth stating in their tool descriptions, not just noting internally:
**`DEBUG SLEEP <secs>`** genuinely blocks a real Tokio worker thread via `std::thread::sleep`
(capped at 10s, `dispatcher.rs:1193-1230`) — capped, but still a real stall an agent could trigger
repeatedly. **`REPLICAOF`** (3 or 6 args: `host port` / `NO ONE` / `host port AUTH user pass`,
`dispatcher.rs:1691-1755`) actually starts/stops live replication — this is the spec's own
"high-risk, topology-changing" flag, confirmed real, not hypothetical. `BGREWRITEAOF` is fully
synchronous despite the name (blocks the calling connection until the rewrite finishes).

### Plan 8 — ACL admin (5 tools)

**`ACL SETUSER`'s parameter must be an array of independent rule-token strings**
(`crates/server/src/acl.rs:73-111`), not one packed string — real Redis's own wire grammar is
already a flat list of tokens (`on`/`nopass`/`~pattern`/`+CMD`/etc.), and rocket-mem's
`AclStore::set_user` applies them **incrementally onto the user's existing state**, never as a
full replace (`acl.rs:280-318`) — state clearly in the tool description that repeated calls
compose rather than reset. `ACL DELUSER` is variadic (multiple usernames) and replies with the
count that *actually existed and were deleted*, not the count of usernames given. `ACL GETUSER`
returns a structured object (`flags`/`passwords`/`commands`/`keys`), and `passwords` holds the
Argon2 **hash**, never plaintext. Two real risks, confirmed at the code level, worth a loud
warning rather than a footnote: `auth_gate` re-resolves the live ACL user on every command
(`dispatcher.rs:3161-3174`), so an agent calling `DELUSER`/`SETUSER ... off` on **its own
currently-connected username** locks itself out immediately, mid-session — there is no built-in
guard against this; and the **first ever successful `SETUSER` turns authentication on for the
whole server, permanently, with no "turn ACL off" command** (`acl.rs:244-251,315-317`) — calling
this tool against a previously-open instance is a one-way trip.

### Plan 9 — `run_transaction`

The spec's "list of commands in, ordered list of results out, `EXECABORT` surfaces as one
tool-level error" design is confirmed correct, with one correction to the mental model:
**"writers-only isolation" describes the write-write mutual-exclusion guarantee during `EXEC`,
not what gets queued** — every command type, reads included, is deferred and queued exactly like
writes (`crates/server/src/dispatcher.rs:3203-3263`); `run_transaction` must accept read commands
too, not just writes. `EXECABORT` triggers only for two queue-time conditions: an unknown command,
or a `SUBSCRIBE`-family command (`dispatcher.rs:3239-3256`) — model the tool as validating the
whole batch against these two cases up front and failing the call with one error if either is
present, rather than attempting the batch. Everything else (WRONGTYPE, etc.) is a normal
per-command inline error inside the ordered results array (`dispatcher.rs:3356-3379`), exactly as
already specced. `DISCARD` has no analog for a single-call tool — confirmed not needed.

### Plan 10 — Pub/Sub + session

The existing pub/sub spec (`docs/superpowers/specs/2026-09-11-pubsub-spec.md`) is still accurate
against the current `pubsub.rs`/`dispatcher.rs` — `PubSubRegistry`'s shape, single-node-only
delivery, `PUBLISH`'s integer reply, and `PUBSUB CHANNELS`/`NUMSUB`/`NUMPAT`'s reply shapes all
match exactly. One unrelated drift surfaced during verification, in the *main* rocket-mem
codebase, not rocket-mem-mcp's concern: RESP2 subscribe-mode's allowed-command gate
(`dispatcher.rs:3745`) is missing `RESET` from its allowlist despite both the spec and the gate's
own error message saying `RESET` is allowed — a real, pre-existing bug, worth a separate report to
the rocket-mem maintainers, out of scope here.

### Plan 11 — Streamable HTTP transport

`rmcp::transport::streamable_http_server::tower::StreamableHttpService` is tower-compatible (works
with axum or any tower-based HTTP server), gated behind the `transport-streamable-http-server` +
`transport-streamable-http-server-session` features. Constructed via
`StreamableHttpService::new(service_factory, session_manager: Arc<M>, config)`;
`LocalSessionManager` (in-memory session map) is the default and sufficient for v1 — no need for a
custom `SessionStore` yet. `.handle(request) -> Response` is the actual per-request entry point,
meaning `main.rs` needs to bind a TCP listener (or embed axum) and route requests into this
service, running alongside — not instead of — the existing `stdio` path, selected by config/CLI.

### Plan 12 — TOML config-file layer

rocket-mem's own pattern (`crates/server/src/config.rs:264-279`): `figment` 0.10 (features
`toml`, `env`), `Figment::from(Serialized::defaults(...)).merge(Toml::file(path)).merge(
Env::prefixed(...)).extract()`, guarded by a `path.exists()` check so a missing file is silently
skipped, never an error; a `--config <path>` CLI flag defaults to auto-picking up
`"rocket-mem.toml"` from the cwd if unset. **Recommendation for rocket-mem-mcp: hand-roll with the
`toml` crate rather than adding `figment` as a new dependency** — this crate has ~4 flat fields
(vs. rocket-mem's much larger config), and `clap`'s own `env` attribute already covers the
env/CLI layers; hand-rolling only needs a defaults→optional-file-merge step ahead of the `clap`
parse that already happens.

## Out of scope (v1)

- **Tool-list scale.** ~103 tools in one MCP server is large; whether that materially hurts an
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
