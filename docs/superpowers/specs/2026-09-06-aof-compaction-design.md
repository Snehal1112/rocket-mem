# AOF Compaction — Generation-Based Rewrite: Spec & Design

**Date:** 2026-09-06
**Status:** Approved
**Scope:** `crates/server/src/aof.rs`, `crates/server/src/dispatcher.rs`, ACL command tables, `docs/command-compatibility.md`. No new configuration surface: generation and manifest paths are always derived from the existing `aof_path`/`snapshot_path` settings, never separately configured. `crates/server/src/replication.rs` is explicitly out of scope — see the dedicated decision below on why.

**Goal:** a `BGREWRITEAOF` command that discards AOF bytes a snapshot has already made obsolete, without ever risking the "silently reconstructs the wrong dataset on restart" failure mode a naive implementation falls into.

## Problem

The AOF never shrinks. `aof.rs:275`'s own doc comment states the reason as a deliberate, load-bearing invariant: *"the 'no compaction' constraint is what makes 'byte 0 onward is always the complete history' always true, and therefore why the [full-replay] fallback is always correct rather than merely convenient."* `replay`'s only `set_len` call is corrupt-tail truncation (`aof.rs:263-267`), never a size-bound or age-bound trim. A long-lived instance's AOF grows forever, including data long since overwritten, deleted, or expired.

`SAVE` (`dispatcher.rs:2163-2183`) already writes a full point-in-time engine snapshot embedding the AOF offset current as of that snapshot (`replication.engine().snapshot(offset)`), and `recover()` (`aof.rs:278-321`) already replays only the AOF tail after that embedded offset. So the data a compaction would discard is already provably dead to recovery — the feature isn't "teach the engine to reconstruct less state," it's "physically reclaim bytes recovery was already going to skip."

### Rejected approach: in-place truncation

The obvious implementation — after a snapshot is durably written, truncate the AOF file (in place, or by rewriting it to just its tail and renaming that over the original path) — is crash-unsafe. Every ordering was checked:

- **Truncate before the new snapshot is durable:** a crash between the truncate and the snapshot write leaves the *old* snapshot (embedding old offset `M`) on disk paired with a *now-shorter* AOF. `recover()`'s existing `offset > len` fallback (`aof.rs:294-304`, "discard the snapshot, replay the full AOF from byte 0") fires, discarding a perfectly good snapshot and replaying only the truncated tail against a fresh engine — silent data loss, not a crash.
- **Truncate after the new snapshot is durable, embedding the real pre-truncation offset `N`:** now the truncated AOF (whose byte 0 used to be byte `N`) no longer matches what the snapshot's offset `N` means. The same `offset > len` fallback misfires identically, just at a different step in the sequence.

Every variant of "one file, rewritten in place, offset reinterpreted after the fact" hits this: the embedded-offset scheme's safety depends entirely on the AOF never being rewritten out from under a snapshot that names a position in it. Making rewriting safe means changing what "a position in the AOF" means, not just adding a truncate call.

## Decision: generations + a manifest, not in-place rewriting

Each rewrite produces a new **generation** `N`: a snapshot file and an AOF file that are only ever read together, never mixed with another generation's pair.

- Snapshot: `<snapshot_path>.<N>`
- AOF: `<aof_path>.<N>`
- Manifest: `<snapshot_path>.manifest` — a single line of plain text, the current generation number.

**No manifest file on disk ⇒ generation 0 ⇒ the plain `<snapshot_path>`/`<aof_path>` exactly as they are today.** No deployment that has never run `BGREWRITEAOF` ever creates a manifest, ever sees a generation-numbered filename, or changes behavior in any way — matching Sprint 8's own "zero behavior change with no configuration" rule (`../../specs/2026-08-31-sprint-8-spec.md`). Generation 0's files use today's bare paths specifically so every existing deployment's files remain valid without a migration step.

```rust
/// Reads the current generation from `<snapshot_path>.manifest`. Missing manifest ⇒ generation
/// 0, meaning "use snapshot_path/aof_path exactly as-is" — the only state every pre-compaction
/// deployment and every existing test is in, and it must require no migration.
fn read_generation(snapshot_path: &Path) -> std::io::Result<u64>;

/// Atomically (tmp + fsync + rename, same primitive as `write_snapshot_atomically`) writes `gen`
/// as the new current generation. This rename is the single commit point of a rewrite: before
/// it, generation `gen - 1`'s files are authoritative and untouched; after it, generation `gen`'s
/// are.
fn write_generation_atomically(snapshot_path: &Path, gen: u64) -> std::io::Result<()>;

/// `<base>` with `.{gen}` appended, or `<base>` unchanged for `gen == 0`.
fn generation_path(base: &Path, gen: u64) -> PathBuf;
```

## Decision: `AofWriter::rotate_to`

```rust
impl AofWriter {
    /// Flushes and fsyncs the current file, opens `new_path` (create, append), and swaps the
    /// writer thread's target to it -- acked like `fsync()`, so the caller knows the rotation
    /// completed before it proceeds. Must be called while holding `lock_for_ordering()`: that
    /// lock is already taken by every write command around "mutate the engine, then log it"
    /// (see the `order` field's doc comment), so rotating under it guarantees no append is
    /// mid-flight to the old file when the swap happens, and the very next append after the
    /// caller releases the lock lands in the new file. No new synchronization primitive needed.
    pub fn rotate_to(&self, new_path: &Path) -> std::io::Result<()>;
}
```

Implemented as a new `AofMsg::Rotate(PathBuf, mpsc::SyncSender<std::io::Result<()>>)` handled by the existing writer thread loop (`aof.rs:65-95`), mirroring `Flush`'s ack pattern. The writer thread's `path` field (used by `current_offset`) is updated to `new_path` as part of the same message so a `current_offset()` call immediately after rotation reports the new file's length, not the old one's.

## Decision: `BGREWRITEAOF` command

New, ACL-gated exactly like `SAVE` (`+bgrewriteaof` required once any ACL user exists — denied by default alongside `SAVE`/`DEBUG`/`REPLICAOF`/`CLUSTER*`, per the transport audit's existing "must-configure" list). Not added to `WRITE_COMMANDS` (`aof.rs:189-228`) — like `SAVE`, it doesn't mutate the keyspace, so it must never be logged into the very AOF it's rewriting.

```
fn handle_bgrewriteaof(aof: &AofWriter, replication: &ReplicationHandle) -> Frame
```

Sequence:

1. **Under `aof.lock_for_ordering()`** (same critical section shape as `handle_save`, `dispatcher.rs:2167-2174`): read the current generation `G` from the manifest (default 0), take `bytes = replication.engine().snapshot(0)` — offset `0`, because this snapshot pairs with a brand-new, currently-empty generation-`G+1` AOF, not the file that exists today — then `aof.rotate_to(generation_path(aof_path, G + 1))`. Release the lock.
2. **Outside the lock** (unbounded latency is fine; no writer is blocked): write `bytes` to `generation_path(snapshot_path, G + 1)` via the existing `write_snapshot_atomically` (`dispatcher.rs:2190-2204`), fsync'd and renamed into place.
3. `write_generation_atomically(snapshot_path, G + 1)` — **the commit point.** Before this rename: a crash leaves the manifest still saying `G` (or absent, meaning `0`), so `recover()` uses generation `G`'s files, completely untouched by the abandoned attempt. After this rename: `recover()` uses generation `G + 1`'s files, which are already fully and durably written by steps 1–2.
4. Best-effort cleanup: delete generation `G`'s snapshot and AOF files. A failure or a crash here is harmless — the files are simply unreferenced by the manifest and can be reclaimed opportunistically (a future rewrite's own step 4, or a startup sweep — not required for this spec's DoD).

Concurrent writes during steps 2–4 land in the new (post-rotation) AOF file exactly like any other write — they're just additional history after the new snapshot's offset-`0` baseline, replayed normally on the next recovery.

## Decision: `recover()` changes

`recover(aof_path, snapshot_path)` gains one step at the front: resolve the current generation via `read_generation(snapshot_path)`, then run its entire existing body (`aof.rs:278-321`) unchanged against `generation_path(aof_path, gen)` / `generation_path(snapshot_path, gen)` instead of the bare paths. Every existing test and code path for generation 0 is byte-for-byte identical to today, since `generation_path(base, 0) == base`.

Once any manifest exists, the bare `aof_path`/`snapshot_path` are authoritative **only** for generation 0, and only until the first `BGREWRITEAOF` cleans them up — `recover()` must always resolve paths through the manifest, never assume the bare paths are current just because they exist on disk. After generation 1 is committed, the bare-path files may still be physically present until step 4's cleanup runs (or may already be gone); either way they must be ignored once the manifest names a later generation.

## Decision: replication is untouched

Followers never read the AOF file. `PSYNC` serves a live `engine.snapshot(0)` plus the in-memory broadcast channel (`connection.rs`/`dispatcher.rs`'s fan-out hook); a follower's own crash recovery uses `recover()` exactly like a leader's, generation-aware by the same change above. No change to `replication.rs` is needed for this feature.

## Testing strategy

- **Manifest round-trip** (`aof.rs`): missing manifest ⇒ generation 0; write-then-read round trips; `generation_path` for gen 0 returns the bare path unchanged, for gen ≥ 1 appends `.{gen}`.
- **`AofWriter::rotate_to`**: old file's content is frozen at rotation; subsequent `append`s land in the new file starting at byte 0; `current_offset()` after rotation reflects the new file.
- **`BGREWRITEAOF` end-to-end**: write data, `SAVE`-equivalent baseline, `BGREWRITEAOF`, confirm generation `G+1` files exist and are correct, generation `G` files are gone, and a fresh `recover()` reconstructs identical state.
- **Crash-safety, constructed directly (no real process kill needed — same style as `aof.rs`'s existing `recover_with_a_snapshot_whose_offset_overshoots_...` tests):**
  - On-disk state frozen *before* the manifest rename (generation `G+1`'s snapshot+AOF written, manifest still says `G` or is absent) ⇒ `recover()` must reconstruct exactly generation `G`'s state, ignoring the orphaned `G+1` files.
  - On-disk state frozen *after* the manifest rename but *before* cleanup (both generations' files present, manifest says `G+1`) ⇒ `recover()` must reconstruct exactly generation `G+1`'s state.
- **Concurrent writes during rewrite**: a write issued while a rewrite is between its lock-protected step and the manifest rename must survive and be recoverable, landing in whichever generation was current at the moment it acquired `lock_for_ordering()`.
- **ACL**: `BGREWRITEAOF` denied without `+bgrewriteaof` once ACL is configured, same shape as the existing `SAVE`/`PSYNC` ACL tests.

A real kill-and-restart proof (extending Sprint 8's `scripts/chaos.sh` to occasionally issue `BGREWRITEAOF`) is valuable but not required for this spec's DoD — noted as a follow-up, not in scope.

## Definition of done

- [ ] This spec committed
- [ ] Manifest read/write + `generation_path` implemented and unit-tested
- [ ] `AofWriter::rotate_to` implemented and unit-tested
- [ ] `BGREWRITEAOF` reachable over RESP/RMP, ACL-gated like `SAVE`, excluded from `WRITE_COMMANDS`
- [ ] `recover()` is generation-aware; every existing generation-0 test passes unmodified
- [ ] Crash-safety tests for both sides of the manifest-rename commit point pass
- [ ] Concurrent-writes-during-rewrite test passes
- [ ] `docs/command-compatibility.md` and README's command table gain `BGREWRITEAOF`
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` all green

## Plan breakdown

Fine-grained on purpose — each of the following is small enough to be its own implementation plan (≤3 tasks each), per this project's TDD-plan convention:

1. Manifest read (missing ⇒ 0) + unit tests — no writer yet, just parsing.
2. Manifest atomic write + `generation_path` helper + unit tests.
3. `AofMsg::Rotate` + `AofWriter::rotate_to` + unit tests (old file frozen, new file receives appends, `current_offset` reflects the new file).
4. `handle_bgrewriteaof`'s lock-protected step (generation read, `snapshot(0)`, `rotate_to`) — unit-testable without the disk-write/manifest steps yet.
5. Wire the post-lock steps: snapshot write to the new generation path, manifest commit rename.
6. Old-generation best-effort cleanup after a successful manifest commit.
7. Register `BGREWRITEAOF` in the dispatcher's specially-handled-command set and ACL command tables (matching `SAVE`'s existing wiring); exclude from `WRITE_COMMANDS`.
8. `recover()` generation resolution — read generation, resolve paths, otherwise unchanged; confirm every existing generation-0 test still passes.
9. Crash-safety test: frozen-before-manifest-rename state.
10. Crash-safety test: frozen-after-manifest-rename-before-cleanup state.
11. Concurrent-writes-during-rewrite integration test.
12. `BGREWRITEAOF` end-to-end integration test (full round trip through a real server).
13. ACL denial test for `BGREWRITEAOF` without `+bgrewriteaof`.
14. Documentation: `docs/command-compatibility.md`, README command table.

Dependency order: 1→2 (manifest) and 3 (rotate) can proceed in parallel; 4 depends on 3; 5 depends on 2 and 4; 6 depends on 5; 7 is independent, needed before 12–13; 8 depends on 1–2; 9–10 depend on 8; 11–12 depend on 5–7; 13 depends on 7; 14 depends on everything else landing.
