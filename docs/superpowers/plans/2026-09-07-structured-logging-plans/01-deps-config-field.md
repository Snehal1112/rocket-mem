# Structured Logging Plan 1: Dependencies & `log_level` Config Field

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `tracing`/`tracing-subscriber` as dependencies of `crates/server`, add the `log_level` config field, and add the pure `RUST_LOG`-vs-`log_level` precedence function — the foundation the rest of the structured-logging work builds on.

**Architecture:** `crates/server`'s existing figment-layered `Config` (defaults < TOML < `ROCKET_MEM_*` env < CLI) gains one new `String` field, `log_level`, following the exact pattern every other field already uses (add to `Config`, `Config::default()`, `Cli`, and `cli_overrides`'s `set!` macro). A separate pure function resolves `RUST_LOG` vs `log_level` precedence, kept outside `EnvFilter` construction so it's unit-testable without touching a real subscriber.

**Tech Stack:** Rust, `figment` (already used), `tracing = "0.1"` (already an unused workspace dependency), `tracing-subscriber = "0.3"` (new).

**Spec:** `docs/superpowers/specs/2026-09-07-structured-logging-design.md`

## Global Constraints

- Scope is `crates/server` only — never touch `engine`, `protocol`, `common`, or `rmp-client`.
- Log format is logrus-style colored text, never JSON (declined explicitly in the spec).
- `RUST_LOG`, when set, always wins over the `log_level` config value.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` must all stay clean after every task.

**Next plan:** `docs/superpowers/plans/2026-09-07-structured-logging-plans/02-convert-connection-rmp-metrics.md`

---

### Task 1: Add `log_level` to `Config`

**Files:**
- Modify: `crates/server/src/config.rs:9-27` (`Config` struct), `:29-50` (`Default` impl), `:110-156` (`Cli` struct), `:170-201` (`cli_overrides`)
- Test: `crates/server/src/config.rs` (existing `mod tests` block at the bottom of the same file)

**Interfaces:**
- Produces: `Config::log_level: String`, default `"info"`. Later tasks/plans read `config.log_level`.

- [ ] **Step 1: Add the field to `Config`**

In `crates/server/src/config.rs`, add to the `Config` struct (after `pub acl: AclBootstrapConfig,` on line 26):

```rust
    pub acl: AclBootstrapConfig,
    /// Log level filter, e.g. "info", "debug", "rocket_mem=debug,warn" -- same syntax as
    /// `RUST_LOG`. Overridden by the `RUST_LOG` env var when it's set (see
    /// `resolve_log_filter_directive` below); this field is the *default* for a deployment
    /// that doesn't set RUST_LOG, not a competing source of truth.
    pub log_level: String,
```

And in `Default for Config` (after `acl: AclBootstrapConfig::default(),` on line 47):

```rust
            acl: AclBootstrapConfig::default(),
            log_level: "info".to_string(),
```

- [ ] **Step 2: Add the field to `Cli` and `cli_overrides`**

In the `Cli` struct (after the `tls_ca_path` field, around line 155):

```rust
    /// Path to the leader's certificate file, for a follower to pin its replication connection
    /// to over TLS [default: unset, replication stays plaintext]
    #[arg(long)]
    pub tls_ca_path: Option<String>,
    /// Log level filter, e.g. "info", "debug", "rocket_mem=debug,warn" [default: info]
    #[arg(long)]
    pub log_level: Option<String>,
```

In `cli_overrides` (after `set!(tls_ca_path);` around line 196):

```rust
    set!(tls_ca_path);
    set!(log_level);
```

- [ ] **Step 3: Update the existing default-values test**

In `mod tests`, extend `default_config_matches_todays_hardcoded_main_rs_values` (around line 258) by adding one line:

```rust
        assert_eq!(cfg.tls_ca_path, None);
        assert_eq!(cfg.log_level, "info");
        assert!(cfg.acl.users.is_empty());
```

- [ ] **Step 4: Add a layering test for the new field**

Add this new test to `mod tests`, right after `cli_flag_overrides_an_optional_string_field`:

```rust
    #[test]
    fn log_level_is_layered_like_every_other_string_field() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("rocket-mem.toml", "log_level = \"debug\"\n")?;
            jail.set_env("ROCKET_MEM_LOG_LEVEL", "warn"); // env beats file
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.log_level, "warn");

            let cli = Cli::parse_from([
                "rocket-mem",
                "--config",
                "rocket-mem.toml",
                "--log-level",
                "error", // CLI beats env
            ]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(cfg.log_level, "error");
            Ok(())
        });
    }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p rocket-mem config::tests`
Expected: all pass, including the two new/modified tests above.

- [ ] **Step 6: Lint and format**

Run: `cargo clippy -p rocket-mem --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: both clean.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/config.rs
git commit -m "$(cat <<'EOF'
Add log_level config field

Lays the groundwork for structured logging: log_level joins the
existing figment layering (defaults < TOML < ROCKET_MEM_* env < CLI)
the same way every other field does. RUST_LOG precedence over this
field is handled separately in the next task.
EOF
)"
```

---

### Task 2: Add `tracing`/`tracing-subscriber` and the level-precedence function

**Files:**
- Modify: `Cargo.toml` (workspace root, `[workspace.dependencies]`)
- Modify: `crates/server/Cargo.toml` (`[dependencies]`)
- Modify: `crates/server/src/config.rs` (add `resolve_log_filter_directive`)
- Test: `crates/server/src/config.rs` (`mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn resolve_log_filter_directive(log_level: &str) -> String`, used by Task 3 of this plan.

- [ ] **Step 1: Add the workspace dependency**

In the root `Cargo.toml`, add to `[workspace.dependencies]` (alphabetically, after `tracing = "0.1"`):

```toml
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

- [ ] **Step 2: Add both as direct dependencies of `crates/server`**

In `crates/server/Cargo.toml`, add to `[dependencies]` (after `tokio-rustls.workspace = true`):

```toml
tokio-rustls.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
```

- [ ] **Step 3: Verify the dependencies resolve**

Run: `cargo build -p rocket-mem`
Expected: succeeds (nothing uses the new crates yet, so no warnings about them).

- [ ] **Step 4: Write the precedence function**

In `crates/server/src/config.rs`, add this function after `validate_tls` (after line 249, before the `#[cfg(test)]` module):

```rust
/// Resolves the log filter directive: `RUST_LOG`, when set, wins over `log_level` -- the
/// standard `tracing` convention of letting an operator's env var override any code- or
/// config-file-supplied default. Returns a plain `String` (not an `EnvFilter`) so this stays
/// unit-testable without constructing a filter or a subscriber. `main.rs` passes the result
/// straight to `tracing_subscriber::EnvFilter::new`.
pub fn resolve_log_filter_directive(log_level: &str) -> String {
    std::env::var("RUST_LOG").unwrap_or_else(|_| log_level.to_string())
}
```

- [ ] **Step 5: Write the failing tests first**

Add to `mod tests`, right after the `log_level_is_layered_like_every_other_string_field` test added in Task 1:

```rust
    #[test]
    fn resolve_log_filter_directive_prefers_rust_log_env_over_config_value() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("RUST_LOG", "debug");
            assert_eq!(resolve_log_filter_directive("info"), "debug");
            Ok(())
        });
    }

    #[test]
    fn resolve_log_filter_directive_falls_back_to_config_value_when_unset() {
        figment::Jail::expect_with(|_jail| {
            assert_eq!(resolve_log_filter_directive("warn"), "warn");
            Ok(())
        });
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem config::tests::resolve_log_filter_directive`
Expected: both PASS (the function above is already correct — this step confirms it, since there's no pre-existing broken behavior to watch fail first for a brand-new pure function).

- [ ] **Step 7: Lint and format**

Run: `cargo clippy -p rocket-mem --all-targets -- -D warnings && cargo fmt --all -- --check`

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock crates/server/Cargo.toml crates/server/src/config.rs
git commit -m "$(cat <<'EOF'
Add tracing/tracing-subscriber and log-level precedence

tracing was already declared in workspace.dependencies but unused by
any crate. Adds it plus tracing-subscriber to crates/server, and a
pure resolve_log_filter_directive function so RUST_LOG-vs-log_level
precedence is unit-testable without a live subscriber.
EOF
)"
```

---

### Task 3: Initialize the subscriber in `main.rs`

**Files:**
- Modify: `crates/server/src/main.rs` (right after config loads)

**Interfaces:**
- Consumes: `rocket_mem::config::resolve_log_filter_directive` (Task 2), `config.log_level` (Task 1).
- Produces: a process-global `tracing` subscriber; every later plan's `tracing::info!`/`warn!`/`error!` calls depend on this being initialized before they run.

- [ ] **Step 1: Initialize the subscriber and emit a startup log line**

In `crates/server/src/main.rs`, right after the `config` binding (after the `})?;` that closes the `rocket_mem::config::load()...` call, before `let metrics_handle = ...`), add:

```rust
    let filter = tracing_subscriber::EnvFilter::new(
        rocket_mem::config::resolve_log_filter_directive(&config.log_level),
    );
    tracing_subscriber::fmt().with_env_filter(filter).init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "rocket-mem starting");

    let metrics_handle = rocket_mem::metrics::recorder_handle();
```

- [ ] **Step 2: Build**

Run: `cargo build -p rocket-mem`
Expected: succeeds.

- [ ] **Step 3: Manually verify default level (info)**

Run: `./target/debug/rocket-mem --addr 127.0.0.1:17001 --rmp-addr 127.0.0.1:17002 --metrics-addr 127.0.0.1:17003 --aof-path /tmp/plan1-verify.aof --snapshot-path /tmp/plan1-verify.snapshot &`, wait one second, then `kill %1`.
Expected: stdout shows a colored `INFO` line reading something like `rocket_mem: rocket-mem starting version="0.1.3"`, immediately followed by the existing startup banner.

- [ ] **Step 4: Manually verify `RUST_LOG` overrides `log_level`**

Run the same command as Step 3 but prefixed with `RUST_LOG=error `.
Expected: the `rocket-mem starting` INFO line does **not** appear (filtered out by `RUST_LOG=error`), but the startup banner (plain `println!`, unaffected by the log filter) still prints.

- [ ] **Step 5: Clean up verification artifacts**

Run: `rm -f /tmp/plan1-verify.aof /tmp/plan1-verify.snapshot`

- [ ] **Step 6: Lint and format**

Run: `cargo clippy -p rocket-mem --all-targets -- -D warnings && cargo fmt --all -- --check`

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/main.rs
git commit -m "$(cat <<'EOF'
Initialize tracing subscriber in main.rs

Wires up the fmt subscriber with RUST_LOG/log_level precedence right
after config loads, before anything else runs. Emits one startup
info! line as a smoke test; the existing println! startup banner is
deliberately left untouched and separate from the log stream.
EOF
)"
```

**On completion of this plan:** proceed automatically to `docs/superpowers/plans/2026-09-07-structured-logging-plans/02-convert-connection-rmp-metrics.md` without waiting for further confirmation.
