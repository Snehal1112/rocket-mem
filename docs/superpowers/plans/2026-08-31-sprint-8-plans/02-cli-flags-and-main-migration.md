# CLI Flags & main.rs Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** finish config layering with a `clap`-based CLI-flag layer (CLI > env > file > default, the production plan's own named precedence test), then migrate `main.rs` off direct `std::env::var` calls onto `config::load()`.

**Architecture:** a `Cli` struct (all-`Option` fields, `clap::Parser`) whose `Some(_)` fields are merged on top of plan 01's `load_layered` result via one more `figment` `Serialized` layer. `main.rs` calls the resulting `config::load()` once at startup and reads every setting off the returned `Config` instead of calling `std::env::var` directly.

**Tech Stack:** `clap = { version = "4", features = ["derive"] }` (new dependency, `server` crate only).

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: config layering" section.

## Global Constraints

- Precedence, from lowest to highest: built-in defaults < TOML file < `ROCKET_MEM_*` env vars < CLI flags. Plan 01 already proved file-under-env; this plan's precedence test must prove CLI-over-env, the higher tier the production plan's own example test names explicitly.
- `main.rs`'s migration must not change observable behavior for any existing test or deployment that only sets env vars — this is verified by the full workspace test suite staying green (those tests spawn the server via library functions, not `main.rs` itself, so the real regression risk is `main.rs` silently reading a different value than before; Task 3's manual field-by-field diff against the pre-migration code is what catches that).

---

### Task 1: `Cli` struct (clap) + CLI-override layer

**Files:**
- Modify: `crates/server/Cargo.toml`
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/server/src/config.rs`

**Interfaces:**
- Consumes: `Config`, `load_layered` (plan 01).
- Produces: `pub fn load() -> Result<Config, figment::Error>` — the full four-layer precedence chain; this is the function `main.rs` (Task 3) calls.

- [ ] **Step 1: Add the `clap` dependency**

In `Cargo.toml`'s `[workspace.dependencies]` (alphabetically, after `bytes`, before `bincode`... actually `clap` sorts before `bytes` alphabetically, so place it first):

```toml
clap = { version = "4", features = ["derive"] }
```

In `crates/server/Cargo.toml`'s `[dependencies]`:

```toml
clap.workspace = true
```

- [ ] **Step 2: Write the failing precedence test**

```rust
// crates/server/src/config.rs — inside `mod tests`
#[test]
fn cli_flag_overrides_env_var_overrides_file_overrides_default() {
    figment::Jail::expect_with(|jail| {
        jail.create_file(
            "rocket-mem.toml",
            "addr = \"127.0.0.1:1111\"\nslowlog_threshold_micros = 2000\n",
        )?;
        jail.set_env("ROCKET_MEM_ADDR", "127.0.0.1:2222"); // beats the file
        jail.set_env("ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS", "3000"); // beats the file, not overridden by CLI below

        let cli = Cli::parse_from([
            "rocket-mem",
            "--config",
            "rocket-mem.toml",
            "--addr",
            "127.0.0.1:4444", // beats the env var
        ]);
        let cfg = load_with_cli(cli).unwrap();
        assert_eq!(cfg.addr, "127.0.0.1:4444", "CLI beats env");
        assert_eq!(cfg.slowlog_threshold_micros, 3000, "env beats file when CLI doesn't set it");
        Ok(())
    });
}

#[test]
fn cli_flags_left_unset_do_not_override_lower_layers() {
    figment::Jail::expect_with(|jail| {
        jail.set_env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:5555");
        let cli = Cli::parse_from(["rocket-mem"]); // no flags at all
        let cfg = load_with_cli(cli).unwrap();
        assert_eq!(cfg.rmp_addr, "127.0.0.1:5555", "unset CLI flag must not clobber the env value with None/default");
        Ok(())
    });
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib config::tests::cli -- --nocapture`
Expected: FAIL to compile — `Cli`/`load_with_cli` don't exist yet.

- [ ] **Step 4: Implement `Cli` and `load_with_cli`**

```rust
// crates/server/src/config.rs
#[derive(clap::Parser, Debug)]
#[command(name = "rocket-mem")]
pub struct Cli {
    /// Path to a TOML config file. Not read via env/CLI layering itself — it names which file
    /// `load_layered` merges, so it's resolved before any other layer applies.
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
    #[arg(long)]
    pub addr: Option<String>,
    #[arg(long)]
    pub rmp_addr: Option<String>,
    #[arg(long)]
    pub metrics_addr: Option<String>,
    #[arg(long)]
    pub aof_path: Option<String>,
    #[arg(long)]
    pub snapshot_path: Option<String>,
    #[arg(long)]
    pub slowlog_threshold_micros: Option<u64>,
    #[arg(long)]
    pub cluster_config: Option<String>,
    #[arg(long)]
    pub cluster_node_id: Option<String>,
    #[arg(long)]
    pub tls_resp_addr: Option<String>,
    #[arg(long)]
    pub tls_rmp_addr: Option<String>,
    #[arg(long)]
    pub tls_cert_path: Option<String>,
    #[arg(long)]
    pub tls_key_path: Option<String>,
}

/// `Serialized::defaults` embeds every field including the unset `None`s, which would make an
/// unset CLI flag overwrite a lower layer's real value with `null` on merge -- exactly what
/// `cli_flags_left_unset_do_not_override_lower_layers` above guards against. Building a
/// `serde_json::Map` by hand and only inserting `Some(_)` fields is what avoids that: an unset
/// flag is simply absent from the merged provider, so figment's merge leaves the lower layer's
/// value untouched.
fn cli_overrides(cli: &Cli) -> figment::providers::Serialized<std::collections::BTreeMap<&'static str, String>> {
    let mut map = std::collections::BTreeMap::new();
    macro_rules! set {
        ($field:ident) => {
            if let Some(v) = &cli.$field {
                map.insert(stringify!($field), v.to_string());
            }
        };
    }
    set!(addr);
    set!(rmp_addr);
    set!(metrics_addr);
    set!(aof_path);
    set!(snapshot_path);
    set!(cluster_config);
    set!(cluster_node_id);
    set!(tls_resp_addr);
    set!(tls_rmp_addr);
    set!(tls_cert_path);
    set!(tls_key_path);
    if let Some(v) = cli.slowlog_threshold_micros {
        map.insert("slowlog_threshold_micros", v.to_string());
    }
    figment::providers::Serialized::defaults(map)
}

pub fn load_with_cli(cli: Cli) -> Result<Config, figment::Error> {
    let base = load_layered(cli.config.as_deref())?;
    Figment::from(Serialized::defaults(base))
        .merge(cli_overrides(&cli))
        .extract()
}

/// Parses `std::env::args()` and applies the full four-layer precedence:
/// defaults < TOML file < `ROCKET_MEM_*` env vars < CLI flags. This is what `main.rs` calls.
pub fn load() -> Result<Config, figment::Error> {
    load_with_cli(<Cli as clap::Parser>::parse())
}
```

Add `use clap::Parser;` near the top of `config.rs` (needed for `Cli::parse_from` in tests and `<Cli as clap::Parser>::parse()` above — or import the trait and call `Cli::parse()` directly).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib config:: -- --nocapture`
Expected: all PASS, including plan 01's tests.

- [ ] **Step 6: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test -p rocket-mem --lib config::`
Expected: all green.

Use the `1-git-commit` skill/command to commit `Cargo.toml`, `crates/server/Cargo.toml`, `crates/server/src/config.rs`.

---

### Task 2: `AclBootstrapConfig`/`AclUserConfig` TOML round-trip test

**Files:**
- Modify: `crates/server/src/config.rs`

**Interfaces:**
- Consumes: `AclBootstrapConfig`, `AclUserConfig` (plan 01, Task 2).
- Produces: nothing new — this is a coverage-only task confirming the `[[acl.users]]` TOML shape the spec documents actually deserializes, before plan 04 depends on it.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/config.rs — inside `mod tests`
#[test]
fn acl_bootstrap_users_parse_from_toml() {
    figment::Jail::expect_with(|jail| {
        jail.create_file(
            "rocket-mem.toml",
            r#"
[[acl.users]]
username = "readonly-app"
password = "pw"
enabled = true
rules = ["~app:*", "+get", "-set"]

[[acl.users]]
username = "nopass-user"
rules = ["allcommands", "allkeys"]
"#,
        )?;
        let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
        assert_eq!(cfg.acl.users.len(), 2);
        assert_eq!(cfg.acl.users[0].username, "readonly-app");
        assert_eq!(cfg.acl.users[0].password.as_deref(), Some("pw"));
        assert!(cfg.acl.users[0].enabled);
        assert_eq!(cfg.acl.users[0].rules, vec!["~app:*", "+get", "-set"]);
        assert_eq!(cfg.acl.users[1].password, None);
        assert!(cfg.acl.users[1].enabled, "enabled defaults to true when omitted");
        Ok(())
    });
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p rocket-mem --lib config::tests::acl_bootstrap_users_parse_from_toml -- --nocapture`
Expected: PASS immediately — `AclBootstrapConfig`/`AclUserConfig` already derive `Deserialize` from plan 01, Task 2. This step is confirmation, not a red-bar step; if it fails, the fix is in plan 01's struct definitions, not here.

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/src/config.rs`.

---

### Task 3: Migrate `main.rs` off `std::env::var` onto `config::load()`

**Files:**
- Modify: `crates/server/src/main.rs`

**Interfaces:**
- Consumes: `config::load() -> Result<Config, figment::Error>`.
- Produces: nothing new — `main.rs`'s observable behavior (bind addresses, file paths, thresholds) is unchanged for anyone who only sets env vars, per this plan's Global Constraints.

- [ ] **Step 1: Replace every `std::env::var` read with a `Config` field read**

In `crates/server/src/main.rs`, at the top of `main()`, replace the whole block of `std::env::var(...)` calls (currently: `addr`, `aof_path`, `snapshot_path`, `slowlog_threshold`, the cluster-config pair, `metrics_addr`, `rmp_addr`) with:

```rust
let config = rocket_mem::config::load().map_err(|e| {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("config error: {e}"))
})?;

let addr = config.addr.clone();
let aof_path = std::path::PathBuf::from(&config.aof_path);
let aof_path = aof_path.as_path();
let snapshot_path = std::path::PathBuf::from(&config.snapshot_path);
let snapshot_path = snapshot_path.as_path();
let slowlog_threshold = std::time::Duration::from_micros(config.slowlog_threshold_micros);
```

Replace the cluster-mode `match` block's two `std::env::var` calls:

```rust
let cluster = match (&config.cluster_config, &config.cluster_node_id) {
    (Some(path), Some(node_id)) => {
        let config =
            rocket_mem::cluster::ClusterConfig::load(std::path::Path::new(path), node_id)?;
        // ... unchanged println!/Some(Arc::new(config)) body
    }
    (Some(_), None) => {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cluster_config is set but cluster_node_id is not",
        ))
    }
    (None, Some(_)) => {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cluster_node_id is set but cluster_config is not",
        ))
    }
    (None, None) => None,
};
```

Replace `metrics_addr`'s and `rmp_addr`'s `std::env::var` lines with `let metrics_addr = config.metrics_addr.clone();` and `let rmp_addr = config.rmp_addr.clone();` respectively, at their existing points of use.

- [ ] **Step 2: Verify a plain build with no config file/env vars behaves identically**

Run: `cargo build -p rocket-mem && ROCKET_MEM_ADDR=127.0.0.1:0 ROCKET_MEM_RMP_ADDR=127.0.0.1:0 ROCKET_MEM_METRICS_ADDR=127.0.0.1:0 timeout 2 ./target/debug/rocket-mem || true`
Expected: starts up, prints the same `Listening on .../RMP listening on .../Metrics on ...` lines as before this change (using the `:0` ephemeral-port trick so nothing collides with a real running instance), then is killed by `timeout` after 2s — a clean start is the pass condition, not the timeout kill itself.

- [ ] **Step 3: Run the full workspace suite**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green — every existing test spawns the server via library functions (`serve`, `rmp_connection::serve`, etc.), not through `main.rs`, so this migration touching only `main.rs` cannot regress them; this run is the safety net confirming that assumption holds.

- [ ] **Step 4: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/src/main.rs`.
