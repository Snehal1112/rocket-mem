# ACL Rules & Password Hashing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the pure data/logic half of the ACL system — `AclRule`/`AclUser` types, `ACL SETUSER`-style token parsing, argon2 password hashing, and the permission-check (`is_allowed`) folding logic — with zero dependency on networking, `ReplicationHandle`, or the dispatcher.

**Architecture:** new `crates/server/src/acl.rs`, fully unit-testable in isolation. `AclUser::is_allowed` folds `rules: Vec<AclRule>` left-to-right (last matching rule wins, real-Redis style) to decide whether a command name and its keys are permitted.

**Tech Stack:** `argon2 = "0.5"` (new dependency, `server` crate only; its `password-hash` feature, enabled by default, brings `PasswordHasher`/`PasswordVerifier`/`SaltString`/`OsRng`).

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: ACL data model, storage, and command surface" section.

## Global Constraints

- No `@category` grants (`+@read`, `+@write`) — explicit `+CMDNAME`/`-CMDNAME`/`allcommands`/`nocommands` only, per the spec's own stated scope cut.
- Command names inside rules are stored uppercased (matching `dispatcher::upper_name`'s convention elsewhere in this codebase), so `is_allowed`'s comparison is case-insensitive by construction rather than by re-uppercasing on every check.
- This plan does not touch `dispatcher.rs`, `replication.rs`, or any command interception — that's plans 04, 05, 06, and 08. `acl.rs` has no dependency on any of them; `engine::glob::glob_match` (already `pub`) is the only cross-crate dependency this plan needs, for key-pattern matching.

---

### Task 1: `AclRule` + token parsing

**Files:**
- Create: `crates/server/src/acl.rs`
- Modify: `crates/server/src/lib.rs` (add `pub mod acl;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub enum AclRule { .. }`, `pub enum AclToken { .. }`, `pub fn parse_token(raw: &[u8]) -> Result<AclToken, AclError>`, `pub enum AclError { .. }` (with a `Display` impl whose message plan 08 surfaces verbatim in `ACL SETUSER`'s error reply).

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/acl.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_on_and_off() {
        assert_eq!(parse_token(b"on").unwrap(), AclToken::On);
        assert_eq!(parse_token(b"off").unwrap(), AclToken::Off);
    }

    #[test]
    fn parses_nopass() {
        assert_eq!(parse_token(b"nopass").unwrap(), AclToken::NoPass);
    }

    #[test]
    fn parses_a_password_token() {
        assert_eq!(
            parse_token(b">hunter2").unwrap(),
            AclToken::Password("hunter2".to_string())
        );
    }

    #[test]
    fn parses_allcommands_and_nocommands() {
        assert_eq!(
            parse_token(b"allcommands").unwrap(),
            AclToken::Rule(AclRule::AllCommands)
        );
        assert_eq!(
            parse_token(b"nocommands").unwrap(),
            AclToken::Rule(AclRule::NoCommands)
        );
    }

    #[test]
    fn parses_allow_and_deny_command_rules_uppercased() {
        assert_eq!(
            parse_token(b"+get").unwrap(),
            AclToken::Rule(AclRule::AllowCommand("GET".to_string()))
        );
        assert_eq!(
            parse_token(b"-Set").unwrap(),
            AclToken::Rule(AclRule::DenyCommand("SET".to_string()))
        );
    }

    #[test]
    fn parses_allkeys_and_a_key_pattern() {
        assert_eq!(parse_token(b"allkeys").unwrap(), AclToken::Rule(AclRule::AllKeys));
        assert_eq!(
            parse_token(b"~app:*").unwrap(),
            AclToken::Rule(AclRule::KeyPattern("app:*".to_string()))
        );
    }

    #[test]
    fn an_empty_command_or_pattern_token_is_a_syntax_error() {
        assert!(parse_token(b"+").is_err());
        assert!(parse_token(b"~").is_err());
    }

    #[test]
    fn an_unrecognized_token_is_a_syntax_error() {
        assert!(parse_token(b"@read").is_err()); // @categories are out of scope, see Global Constraints
        assert!(parse_token(b"garbage").is_err());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib acl:: -- --nocapture`
Expected: FAIL to compile — nothing in `acl.rs` exists yet.

- [ ] **Step 3: Implement the types and parser**

```rust
// crates/server/src/acl.rs — above the tests module
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclRule {
    AllCommands,
    NoCommands,
    AllowCommand(String), // uppercased
    DenyCommand(String),  // uppercased
    AllKeys,
    KeyPattern(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclToken {
    On,
    Off,
    Password(String),
    NoPass,
    Rule(AclRule),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclError {
    SyntaxError(String), // the offending raw token, echoed in the error message
}

impl std::fmt::Display for AclError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AclError::SyntaxError(tok) => write!(f, "ERR syntax error at '{tok}'"),
        }
    }
}

/// Parses one `ACL SETUSER`-style token. Command names in `+CMD`/`-CMD` are uppercased here so
/// every later comparison (`AclUser::is_allowed`) is a plain string-equality check, never a
/// case-insensitive one.
pub fn parse_token(raw: &[u8]) -> Result<AclToken, AclError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| AclError::SyntaxError(String::from_utf8_lossy(raw).into_owned()))?;
    match text {
        "on" => Ok(AclToken::On),
        "off" => Ok(AclToken::Off),
        "nopass" => Ok(AclToken::NoPass),
        "allcommands" => Ok(AclToken::Rule(AclRule::AllCommands)),
        "nocommands" => Ok(AclToken::Rule(AclRule::NoCommands)),
        "allkeys" => Ok(AclToken::Rule(AclRule::AllKeys)),
        _ if text.starts_with('>') && text.len() > 1 => {
            Ok(AclToken::Password(text[1..].to_string()))
        }
        _ if text.starts_with('+') && text.len() > 1 => {
            Ok(AclToken::Rule(AclRule::AllowCommand(text[1..].to_ascii_uppercase())))
        }
        _ if text.starts_with('-') && text.len() > 1 => {
            Ok(AclToken::Rule(AclRule::DenyCommand(text[1..].to_ascii_uppercase())))
        }
        _ if text.starts_with('~') && text.len() > 1 => {
            Ok(AclToken::Rule(AclRule::KeyPattern(text[1..].to_string())))
        }
        _ => Err(AclError::SyntaxError(text.to_string())),
    }
}
```

Add to `crates/server/src/lib.rs`: `pub mod acl;` (alphabetically, before `aof`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib acl:: -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/src/acl.rs` and `crates/server/src/lib.rs`.

---

### Task 2: Password hashing (argon2)

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/server/Cargo.toml`
- Modify: `crates/server/src/acl.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn hash_password(password: &str) -> String`, `pub fn verify_password(password: &str, hash: &str) -> bool` — used by plan 04's `AclStore::authenticate` and bootstrap conversion.

- [ ] **Step 1: Add the `argon2` dependency**

In `Cargo.toml`'s `[workspace.dependencies]` (alphabetically, after `argon2`... i.e. after nothing, it sorts near the top — place it right after `[workspace.dependencies]`'s opening, before `bincode`):

```toml
argon2 = "0.5"
```

In `crates/server/Cargo.toml`'s `[dependencies]`:

```toml
argon2.workspace = true
```

- [ ] **Step 2: Write the failing tests**

```rust
// crates/server/src/acl.rs — inside `mod tests`
#[test]
fn a_password_verifies_against_its_own_hash() {
    let hash = hash_password("hunter2");
    assert!(verify_password("hunter2", &hash));
}

#[test]
fn the_wrong_password_does_not_verify() {
    let hash = hash_password("hunter2");
    assert!(!verify_password("wrong", &hash));
}

#[test]
fn two_hashes_of_the_same_password_differ_by_salt() {
    let a = hash_password("hunter2");
    let b = hash_password("hunter2");
    assert_ne!(a, b, "each hash must use a fresh random salt");
    assert!(verify_password("hunter2", &a));
    assert!(verify_password("hunter2", &b));
}

#[test]
fn verify_against_a_malformed_hash_string_returns_false_not_a_panic() {
    assert!(!verify_password("anything", "not-a-real-argon2-hash"));
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib acl::tests::a_password -- --nocapture`
Expected: FAIL to compile — `hash_password`/`verify_password` don't exist yet.

- [ ] **Step 4: Implement**

```rust
// crates/server/src/acl.rs
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

/// Hashes `password` with a fresh random salt using argon2's own recommended default
/// parameters -- no manual tuning this sprint, per the spec's own scope note.
pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing with a freshly generated salt cannot fail")
        .to_string()
}

/// `false` for both a genuine mismatch and a malformed `hash` string -- a caller never needs to
/// distinguish "wrong password" from "corrupt stored hash", and both must fail closed.
pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib acl:: -- --nocapture`
Expected: all PASS.

- [ ] **Step 6: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test -p rocket-mem --lib acl::`
Expected: all green.

Use the `1-git-commit` skill/command to commit `Cargo.toml`, `crates/server/Cargo.toml`, `crates/server/src/acl.rs`.

---

### Task 3: `AclUser` + `is_allowed` permission folding

**Files:**
- Modify: `crates/server/src/acl.rs`

**Interfaces:**
- Consumes: `AclRule` (Task 1), `engine::glob::glob_match(pattern: &[u8], text: &[u8]) -> bool` (already `pub` in the `engine` crate).
- Produces: `pub struct AclUser { pub username: String, pub password_hash: Option<String>, pub enabled: bool, pub rules: Vec<AclRule> }`, `impl AclUser { pub fn is_allowed(&self, command: &str, keys: &[&bytes::Bytes]) -> bool }` — plan 06's auth gate and plan 08's admin commands both call this directly on a resolved `Arc<AclUser>`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/acl.rs — inside `mod tests`
use bytes::Bytes;

fn user(rules: Vec<AclRule>) -> AclUser {
    AclUser {
        username: "test".to_string(),
        password_hash: None,
        enabled: true,
        rules,
    }
}

#[test]
fn a_user_with_no_rules_is_allowed_nothing() {
    let u = user(vec![]);
    assert!(!u.is_allowed("GET", &[]));
}

#[test]
fn allcommands_and_allkeys_permits_any_command_and_key() {
    let u = user(vec![AclRule::AllCommands, AclRule::AllKeys]);
    let k = Bytes::from_static(b"anything");
    assert!(u.is_allowed("GET", &[&k]));
    assert!(u.is_allowed("FLUSHALL", &[]));
}

#[test]
fn the_spec_example_plus_get_minus_set_with_a_key_pattern() {
    // ["on", ">pw", "~app:*", "+get", "-set"] from the spec's own worked example
    let u = user(vec![
        AclRule::KeyPattern("app:*".to_string()),
        AclRule::AllowCommand("GET".to_string()),
        AclRule::DenyCommand("SET".to_string()),
    ]);
    let allowed_key = Bytes::from_static(b"app:1");
    let other_key = Bytes::from_static(b"other:1");
    assert!(u.is_allowed("GET", &[&allowed_key]));
    assert!(!u.is_allowed("SET", &[&allowed_key]), "explicitly denied");
    assert!(!u.is_allowed("GET", &[&other_key]), "key outside the pattern");
    assert!(!u.is_allowed("DEL", &[&allowed_key]), "never granted");
}

#[test]
fn a_later_rule_overrides_an_earlier_one_for_the_same_command() {
    let u = user(vec![
        AclRule::AllowCommand("GET".to_string()),
        AclRule::DenyCommand("GET".to_string()), // revokes it again
        AclRule::AllKeys,
    ]);
    assert!(!u.is_allowed("GET", &[]));
}

#[test]
fn allcommands_then_a_later_deny_still_denies_that_one_command() {
    let u = user(vec![
        AclRule::AllCommands,
        AclRule::DenyCommand("FLUSHALL".to_string()),
        AclRule::AllKeys,
    ]);
    assert!(!u.is_allowed("FLUSHALL", &[]));
    assert!(u.is_allowed("GET", &[]));
}

#[test]
fn a_keyless_command_needs_no_key_rule_at_all() {
    let u = user(vec![AclRule::AllowCommand("PING".to_string())]); // no AllKeys/KeyPattern rule at all
    assert!(u.is_allowed("PING", &[]));
}

#[test]
fn a_command_with_keys_needs_every_key_to_match_some_key_rule() {
    let u = user(vec![
        AclRule::AllCommands,
        AclRule::KeyPattern("app:*".to_string()),
    ]);
    let ok = Bytes::from_static(b"app:1");
    let bad = Bytes::from_static(b"other:1");
    assert!(u.is_allowed("MGET", &[&ok, &ok]));
    assert!(!u.is_allowed("MGET", &[&ok, &bad]), "one key outside the pattern denies the whole command");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib acl::tests -- --nocapture`
Expected: FAIL to compile — `AclUser`/`is_allowed` don't exist yet.

- [ ] **Step 3: Implement**

```rust
// crates/server/src/acl.rs
#[derive(Debug, Clone)]
pub struct AclUser {
    pub username: String,
    pub password_hash: Option<String>,
    pub enabled: bool,
    pub rules: Vec<AclRule>,
}

impl AclUser {
    /// Folds `self.rules` left-to-right: the last rule that matches a given question (command
    /// allowed? this key allowed?) wins, matching real Redis's own rule-order semantics. A
    /// command with no keys needs only the command check; a command with keys additionally
    /// needs every one of them to match at least one key rule.
    pub fn is_allowed(&self, command: &str, keys: &[&bytes::Bytes]) -> bool {
        let mut command_allowed = false;
        for rule in &self.rules {
            match rule {
                AclRule::AllCommands => command_allowed = true,
                AclRule::NoCommands => command_allowed = false,
                AclRule::AllowCommand(c) if c == command => command_allowed = true,
                AclRule::DenyCommand(c) if c == command => command_allowed = false,
                _ => {}
            }
        }
        if !command_allowed {
            return false;
        }
        keys.iter().all(|key| self.key_allowed(key))
    }

    fn key_allowed(&self, key: &[u8]) -> bool {
        let mut allowed = false;
        for rule in &self.rules {
            match rule {
                AclRule::AllKeys => allowed = true,
                AclRule::KeyPattern(p) if engine::glob::glob_match(p.as_bytes(), key) => {
                    allowed = true
                }
                _ => {}
            }
        }
        allowed
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib acl:: -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test -p rocket-mem --lib acl::`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/src/acl.rs`.
