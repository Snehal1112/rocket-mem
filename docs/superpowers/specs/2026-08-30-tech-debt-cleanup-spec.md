# Tech Debt Cleanup Spec

## Why

`docs/phase-1-retro.md` and `docs/superpowers/specs/2026-08-30-sprint-4-spec.md` each name a
piece of technical debt that was deliberately deferred rather than accidentally skipped. This
spec fixes three of them. It is orthogonal to Sprint 4's own feature work (TTL/eviction/AOF) —
none of the three items below touch expiry or eviction code — so it is tracked as its own
cleanup unit rather than folded into the Sprint 4 plans in
`docs/superpowers/plans/2026-08-30-sprint-4-plans/`.

A fourth known item, `KEYS`'s lack of `[a-z]` ranges/negation/escaping, was originally scoped
as "deliberate, not a bug" in `docs/superpowers/specs/2026-08-29-sprint-3-spec.md`. It is
included here because the project owner asked for it explicitly when this cleanup was
scoped — treat that spec's original decision as superseded for this one capability, not
reversed project-wide.

A separate known item — AOF blocking I/O — turns out to require the same mechanism as the AOF
ordering fix below (a dedicated writer thread), so both are covered by Item 2 rather than two
separate items.

## Item 1: `RPUSH`/`LPUSH` multi-value

**Current state:** `engine::commands::list::rpush`/`lpush` (`crates/engine/src/commands/list.rs:12,31`)
each take one `Bytes` value per call. The dispatcher (`crates/server/src/dispatcher.rs:372-393`)
already loops over every value in a multi-value `RPUSH`/`LPUSH` and calls the engine function
once per value, then makes a separate `llen` call to get the final count. **This is already
functionally correct** — `RPUSH key a b c` over RESP already pushes all three values in order
and returns 3. The bug is purely internal: an N-value push takes N+1 separate shard-lock
acquisitions (N calls to `with_mut`, one call to `llen`) instead of one.

**Fix:** Change the engine functions to `rpush(engine: &Engine, key: Bytes, values: Vec<Bytes>) -> Result<usize, common::EngineError>` (and the `lpush` equivalent), pushing every value inside a single `with_mut` closure when the key already holds a list, and returning the resulting length directly. When the key doesn't exist yet, build the whole `VecDeque` from `values` in one `Engine::set` call — this preserves the existing missing-key convention (an absent key costs a `with_mut` probe plus one `set`, matching every other collection command's missing-key path; see `CLAUDE.md`'s "Missing key ≠ error" convention) without adding a third lock acquisition for the common existing-list case.

The dispatcher's `RPUSH`/`LPUSH` arms then call the engine function once with all of `rest[1..]` and use the returned length directly — the loop and the compensating `llen` call are deleted.

**Compatibility:** no other call site in the codebase calls these two functions (confirmed by
grep) besides the dispatcher and `list.rs`'s own unit tests, which are updated in the same task.

## Item 2: AOF append ordering + AOF blocking I/O (combined)

**Current state (`crates/server/src/aof.rs`, `crates/server/src/dispatcher.rs:930-996`):**
`dispatch_and_log` calls `dispatch` (which mutates the engine under a per-shard lock and
returns), then calls `aof.append(...)`, which synchronously encodes the frame and writes/fsyncs
it using a plain `std::fs::File` wrapped in `Mutex<BufWriter<File>>`. Two problems, both named
in `docs/superpowers/specs/2026-08-30-sprint-4-spec.md:110,112`:

1. **Ordering:** nothing serializes "mutate, then log" across concurrent connections. Two
   clients writing the same key can commit to the engine in one order but land in the AOF in
   the other order, breaking the "replay reproduces the original state" argument for
   command-level logging.
2. **Blocking I/O:** `aof.append`'s file write (and, under `FsyncPolicy::Always`, its
   `flush()`/`sync_data()`) run synchronously on whichever tokio worker thread is running the
   connection's async task, since `dispatch_and_log` is called directly (not via
   `spawn_blocking`) from `connection.rs`'s per-connection loop.

**Fix — one mechanism for both:**

- `AofWriter` gets a dedicated OS thread that owns the `BufWriter<File>` exclusively — no
  `Mutex<BufWriter<File>>` field. `AofWriter` communicates with it over an `std::sync::mpsc`
  channel carrying an internal `AofMsg` enum with two variants: `Append(Vec<u8>)` (fire-and-forget:
  `EverySecond`/`Never` policies) and `AppendAndFsync(Vec<u8>, mpsc::SyncSender<()>)` (used for
  `FsyncPolicy::Always` and for the existing explicit `fsync()` method — both need the caller to
  block until the write is actually durable, so both send this variant and wait on the ack).
  `AofWriter::append` picks the variant based on `self.policy`; the writer thread performs the
  actual `write_all`/`flush`/`sync_data` calls and, for `AppendAndFsync`, sends `()` down the
  provided ack channel afterward.
- `AofWriter` gains a second field, `order: std::sync::Mutex<()>`, and a new method
  `lock_for_ordering(&self) -> std::sync::MutexGuard<'_, ()>`. `dispatch_and_log` determines
  whether the incoming command is in `WRITE_COMMANDS` *before* calling `dispatch` (it already
  has the command name available from the frame at that point), and if so holds
  `aof.lock_for_ordering()` across the entire "call `dispatch`, compute frames to log, send them
  to the channel" sequence. This serializes write-command dispatch+logging globally (read
  commands are untouched and remain fully concurrent), which guarantees channel-send order
  always matches engine-mutation-commit order for writes — the property `sprint-4-spec.md:112`
  says is currently missing.

**Explicitly accepted tradeoff:** write commands lose the sharded-lock concurrency benefit
relative to each other (they're now globally serialized at the `dispatch_and_log` layer, not
just per-shard) — this is the same tradeoff the spec's own suggested fix ("funnel all AOF
writes through a single ordered channel/writer task") implies, made explicit rather than
silently accepted, and it does not change `Store`'s 16-shard design for reads or for
non-write-logged operations.

**Explicitly accepted partial fix:** under `FsyncPolicy::Always`, `append`'s caller (the
connection task, running on a tokio worker thread) still blocks waiting for the writer
thread's ack before returning — this is inherent to `Always`'s contract (the client's reply
must not precede durability) and is not something a background thread can hide. What changes
is that the actual blocking syscalls (`write`, `fsync`) move off the tokio worker thread onto
the dedicated writer thread; the calling thread's wait is now a channel `recv()`, not a direct
syscall. `EverySecond`/`Never` policies get the full benefit — `append` returns as soon as the
message is enqueued, with no blocking at all.

**Unaffected:** `replay()` reads the AOF file directly via `std::fs::read` before any
`AofWriter` is constructed, so it needs no changes. The periodic fsync loop in `connection.rs`
calls `aof.fsync()` exactly as before — its signature doesn't change.

## Item 3: `KEYS` glob completeness

**Current state:** `crates/engine/src/glob.rs`'s `glob_match` supports `*`, `?`, and `[abc]`
literal-set brackets only.

**Fix:** extend bracket-class matching to support:
- **Ranges** — `[a-z]` expands to every byte from `a` to `z` inclusive. Implemented via a new
  private helper, `class_matches(class: &[u8], c: u8) -> bool`, that scans the class body three
  bytes at a time when it sees a `-` in the middle position (`lo - hi`), and one byte at a time
  otherwise. A trailing lone `-` (e.g. `[a-]`) is treated as a literal hyphen, matching the
  common glob convention (there's no third byte to form a range with).
- **Negation** — a class body starting with `^` or `!` (either accepted, since Redis's own
  `stringmatchlen` accepts both) inverts the match: the class matches any byte *not* in the
  (possibly-range-expanded) set.
- **Escaping** — a `\` at the top level of the pattern (outside any `[...]`) makes the next
  byte match literally, consuming both bytes from the pattern in one step. This covers `\*`,
  `\?`, `\[`, and `\\`. Escaping is *not* extended inside bracket classes (e.g. `[\]]`) — out
  of scope for this fix, matching the fact that upstream Redis's own bracket-class parser has
  the same limitation.

Same public signature (`glob_match(pattern: &[u8], text: &[u8]) -> bool`), same call site
(`KEYS`'s dispatcher arm) — purely additive parsing logic plus new tests in `glob.rs`.

## Out of scope

- RESP3/`HELLO` — a deliberate, permanent scope decision (`2026-08-29-sprint-2-spec.md`), not
  debt.
- Snapshot/RDB format — doesn't exist yet in this codebase; nothing to fix.
- Sprint 4's own TTL/eviction/AOF-replay work (`docs/superpowers/plans/2026-08-30-sprint-4-plans/`)
  — untouched by this spec, tracked separately.
