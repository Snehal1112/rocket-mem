# Phase 1 Retro (Weeks 1–4 / Sprints 1–2)

Per the master plan's Week 4 task: what shipped vs. planned, where effort diverged,
what surprised us, and what's technical debt to revisit in Phase 3.

## What shipped vs. what was planned

**Sprint 1** (protocol-agnostic storage engine) landed its full P0/P1 scope: sharded
keyspace (`617fedd`, `672c58b`), the `Engine` facade (`dd835b3`), and all four Sprint 1
data types' commands — strings (`0438a5e`), hashes (`1752d6a`), lists (`d249bde`), sets
(`073c105`) — plus the WRONGTYPE/missing-key correctness sweeps described below. No P1
line item was cut; `SET EX/PX` was the one deliberate P2 deferral, called out in
`CLAUDE.md` from the start rather than discovered as a gap later.

**Sprint 2** (RESP2 protocol + networking) also landed its full P0/P1 scope: `Frame` +
`RespCodec` (`1e58201`, `fff8e53`, `36cd589`), the command dispatcher (`a4f9906`), the
TCP accept loop (`a399a77`), stub commands (`33da52f`, `1859384`), the full remaining
Sprint 1 command surface wired through the dispatcher (`56b65bf`), the `redis-rs`
integration harness (`a2eac8e`, `50e6ff5`), and manual client verification against
`redis-py`/`ioredis`/`redis-cli` (`be615f6`). `08-benchmark-smoke-test.md`'s
`redis-benchmark` pass — the sprint's explicitly-named P2, "the first thing to cut if
the sprint runs long" — is the only item run after this retro rather than before it.

## Where effort diverged from the sprint plan's estimates

The sprint plan's hour estimates aren't visible in this retro's context in enough detail
to compare line-by-line, but two structural surprises cost more real effort than a
typical "wire up N commands" task would suggest:

- **Two of Sprint 2's plan documents shipped with code snippets that didn't compile
  as written**, because they specified which files to modify but omitted a dependency
  the snippet actually needed: `01-resp-frame-and-parser.md`'s Task 1 (`Frame` enum)
  uses `bytes::Bytes` but didn't list `crates/protocol/Cargo.toml` as a file to touch
  until Task 2; `03-command-dispatcher.md`'s Task 1 similarly omitted `bytes` from
  `crates/server/Cargo.toml`. Both were one-line fixes, folded into the same commit as
  the task they blocked (`1e58201`, `a4f9906`) rather than treated as separate bugs.
- **`futures-util = "0.3"` alone wasn't enough** for `SinkExt::send()` in
  `04-tcp-listener.md`'s `connection.rs` — the installed `futures-util 0.3.34` gates
  `Sink`/`SinkExt` behind a `sink` feature not on by default. Caught immediately at
  compile time, fixed by adding `features = ["sink"]`, folded into the TCP listener
  commit (`a399a77`) since it was needed to compile that task's own code.

## Real bugs the test suite caught during implementation

**Sprint 1** had two, both documented in `CLAUDE.md`:

- `8188de1` — `lpop`/`rpop`/`srem` wrote back an empty `List`/`Set` for a key that was
  never set, turning a read on a missing key into a phantom-key mutation. Caught by
  `missing_key_semantics_tests.rs`.
- `f2d45d7` — `hset`/`rpush`/`lpush`/`sadd` silently swallowed `WRONGTYPE` instead of
  returning it. Caught by `6c384f6`'s cross-command WRONGTYPE test matrix
  (`wrongtype_matrix_tests.rs`).

**Sprint 2** had one, exactly the arg-count panic gap `03-command-dispatcher.md` itself
flagged as a known, deliberately-deferred issue rather than a silently-shipped one: a
short command like a bare `HSET` with no key/field/value would panic the connection
task on `rest[2]` instead of returning a RESP error. Closed by `6ba2c1b`, which added a
`require_args!` macro and applied it to every arm that indexes `rest`, per the plan's
own instruction not to wire the remaining commands through before fixing it.

A second, smaller Sprint 2 issue was a stale test rather than a production bug:
`a399a77`'s `serve_closes_the_connection_cleanly_when_the_client_disconnects` asserted
`PING` returned "unknown command" — true when written, since `PING` wasn't wired yet.
Once `33da52f` wired `PING` up, the assertion went stale and the test started failing
the moment `06`'s Task 2 forced a full-workspace test run. Fixed in `cc561e5`.

## Technical debt explicitly deferred, not accidentally skipped

- **`SET EX`/`PX`** — Sprint 4 (no expiry reaper exists yet; the flags would be dead
  code). `56b65bf`'s `SET` arm returns a clear "not supported yet" error rather than
  silently ignoring the flags.
- **Multi-value `RPUSH`/`LPUSH`** — Sprint 3. `engine::commands::list::{rpush,lpush}`
  only accept one value per call; the dispatcher compensates by calling `llen` right
  after to return the length real clients expect (`56b65bf`).
- **RESP3 / `HELLO`** — not planned for this project at all, a deliberate scope
  decision (`2026-08-29-sprint-2-spec.md`), not a Sprint 2 cut. `HELLO` falls through
  to the generic unknown-command error on purpose (`33da52f`'s tests confirm this).

## What did or didn't hold up once actually implemented

From `2026-08-29-sprint-2-spec.md`'s design decisions:

- **The lib+bin crate split** (`crates/server` → `rocket_mem` lib + `rocket-mem` bin)
  held up exactly as designed — it gave `engine::commands` and `dispatcher::dispatch` a
  real non-test caller before `clippy -D warnings`'s dead-code lint could complain,
  with zero friction once `a4f9906` made the split.
- **In-process integration testing** (bind `127.0.0.1:0`, call `serve()`/`dispatch()`
  directly rather than spawning a subprocess) held up well — `connection.rs`'s 3 tests
  and `tests/integration.rs`'s 4 tests all run fast and deterministically with no
  subprocess-management flakiness.
- **The RESP3/`HELLO` "all three target clients fall back automatically" claim did
  NOT fully hold up.** Manual verification (`be615f6`) found `redis-py` 8.1.0's default
  client health check raises `ResponseError` on `HELLO` failure rather than falling
  back — it needs `protocol=2` passed explicitly. `ioredis` 6.0.0 and `redis-cli`
  behaved exactly as the spec predicted. This doesn't change the RESP3-rejection
  decision itself (the fix is client-side, one connection option), but the spec's
  blanket claim needs a footnote for current redis-py versions if Phase 3 revisits
  RESP3 scope.
