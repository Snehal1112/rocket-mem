# Verbose Logging Plan 03: Credential Redaction

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `is_sensitive` and `redact_args` to `crates/server/src/logging.rs`, so that no command carrying a credential can ever have its arguments rendered into a log line.

**Architecture:** Two pure functions layered on plan 02's `fmt_value`. `is_sensitive` answers "does this command carry a secret?" as a standalone predicate so it can be tested exhaustively and reused at span-creation sites; `redact_args` is the renderer that consults it. This is the single security-critical piece of the whole logging series — the spec confines redaction policy to `crates/server` precisely so this one file is the only thing an auditor has to read.

**Tech Stack:** Rust 2021, `bytes::Bytes`.

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see "New module: `crates/server/src/logging.rs`".

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting.

**Additional constraint specific to this plan:** a failing test here is a security defect, not a style nit. If any test in this plan fails, stop and report — do not weaken the assertion to make it pass.

---

### Task 1: `is_sensitive` — which commands carry credentials

**Files:**
- Modify: `crates/server/src/logging.rs`
- Test: `crates/server/src/logging.rs` (same inline `mod tests`)

**Interfaces:**
- Consumes: nothing from plan 02 (independent of `fmt_value`).
- Produces: `pub fn is_sensitive(cmd: &str, args: &[Bytes]) -> bool`. `cmd` is the **uppercased** command name, as produced by `dispatcher.rs`'s existing `command_name_upper`. `args` are the arguments **excluding** the command name, matching the `rest` slice in `dispatcher::dispatch`. Task 2's `redact_args` consumes this; plan 07's `cmd`-span site consumes it directly.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `crates/server/src/logging.rs`. Note the `use bytes::Bytes;` — add it to the test module's imports if not already present.

```rust
    fn args(items: &[&[u8]]) -> Vec<Bytes> {
        items.iter().map(|b| Bytes::copy_from_slice(b)).collect()
    }

    #[test]
    fn auth_is_always_sensitive() {
        assert!(is_sensitive("AUTH", &args(&[b"hunter2"])));
        assert!(is_sensitive("AUTH", &args(&[b"alice", b"hunter2"])));
        // Even a malformed AUTH with no arguments stays sensitive -- the classification is
        // by command, never by whether this particular call looks well-formed.
        assert!(is_sensitive("AUTH", &args(&[])));
    }

    #[test]
    fn hello_is_sensitive_only_when_it_carries_an_auth_clause() {
        assert!(is_sensitive("HELLO", &args(&[b"3", b"AUTH", b"alice", b"hunter2"])));
        assert!(is_sensitive("HELLO", &args(&[b"3", b"auth", b"alice", b"hunter2"])));
        // A bare protocol negotiation carries no secret and stays loggable -- this is the
        // common case and losing it would blind the RESP3 upgrade log in plan 11.
        assert!(!is_sensitive("HELLO", &args(&[b"3"])));
        assert!(!is_sensitive("HELLO", &args(&[])));
    }

    #[test]
    fn acl_is_sensitive_for_setuser_and_getuser_only() {
        assert!(is_sensitive("ACL", &args(&[b"SETUSER", b"alice", b">hunter2"])));
        assert!(is_sensitive("ACL", &args(&[b"setuser", b"alice"])));
        assert!(is_sensitive("ACL", &args(&[b"GETUSER", b"alice"])));
        // ACL LIST / WHOAMI / CAT expose no credential material.
        assert!(!is_sensitive("ACL", &args(&[b"WHOAMI"])));
        assert!(!is_sensitive("ACL", &args(&[b"LIST"])));
        assert!(!is_sensitive("ACL", &args(&[])));
    }

    #[test]
    fn ordinary_data_commands_are_not_sensitive() {
        assert!(!is_sensitive("SET", &args(&[b"k", b"v"])));
        assert!(!is_sensitive("GET", &args(&[b"k"])));
        assert!(!is_sensitive("CONFIG", &args(&[b"SET", b"maxmemory", b"100"])));
    }

    #[test]
    fn a_key_named_auth_does_not_make_an_ordinary_command_sensitive() {
        // Classification keys off the command name, never off argument contents -- otherwise
        // `SET auth v` would silently stop being loggable.
        assert!(!is_sensitive("SET", &args(&[b"AUTH", b"v"])));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem logging::tests
```

Expected: FAIL to compile with `cannot find function 'is_sensitive' in this scope`.

- [ ] **Step 3: Implement `is_sensitive`**

Add to `crates/server/src/logging.rs`, above the test module:

```rust
use bytes::Bytes;

/// Whether `cmd`'s argument list carries credential material and must never be rendered into
/// a log line at any level.
///
/// `cmd` is the uppercased command name (`dispatcher::command_name_upper`'s output); `args`
/// excludes the command name itself, matching `dispatcher::dispatch`'s `rest` slice.
///
/// Classification is by command name, never by argument contents: keying off contents would
/// make `SET auth v` mysteriously unloggable, and would still miss a credential passed under
/// any other shape. `dispatcher.rs` already carries the same command-shape knowledge -- see
/// `command_key_and_arity`'s AUTH special case and `key_spec`'s comment naming the AUTH
/// plaintext password and the ACL SETUSER rule token as non-key bytes.
///
/// `CONFIG` is deliberately absent: rocket-mem's CONFIG surface exposes no credential
/// parameter (there is no `requirepass` or `masterauth`), so redacting it would guard
/// nothing. Add an arm here if that ever changes.
pub fn is_sensitive(cmd: &str, args: &[Bytes]) -> bool {
    match cmd {
        // Every form of AUTH: the one-argument password form and the two-argument
        // user+password form both carry the secret.
        "AUTH" => true,
        // Only the `HELLO <proto> AUTH <user> <pass>` form. A bare protocol negotiation is
        // the common case and stays loggable.
        "HELLO" => args.iter().any(|a| a.eq_ignore_ascii_case(b"AUTH")),
        // SETUSER carries rule tokens including `>password`; GETUSER echoes a user's rules
        // back. ACL LIST/WHOAMI/CAT expose no credential material.
        "ACL" => matches!(
            args.first(),
            Some(sub) if sub.eq_ignore_ascii_case(b"SETUSER") || sub.eq_ignore_ascii_case(b"GETUSER")
        ),
        _ => false,
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem logging::tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all tests PASS, fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/logging.rs
git commit -m "feat(logging): add is_sensitive credential classifier"
```

---

### Task 2: `redact_args` — the renderer

**Files:**
- Modify: `crates/server/src/logging.rs`
- Test: `crates/server/src/logging.rs` (same inline `mod tests`)

**Interfaces:**
- Consumes: `fmt_value` (plan 02, Task 2) and `is_sensitive` (Task 1 above).
- Produces: `pub fn redact_args(cmd: &str, args: &[Bytes], cap: usize) -> String`. Plan 08's per-command `trace!` site is the caller.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn redact_args_renders_ordinary_arguments_space_separated() {
        assert_eq!(redact_args("SET", &args(&[b"k", b"v"]), 128), "k v");
    }

    #[test]
    fn redact_args_renders_no_arguments_as_an_empty_string() {
        assert_eq!(redact_args("PING", &args(&[]), 128), "");
    }

    #[test]
    fn redact_args_applies_the_cap_to_each_argument_independently() {
        assert_eq!(
            redact_args("SET", &args(&[b"k", b"abcdefghij"]), 4),
            "k abcd…(6 more)"
        );
    }

    #[test]
    fn redact_args_escapes_control_bytes_in_arguments() {
        assert_eq!(redact_args("SET", &args(&[b"k", b"a\nb"]), 128), "k a\\x0ab");
    }

    #[test]
    fn redact_args_never_renders_an_auth_password() {
        let rendered = redact_args("AUTH", &args(&[b"alice", b"hunter2"]), 128);
        assert_eq!(rendered, "<redacted>");
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("alice"));
    }

    #[test]
    fn redact_args_never_renders_a_hello_auth_password() {
        let rendered = redact_args("HELLO", &args(&[b"3", b"AUTH", b"alice", b"hunter2"]), 128);
        assert_eq!(rendered, "<redacted>");
        assert!(!rendered.contains("hunter2"));
    }

    #[test]
    fn redact_args_never_renders_an_acl_setuser_rule_token() {
        let rendered = redact_args("ACL", &args(&[b"SETUSER", b"alice", b">hunter2"]), 128);
        assert_eq!(rendered, "<redacted>");
        assert!(!rendered.contains("hunter2"));
    }

    #[test]
    fn redact_args_redacts_the_whole_list_not_just_the_secret_argument() {
        // Partial redaction is a trap: the argument layout differs between AUTH's one- and
        // two-argument forms, so "redact the last one" would leak the password of the other.
        assert_eq!(redact_args("AUTH", &args(&[b"hunter2"]), 128), "<redacted>");
    }

    #[test]
    fn redact_args_still_renders_a_bare_hello() {
        assert_eq!(redact_args("HELLO", &args(&[b"3"]), 128), "3");
    }

    #[test]
    fn redact_args_ignores_the_cap_when_redacting() {
        // A tiny cap must not truncate the marker into something that looks like data.
        assert_eq!(redact_args("AUTH", &args(&[b"hunter2"]), 1), "<redacted>");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem logging::tests
```

Expected: FAIL to compile with `cannot find function 'redact_args' in this scope`.

- [ ] **Step 3: Implement `redact_args`**

```rust
/// The text that replaces a sensitive command's entire argument list.
const REDACTED: &str = "<redacted>";

/// Renders a command's argument list for a `trace`-level log line, or `<redacted>` when the
/// command carries credential material.
///
/// The whole list is replaced, never just the argument believed to hold the secret: AUTH's
/// one-argument and two-argument forms put the password in different positions, so any
/// positional rule would leak one form while guarding the other.
///
/// Allocates, which is acceptable because every caller sits behind a `trace`-level check --
/// `tracing`'s macros only evaluate field expressions when the callsite is enabled, so this
/// never runs at the default `info` level.
pub fn redact_args(cmd: &str, args: &[Bytes], cap: usize) -> String {
    if is_sensitive(cmd, args) {
        return REDACTED.to_string();
    }
    args.iter()
        .map(|a| fmt_value(a, cap))
        .collect::<Vec<_>>()
        .join(" ")
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem logging::tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: every test PASSES, fmt clean, clippy clean, full workspace suite green.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/logging.rs
git commit -m "feat(logging): add redact_args for trace-level argument rendering"
```

---

## Next plan

[`04-config-log-value-max-bytes.md`](04-config-log-value-max-bytes.md) — adds the `log_value_max_bytes` config field that supplies the `cap` argument these functions take.
