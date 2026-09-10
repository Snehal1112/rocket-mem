# Replica-Fencing Config Fields Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** two new config fields, `min_replicas_to_write` and `min_replicas_max_lag_secs`, flow through all four existing config layers (built-in defaults, TOML file, `ROCKET_MEM_*` env vars, CLI flags) exactly like every other field in `crates/server/src/config.rs`, plus a `validate_min_replicas` startup guard against the one config shape that would turn fencing into a permanent silent write outage, and the `docs/config-reference.md` entries an operator needs to use either of them. This plan adds no runtime behavior — `min_replicas_to_write` is not read by the dispatcher yet (that's `08-fencing-enforcement.md`). It only makes the two fields loadable, validated, and documented.

**Architecture:** both fields are plain `u64`s on `Config`, following the exact shape `slowlog_threshold_micros` already has — `#[serde(default)]` on the struct picks up `Default::default()`'s value for a field a TOML file omits, and `Cli`'s matching fields are `Option<u64>` so an unset flag doesn't clobber a lower layer. Because both are numeric, `cli_overrides` cannot use its `set!` macro (`set!` calls `v.as_str()`, which does not exist on `u64`) — both use the manual `if let Some(v) = cli.field { map.insert(...) }` pattern `slowlog_threshold_micros` already uses. `validate_min_replicas` follows the exact shape of `validate_tls`/`validate_replicaof`: a pure `fn(&Config) -> Result<(), std::io::Error>`, called from `main.rs` before any listener binds.

**Tech Stack:** nothing new — `figment`, `clap`, and `figment::Jail` are already workspace dependencies exercised by every other field in this file.

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md)

## Global Constraints

- [`00-design-contract.md`](00-design-contract.md) is normative. §2.4's "Config fields" table fixes the two field names, their types (`u64`/`u64`), defaults (`0`/`10`), env var names (`ROCKET_MEM_MIN_REPLICAS_TO_WRITE`/`ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS`), and CLI flags (`--min-replicas-to-write`/`--min-replicas-max-lag-secs`) exactly. §2.5's last paragraph fixes the validation rule this plan implements.
- **`min_replicas_to_write = 0` is the default and must leave every existing deployment completely unaffected.** This plan only adds config plumbing — nothing reads `min_replicas_to_write` yet — so this constraint is automatically satisfied here, but every test in this plan must still assert the default explicitly, since `08-fencing-enforcement.md` depends on it staying `0`.
- Numeric fields use the manual `if let Some(v) = cli.field { map.insert(...) }` pattern in `cli_overrides`, never the `set!` macro — `set!` only compiles for `Option<String>` fields.
- The three CI gates must be clean before every commit:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
- Comment style: short, easy, full sentences ending in punctuation. No emojis.

---

### Task 1: `Config`/`Cli`/`cli_overrides` — the two new numeric fields

**Files:**
- Modify: `crates/server/src/config.rs`

**Interfaces:**
- Consumes: nothing new — extends the existing `Config`/`Cli`/`cli_overrides`/`load_layered`/`load_with_cli` machinery already in this file.
- Produces: `Config::min_replicas_to_write: u64`, `Config::min_replicas_max_lag_secs: u64`, and their `Cli`/env/TOML equivalents. `08-fencing-enforcement.md` Task 3 (`main.rs` wiring) and `09-fencing-observability.md` consume these field names directly.

- [ ] **Step 1: Write the failing tests**

Add these tests to `crates/server/src/config.rs`'s `mod tests`, immediately after `replicaof_is_layered_like_every_other_optional_string_field` (currently ending at line 536):

```rust
#[test]
fn default_config_disables_replica_fencing() {
    let cfg = Config::default();
    assert_eq!(
        cfg.min_replicas_to_write, 0,
        "fencing must be off by default -- every deployment before this field existed must be unaffected"
    );
    assert_eq!(cfg.min_replicas_max_lag_secs, 10);
}

#[test]
fn min_replicas_fields_are_layered_like_every_other_numeric_field() {
    figment::Jail::expect_with(|jail| {
        jail.create_file(
            "rocket-mem.toml",
            "min_replicas_to_write = 1\nmin_replicas_max_lag_secs = 20\n",
        )?;
        let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
        assert_eq!(cfg.min_replicas_to_write, 1, "file overrides default");
        assert_eq!(cfg.min_replicas_max_lag_secs, 20, "file overrides default");

        jail.set_env("ROCKET_MEM_MIN_REPLICAS_TO_WRITE", "2"); // env beats file
        jail.set_env("ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS", "30"); // env beats file
        let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
        assert_eq!(cfg.min_replicas_to_write, 2, "env overrides file");
        assert_eq!(cfg.min_replicas_max_lag_secs, 30, "env overrides file");

        let cli = Cli::parse_from([
            "rocket-mem",
            "--config",
            "rocket-mem.toml",
            "--min-replicas-to-write",
            "3", // CLI beats env
            "--min-replicas-max-lag-secs",
            "40", // CLI beats env
        ]);
        let cfg = load_with_cli(cli).unwrap();
        assert_eq!(cfg.min_replicas_to_write, 3, "CLI overrides env");
        assert_eq!(cfg.min_replicas_max_lag_secs, 40, "CLI overrides env");
        Ok(())
    });
}

#[test]
fn cli_flags_left_unset_do_not_override_lower_layers_for_min_replicas_fields() {
    figment::Jail::expect_with(|jail| {
        jail.set_env("ROCKET_MEM_MIN_REPLICAS_TO_WRITE", "1");
        let cli = Cli::parse_from(["rocket-mem"]); // no flags at all
        let cfg = load_with_cli(cli).unwrap();
        assert_eq!(
            cfg.min_replicas_to_write, 1,
            "an unset CLI flag must not clobber the env value with the default"
        );
        Ok(())
    });
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib config::tests::default_config_disables_replica_fencing config::tests::min_replicas_fields_are_layered_like_every_other_numeric_field config::tests::cli_flags_left_unset_do_not_override_lower_layers_for_min_replicas_fields -- --nocapture`
Expected: FAIL to compile — `Config` has no field `min_replicas_to_write`/`min_replicas_max_lag_secs`, and `Cli` has no such fields or flags either (`no field \`min_replicas_to_write\` on type \`config::Config\``, `no method named \`min_replicas_to_write\` found for struct \`Cli\``, or similar).

- [ ] **Step 3: Add the fields to `Config`, `Default`, `Cli`, and `cli_overrides`**

In `crates/server/src/config.rs`, add to the `Config` struct definition (after `pub replicaof_auth_password: Option<String>,` and before `pub acl: AclBootstrapConfig,`):

```rust
    /// Minimum number of replicas that must have acked within `min_replicas_max_lag_secs` for
    /// this node to accept a write. `0` disables fencing entirely -- the default, matching every
    /// deployment before this field existed. See `docs/superpowers/plans/
    /// 2026-09-09-failover-safety-primitives/00-design-contract.md` §2.5 for full semantics;
    /// enforcement lives in `dispatch_and_log_inner`, wired via `ReplicationHandle::with_min_replicas`
    /// (`08-fencing-enforcement.md`), not in this struct.
    pub min_replicas_to_write: u64,
    /// How many seconds old a replica's last acked offset may be and still count as "good" for
    /// `min_replicas_to_write`. Ignored while `min_replicas_to_write` is `0`. `validate_min_replicas`
    /// below rejects a nonzero `min_replicas_to_write` paired with `0` here, since no replica could
    /// ever qualify and every write would be refused forever.
    pub min_replicas_max_lag_secs: u64,
```

Add to `Default for Config`'s `fn default()` body (after `replicaof_auth_password: None,` and before `acl: AclBootstrapConfig::default(),`):

```rust
            min_replicas_to_write: 0,
            min_replicas_max_lag_secs: 10,
```

Add to the `Cli` struct definition (after `pub replicaof_auth_password: Option<String>,` and before `pub log_level: Option<String>,`):

```rust
    /// Minimum number of replicas that must have acked within --min-replicas-max-lag-secs for
    /// this node to accept writes; 0 disables fencing entirely [default: 0]
    #[arg(long)]
    pub min_replicas_to_write: Option<u64>,
    /// How many seconds old a replica's last ack may be and still count as "good" for
    /// --min-replicas-to-write [default: 10]
    #[arg(long)]
    pub min_replicas_max_lag_secs: Option<u64>,
```

Add to `cli_overrides`, after the existing `if let Some(v) = cli.slowlog_threshold_micros { ... }` block and before `Serialized::defaults(map)`:

```rust
    if let Some(v) = cli.min_replicas_to_write {
        map.insert("min_replicas_to_write", Value::from(v));
    }
    if let Some(v) = cli.min_replicas_max_lag_secs {
        map.insert("min_replicas_max_lag_secs", Value::from(v));
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib config:: -- --nocapture`
Expected: all PASS, including every pre-existing `config::tests` test (the two new fields' `Default`-driven initialization must not disturb any of them).

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --lib config::`
Expected: all green.

```bash
git add crates/server/src/config.rs
git commit -m "$(cat <<'EOF'
Add min_replicas_to_write/min_replicas_max_lag_secs config fields

Plumbs the two replica-fencing thresholds through all four config
layers (defaults, TOML, ROCKET_MEM_* env, CLI flags), following the
existing numeric-field pattern slowlog_threshold_micros already uses.
No runtime behavior yet -- nothing reads these fields until
08-fencing-enforcement.md wires them onto ReplicationHandle.
EOF
)"
```

---

### Task 2: `validate_min_replicas` and `main.rs` wiring

**Files:**
- Modify: `crates/server/src/config.rs`
- Modify: `crates/server/src/main.rs`

**Interfaces:**
- Consumes: `Config::min_replicas_to_write`, `Config::min_replicas_max_lag_secs` (Task 1).
- Produces: `pub fn validate_min_replicas(config: &Config) -> Result<(), std::io::Error>`, called from `main.rs`. No other plan consumes this function directly — it is a pure startup guard.

- [ ] **Step 1: Write the failing tests**

Add these tests to `crates/server/src/config.rs`'s `mod tests`, immediately after `validate_replicaof_accepts_no_auth_at_all` (the last test in the file):

```rust
#[test]
fn validate_min_replicas_rejects_zero_lag_with_fencing_enabled() {
    let cfg = Config {
        min_replicas_to_write: 1,
        min_replicas_max_lag_secs: 0,
        ..Config::default()
    };
    assert!(
        validate_min_replicas(&cfg).is_err(),
        "min_replicas_to_write > 0 with a 0-second lag window means no replica could ever qualify"
    );
}

#[test]
fn validate_min_replicas_accepts_the_default() {
    assert!(validate_min_replicas(&Config::default()).is_ok());
}

#[test]
fn validate_min_replicas_accepts_fencing_enabled_with_a_nonzero_lag() {
    let cfg = Config {
        min_replicas_to_write: 2,
        min_replicas_max_lag_secs: 5,
        ..Config::default()
    };
    assert!(validate_min_replicas(&cfg).is_ok());
}

#[test]
fn validate_min_replicas_accepts_zero_lag_when_fencing_is_disabled() {
    let cfg = Config {
        min_replicas_to_write: 0,
        min_replicas_max_lag_secs: 0,
        ..Config::default()
    };
    assert!(
        validate_min_replicas(&cfg).is_ok(),
        "fencing disabled means the lag value is irrelevant"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib config::tests::validate_min_replicas -- --nocapture`
Expected: FAIL to compile — `cannot find function \`validate_min_replicas\` in this scope`.

- [ ] **Step 3: Implement `validate_min_replicas`**

In `crates/server/src/config.rs`, add after `validate_replicaof` (and before `replicaof_auth`):

```rust
/// Enforces the design contract's fencing safety rule (`00-design-contract.md` §2.5): a
/// `min_replicas_to_write` above zero paired with a `min_replicas_max_lag_secs` of zero means no
/// replica's ack could ever be recent enough to count as "good" -- `good_replicas` requires an ack
/// strictly within the lag window, and no ack arrives in zero seconds. That combination would
/// silently refuse every write forever, which is a permanent outage spelled as a config typo, not
/// a real deployment intent. `main.rs` calls this before any listener binds, alongside
/// `validate_tls` and `validate_replicaof`.
pub fn validate_min_replicas(config: &Config) -> Result<(), std::io::Error> {
    if config.min_replicas_to_write > 0 && config.min_replicas_max_lag_secs == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "min_replicas_to_write is set but min_replicas_max_lag_secs is 0 -- no replica could ever qualify, so every write would be refused forever",
        ));
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib config:: -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Wire it into `main.rs`**

In `crates/server/src/main.rs`, the validation block currently reads:

```rust
    rocket_mem::config::validate_replicaof(&config)?;
    rocket_mem::config::validate_tls(&config)?;
```

Change it to:

```rust
    rocket_mem::config::validate_replicaof(&config)?;
    rocket_mem::config::validate_tls(&config)?;
    rocket_mem::config::validate_min_replicas(&config)?;
```

This runs before the `replicaof` auto-connect block and before any listener binds, matching the existing comment above this block ("A broken TLS config must abort startup before the auto-connect block below can load a leader's snapshot...") — a broken fencing config must abort for the same reason: it is a pure function of `&Config` with no dependency on anything constructed later.

- [ ] **Step 6: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo build --workspace && cargo test -p rocket-mem --lib config::`
Expected: all green.

```bash
git add crates/server/src/config.rs crates/server/src/main.rs
git commit -m "$(cat <<'EOF'
Reject min_replicas_to_write > 0 with a zero lag window at startup

A nonzero min_replicas_to_write paired with min_replicas_max_lag_secs
== 0 would refuse every write forever, since no replica's ack could
ever be recent enough to count as good. Fail startup loudly instead
of silently deploying a permanent write outage.
EOF
)"
```

---

### Task 3: `docs/config-reference.md`

**Files:**
- Modify: `docs/config-reference.md`

**Interfaces:**
- Consumes: nothing — pure documentation of Task 1/2's fields.
- Produces: nothing — this is the last task in this plan.

- [ ] **Step 1: Add the two fields to the `## Fields` table**

In `docs/config-reference.md`, the `## Fields` table currently ends its replication-related rows with:

```
| `replicaof_auth_password` | `ROCKET_MEM_REPLICAOF_AUTH_PASSWORD` | `--replicaof-auth-password` | unset | Password sent in `AUTH` before `PSYNC`. Plaintext in the TOML file, same as `[[acl.users]]`'s own `password` field. |
| `[[acl.users]]` | *(file-only — no flat env var for an array)* | *(file-only)* | empty | Bootstrap ACL users, loaded once at startup. See "The `[[acl.users]]` array" below. |
```

Insert two new rows between them:

```
| `replicaof_auth_password` | `ROCKET_MEM_REPLICAOF_AUTH_PASSWORD` | `--replicaof-auth-password` | unset | Password sent in `AUTH` before `PSYNC`. Plaintext in the TOML file, same as `[[acl.users]]`'s own `password` field. |
| `min_replicas_to_write` | `ROCKET_MEM_MIN_REPLICAS_TO_WRITE` | `--min-replicas-to-write` | `0` | Minimum number of replicas that must have acked within `min_replicas_max_lag_secs` for this node to accept a write. `0` disables fencing entirely — every write is accepted regardless of replica state, matching every deployment before this field existed. See "Replica fencing" below. |
| `min_replicas_max_lag_secs` | `ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS` | `--min-replicas-max-lag-secs` | `10` | How many seconds old a replica's last acknowledged offset may be and still count as "good" for `min_replicas_to_write`. Ignored while `min_replicas_to_write` is `0`. |
| `[[acl.users]]` | *(file-only — no flat env var for an array)* | *(file-only)* | empty | Bootstrap ACL users, loaded once at startup. See "The `[[acl.users]]` array" below. |
```

- [ ] **Step 2: Add a prose subsection**

In `docs/config-reference.md`, insert a new subsection after "### `replicaof`'s auth pair is all-or-nothing; the target itself is not validated" and before "### Malformed values fail startup, not silently":

```markdown
### Replica fencing (`min_replicas_to_write`) requires a nonzero lag window

`min_replicas_to_write` makes this node refuse client writes with a `NOREPLICAS` error unless at
least that many replicas have sent a `REPLCONF ACK` within the last `min_replicas_max_lag_secs`
seconds. It is disabled by default (`0`) — every deployment that doesn't set it is completely
unaffected, and every write is accepted regardless of replica state, exactly as before this field
existed.

Setting `min_replicas_to_write` above `0` while leaving `min_replicas_max_lag_secs` at `0` is
rejected at startup: no replica's ack could ever be recent enough to satisfy a zero-second window,
so every write would be refused forever — a permanent outage spelled as a config typo, not a real
deployment intent. `rocket-mem` checks this before any listener binds and aborts immediately if it
sees that combination, the same way it aborts on a broken `tls_*` or `replicaof_auth_*` pairing.

This gate applies only to this node's own client-originated writes. A replica already rejects
client writes with `READONLY` regardless of `min_replicas_to_write` — fencing and read-only mode
are independent gates, and `READONLY` always wins on a replica.
```

- [ ] **Step 3: Verify**

Run: `grep -n "min_replicas_to_write\|min_replicas_max_lag_secs" docs/config-reference.md`
Expected: matches in the `## Fields` table (two rows) and in the new "Replica fencing" subsection (several mentions).

- [ ] **Step 4: Commit**

```bash
git add docs/config-reference.md
git commit -m "$(cat <<'EOF'
Document min_replicas_to_write/min_replicas_max_lag_secs

Adds the two fencing config fields to the reference table and a prose
section explaining the default-off behavior and the zero-lag startup
guard added in the previous commit.
EOF
)"
```

---

## Next plan

[`08-fencing-enforcement.md`](08-fencing-enforcement.md) — wires `min_replicas_to_write`/`min_replicas_max_lag_secs` onto `ReplicationHandle` via `with_min_replicas` and adds the `NOREPLICAS` gate in `dispatch_and_log_inner`, immediately after the `READONLY` check.
