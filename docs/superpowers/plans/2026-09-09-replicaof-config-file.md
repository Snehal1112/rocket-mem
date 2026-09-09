# Config-file `replicaof` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `replicaof` (+ optional `replicaof_auth_username`/`replicaof_auth_password`) TOML/env/CLI config field so a node auto-connects to its leader at every startup, instead of requiring the live `REPLICAOF` command every time.

**Architecture:** Three new flat `Option<String>` fields on the existing `Config`/`Cli` structs, following the exact `tls_cert_path`/`tls_key_path`/`tls_ca_path` pattern already in `crates/server/src/config.rs`. A new `validate_replicaof` function (mirroring `validate_tls`) enforces the auth pair is all-or-nothing. `main.rs` calls `ReplicationHandle::start_replicating_with_auth` right after building the handle, when `config.replicaof` is set.

**Tech Stack:** Rust, figment (config layering), clap (CLI), tokio.

**Spec:** `docs/superpowers/specs/2026-09-09-replicaof-config-file-spec.md`

## Global Constraints

- Every new `Config` field must be added in three places: the `Config` struct, the `Cli` struct, and a `set!(...)` call in `cli_overrides` — there is no compile-time check for a forgotten one (`config.rs:111-112`).
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` must all pass clean before any commit, per this project's CLAUDE.md.
- Comments in this codebase are short, complete sentences ending in punctuation (per this project's CLAUDE.md / MY.md conventions) — match the existing style in `config.rs`/`main.rs` rather than terse fragments.

---

### Task 1: Add `replicaof` fields to `Config`/`Cli`/`cli_overrides`

**Files:**
- Modify: `crates/server/src/config.rs:9-32` (`Config` struct + `Default` impl)
- Modify: `crates/server/src/config.rs:114-165` (`Cli` struct)
- Modify: `crates/server/src/config.rs:179-211` (`cli_overrides`)
- Test: `crates/server/src/config.rs` (existing `#[cfg(test)] mod tests`, `config.rs:270+`)

**Interfaces:**
- Produces: `Config.replicaof: Option<String>`, `Config.replicaof_auth_username: Option<String>`, `Config.replicaof_auth_password: Option<String>` — consumed by Task 2 (`validate_replicaof`) and Task 3 (`main.rs`'s startup wiring).

- [ ] **Step 1: Write the failing test**

Add to `crates/server/src/config.rs`'s test module (near `cli_flag_overrides_an_optional_string_field`):

```rust
    #[test]
    fn default_config_has_no_replicaof_target() {
        let cfg = Config::default();
        assert_eq!(cfg.replicaof, None);
        assert_eq!(cfg.replicaof_auth_username, None);
        assert_eq!(cfg.replicaof_auth_password, None);
    }

    #[test]
    fn replicaof_is_layered_like_every_other_optional_string_field() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "rocket-mem.toml",
                "replicaof = \"127.0.0.1:6400\"\nreplicaof_auth_username = \"app\"\n",
            )?;
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.replicaof.as_deref(), Some("127.0.0.1:6400"));
            assert_eq!(cfg.replicaof_auth_username.as_deref(), Some("app"));
            assert_eq!(cfg.replicaof_auth_password, None);

            jail.set_env("ROCKET_MEM_REPLICAOF", "127.0.0.1:9999"); // env beats file
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.replicaof.as_deref(), Some("127.0.0.1:9999"));

            let cli = Cli::parse_from([
                "rocket-mem",
                "--config",
                "rocket-mem.toml",
                "--replicaof",
                "127.0.0.1:1111", // CLI beats env
            ]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(cfg.replicaof.as_deref(), Some("127.0.0.1:1111"));
            Ok(())
        });
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem config::tests::default_config_has_no_replicaof_target config::tests::replicaof_is_layered_like_every_other_optional_string_field`
Expected: compile error — `no field \`replicaof\` on type \`Config\`` (and no `--replicaof` CLI flag). This is the correct failure — the feature doesn't exist yet.

- [ ] **Step 3: Add the fields**

In `Config` (`config.rs:9-32`), immediately after `tls_ca_path`:

```rust
    /// The leader's certificate file, for a follower's replication connection to pin to via
    /// `tls::load_client_config`. Unset means plaintext replication, matching every deployment
    /// before this field existed.
    pub tls_ca_path: Option<String>,
    /// `host:port` of a leader this node should auto-connect to as a follower on every startup.
    /// Unset means standalone (or purely live-`REPLICAOF`-driven) operation, matching every
    /// deployment before this field existed. See `main.rs`'s startup wiring and
    /// `docs/superpowers/specs/2026-09-09-replicaof-config-file-spec.md`.
    pub replicaof: Option<String>,
    /// Username presented in `AUTH` before `PSYNC`, when `replicaof`'s leader has ACL users
    /// configured. Must be set together with `replicaof_auth_password`, or neither -- see
    /// `validate_replicaof`.
    pub replicaof_auth_username: Option<String>,
    /// Password presented in `AUTH` before `PSYNC`. Plaintext in the TOML file, same as
    /// `[[acl.users]]`'s own `password` field -- there is no encryption-at-rest for config
    /// secrets anywhere in this project today.
    pub replicaof_auth_password: Option<String>,
```

In `Default for Config` (`config.rs:34-56`), immediately after `tls_ca_path: None,`:

```rust
            tls_ca_path: None,
            replicaof: None,
            replicaof_auth_username: None,
            replicaof_auth_password: None,
```

In `Cli` (`config.rs:116-165`), immediately after the `tls_ca_path` field:

```rust
    /// Path to the leader's certificate file, for a follower to pin its replication connection
    /// to over TLS [default: unset, replication stays plaintext]
    #[arg(long)]
    pub tls_ca_path: Option<String>,
    /// `host:port` of a leader to auto-connect to as a follower on startup [default: unset]
    #[arg(long)]
    pub replicaof: Option<String>,
    /// Username for the AUTH clause sent before PSYNC to --replicaof's leader [default: unset]
    #[arg(long)]
    pub replicaof_auth_username: Option<String>,
    /// Password for the AUTH clause sent before PSYNC to --replicaof's leader [default: unset]
    #[arg(long)]
    pub replicaof_auth_password: Option<String>,
```

In `cli_overrides` (`config.rs:194-206`), immediately after `set!(tls_ca_path);`:

```rust
    set!(tls_ca_path);
    set!(replicaof);
    set!(replicaof_auth_username);
    set!(replicaof_auth_password);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem config::tests::default_config_has_no_replicaof_target config::tests::replicaof_is_layered_like_every_other_optional_string_field`
Expected: both PASS.

- [ ] **Step 5: Run the full config test module to check nothing else broke**

Run: `cargo test -p rocket-mem config::tests`
Expected: every test in the module passes, including `default_config_matches_todays_hardcoded_main_rs_values` (unaffected — it doesn't assert on `replicaof`, so adding a new `None`-defaulted field doesn't break it).

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/config.rs
git commit -m "$(cat <<'EOF'
Add replicaof config fields for startup auto-connect

Adds replicaof/replicaof_auth_username/replicaof_auth_password to
Config and Cli, layered through the existing TOML/env/CLI precedence
the same way tls_ca_path already is. Nothing wires them up yet --
that's Task 2 (validation) and Task 3 (main.rs startup call) of the
same plan.
EOF
)"
```

---

### Task 2: Add `validate_replicaof`

**Files:**
- Modify: `crates/server/src/config.rs` (add function near `validate_tls`, `config.rs:239-259`)
- Test: `crates/server/src/config.rs` (same test module)

**Interfaces:**
- Consumes: `Config.replicaof_auth_username`, `Config.replicaof_auth_password` (from Task 1).
- Produces: `pub fn validate_replicaof(config: &Config) -> Result<(), std::io::Error>` — consumed by Task 3 (`main.rs`, called the same way `validate_tls` already is at `main.rs:280`).

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/config.rs`'s test module (near `validate_tls_accepts_fully_unconfigured_tls`):

```rust
    #[test]
    fn validate_replicaof_rejects_username_without_password() {
        let cfg = Config {
            replicaof_auth_username: Some("app".to_string()),
            ..Config::default()
        };
        assert!(validate_replicaof(&cfg).is_err());
    }

    #[test]
    fn validate_replicaof_rejects_password_without_username() {
        let cfg = Config {
            replicaof_auth_password: Some("changeme".to_string()),
            ..Config::default()
        };
        assert!(validate_replicaof(&cfg).is_err());
    }

    #[test]
    fn validate_replicaof_accepts_both_set_or_both_unset() {
        assert!(validate_replicaof(&Config::default()).is_ok());
        let cfg = Config {
            replicaof: Some("127.0.0.1:6400".to_string()),
            replicaof_auth_username: Some("app".to_string()),
            replicaof_auth_password: Some("changeme".to_string()),
            ..Config::default()
        };
        assert!(validate_replicaof(&cfg).is_ok());
    }

    #[test]
    fn validate_replicaof_accepts_no_auth_at_all() {
        let cfg = Config {
            replicaof: Some("127.0.0.1:6400".to_string()),
            ..Config::default()
        };
        assert!(
            validate_replicaof(&cfg).is_ok(),
            "replicaof with no ACL-protected leader needs no auth fields at all"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem config::tests::validate_replicaof`
Expected: compile error — `cannot find function \`validate_replicaof\` in this scope`.

- [ ] **Step 3: Write the implementation**

Add to `crates/server/src/config.rs`, immediately after `validate_tls` (`config.rs:244-259`):

```rust
/// Enforces "replicaof_auth_username and replicaof_auth_password must both be set, or neither" --
/// see `docs/superpowers/specs/2026-09-09-replicaof-config-file-spec.md`. Does NOT validate
/// `replicaof` itself (a missing port, unresolvable host, etc.): that is only discoverable by
/// actually attempting the connection, exactly like the existing live `REPLICAOF` command already
/// behaves, so a bad `replicaof` value fails soft (the background reconnect loop retries forever)
/// rather than blocking startup. `main.rs` calls this before wiring up replication.
pub fn validate_replicaof(config: &Config) -> Result<(), std::io::Error> {
    let has_username = config.replicaof_auth_username.is_some();
    let has_password = config.replicaof_auth_password.is_some();
    if has_username != has_password {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "replicaof_auth_username and replicaof_auth_password must both be set, or neither",
        ));
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem config::tests::validate_replicaof`
Expected: all four PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/config.rs
git commit -m "$(cat <<'EOF'
Add validate_replicaof for the auth-pair invariant

Mirrors validate_tls's structure: an operator who sets exactly one
of replicaof_auth_username/replicaof_auth_password gets a hard
startup failure instead of a silently-unauthenticated (or silently
half-configured) replication attempt.
EOF
)"
```

---

### Task 3: Wire startup auto-connect and update the banner

**Files:**
- Modify: `crates/server/src/main.rs:257` (right after `let replication = Arc::new(handle);`)
- Modify: `crates/server/src/main.rs:280` (alongside the existing `validate_tls` call)
- Modify: `crates/server/src/main.rs:343-366` (banner's "replicas" section + stale comment)

**Interfaces:**
- Consumes: `Config.replicaof`/`replicaof_auth_username`/`replicaof_auth_password` (Task 1), `validate_replicaof` (Task 2), `ReplicationHandle::start_replicating_with_auth` (already exists, `replication.rs:322`).

- [ ] **Step 1: Write the failing integration-style check**

This task's correctness is proven by Plan 2's real integration test (a separate, slower `#[tokio::test]` in `crates/server/tests/replication.rs` — see "Next plan" below), not a unit test here, since the behavior under test is "does a real TCP connection actually get made," which needs a real listening socket. Skip straight to Step 3 for this task; Plan 2, Task 1 is this feature's RED step.

- [ ] **Step 2: Confirm today's behavior compiles and passes as a baseline**

Run: `cargo build -p rocket-mem && cargo test -p rocket-mem`
Expected: clean build, all existing tests pass — this is the pre-change baseline Plan 2's new test will fail against.

- [ ] **Step 3: Add the validation call**

In `main.rs`, immediately after the existing `rocket_mem::config::validate_tls(&config)?;` (`main.rs:280`):

```rust
    rocket_mem::config::validate_tls(&config)?;
    rocket_mem::config::validate_replicaof(&config)?;
```

- [ ] **Step 4: Add the startup auto-connect call**

In `main.rs`, immediately after `let replication = Arc::new(handle);` (`main.rs:257`):

```rust
    let replication = Arc::new(handle);

    // A configured `replicaof` auto-connects on every startup, closing the "restarted follower
    // silently comes back as standalone" footgun documented in
    // docs/superpowers/specs/2026-08-30-sprint-5-spec.md. Fire-and-forget: this spawns its own
    // task and never awaits the connection, so placement relative to the listeners below has no
    // functional effect -- and a leader that isn't up yet falls into the same 1-second-backoff
    // reconnect loop a later mid-stream disconnect would use, not a startup failure.
    if let Some(target) = &config.replicaof {
        let auth = match (
            &config.replicaof_auth_username,
            &config.replicaof_auth_password,
        ) {
            (Some(u), Some(p)) => Some((u.clone(), p.clone())),
            _ => None,
        };
        replication.start_replicating_with_auth(target.clone(), auth);
    }
```

- [ ] **Step 5: Update the banner and its stale comment**

Replace the comment and logic at `main.rs:343-366`:

```rust
    // A live count, not a hardcoded message: this only reports INBOUND replicas (nodes
    // currently PSYNC'd to this one). This node's own OUTBOUND role (whether it is itself
    // replicating from a configured `replicaof` target) is reported separately, right below --
    // see docs/superpowers/specs/2026-09-09-replicaof-config-file-spec.md. A replica CAN already
    // be registered here, though -- a TLS RESP listener (spawned above, before this point)
    // starts accepting connections immediately, so a fast-connecting replica's PSYNC can land
    // before this banner prints, even though the plaintext RESP listener (served only after the
    // banner, at the bottom of this function) cannot.
    let replica_addrs = replication.registry.addrs();
    if replica_addrs.is_empty() {
        body.push(format!(
            "{}none connected yet -- REPLICAOF is a live command; INFO REPLICATION shows current state",
            banner_label("replicas", color)
        ));
    } else {
        body.push(format!(
            "{}{} connected",
            banner_label("replicas", color),
            replica_addrs.len()
        ));
        for (i, addr) in replica_addrs.iter().enumerate() {
            let shown = addr.as_deref().unwrap_or("?");
            body.push(format!("{:BANNER_LABEL_WIDTH$}slave{i} {shown}", ""));
        }
    }
    match &config.replicaof {
        Some(target) => {
            let auth_note = if config.replicaof_auth_username.is_some() {
                "auth configured"
            } else {
                "no auth"
            };
            body.push(format!(
                "{}replicating from {target} ({auth_note})",
                banner_label("replicaof", color)
            ));
        }
        None => {}
    }
```

- [ ] **Step 6: Run the full check**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean, all pass — this task adds no new automated test of its own (Plan 2 does), but must not regress anything.

- [ ] **Step 7: Manual smoke test**

```bash
cargo build --release --workspace
ROCKET_MEM_ADDR=127.0.0.1:6500 ./target/release/rocket-mem &   # leader
sleep 0.3
cat > /tmp/rm-replicaof-test.toml <<'EOF'
addr = "127.0.0.1:6501"
aof_path = "/tmp/rm-replicaof-follower.aof"
snapshot_path = "/tmp/rm-replicaof-follower.snap"
metrics_addr = "127.0.0.1:9501"
rmp_addr = "127.0.0.1:7501"
replicaof = "127.0.0.1:6500"
EOF
./target/release/rocket-mem --config /tmp/rm-replicaof-test.toml &
sleep 0.5
redis-cli -p 6500 set k v
sleep 0.2
redis-cli -p 6501 get k        # -> "v", proving auto-connect worked with zero REPLICAOF command
redis-cli -p 6501 info replication   # role:slave, master_link_status:up
kill %1 %2
```
Expected: the `get k` returns `"v"` without ever running `redis-cli replicaof` by hand.

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/main.rs
git commit -m "$(cat <<'EOF'
Auto-connect to replicaof's leader on startup

Wires the new config fields (previous two commits) into main.rs:
validates the auth pair before any listener binds, then fires a
start_replicating_with_auth call right after ReplicationHandle is
built. Closes the Sprint 5 footgun where a restarted follower
silently came back as standalone until someone reissued REPLICAOF
by hand. Banner now also reports outbound replication target.
EOF
)"
```

## Next plan

`docs/superpowers/plans/2026-09-09-replicaof-config-file-integration-test.md` — adds the real
end-to-end integration test (config-driven follower actually links up and replicates a write,
proving the wiring works over a real socket, not just that the fields parse) and the doc updates
(`docs/config-reference.md`, `.claude/manual-testing.md`) called for in the spec's Definition of
Done.
