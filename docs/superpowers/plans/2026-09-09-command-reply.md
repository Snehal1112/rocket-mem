# Real `COMMAND` Replies Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `crates/server/src/dispatcher.rs`'s `"COMMAND" => Frame::Array(vec![])` stub with a
real per-command metadata reply (plus `COMMAND COUNT`/`COMMAND INFO`), so RESP clients that build
local command-routing metadata from `COMMAND` — concretely `go-redis/v9`'s `ClusterClient` — stop
treating every rocket-mem command as unknown.

**Architecture:** One new helper function, `command_info_entry`, builds the 6-element
`[name, arity, flags, first_key, last_key, step]` `Frame::Array` for a single command, deriving
everything from three tables that already exist and are already each other's source of truth for a
related concern: `KNOWN_COMMANDS_LOWER` (the command's own name), `key_spec` (key
position/arity), and `aof::WRITE_COMMANDS` (the read/write flag). Bare `COMMAND` maps this helper
over every entry in `KNOWN_COMMANDS_LOWER`; `COMMAND COUNT` and `COMMAND INFO` reuse the same
helper.

**Tech Stack:** Rust, the existing `Frame`/`Bytes` types from the `protocol`/`bytes` crates already
used throughout `dispatcher.rs`.

**Spec:** `docs/superpowers/specs/2026-09-09-command-reply-spec.md`

## Global Constraints

- No wire-protocol change beyond `COMMAND`'s own reply payload — no new command support, no change
  to `key_spec`, `command_keys`, or `aof::WRITE_COMMANDS` (this plan is a new *consumer* of all
  three, never a modifier).
- The 6-element RESP array shape is complete and correct on its own — do not add a 7th or 10th
  field (ACL-flags, tips, key-specs, subcommand arrays). See the spec's Evidence section for why.
- `flags` is always a one-element array: `["write"]` or `["readonly"]`. No other flag string.
- `arity`/`first_key`/`last_key`/`step` are derived *only* from `key_spec`, per the table in the
  spec's Decision section — never hand-authored per command, and never mined from each command's
  own `require_args!` call site.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` must all pass before every commit, per `CLAUDE.md`.
- Comments are short, complete sentences ending in periods, matching this file's existing style
  (see `key_spec`'s and `KNOWN_COMMANDS_LOWER`'s doc comments for the house voice).

---

## Task 1: `command_info_entry` helper and bare `COMMAND`

**Files:**
- Modify: `crates/server/src/dispatcher.rs:1013` (the `"COMMAND"` match arm)
- Modify: `crates/server/src/dispatcher.rs` (new helper function, placed directly after
  `KNOWN_COMMANDS_LOWER`'s closing `];` at line 2917, before `dispatch_and_log` at line 2934 —
  same neighborhood as `metric_label`, which already reads `KNOWN_COMMANDS_LOWER`)
- Modify: `crates/server/src/dispatcher.rs:4761-4768` (replace the now-stale
  `command_replies_with_an_empty_array_rather_than_erroring` test)
- Test: `crates/server/src/dispatcher.rs` (inline `#[cfg(test)] mod tests`, same file — this
  codebase keeps unit tests in-file; do not create a separate test file)

**Interfaces:**
- Produces: `fn command_info_entry(name_lower: &str) -> Frame` — `name_lower` must be a member of
  `KNOWN_COMMANDS_LOWER` (lowercase). Returns a `Frame::Array` of exactly 6 elements:
  `[Frame::Bulk(name), Frame::Integer(arity), Frame::Array(flags), Frame::Integer(first_key), Frame::Integer(last_key), Frame::Integer(step)]`.
  Task 2 calls this directly for `COMMAND INFO`.

- [ ] **Step 1: Write the failing tests**

Replace the stale test at `crates/server/src/dispatcher.rs:4761-4768`:

```rust
    #[test]
    fn command_replies_with_full_command_metadata() {
        let engine = Engine::new();
        let Frame::Array(entries) = dispatch(&engine, cmd(&[b"COMMAND"]), &mut Protocol::default(), 1)
        else {
            panic!("COMMAND must reply with an array");
        };
        assert_eq!(entries.len(), KNOWN_COMMANDS_LOWER.len());
        for entry in &entries {
            let Frame::Array(fields) = entry else {
                panic!("each COMMAND entry must be an array");
            };
            assert_eq!(fields.len(), 6, "each COMMAND entry must have exactly 6 fields");
            let Frame::Bulk(name) = &fields[0] else {
                panic!("field 0 must be the command name");
            };
            let name = std::str::from_utf8(name).unwrap();
            assert!(
                KNOWN_COMMANDS_LOWER.contains(&name),
                "{name} is not in KNOWN_COMMANDS_LOWER"
            );
        }
    }
```

Add these new tests directly after it, in the same `mod tests` block:

```rust
    #[test]
    fn command_info_entry_for_a_keyless_command() {
        let Frame::Array(fields) = command_info_entry("ping") else {
            panic!("expected an array");
        };
        assert_eq!(fields[0], Frame::Bulk(Bytes::from_static(b"ping")));
        assert_eq!(fields[1], Frame::Integer(-1)); // key_spec::None -> k=0 -> arity -(0+1)
        assert_eq!(fields[2], Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"readonly"))]));
        assert_eq!(fields[3], Frame::Integer(0)); // first_key
        assert_eq!(fields[4], Frame::Integer(0)); // last_key
        assert_eq!(fields[5], Frame::Integer(0)); // step
    }

    #[test]
    fn command_info_entry_for_a_single_key_write() {
        let Frame::Array(fields) = command_info_entry("set") else {
            panic!("expected an array");
        };
        assert_eq!(fields[1], Frame::Integer(-2)); // key_spec::First -> k=1 -> arity -(1+1)
        assert_eq!(fields[2], Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"write"))]));
        assert_eq!(fields[3], Frame::Integer(1));
        assert_eq!(fields[4], Frame::Integer(1));
        assert_eq!(fields[5], Frame::Integer(1));
    }

    #[test]
    fn command_info_entry_for_key_spec_all() {
        let Frame::Array(fields) = command_info_entry("del") else {
            panic!("expected an array");
        };
        assert_eq!(fields[1], Frame::Integer(-2)); // key_spec::All -> k=1 -> arity -(1+1)
        assert_eq!(fields[2], Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"write"))]));
        assert_eq!(fields[3], Frame::Integer(1));
        assert_eq!(fields[4], Frame::Integer(-1));
        assert_eq!(fields[5], Frame::Integer(1));
    }

    #[test]
    fn command_info_entry_for_key_spec_every_other() {
        let Frame::Array(fields) = command_info_entry("mset") else {
            panic!("expected an array");
        };
        assert_eq!(fields[1], Frame::Integer(-3)); // key_spec::EveryOther -> k=2 -> arity -(2+1)
        assert_eq!(fields[3], Frame::Integer(1));
        assert_eq!(fields[4], Frame::Integer(-1));
        assert_eq!(fields[5], Frame::Integer(2));
    }

    #[test]
    fn command_info_entry_for_key_spec_second() {
        let Frame::Array(fields) = command_info_entry("memory") else {
            panic!("expected an array");
        };
        assert_eq!(fields[1], Frame::Integer(-3)); // key_spec::Second -> k=2 -> arity -(2+1)
        assert_eq!(fields[3], Frame::Integer(2));
        assert_eq!(fields[4], Frame::Integer(2));
        assert_eq!(fields[5], Frame::Integer(1));
    }

    #[test]
    fn every_command_info_entry_has_negative_arity() {
        for name in KNOWN_COMMANDS_LOWER {
            let Frame::Array(fields) = command_info_entry(name) else {
                panic!("expected an array for {name}");
            };
            let Frame::Integer(arity) = fields[1] else {
                panic!("field 1 must be an integer for {name}");
            };
            assert!(arity < 0, "{name} reported a non-negative arity: {arity}");
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem command_info_entry 2>&1 | head -40` and
`cargo test -p rocket-mem command_replies_with_full_command_metadata 2>&1 | head -40`
Expected: FAIL to compile — `command_info_entry` doesn't exist yet, and the old
`command_replies_with_an_empty_array_rather_than_erroring` test name no longer exists (replaced).

- [ ] **Step 3: Implement `command_info_entry` and wire it into `"COMMAND"`**

Add this function directly after `KNOWN_COMMANDS_LOWER`'s closing `];` (line 2917), before the
`dispatch_and_log` doc comment:

```rust
/// Builds one `COMMAND`/`COMMAND INFO` reply entry for `name_lower`, a lowercase member of
/// `KNOWN_COMMANDS_LOWER`. Every field is derived from `key_spec` and `aof::WRITE_COMMANDS` --
/// tables that already exist for CROSSSLOT enforcement and AOF replay respectively -- so this
/// function introduces no new per-command classification data. See
/// `docs/superpowers/specs/2026-09-09-command-reply-spec.md` for why a 6-element reply is
/// sufficient and why `arity` is reported as a lower bound rather than an exact value.
fn command_info_entry(name_lower: &str) -> Frame {
    let name_upper = name_lower.to_ascii_uppercase();
    let (first_key, last_key, step, key_count) = match key_spec(name_upper.as_str()) {
        KeySpec::None => (0, 0, 0, 0),
        KeySpec::First => (1, 1, 1, 1),
        KeySpec::Second => (2, 2, 1, 2),
        KeySpec::All => (1, -1, 1, 1),
        KeySpec::EveryOther => (1, -1, 2, 2),
    };
    let arity = -(key_count + 1);
    let flag = if crate::aof::WRITE_COMMANDS.contains(&name_upper.as_str()) {
        "write"
    } else {
        "readonly"
    };
    Frame::Array(vec![
        Frame::Bulk(Bytes::from(name_lower.to_string())),
        Frame::Integer(arity),
        Frame::Array(vec![Frame::Bulk(Bytes::from_static(flag.as_bytes()))]),
        Frame::Integer(first_key),
        Frame::Integer(last_key),
        Frame::Integer(step),
    ])
}
```

Replace the `"COMMAND"` match arm at `dispatcher.rs:1013`:

```rust
        "COMMAND" => {
            if rest.is_empty() {
                Frame::Array(KNOWN_COMMANDS_LOWER.iter().map(|n| command_info_entry(n)).collect())
            } else {
                Frame::Array(vec![]) // subcommands (COUNT, INFO, ...) land in Task 2
            }
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem command_info_entry command_replies_with_full_command_metadata every_command_info_entry -- --nocapture`
Expected: PASS, all 7 new/replaced tests green.

- [ ] **Step 5: Run the full workspace gate**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all three clean/passing, no regressions elsewhere (in particular
`command_keys_is_empty_for_commands_that_take_no_key` at `dispatcher.rs:9214`, which sends
`cmd(&[b"COMMAND"])` through `command_keys` — unrelated to `dispatch`'s reply, but confirm it
still passes since it exercises the same command name).

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(command): reply with real per-command metadata instead of an empty array"
```

---

## Task 2: `COMMAND COUNT` and `COMMAND INFO`

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (the `"COMMAND"` match arm from Task 1, replacing its
  `else` branch)
- Test: `crates/server/src/dispatcher.rs` (same `mod tests` block as Task 1)

**Interfaces:**
- Consumes: `command_info_entry(name_lower: &str) -> Frame` (Task 1).
- Produces: no new function — this task only extends the `"COMMAND"` match arm's subcommand
  handling. Nothing later depends on new names from this task.

- [ ] **Step 1: Write the failing tests**

Add to the same `mod tests` block:

```rust
    #[test]
    fn command_count_returns_the_total_known_command_count() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, cmd(&[b"COMMAND", b"COUNT"]), &mut Protocol::default(), 1),
            Frame::Integer(KNOWN_COMMANDS_LOWER.len() as i64)
        );
    }

    #[test]
    fn command_info_returns_entries_for_known_names_and_nil_for_unknown() {
        let engine = Engine::new();
        let Frame::Array(entries) = dispatch(
            &engine,
            cmd(&[b"COMMAND", b"INFO", b"get", b"not-a-real-command"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("COMMAND INFO must reply with an array");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], command_info_entry("get"));
        assert_eq!(entries[1], Frame::Null);
    }

    #[test]
    fn command_info_is_case_insensitive() {
        let engine = Engine::new();
        let Frame::Array(entries) = dispatch(
            &engine,
            cmd(&[b"COMMAND", b"INFO", b"GeT"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("COMMAND INFO must reply with an array");
        };
        assert_eq!(entries[0], command_info_entry("get"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem command_count_returns command_info_returns command_info_is_case_insensitive`
Expected: FAIL — `COMMAND COUNT` and `COMMAND INFO` still fall into Task 1's `else` branch and
return an empty array, so `Frame::Integer(...)` / real entries are never produced.

- [ ] **Step 3: Implement the subcommands**

Replace the `else` branch added in Task 1:

```rust
        "COMMAND" => {
            if rest.is_empty() {
                Frame::Array(KNOWN_COMMANDS_LOWER.iter().map(|n| command_info_entry(n)).collect())
            } else {
                let subcommand = String::from_utf8_lossy(&rest[0]).to_ascii_uppercase();
                match subcommand.as_str() {
                    "COUNT" => Frame::Integer(KNOWN_COMMANDS_LOWER.len() as i64),
                    "INFO" => Frame::Array(
                        rest[1..]
                            .iter()
                            .map(|arg| {
                                let name_lower = String::from_utf8_lossy(arg).to_ascii_lowercase();
                                if KNOWN_COMMANDS_LOWER.contains(&name_lower.as_str()) {
                                    command_info_entry(&name_lower)
                                } else {
                                    Frame::Null
                                }
                            })
                            .collect(),
                    ),
                    _ => Frame::Array(vec![]),
                }
            }
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem command_count_returns command_info_returns command_info_is_case_insensitive -- --nocapture`
Expected: PASS, all 3 new tests green.

- [ ] **Step 5: Run the full workspace gate**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all three clean/passing.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(command): add COMMAND COUNT and COMMAND INFO subcommands"
```

---

## Final Verification

- [ ] `cargo build --workspace` succeeds.
- [ ] `cargo test --workspace` passes (full suite, not just the new tests).
- [ ] `cargo fmt --all -- --check` is clean.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- [ ] Manually confirm with `redis-cli` (or `nc`) against a locally-running rocket-mem instance:
      `COMMAND` returns 91 entries, `COMMAND COUNT` returns `91`, `COMMAND INFO get bogus` returns
      one real entry and one nil.
- [ ] Note for the human operator (not an automated step): once this ships, re-run RocketVault's
      live rocket-mem-cluster integration suite and confirm the
      `"info for cmd=... not found"` log line from `go-redis`'s `ClusterClient` no longer appears.
