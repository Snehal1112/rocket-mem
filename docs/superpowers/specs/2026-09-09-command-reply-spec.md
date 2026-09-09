# Real `COMMAND` Replies: Spec & Design

**Date:** 2026-09-09
**Status:** Proposed
**Scope:** `crates/server/src/dispatcher.rs` only — the `"COMMAND"` match arm and a new
per-command metadata helper it (and `COMMAND INFO`/`COMMAND COUNT`) reuse. No wire-protocol
change beyond `COMMAND`'s own reply payload, no command-semantics change, no new command support,
no change to `key_spec`/`command_keys`/`WRITE_COMMANDS` (all three become read-only inputs to the
new table).

**Goal:** stop clients that build local routing/introspection metadata from `COMMAND` (concretely:
`github.com/redis/go-redis/v9`'s `ClusterClient`) from treating every rocket-mem command as
unknown, without introducing any risk of misrouting a real command.

## Problem

`dispatcher.rs:1013`:

```rust
"COMMAND" => Frame::Array(vec![]), // enough that clients probing capabilities don't choke
```

Discovered via RocketVault's rocket-mem-cluster integration (a sibling project, same author) going
live against a real 3-node cluster: its background health-check pings the cluster every 30s, and
each `PING` — plus, by the same code path, every other command sent through
`go-redis/v9`'s `ClusterClient` — logs `redis: osscluster.go:2858: info for cmd=ping not found` to
stderr, once per call. Traced to source: go-redis populates a per-command metadata cache by
calling plain `COMMAND` against the server once, then looks up each outgoing command's name in
that cache before routing it. An empty `COMMAND` reply means the cache is permanently empty, so
every lookup misses and logs.

## Evidence (verified against go-redis v9.22.0 source, 2026-09-09)

- **Reply shape:** `CommandsInfoCmd.readReply` (`command.go`) accepts a per-command array of
  **exactly 6, 7, or 10 elements** — any other count is a hard parse error for the whole `COMMAND`
  call. The 6-element shape is `[name, arity, flags, first_key, last_key, step]`; 7 adds an
  ACL-flags array; 10 adds a tips array (parsed into `CommandPolicy`) then discards two more
  fields. **6 elements is a complete, valid reply shape on its own** — the extra fields are not
  required for a well-formed entry.
- **What the cluster router actually reads:** `ReadOnly` (derived from a `"readonly"`/`"write"`
  flag string) is used only to decide whether a read may be sent to a replica when
  `ClusterOptions.ReadOnly` is set — irrelevant to correctness here since rocket-mem's cluster mode
  has no replicas yet. `FirstKeyPos`/`LastKeyPos`/`StepCount` are consulted, but the router falls
  back to its own hardcoded single-key default (`return 1`) when the cache has nothing better —
  meaning **a correct 6-element entry can only improve on today's behavior, never regress it**.
  `CommandPolicy` (multi-shard aggregation: `ReqAllNodes`/`ReqAllShards`/`ReqMultiShard`) is `nil`
  unless the reply includes the 10-element tips array — so omitting tips entirely guarantees every
  command keeps routing through the plain per-key-slot path it uses today. **A 6-element reply
  cannot cause go-redis to misroute anything rocket-mem currently implements correctly.**
- **Partial replies are strictly better than none:** the "not found" log fires per command name on
  a cache miss. Every command given a real entry stops logging; anything still missing keeps
  logging. Full coverage of every implemented command is required to eliminate the log entirely,
  but there is no downside to shipping this incrementally if that were ever necessary.

## Decision

`"COMMAND"` (no subcommand) returns one 6-element entry per `KNOWN_COMMANDS` entry
(`dispatcher.rs:2825-2917`, 88 commands today). A new helper builds each entry entirely from
tables that already exist and are already each other's source of truth for a related concern —
no new classification is invented:

```
[name, arity, flags, first_key, last_key, step]
```

- **`name`**: the lowercase form of the command's `KNOWN_COMMANDS` entry.
- **`flags`**: a one-element array, `["write"]` if the uppercased name appears in
  `aof::WRITE_COMMANDS` (`aof.rs:399-438`, 38 commands), else `["readonly"]`. No other flag
  category (`admin`, `pubsub`, `fast`, `loading`, `stale`, ...) is populated — see Out of scope.
- **`first_key` / `last_key` / `step`**: derived deterministically from the existing `key_spec`
  function (`dispatcher.rs:1338-1358`), which is already the authoritative per-command key table
  used for `-CROSSSLOT` enforcement:

  | `key_spec` variant | first_key | last_key | step |
  |---|---:|---:|---:|
  | `None` | 0 | 0 | 0 |
  | `First` | 1 | 1 | 1 |
  | `Second` | 2 | 2 | 1 |
  | `All` | 1 | -1 | 1 |
  | `EveryOther` | 1 | -1 | 2 |

- **`arity`**: reported as `-(k + 1)`, where `k` is the same `key_spec`-derived minimum
  argument count used for `first_key`/`last_key`/`step` above (`+1` converts to Redis's convention
  of counting the command name itself as argument 0):

  | `key_spec` variant | `k` | arity |
  |---|---:|---:|
  | `None` | 0 | -1 |
  | `First` | 1 | -2 |
  | `Second` | 2 | -3 |
  | `All` | 1 | -2 |
  | `EveryOther` | 2 | -3 |

  This is a deliberate scope decision, not an oversight: go-redis's `ClusterClient` never reads
  `Arity` for any routing decision (confirmed above), so reporting a *lower bound* rather than
  hand-mining every command's own argument-count validation (scattered across ~90 match arms, each
  with its own `require_args!` call) is strictly sufficient for this goal — and deriving it from
  `key_spec`, the same table already driving `first_key`/`last_key`/`step`, means this feature adds
  **zero** new per-command classification data, matching the Decision section's framing above. The
  reported value is not always the *tightest* possible bound (e.g. `SET` truly needs 2 arguments
  but this reports arity `-2`, i.e. "at least 1"), but it is never wrong in the direction that
  matters: every command's true minimum argument count is always ≥ what's reported. If a future
  need (e.g. a strict client-side arity validator) requires exact, tight bounds, that is a
  separate, narrower follow-up requiring its own per-command review, not blocked by this one.
- **No tips, key-specs, ACL-categories, or subcommand arrays.** The 10-element format exists to
  describe features rocket-mem doesn't have (multi-shard aggregation, scripting, pub/sub) — see
  Evidence above for why this cannot cause misrouting, and Out of scope for why building it now
  would be speculative.

### Subcommands

Today, `"COMMAND"` matches on the top-level command name alone and never inspects `rest`, so
`COMMAND COUNT`, `COMMAND INFO ...`, and any other subcommand all hit the same empty-array stub.
This spec adds the two subcommands a client is actually likely to send:

- **`COMMAND COUNT`** → `Frame::Integer(KNOWN_COMMANDS.len() as i64)`.
- **`COMMAND INFO [name ...]`** → one entry per requested name, using the same per-command builder
  as bare `COMMAND`; an unrecognized name maps to `Frame::Null` at that position (matching real
  Redis's behavior of returning nil for a command it doesn't know), never an error.
- Any other subcommand (`DOCS`, `LIST`, `GETKEYS`, ...) is unchanged — falls through to today's
  behavior. Real Redis's `COMMAND` with no subcommand returns *every* command's info, which is what
  the bare-`COMMAND` case already does under this design, so no separate branch is needed for it.

## Out of scope

- ACL categories, tips (`request_policy`/`response_policy`), key-specs, and the rest of Redis 7's
  richer `COMMAND` fields. Go-redis's cluster router never reads any of them for a command shaped
  like the ones rocket-mem implements (see Evidence), and rocket-mem has neither scripting nor
  pub/sub for them to meaningfully describe. Building them now would be speculative work against
  no concrete consumer.
- `COMMAND DOCS`, `COMMAND LIST`, `COMMAND GETKEYS`. `GETKEYS` in particular would be nearly free
  (it could call the existing `command_keys` directly), but nothing in this bug's chain needs it —
  worth a one-line follow-up note, not part of this change.
- Exact (non-minimum) arity for every command. See the `arity` bullet above.
- Any change to `key_spec`, `command_keys`, or `WRITE_COMMANDS` themselves — this spec is a new
  consumer of all three, not a modification to any of them.

## Testing strategy

`cargo test --workspace`, `cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets -- -D warnings` gate every commit, per `CLAUDE.md`.

New tests:

1. **Full coverage.** `COMMAND`'s reply array has exactly `KNOWN_COMMANDS.len()` entries, and each
   entry's name matches, in order, the lowercased form of the corresponding `KNOWN_COMMANDS`
   entry — an order-preserving correspondence check against `KNOWN_COMMANDS` itself, rather than a
   cross-table drift guard, since the reply is now derived from that single table directly and
   there is no second table to drift out of sync with it.
2. **Keyless commands.** `PING`'s entry has `first_key=0, last_key=0, step=0` and
   `flags=["readonly"]` (it is `key_spec::None` and not in `WRITE_COMMANDS`).
3. **Single-key write.** `SET`'s entry has `flags=["write"]`, `first_key=1, last_key=1, step=1`.
4. **Multi-key `All`.** `DEL`'s entry has `first_key=1, last_key=-1, step=1`.
5. **`EveryOther`.** `MSET`'s entry has `first_key=1, last_key=-1, step=2`.
6. **`COMMAND COUNT`** returns `Frame::Integer(KNOWN_COMMANDS.len() as i64)`.
7. **`COMMAND INFO`** with a mix of a known command (e.g. `GET`) and an unknown name returns the
   known command's real entry and `Frame::Null` for the unknown one, in request order.
8. **Arity sign.** Every entry's arity is negative (the `key_spec`-derived convention above never
   produces an exact positive arity under this design — see the `arity` bullet), guarding against
   a future edit accidentally emitting a positive value that would falsely claim an
   exact-argument-count command.

Manual verification (not part of the automated suite, since it requires the sibling
`rocketvault` repo and a live cluster): once RocketVault's rocket-mem cluster integration test
suite can authenticate against a real 3-node cluster, confirm the
`"info for cmd=... not found"` log line no longer appears in its output.

## Risks

- **`KNOWN_COMMANDS` / `WRITE_COMMANDS` / `key_spec` drifting out of sync** as new commands are
  added over time — this spec adds a new consumer of tables that already had to stay in sync for
  metrics labeling and CROSSSLOT enforcement; Test 1 above is the specific regression guard for
  this design's own risk of a silently-incomplete `COMMAND` reply.
- **Reporting a minimum-only arity** could theoretically confuse a client library that validates
  argument counts against `COMMAND`'s arity client-side before sending. No client in this
  project's actual usage (go-redis, `redis-cli` in its default mode) does this — both rely on the
  server's own `require_args!`-driven wrong-number-of-arguments error — so this is a documented,
  accepted trade-off rather than an open risk.
