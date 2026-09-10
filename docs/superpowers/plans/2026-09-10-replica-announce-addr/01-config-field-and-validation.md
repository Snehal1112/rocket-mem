# Replica Announce Address — Plan 01: Config Field and Shape Validation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an optional `replica_announce_addr` config field that flows through all four
figment layers, and reject a malformed value at startup instead of letting it degrade into
`ip=?,port=0` at display time.

**Architecture:** Two additions to `crates/server/src/config.rs` and nothing else. The field is a
plain `Option<String>` alongside `replicaof`/`tls_ca_path`, so it inherits the existing
defaults < TOML < `ROCKET_MEM_*` env < CLI precedence for free. Validation is a pure
`fn(&Config) -> Result<(), std::io::Error>` following `validate_replicaof`/`validate_tls`'s exact
shape — a *shape* check (`host:port`, port parses as `u16`), never a reachability check.

**Tech Stack:** Rust 2021, `figment` (config layering), `clap` derive (CLI), `serde`, `tracing`.

**Spec:** [`../../specs/2026-09-10-replica-announce-addr-spec.md`](../../specs/2026-09-10-replica-announce-addr-spec.md)

## Global Constraints

Every task in **every plan in this series** (01–04) must satisfy all of these. Later plans link
back here rather than restating them.

- **Working directory.** Run every command from the **root of the checkout you are editing**. If
  you are working in a git worktree, that is the worktree root — *not*
  `/home/numericlabs/data/rocket/rocket-mem`. Never `cd` to a hardcoded absolute repo path in a
  build, test, or benchmark step; you would measure or test code that does not contain your
  change.
- **Three gates, all green, before every commit** (these are exactly what CI runs):
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
  `clippy` is strict: no warnings at all, dead code included, and it lints test code too.
- **Baseline: 994 tests passing.** Every task adds tests; none may remove or weaken one. If the
  count drops, stop and find out why before committing.
- **Two tests pin today's behaviour and must keep passing byte for byte, untouched:**
  - `info_lists_each_connected_slaves_advertised_address` (`crates/server/src/dispatcher.rs`),
    which pins `slave0:ip=127.0.0.1,port=6480,state=online\r\n`.
  - `sync_once_advertises_its_own_address_in_the_psync_frame_when_configured`
    (`crates/server/src/replication.rs`), which pins the raw `PSYNC` frame bytes.

  **Unset `replica_announce_addr` means today's behaviour byte for byte.** If a change to either
  of those tests looks necessary, the change is wrong.
- **Scope.** `crates/server/src/config.rs`, `crates/server/src/main.rs`, `crates/server/tests/`,
  and docs. No `engine`, `protocol`, `common`, or `rmp-client` change. No wire-format change.
- **Log-capture assertions live in `crates/server/tests/`, never in a `#[cfg(test)] mod tests`
  inside `src/`.** `tracing` caches per-callsite `Interest` process-globally, and a callsite first
  reached with no subscriber installed can be cached as never-enabled for the whole process. This
  was a real bug, hit twice; commit `4e646d2` fixed it.
- **Any snippet that reads from a spawned process needs a deadline.** A blocking `read_line` (or
  `Command::output()`) against a server that never exits blocks forever instead of failing —
  the red case must *fail*, not hang.
- **Any assertion on log output must state the exact rendered form.** `tracing_subscriber::fmt`
  renders a `%`-sigil (Display) field unquoted (`announced=127.0.0.1:6379`) and a bare `&str`
  passed through `?` (Debug) *quoted* (`protocol="metrics"`). Use `%` and assert unquoted.
- **No test may load the repo-root `rocket-mem.toml`.** It is a live deployment's
  credential-bearing config. Integration tests write their own TOML into a `tempfile::tempdir()`
  and pass `--config <that path>`; tests that pass no `--config` rely on the default relative
  `rocket-mem.toml` not existing in `crates/server/` (cargo runs integration tests with cwd at the
  package root).
- **Comment style.** Short, full sentences ending in a punctuation mark. No emoji.

---

### Task 1: `replica_announce_addr` through all four config layers

**Files:**
- Modify: `crates/server/src/config.rs` — `Config` struct, `Default`, the hand-written `Debug`,
  `Cli`, `cli_overrides`
- Test: `crates/server/src/config.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub replica_announce_addr: Option<String>` on `rocket_mem::config::Config`, defaulting
  to `None`; the `--replica-announce-addr` CLI flag; the `ROCKET_MEM_REPLICA_ANNOUNCE_ADDR` env
  var. Tasks in plans 02 and 03 read this field.

- [ ] **Step 1: Write the failing tests**

Add these two tests to the `mod tests` block at the bottom of `crates/server/src/config.rs`. Put
them directly after the existing `replicaof_is_layered_like_every_other_optional_string_field`, so
the related layering tests stay together.

```rust
    #[test]
    fn default_config_has_no_replica_announce_addr() {
        // Unset is the whole compatibility guarantee: a node with no `replica_announce_addr`
        // must announce `addr`, exactly as every deployment did before this field existed.
        assert_eq!(Config::default().replica_announce_addr, None);
    }

    #[test]
    fn replica_announce_addr_is_layered_like_every_other_optional_string_field() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "rocket-mem.toml",
                "replica_announce_addr = \"numericlabs.lxd:16479\"\n",
            )?;
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(
                cfg.replica_announce_addr.as_deref(),
                Some("numericlabs.lxd:16479"),
                "file overrides default"
            );

            jail.set_env("ROCKET_MEM_REPLICA_ANNOUNCE_ADDR", "numericlabs.lxd:26479");
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(
                cfg.replica_announce_addr.as_deref(),
                Some("numericlabs.lxd:26479"),
                "env overrides file"
            );

            let cli = Cli::parse_from([
                "rocket-mem",
                "--config",
                "rocket-mem.toml",
                "--replica-announce-addr",
                "numericlabs.lxd:36479",
            ]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(
                cfg.replica_announce_addr.as_deref(),
                Some("numericlabs.lxd:36479"),
                "CLI overrides env"
            );

            // The layer that is easiest to break by forgetting a `set!` line: an unset flag must
            // leave the env value alone rather than clobbering it with `None`.
            let cli = Cli::parse_from(["rocket-mem", "--config", "rocket-mem.toml"]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(
                cfg.replica_announce_addr.as_deref(),
                Some("numericlabs.lxd:26479"),
                "an unset CLI flag must not clobber the env value"
            );
            Ok(())
        });
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --lib config::tests::replica_announce_addr
```

Expected: **compile error**, not a failed assertion — `error[E0609]: no field
'replica_announce_addr' on type 'Config'` and `error[E0609]: no field 'replica_announce_addr' on
type 'Cli'`. That is the correct red state for a new field.

- [ ] **Step 3: Add the field to `Config`, `Default`, and the hand-written `Debug`**

In `crates/server/src/config.rs`, add the field to the `Config` struct immediately after
`replicaof_auth_password` and before `pub acl: AclBootstrapConfig`:

```rust
    /// The address this node advertises to its leader in its `PSYNC <addr>` frame, and which the
    /// leader then reports in `INFO REPLICATION`'s `slaveN:ip=...,port=...` lines. Unset means
    /// `addr` -- today's behaviour byte for byte, for every deployment that predates this field.
    ///
    /// Set it when the address a peer must dial differs from the address this node binds: a TLS
    /// deployment (announce `tls_resp_addr`, since `addr` is the plaintext port), or NAT and
    /// container port mapping (announce the externally reachable `host:port`). Shape-validated at
    /// startup by `validate_replica_announce_addr`; never checked for reachability, because this
    /// node cannot know how a peer routes to it. See
    /// `docs/superpowers/specs/2026-09-10-replica-announce-addr-spec.md`.
    pub replica_announce_addr: Option<String>,
```

In `impl Default for Config`, add `replica_announce_addr: None,` in the same position (after
`replicaof_auth_password: None,`).

In `impl std::fmt::Debug for Config`, add `replica_announce_addr,` to the destructuring pattern in
the same position, and the matching render line to the `debug_struct` chain, after the
`replicaof_auth_password` line:

```rust
            .field("replica_announce_addr", replica_announce_addr)
```

The field is an operator-supplied bind-style address, not key material, so it renders in full —
same treatment as `addr` and `replicaof`. **Never add a `..` rest pattern to the destructuring**;
its exhaustiveness is what forced you to make this decision at all.

- [ ] **Step 4: Add the CLI flag and its `cli_overrides` entry**

In the `Cli` struct, after the `--replicaof-auth-password` flag:

```rust
    /// `host:port` this node advertises to its leader in PSYNC, when the address a peer must dial
    /// differs from --addr [default: unset, announces --addr]
    #[arg(long)]
    pub replica_announce_addr: Option<String>,
```

In `cli_overrides`, add to the `set!` block, after `set!(replicaof_auth_password);`:

```rust
    set!(replica_announce_addr);
```

- [ ] **Step 5: Fix the two `Debug` tests the new field breaks at compile time**

`config_debug_still_renders_every_non_secret_field` builds a `Config { ... }` literal with **no**
`..Config::default()`, so it now fails to compile with "missing field
`replica_announce_addr`". That is the guard working. Add the field to its literal, in the same
position as in the struct:

```rust
            replicaof_auth_password: Some("zzleaderpassword".to_string()),
            replica_announce_addr: Some("1.1.1.1:7".to_string()),
            acl: AclBootstrapConfig::default(),
```

and add `"1.1.1.1:7"` to that test's `for expected in [...]` list, after `"zzuser"`:

```rust
            "zzuser",
            "1.1.1.1:7",
            "acl",
```

Also extend `default_config_matches_todays_hardcoded_main_rs_values` with one line, next to the
other `None` assertions:

```rust
        assert_eq!(cfg.replica_announce_addr, None);
```

- [ ] **Step 6: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --lib config::tests
```

Expected: PASS, including `replica_announce_addr_is_layered_like_every_other_optional_string_field`,
`default_config_has_no_replica_announce_addr`, and the two updated `Debug` tests.

- [ ] **Step 7: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean. Test count 994 + 2 = **996 passing**. In particular
`info_lists_each_connected_slaves_advertised_address` and
`sync_once_advertises_its_own_address_in_the_psync_frame_when_configured` must still pass with no
edit — nothing reads the new field yet.

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/config.rs
git commit -m "feat(config): add an optional replica_announce_addr field

Layered like every other optional string field: defaults < TOML <
ROCKET_MEM_REPLICA_ANNOUNCE_ADDR < --replica-announce-addr. Unset means
addr, so no existing deployment or test changes. Nothing reads it yet."
```

---

### Task 2: `validate_replica_announce_addr` shape check

**Files:**
- Modify: `crates/server/src/config.rs` — new `pub fn` next to `validate_replicaof`
- Test: `crates/server/src/config.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Config::replica_announce_addr` from Task 1.
- Produces:
  `pub fn validate_replica_announce_addr(config: &Config) -> Result<(), std::io::Error>` —
  `Ok(())` when the field is unset or shaped `host:port` with a non-empty host and a port that
  parses as `u16`; otherwise `Err` with `ErrorKind::InvalidInput` and a message that names the
  field and echoes the offending value. Plan 02, Task 2 is the only production caller.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/server/src/config.rs`, directly after the existing
`validate_replicaof_accepts_no_auth_at_all`:

```rust
    /// A helper rather than four near-identical literals: every case below differs only in the
    /// one field under test.
    fn with_announce(addr: &str) -> Config {
        Config {
            replica_announce_addr: Some(addr.to_string()),
            ..Config::default()
        }
    }

    #[test]
    fn validate_replica_announce_addr_accepts_unset_and_well_shaped_values() {
        assert!(
            validate_replica_announce_addr(&Config::default()).is_ok(),
            "unset is the default and must never fail startup"
        );
        for good in [
            "numericlabs.lxd:16479",
            "127.0.0.1:6479",
            "10.0.0.7:1",
            "host:0",
            "host:65535",
            // The bracketed IPv6 form. `rsplit_once(':')` splits on the LAST colon, so the
            // bracketed host survives intact -- exactly as `dispatcher.rs`'s `split_addr`, the
            // consumer of this value, will later split it.
            "[::1]:16479",
        ] {
            assert!(
                validate_replica_announce_addr(&with_announce(good)).is_ok(),
                "{good:?} must be accepted"
            );
        }
    }

    #[test]
    fn validate_replica_announce_addr_rejects_a_value_split_addr_would_render_as_ip_question_port_zero()
    {
        for (bad, why) in [
            ("numericlabs.lxd", "no ':' separator, so there is no port at all"),
            ("numericlabs.lxd:", "empty port"),
            ("numericlabs.lxd:notaport", "non-numeric port"),
            ("numericlabs.lxd:99999", "port above u16::MAX"),
            ("numericlabs.lxd:-1", "negative port"),
            (":16479", "empty host"),
        ] {
            let err = validate_replica_announce_addr(&with_announce(bad))
                .expect_err(&format!("{bad:?} must be rejected: {why}"));
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
            let msg = err.to_string();
            assert!(
                msg.contains("replica_announce_addr"),
                "the error must name the field so an operator has something to grep for, got: {msg}"
            );
            assert!(
                msg.contains(bad),
                "the error must echo the offending value, got: {msg}"
            );
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --lib config::tests::validate_replica_announce_addr
```

Expected: **compile error** — `error[E0425]: cannot find function
'validate_replica_announce_addr' in this scope`.

- [ ] **Step 3: Write the implementation**

Add to `crates/server/src/config.rs` immediately after `validate_replicaof`:

```rust
/// Enforces that `replica_announce_addr`, when set, is at least *shaped* like something a peer
/// could dial: `host:port`, with a non-empty host and a port that parses as a `u16`. Follows
/// `validate_replicaof`/`validate_tls`'s precedent of rejecting a malformed value at startup,
/// before anything binds, rather than degrading at display time. Today a bad value survives all
/// the way to `INFO REPLICATION`'s `split_addr`, which falls back to `("?", 0)` -- an operator
/// then sees `ip=?,port=0` and has nothing to grep for. A startup failure naming the field is
/// strictly more useful. `main.rs` calls this alongside the other two validators.
///
/// **A shape check, never a reachability check.** This node cannot know whether a *peer* can
/// reach an address, and pretending to check would be worse than not checking. The host half is
/// therefore not resolved either: a hostname whose DNS record lands after this process starts is
/// a legitimate value.
///
/// `rsplit_once(':')` deliberately matches `dispatcher.rs`'s `split_addr`, the function that
/// consumes this value downstream, so a value this accepts is exactly a value `split_addr`
/// renders correctly -- including the bracketed IPv6 form `[::1]:16479`, whose last colon is
/// still the separator.
pub fn validate_replica_announce_addr(config: &Config) -> Result<(), std::io::Error> {
    let Some(addr) = &config.replica_announce_addr else {
        return Ok(());
    };
    let invalid = |reason: &str| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "replica_announce_addr '{addr}' {reason} -- it must be host:port, \
                 e.g. \"10.0.0.7:16379\""
            ),
        )
    };
    let Some((host, port)) = addr.rsplit_once(':') else {
        return Err(invalid("has no ':' port separator"));
    };
    if host.is_empty() {
        return Err(invalid("has an empty host"));
    }
    if port.parse::<u16>().is_err() {
        return Err(invalid("has a port that is not a number in 0..=65535"));
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --lib config::tests::validate_replica_announce_addr
```

Expected: PASS, both tests.

- [ ] **Step 5: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, **998 passing**.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/config.rs
git commit -m "feat(config): shape-validate replica_announce_addr

host:port with a non-empty host and a u16 port, matching split_addr's
own rsplit_once(':') so an accepted value is one INFO REPLICATION can
render. Deliberately not a reachability check. No caller yet."
```

---

## Next plan

[`02-startup-wiring.md`](02-startup-wiring.md) — resolve the announced address from the config
and wire it into `main.rs`'s `.with_own_addr(...)`.
