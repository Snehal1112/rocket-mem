# ACL Store & Bootstrap Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** an in-memory `AclStore` (create/update/delete/list/authenticate users), wired onto `ReplicationHandle` and populated at startup from the TOML config's `[[acl.users]]` bootstrap list — the full path from config file to a queryable store, with no enforcement yet (that's plan 06).

**Architecture:** `AclStore` is a `RwLock<HashMap<String, Arc<AclUser>>>` inside `crates/server/src/acl.rs`, mirroring `SlowLog`'s existing shape and lock choice in this codebase. `ReplicationHandle` gains a `pub acl: AclStore` field (mirroring its existing `pub slowlog: SlowLog` field) and a `with_acl_bootstrap` builder. `main.rs` converts `Config::acl.users` into real `AclUser`s and passes them to that builder.

**Tech Stack:** nothing new — builds entirely on plan 01 (`Config`/`AclUserConfig`) and plan 03 (`AclRule`/`AclUser`/`parse_token`/`hash_password`).

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: ACL data model, storage, and command surface" section (storage location and bootstrap paragraphs).

## Global Constraints

- `AclStore` is never persisted to the AOF or snapshot — a runtime `ACL SETUSER` (plan 08) is in-memory only, lost on restart unless it's also in the bootstrap config. This plan doesn't add any AOF/snapshot code, which is itself how that constraint is honored (there is nothing to opt out of).
- `ACL SETUSER` is incremental, real-Redis style: applying tokens to an existing user updates it in place; applying tokens to a name that doesn't exist yet creates one starting from `enabled: false, password_hash: None, rules: []` (an all-closed default an operator must explicitly open with `on`/`allcommands`/`allkeys`/etc.).
- `AclStore::is_empty()` is the fast-path check plan 06's auth gate uses to skip enforcement entirely when no ACL users are configured — it must be O(1)-ish (a lock + `HashMap::is_empty`), not a scan.

---

### Task 1: `AclStore` — CRUD, authenticate, bootstrap insert

**Files:**
- Modify: `crates/server/src/acl.rs`

**Interfaces:**
- Consumes: `AclUser`, `AclRule`, `AclToken`, `AclError`, `parse_token`, `hash_password`, `verify_password` (all plan 03).
- Produces: `pub struct AclStore { .. }` with `new`/`Default`, `is_empty`, `set_user(&self, username: &str, raw_tokens: &[bytes::Bytes]) -> Result<(), AclError>`, `del_user(&self, username: &str) -> bool`, `get_user(&self, username: &str) -> Option<Arc<AclUser>>`, `list(&self) -> Vec<Arc<AclUser>>`, `authenticate(&self, username: &str, password: &str) -> Option<Arc<AclUser>>`, `insert_bootstrap(&self, user: AclUser)`. Plan 04 Task 2 wires this onto `ReplicationHandle`; plan 06's auth gate and plan 08's admin commands call these directly.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/acl.rs — inside `mod tests`
use bytes::Bytes;

fn tokens(strs: &[&str]) -> Vec<Bytes> {
    strs.iter().map(|s| Bytes::from(s.to_string())).collect()
}

#[test]
fn a_new_store_is_empty() {
    let store = AclStore::default();
    assert!(store.is_empty());
    assert!(store.list().is_empty());
}

#[test]
fn set_user_creates_a_user_disabled_and_closed_by_default_then_applies_tokens() {
    let store = AclStore::default();
    store
        .set_user("app", &tokens(&["on", ">pw", "~app:*", "+get", "-set"]))
        .unwrap();
    let user = store.get_user("app").unwrap();
    assert!(user.enabled);
    assert!(!store.is_empty());
}

#[test]
fn set_user_is_incremental_not_replace_whole_user() {
    let store = AclStore::default();
    store.set_user("app", &tokens(&["on", "+get"])).unwrap();
    store.set_user("app", &tokens(&["+set"])).unwrap(); // adds to, doesn't reset, the existing rules
    let user = store.get_user("app").unwrap();
    let k = Bytes::from_static(b"k");
    // AllKeys was never granted, so this only proves both command grants survived, not key access.
    assert!(user.rules.contains(&AclRule::AllowCommand("GET".to_string())));
    assert!(user.rules.contains(&AclRule::AllowCommand("SET".to_string())));
}

#[test]
fn set_user_with_a_malformed_token_returns_a_syntax_error_and_does_not_partially_apply() {
    let store = AclStore::default();
    let result = store.set_user("app", &tokens(&["on", "garbage-token"]));
    assert!(result.is_err());
    assert!(store.get_user("app").is_none(), "a failed SETUSER must not create a half-applied user");
}

#[test]
fn del_user_removes_an_existing_user_and_returns_false_for_an_unknown_one() {
    let store = AclStore::default();
    store.set_user("app", &tokens(&["on"])).unwrap();
    assert!(store.del_user("app"));
    assert!(store.get_user("app").is_none());
    assert!(!store.del_user("app"));
}

#[test]
fn authenticate_succeeds_with_the_right_password_and_fails_with_the_wrong_one() {
    let store = AclStore::default();
    store.set_user("app", &tokens(&["on", ">hunter2"])).unwrap();
    assert!(store.authenticate("app", "hunter2").is_some());
    assert!(store.authenticate("app", "wrong").is_none());
}

#[test]
fn authenticate_a_nopass_user_accepts_any_password() {
    let store = AclStore::default();
    store.set_user("app", &tokens(&["on", "nopass"])).unwrap();
    assert!(store.authenticate("app", "literally-anything").is_some());
}

#[test]
fn authenticate_a_disabled_user_always_fails() {
    let store = AclStore::default();
    store.set_user("app", &tokens(&["off", "nopass"])).unwrap();
    assert!(store.authenticate("app", "anything").is_none());
}

#[test]
fn authenticate_an_unknown_username_fails() {
    let store = AclStore::default();
    assert!(store.authenticate("nobody", "anything").is_none());
}

#[test]
fn list_returns_every_user() {
    let store = AclStore::default();
    store.set_user("a", &tokens(&["on"])).unwrap();
    store.set_user("b", &tokens(&["on"])).unwrap();
    let mut names: Vec<String> = store.list().iter().map(|u| u.username.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn insert_bootstrap_adds_an_already_formed_user_directly() {
    let store = AclStore::default();
    store.insert_bootstrap(AclUser {
        username: "seed".to_string(),
        password_hash: None,
        enabled: true,
        rules: vec![AclRule::AllCommands, AclRule::AllKeys],
    });
    assert!(!store.is_empty());
    assert!(store.get_user("seed").unwrap().enabled);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib acl::tests -- --nocapture`
Expected: FAIL to compile — `AclStore` doesn't exist yet.

- [ ] **Step 3: Implement `AclStore`**

```rust
// crates/server/src/acl.rs
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// In-memory ACL users, keyed by username. A plain `std::sync::RwLock`, matching `SlowLog`'s and
/// `ReplicaRegistry`'s existing choice in this codebase: every access here is a quick map
/// read/write, never held across an `.await`. Never persisted to the AOF or snapshot -- see this
/// plan's Global Constraints.
#[derive(Default)]
pub struct AclStore {
    users: RwLock<HashMap<String, Arc<AclUser>>>,
}

impl AclStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The fast-path check plan 06's auth gate uses to skip enforcement entirely.
    pub fn is_empty(&self) -> bool {
        self.users.read().unwrap_or_else(|e| e.into_inner()).is_empty()
    }

    /// Applies `raw_tokens` (parsed via `parse_token`) on top of `username`'s existing user, or a
    /// fresh `enabled: false, password_hash: None, rules: []` default if it doesn't exist yet --
    /// real-Redis-style incremental `ACL SETUSER`. Parses every token before applying any of
    /// them, so a malformed token in the middle of the list leaves the store unchanged rather
    /// than half-applying the earlier tokens.
    pub fn set_user(&self, username: &str, raw_tokens: &[bytes::Bytes]) -> Result<(), AclError> {
        let tokens = raw_tokens
            .iter()
            .map(|t| parse_token(t))
            .collect::<Result<Vec<_>, _>>()?;
        let mut users = self.users.write().unwrap_or_else(|e| e.into_inner());
        let base = users
            .get(username)
            .map(|u| (**u).clone())
            .unwrap_or_else(|| AclUser {
                username: username.to_string(),
                password_hash: None,
                enabled: false,
                rules: Vec::new(),
            });
        let updated = apply_tokens(base, &tokens);
        users.insert(username.to_string(), Arc::new(updated));
        Ok(())
    }

    pub fn del_user(&self, username: &str) -> bool {
        self.users
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(username)
            .is_some()
    }

    pub fn get_user(&self, username: &str) -> Option<Arc<AclUser>> {
        self.users
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(username)
            .cloned()
    }

    pub fn list(&self) -> Vec<Arc<AclUser>> {
        self.users
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// `None` for: unknown username, a disabled user, or a wrong password. A `nopass` user
    /// (`password_hash: None`) authenticates with any password, including an empty one.
    pub fn authenticate(&self, username: &str, password: &str) -> Option<Arc<AclUser>> {
        let users = self.users.read().unwrap_or_else(|e| e.into_inner());
        let user = users.get(username)?;
        if !user.enabled {
            return None;
        }
        match &user.password_hash {
            None => Some(Arc::clone(user)),
            Some(hash) if verify_password(password, hash) => Some(Arc::clone(user)),
            Some(_) => None,
        }
    }

    /// Inserts an already-fully-formed `AclUser` directly, bypassing token parsing/incremental
    /// application -- used only by bootstrap loading (Task 3), which builds a complete `AclUser`
    /// from `AclUserConfig` in one step.
    pub fn insert_bootstrap(&self, user: AclUser) {
        self.users
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(user.username.clone(), Arc::new(user));
    }
}

fn apply_tokens(mut base: AclUser, tokens: &[AclToken]) -> AclUser {
    for token in tokens {
        match token {
            AclToken::On => base.enabled = true,
            AclToken::Off => base.enabled = false,
            AclToken::NoPass => base.password_hash = None,
            AclToken::Password(pw) => base.password_hash = Some(hash_password(pw)),
            AclToken::Rule(r) => base.rules.push(r.clone()),
        }
    }
    base
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib acl:: -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/src/acl.rs`.

---

### Task 2: Wire `AclStore` onto `ReplicationHandle`

**Files:**
- Modify: `crates/server/src/replication.rs`

**Interfaces:**
- Consumes: `AclStore` (Task 1).
- Produces: `pub acl: crate::acl::AclStore` field on `ReplicationHandle`, `pub fn with_acl_bootstrap(mut self, users: Vec<crate::acl::AclUser>) -> Self` builder. Plan 06's auth gate reads `replication.acl`; `main.rs` (Task 3 here) calls `with_acl_bootstrap`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/replication.rs — inside `mod tests`
#[test]
fn a_new_handle_has_an_empty_acl_store() {
    let h = ReplicationHandle::default();
    assert!(h.acl.is_empty());
}

#[test]
fn with_acl_bootstrap_populates_the_store() {
    let h = ReplicationHandle::new(Arc::new(Engine::new()), "/tmp/does-not-matter".into())
        .with_acl_bootstrap(vec![crate::acl::AclUser {
            username: "seed".to_string(),
            password_hash: None,
            enabled: true,
            rules: vec![crate::acl::AclRule::AllCommands, crate::acl::AclRule::AllKeys],
        }]);
    assert!(!h.acl.is_empty());
    assert!(h.acl.get_user("seed").unwrap().enabled);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib replication::tests::a_new_handle_has_an_empty_acl_store replication::tests::with_acl_bootstrap -- --nocapture`
Expected: FAIL to compile — no `acl` field or `with_acl_bootstrap` method yet.

- [ ] **Step 3: Add the field and builder**

In `crates/server/src/replication.rs`, add to the `ReplicationHandle` struct definition (near `pub slowlog: crate::slowlog::SlowLog`):

```rust
    /// In-memory ACL users. Empty by default -- every existing test and deployment through
    /// Sprint 7 -- populated only via `with_acl_bootstrap` (from the TOML config's
    /// `[[acl.users]]`) and at runtime via `ACL SETUSER` (plan 08). Never persisted; see
    /// ../../docs/superpowers/plans/2026-08-31-sprint-8-plans/04-acl-store-and-bootstrap-wiring.md.
    pub acl: crate::acl::AclStore,
```

In `ReplicationHandle::new`, add `acl: crate::acl::AclStore::default(),` to the struct literal (alongside the other field initializers).

Add the builder method, alongside `with_slowlog_threshold`:

```rust
    /// Seeds the ACL store from the config file's `[[acl.users]]` bootstrap list. A builder
    /// method, matching `with_aof`/`with_cluster`/`with_slowlog_threshold`'s existing pattern, so
    /// the ~25 existing `ReplicationHandle::new` call sites (all tests, none configuring ACLs)
    /// stay untouched.
    pub fn with_acl_bootstrap(self, users: Vec<crate::acl::AclUser>) -> Self {
        for user in users {
            self.acl.insert_bootstrap(user);
        }
        self
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib replication:: -- --nocapture`
Expected: all PASS, including every pre-existing `replication::tests` test (the new field's `Default`-driven initialization in `new` must not disturb any of them).

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test -p rocket-mem --lib replication::`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/src/replication.rs`.

---

### Task 3: Bootstrap conversion (`AclUserConfig` → `AclUser`) + `main.rs` wiring

**Files:**
- Modify: `crates/server/src/acl.rs`
- Modify: `crates/server/src/main.rs`

**Interfaces:**
- Consumes: `crate::config::AclUserConfig` (plan 01), `AclUser`/`parse_token`/`hash_password`/`AclError` (Task 1/plan 03), `ReplicationHandle::with_acl_bootstrap` (Task 2).
- Produces: `pub fn from_bootstrap_config(cfg: &crate::config::AclUserConfig) -> Result<AclUser, AclError>`. This closes the loop: a `rocket-mem.toml` with `[[acl.users]]` now produces real, queryable `AclStore` entries at process start.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/acl.rs — inside `mod tests`
fn cfg(username: &str, password: Option<&str>, enabled: bool, rules: &[&str]) -> crate::config::AclUserConfig {
    crate::config::AclUserConfig {
        username: username.to_string(),
        password: password.map(|p| p.to_string()),
        enabled,
        rules: rules.iter().map(|r| r.to_string()).collect(),
    }
}

#[test]
fn from_bootstrap_config_builds_a_matching_acl_user() {
    let user = from_bootstrap_config(&cfg("app", Some("pw"), true, &["~app:*", "+get", "-set"]))
        .unwrap();
    assert_eq!(user.username, "app");
    assert!(user.enabled);
    assert!(user.password_hash.is_some());
    assert!(verify_password("pw", user.password_hash.as_deref().unwrap()));
    assert_eq!(
        user.rules,
        vec![
            AclRule::KeyPattern("app:*".to_string()),
            AclRule::AllowCommand("GET".to_string()),
            AclRule::DenyCommand("SET".to_string()),
        ]
    );
}

#[test]
fn from_bootstrap_config_with_no_password_is_nopass() {
    let user = from_bootstrap_config(&cfg("nopass-user", None, true, &["allcommands", "allkeys"]))
        .unwrap();
    assert_eq!(user.password_hash, None);
}

#[test]
fn from_bootstrap_config_rejects_an_on_off_or_password_token_inside_rules() {
    // `enabled`/`password` are their own AclUserConfig fields; the `rules` list must contain
    // only rule tokens (+CMD/-CMD/~pattern/allcommands/.../allkeys), not "on"/"off"/">pw".
    assert!(from_bootstrap_config(&cfg("bad", None, true, &["on"])).is_err());
    assert!(from_bootstrap_config(&cfg("bad", None, true, &[">oops"])).is_err());
}

#[test]
fn from_bootstrap_config_rejects_a_malformed_rule_token() {
    assert!(from_bootstrap_config(&cfg("bad", None, true, &["garbage"])).is_err());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib acl::tests::from_bootstrap_config -- --nocapture`
Expected: FAIL to compile — `from_bootstrap_config` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
// crates/server/src/acl.rs
/// Converts one TOML `[[acl.users]]` entry into a fully-formed `AclUser`. `cfg.rules` must
/// contain only rule tokens (`+CMD`/`-CMD`/`~pattern`/`allcommands`/`nocommands`/`allkeys`) --
/// `enabled` and `password` are `AclUserConfig`'s own fields precisely so they don't also need
/// to appear as `on`/`off`/`>pw` tokens inside `rules`, and a `rules` entry that parses as one of
/// those (or fails to parse at all) is rejected rather than silently ignored.
pub fn from_bootstrap_config(cfg: &crate::config::AclUserConfig) -> Result<AclUser, AclError> {
    let mut rules = Vec::with_capacity(cfg.rules.len());
    for raw in &cfg.rules {
        match parse_token(raw.as_bytes())? {
            AclToken::Rule(r) => rules.push(r),
            AclToken::On | AclToken::Off | AclToken::NoPass | AclToken::Password(_) => {
                return Err(AclError::SyntaxError(raw.clone()))
            }
        }
    }
    Ok(AclUser {
        username: cfg.username.clone(),
        password_hash: cfg.password.as_deref().map(hash_password),
        enabled: cfg.enabled,
        rules,
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib acl:: -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Wire it into `main.rs`**

In `crates/server/src/main.rs`, after building `config` (plan 02, Task 3) and before constructing the `ReplicationHandle` chain, add:

```rust
let acl_users: Vec<rocket_mem::acl::AclUser> = config
    .acl
    .users
    .iter()
    .map(rocket_mem::acl::from_bootstrap_config)
    .collect::<Result<_, _>>()
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("acl bootstrap: {e}")))?;
```

Extend the existing `ReplicationHandle` builder chain with one more call:

```rust
let mut handle = rocket_mem::replication::ReplicationHandle::new(
    Arc::clone(&engine),
    snapshot_path.to_path_buf(),
)
.with_aof(Arc::clone(&aof))
.with_slowlog_threshold(slowlog_threshold)
.with_acl_bootstrap(acl_users);
```

- [ ] **Step 6: Verify a config file with bootstrap users starts cleanly**

Run:
```bash
cargo build -p rocket-mem
mkdir -p /tmp/rocket-mem-acl-smoke && cd /tmp/rocket-mem-acl-smoke
cat > rocket-mem.toml <<'EOF'
addr = "127.0.0.1:0"
rmp_addr = "127.0.0.1:0"
metrics_addr = "127.0.0.1:0"

[[acl.users]]
username = "readonly-app"
password = "pw"
enabled = true
rules = ["~app:*", "+get", "-set"]
EOF
timeout 2 /path/to/target/debug/rocket-mem --config rocket-mem.toml || true
```
Expected: starts up and prints its normal `Listening on .../RMP listening on .../Metrics on ...` lines with no `acl bootstrap:` error, then is killed by `timeout` — a clean start is the pass condition. (Substitute the real absolute path to the built binary.)

- [ ] **Step 7: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/src/acl.rs` and `crates/server/src/main.rs`.
