# Config Struct & Figment Layering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a `Config` struct that every later Sprint 8 plan (ACL bootstrap, TLS) reads fields from, loaded via `figment` with TOML-file and `ROCKET_MEM_*`-env-var layers (CLI flags land in plan 02).

**Architecture:** new `crates/server/src/config.rs`. `Config` derives `serde::Deserialize`/`Serialize` so `figment` can merge providers into it. `config::load()` builds a `Figment` from built-in defaults, merges an optional TOML file, then merges `Env::prefixed("ROCKET_MEM_")` — so every existing `ROCKET_MEM_*` variable keeps working unchanged, just layered under the file instead of being the only source.

**Tech Stack:** `figment = { version = "0.10", features = ["toml", "env"] }` (new dependency, `server` crate only).

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: config layering" section.

## Global Constraints

- No behavior change: every `ROCKET_MEM_*` env var this project reads today must keep working identically once this plan lands (main.rs migration itself is plan 02, Task 3 — this plan only builds `config.rs`, it does not wire `main.rs` yet).
- `figment`'s `Env::prefixed("ROCKET_MEM_")` provider maps `ROCKET_MEM_ADDR` → the `addr` field, `ROCKET_MEM_AOF_PATH` → `aof_path`, etc. — figment lowercases and strips the prefix automatically, so field names must exactly match the existing env var suffixes (lowercased): `addr`, `rmp_addr`, `metrics_addr`, `aof_path`, `snapshot_path`, `slowlog_threshold_micros`, `cluster_config`, `cluster_node_id`.
- A missing/absent TOML file is not an error — only an explicitly-configured-but-unreadable file (bad TOML syntax) is.

---

### Task 1: Add the `figment` dependency

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/server/Cargo.toml`

**Interfaces:**
- Consumes: nothing new.
- Produces: `figment` crate available to `server`.

- [ ] **Step 1: Add the workspace dependency**

In `Cargo.toml`'s `[workspace.dependencies]` table, add (alphabetically, after `bytes`):

```toml
figment = { version = "0.10", features = ["toml", "env"] }
```

- [ ] **Step 2: Add it to the server crate**

In `crates/server/Cargo.toml`'s `[dependencies]`, add:

```toml
figment.workspace = true
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p rocket-mem`
Expected: succeeds (figment is unused so far, which `cargo build` — not `clippy -D warnings` — tolerates; clippy's dead-code check is satisfied because `config.rs` doesn't exist yet to be linted as unused).

- [ ] **Step 4: Commit**

Use the `1-git-commit` skill/command to commit `Cargo.toml` and `crates/server/Cargo.toml`.

---

### Task 2: `Config` struct with defaults matching today's hardcoded values

**Files:**
- Create: `crates/server/src/config.rs`
- Modify: `crates/server/src/lib.rs` (add `pub mod config;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub struct Config { .. }` with a `Default` impl, and every field name/type later tasks and plans (Task 3 here; plans 04's `AclBootstrapConfig` reference; plan 10's TLS fields) build on.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/config.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_todays_hardcoded_main_rs_values() {
        let cfg = Config::default();
        assert_eq!(cfg.addr, "127.0.0.1:6379");
        assert_eq!(cfg.rmp_addr, "127.0.0.1:6380");
        assert_eq!(cfg.metrics_addr, "127.0.0.1:9121");
        assert_eq!(cfg.aof_path, "./appendonly.aof");
        assert_eq!(cfg.snapshot_path, "./dump.snapshot");
        assert_eq!(cfg.slowlog_threshold_micros, 10_000);
        assert_eq!(cfg.cluster_config, None);
        assert_eq!(cfg.cluster_node_id, None);
        assert_eq!(cfg.tls_resp_addr, None);
        assert_eq!(cfg.tls_rmp_addr, None);
        assert_eq!(cfg.tls_cert_path, None);
        assert_eq!(cfg.tls_key_path, None);
        assert!(cfg.acl.users.is_empty());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --lib config:: -- --nocapture`
Expected: FAIL to compile — `Config` doesn't exist yet.

- [ ] **Step 3: Write the struct**

```rust
// crates/server/src/config.rs — above the tests module
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Config {
    pub addr: String,
    pub rmp_addr: String,
    pub metrics_addr: String,
    pub aof_path: String,
    pub snapshot_path: String,
    pub slowlog_threshold_micros: u64,
    pub cluster_config: Option<String>,
    pub cluster_node_id: Option<String>,
    pub tls_resp_addr: Option<String>,
    pub tls_rmp_addr: Option<String>,
    pub tls_cert_path: Option<String>,
    pub tls_key_path: Option<String>,
    pub acl: AclBootstrapConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:6379".to_string(),
            rmp_addr: "127.0.0.1:6380".to_string(),
            metrics_addr: "127.0.0.1:9121".to_string(),
            aof_path: "./appendonly.aof".to_string(),
            snapshot_path: "./dump.snapshot".to_string(),
            slowlog_threshold_micros: 10_000,
            cluster_config: None,
            cluster_node_id: None,
            tls_resp_addr: None,
            tls_rmp_addr: None,
            tls_cert_path: None,
            tls_key_path: None,
            acl: AclBootstrapConfig::default(),
        }
    }
}

/// ACL bootstrap users, read from the TOML config's `[[acl.users]]` array. Converted into real
/// `acl::AclUser`s by `ReplicationHandle::with_acl_bootstrap` — see
/// ../../docs/superpowers/plans/2026-08-31-sprint-8-plans/04-acl-store-and-bootstrap-wiring.md.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct AclBootstrapConfig {
    pub users: Vec<AclUserConfig>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AclUserConfig {
    pub username: String,
    /// Plaintext in the TOML file, hashed once at load time by plan 04's bootstrap conversion.
    /// `None` means `nopass` — the user authenticates with any password or none at all.
    pub password: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Raw rule tokens, parsed the same way `ACL SETUSER`'s tokens are (plan 03).
    pub rules: Vec<String>,
}

fn default_true() -> bool {
    true
}
```

Add to `crates/server/src/lib.rs`:

```rust
pub mod config;
```

(Insert alphabetically among the existing `pub mod` lines — after `cluster`, before `connection`.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rocket-mem --lib config:: -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/src/config.rs` and `crates/server/src/lib.rs`.

---

### Task 3: `figment` TOML + env layering via `config::load_layered`

**Files:**
- Modify: `crates/server/src/config.rs`

**Interfaces:**
- Consumes: `Config` (Task 2).
- Produces: `pub fn load_layered(toml_path: Option<&std::path::Path>) -> Result<Config, figment::Error>` — takes an already-resolved TOML path (CLI-flag resolution is plan 02's job; this function's job is purely the merge order). Plan 02's `load()` calls this and then applies the CLI-override layer on top of its result.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/config.rs — inside `mod tests`
#[test]
fn load_layered_with_no_file_and_no_env_returns_defaults() {
    figment::Jail::expect_with(|_jail| {
        let cfg = load_layered(None).unwrap();
        assert_eq!(cfg.addr, "127.0.0.1:6379");
        Ok(())
    });
}

#[test]
fn load_layered_reads_the_existing_rocket_mem_env_var_names() {
    figment::Jail::expect_with(|jail| {
        jail.set_env("ROCKET_MEM_ADDR", "0.0.0.0:9999");
        jail.set_env("ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS", "5000");
        let cfg = load_layered(None).unwrap();
        assert_eq!(cfg.addr, "0.0.0.0:9999");
        assert_eq!(cfg.slowlog_threshold_micros, 5000);
        Ok(())
    });
}

#[test]
fn load_layered_applies_a_toml_file_under_the_env_layer() {
    figment::Jail::expect_with(|jail| {
        jail.create_file("rocket-mem.toml", "addr = \"127.0.0.1:1111\"\nrmp_addr = \"127.0.0.1:2222\"\n")?;
        jail.set_env("ROCKET_MEM_ADDR", "127.0.0.1:3333"); // env must win over the file
        let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
        assert_eq!(cfg.addr, "127.0.0.1:3333", "env overrides file");
        assert_eq!(cfg.rmp_addr, "127.0.0.1:2222", "file overrides default");
        Ok(())
    });
}

#[test]
fn load_layered_with_a_missing_toml_path_is_not_an_error() {
    figment::Jail::expect_with(|_jail| {
        let cfg = load_layered(Some(std::path::Path::new("does-not-exist.toml"))).unwrap();
        assert_eq!(cfg.addr, "127.0.0.1:6379"); // fell back to defaults, no error
        Ok(())
    });
}
```

(`figment::Jail` runs each closure in a temp directory with a scoped, restored-on-drop environment — the standard way `figment` itself tests env/file layering, avoiding real env-var pollution across parallel test threads.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib config::tests::load_layered -- --nocapture`
Expected: FAIL to compile — `load_layered` doesn't exist yet.

- [ ] **Step 3: Implement `load_layered`**

```rust
// crates/server/src/config.rs
use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;

/// Merges, in order (later wins): built-in defaults, an optional TOML file, then
/// `ROCKET_MEM_*` env vars. CLI-flag overrides are a further layer plan 02's `load()` applies
/// on top of this function's result — kept separate so this layer stays testable without
/// needing to construct a `clap::Parser` in every test above.
pub fn load_layered(toml_path: Option<&std::path::Path>) -> Result<Config, figment::Error> {
    let mut figment = Figment::from(Serialized::defaults(Config::default()));
    if let Some(path) = toml_path {
        if path.exists() {
            figment = figment.merge(Toml::file(path));
        }
    }
    figment = figment.merge(Env::prefixed("ROCKET_MEM_"));
    figment.extract()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib config:: -- --nocapture`
Expected: all PASS, including Task 2's test.

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test -p rocket-mem --lib config::`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/src/config.rs`.
