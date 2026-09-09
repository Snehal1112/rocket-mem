# `CONFIG REWRITE`-equivalent — Scoping Spec

**Status: scoping only — not yet broken into an implementation plan.** This spec exists to
record the investigation findings and the design risks that must be resolved before a plan can
be written, per this project's own convention of separating "we understand the problem" from
"here's the TDD task list." See `docs/superpowers/specs/2026-09-09-replicaof-config-file-spec.md`
for the sibling feature this depends on (`Config.replicaof` must exist first — this spec assumes
it does).

## Why

Real Redis's `CONFIG REWRITE` persists the server's current live configuration — including
whatever `REPLICAOF` relationship is currently active — back into `redis.conf`, so a restart
picks it back up automatically. rocket-mem has no equivalent: a live `REPLICAOF` (or `REPLICAOF
NO ONE`) changes only in-memory state (`ReplicationHandle`), never the TOML file, so a restart
after a manual promotion or re-pointing always reverts to whatever `replicaof` (if any) the file
already said — silently diverging from the operator's actual last intent.

## Investigation findings (verified against source, 2026-09-09)

- **TOML writing is entirely unprecedented in this codebase.** No `toml::to_string`/
  `toml::to_string_pretty` call exists anywhere in `crates/server/src/`, and `toml` is not even a
  direct dependency of the `rocket-mem` package (only `figment`, which only *reads* TOML).
  `Config` does derive `serde::Serialize`, but every existing use is figment's in-memory
  `Serialized::defaults(...)` merge-layer construction — never a file write.
- **Figment's layered `Config` has no provenance tracking.** By the time `load_with_cli` returns
  a `Config`, there is no per-field record of which layer (defaults, TOML file, env var, CLI
  flag) supplied its value. A naive `toml::to_string(&config)` would therefore bake an
  env-var-sourced or CLI-flag-sourced value into the file as if an operator had hand-typed it —
  silently promoting a temporary override into a permanent one on the very first rewrite. This is
  a **worse** version of a hazard real Redis's own `CONFIG REWRITE` documentation already warns
  about, because rocket-mem's config layer gives no built-in way to even ask "which layer won."
- **`rocket-mem.toml`/`rocket-mem-shard-*.toml` are heavily hand-commented.** A full
  `serde`-based re-serialize discards every comment and any commented-out example line (verified
  against this project's own `rocket-mem.toml`, which has a full field-by-field ACL doc block and
  a commented-out example user). Real Redis's own `CONFIG REWRITE` does NOT do a full
  re-serialize either — it does line-level surgical patching of only the changed directives,
  preserving surrounding comments/formatting. The Rust-ecosystem equivalent of that approach is
  `toml_edit`'s document-preserving API (parses into an editable AST that round-trips comments),
  not `serde` + the plain `toml` crate.
- **`ReplicationHandle` already exposes everything needed on the read side**: `master_addr() ->
  Option<String>` and the public `is_replica: AtomicBool` field are sufficient to derive "no
  replicaof" vs. "currently replicating from X" — no new runtime-state plumbing needed there.

## Decision: scope for a real plan (not yet written)

1. Add `crates/server/src/config_rewrite.rs` (or similar) built on `toml_edit`, not `serde`+`toml`.
2. On a `CONFIG REWRITE`-equivalent command (naming TBD — real Redis's own name, or something
   rocket-mem-specific), read the **raw TOML file** independently via `toml_edit` (not the merged
   `Config`), patch only the `replicaof`/`replicaof_auth_username`/`replicaof_auth_password` keys
   to the handle's current live values (adding them if absent, updating in place if present,
   removing if the node is no longer a replica), and write the result back — leaving every other
   key, comment, and blank line untouched.
3. Explicitly refuse (or explicitly document as intentional overwrite behavior — this is the one
   open design question a plan must resolve) the case where the *live* value came from an env var
   or CLI flag, not the file itself. The simplest safe default: only ever rewrite the file's
   `replicaof`-related keys to match `ReplicationHandle`'s current state, regardless of what
   sourced the value that got the node into that state — i.e., treat this command as "make the
   file match reality going forward," not "record how reality was achieved." This avoids needing
   provenance tracking at all, at the cost of the rewritten file not literally reflecting how the
   *current* process was configured. Flag this tradeoff explicitly to the user/reviewer before
   implementing — it is a real behavioral choice, not a detail.
4. File-write safety: write to a temp file in the same directory and atomically rename over the
   original (standard crash-safety pattern; verify no existing precedent/helper for this in the
   codebase before assuming one).

## Non-goals

- Rewriting any field other than the three `replicaof*` ones — this is not a general
  "persist everything" mechanism.
- Solving the provenance problem in general (see Decision point 3 — sidestepped, not solved).
