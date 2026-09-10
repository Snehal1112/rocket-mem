# Replica Announce Address — Plan 03: The Plaintext-Announce Warning

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Emit exactly one `warn` at startup when a node is configured as a follower, serves at
least one TLS listener, and has no `replica_announce_addr` — the misconfiguration the spec exists
to make visible, since defaulting is deliberately dumb.

**Architecture:** The three-condition test is a pure `fn(&Config) -> bool` in `config.rs`,
exhaustively unit-tested there. `main.rs` guards a single `tracing::warn!` on it, placed with the
validator calls — above every `TcpListener::bind`, so the line lands before the listener-bound
lines an operator scrolls past. One line, at startup, never per-command.

**Tech Stack:** Rust 2021, `tracing` / `tracing_subscriber::fmt`, `tempfile`.

**Spec:** [`../../specs/2026-09-10-replica-announce-addr-spec.md`](../../specs/2026-09-10-replica-announce-addr-spec.md)
(§ "Warn when the announced address contradicts the transport")

**Global Constraints:** see
[`01-config-field-and-validation.md` § Global Constraints](01-config-field-and-validation.md#global-constraints).
Two of them bite hard in this plan: **log-capture assertions live in `crates/server/tests/`, never
in a `#[cfg(test)] mod tests` inside `src/`**, and **any assertion on log output must state the
exact rendered form** — `%` (Display) renders unquoted, `?` (Debug) renders a `&str` quoted.

---

### Task 1: `config::should_warn_plaintext_announce` — the three-condition predicate

**Files:**
- Modify: `crates/server/src/config.rs` — new `pub fn` after `announce_addr`
- Test: `crates/server/src/config.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Config::replicaof`, `Config::tls_resp_addr`, `Config::tls_rmp_addr`,
  `Config::replica_announce_addr`.
- Produces: `pub fn should_warn_plaintext_announce(config: &Config) -> bool`. Task 2's `main.rs`
  guard is the only production caller.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/server/src/config.rs`, directly after the `announce_addr` tests from
plan 02.

```rust
    /// A config where all three of the spec's warn conditions hold at once: this node is a
    /// follower, it serves a TLS listener, and it announces nothing. Each negative test below
    /// flips exactly one of them, so a passing negative can only mean that one condition matters.
    fn warnable() -> Config {
        Config {
            addr: "127.0.0.1:6479".to_string(),
            replicaof: Some("127.0.0.1:6379".to_string()),
            tls_resp_addr: Some("127.0.0.1:16479".to_string()),
            tls_cert_path: Some("/certs/cert.pem".to_string()),
            tls_key_path: Some("/certs/key.pem".to_string()),
            ..Config::default()
        }
    }

    #[test]
    fn should_warn_plaintext_announce_fires_for_a_tls_follower_that_announces_nothing() {
        assert!(should_warn_plaintext_announce(&warnable()));

        // An RMP TLS listener counts too. Either address means this deployment intended TLS, and
        // `main.rs`'s own `tls_enabled` summary field is derived the same way.
        let rmp_only = Config {
            tls_resp_addr: None,
            tls_rmp_addr: Some("127.0.0.1:17479".to_string()),
            ..warnable()
        };
        assert!(should_warn_plaintext_announce(&rmp_only));
    }

    #[test]
    fn should_warn_plaintext_announce_is_silent_when_any_single_condition_is_missing() {
        let not_a_follower = Config {
            replicaof: None,
            ..warnable()
        };
        assert!(
            !should_warn_plaintext_announce(&not_a_follower),
            "a leader announces nothing to anyone -- there is nothing to warn about"
        );

        let no_tls = Config {
            tls_resp_addr: None,
            tls_rmp_addr: None,
            ..warnable()
        };
        assert!(
            !should_warn_plaintext_announce(&no_tls),
            "a plaintext follower announcing its plaintext address is correct, not a mistake"
        );

        let announced = Config {
            replica_announce_addr: Some("127.0.0.1:16479".to_string()),
            ..warnable()
        };
        assert!(
            !should_warn_plaintext_announce(&announced),
            "the operator has already said where a peer must dial -- warning again is noise"
        );

        assert!(
            !should_warn_plaintext_announce(&Config::default()),
            "the all-defaults standalone case must be silent"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --lib config::tests::should_warn_plaintext_announce
```

Expected: **compile error** — `error[E0425]: cannot find function
'should_warn_plaintext_announce' in this scope`.

- [ ] **Step 3: Write the implementation**

Add to `crates/server/src/config.rs` immediately after `announce_addr`:

```rust
/// Whether startup should warn that this node is about to announce its *plaintext* address to its
/// leader. True when all three of the spec's conditions hold at once: this node is configured as a
/// follower, it serves at least one TLS listener, and `replica_announce_addr` is unset -- so
/// `announce_addr` falls back to `addr`, the plaintext RESP listen address, on a deployment that
/// clearly intended TLS. See
/// `docs/superpowers/specs/2026-09-10-replica-announce-addr-spec.md`'s "Warn when the announced
/// address contradicts the transport".
///
/// This is the visibility that pays for `announce_addr` being deliberately dumb. The spec rejected
/// silently defaulting to `tls_resp_addr`, because that changes what every existing follower
/// reports the moment TLS is switched on and still assumes the reachable address is one this node
/// binds locally -- false under NAT, container port mapping, or a load balancer. Making the
/// mismatch loud is the honest alternative to guessing at it.
///
/// **Deliberately startup-only, and deliberately keyed on the `replicaof` config field rather than
/// on live follower state.** The spec asks for one line at startup and never one per command, and
/// a node made a follower later by a live `REPLICAOF` has no startup moment to warn at. A
/// TLS-serving, announce-address-less node that only ever becomes a follower via the live command
/// is therefore not warned about. That is accepted, and it is why this is a `&Config` predicate
/// rather than a check inside `handle_replicaof`.
pub fn should_warn_plaintext_announce(config: &Config) -> bool {
    config.replicaof.is_some()
        && (config.tls_resp_addr.is_some() || config.tls_rmp_addr.is_some())
        && config.replica_announce_addr.is_none()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --lib config::tests::should_warn_plaintext_announce
```

Expected: PASS, both tests.

- [ ] **Step 5: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, **1005 passing** (1003 after plan 02, plus 2).

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/config.rs
git commit -m "feat(config): add should_warn_plaintext_announce

True only when a node is a configured follower, serves a TLS listener,
and announces nothing -- so announce_addr falls back to the plaintext
addr on a deployment that intended TLS. No caller yet."
```

---

### Task 2: `main.rs` emits the warning, once, at startup

**Files:**
- Modify: `crates/server/src/main.rs` — one guarded `tracing::warn!` after the validator block
- Test: `crates/server/tests/startup_logging.rs` — two new tests

**Interfaces:**
- Consumes: `config::should_warn_plaintext_announce` (Task 1); `spawn_and_capture_stderr`
  (existing helper in `startup_logging.rs`).
- Produces: nothing new. This closes the feature's code path.

- [ ] **Step 1: Write the failing tests**

Add to the end of `crates/server/tests/startup_logging.rs`. The exact rendered form matters: the
`%` sigil is `Display`, so `tracing_subscriber::fmt` writes `announced=127.0.0.1:0` **unquoted**.
A `?` sigil would render a `&str` as `announced="127.0.0.1:0"` and these assertions would fail —
that is intentional, it pins the sigil.

```rust
/// A TLS follower that never says where a peer should reach it announces its *plaintext* address,
/// which is the misconfiguration the announce-address spec exists to surface. It must be visible
/// at startup, at `warn`, exactly once -- not discovered later by reading a leader's
/// `INFO REPLICATION` and noticing the port is the wrong one.
#[test]
fn a_tls_follower_with_no_announce_addr_is_warned_about_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let cert = fixtures.join("test-cert.pem");
    let key = fixtures.join("test-key.pem");

    // `replicaof` points at a port nothing listens on: the warn is a pure function of the config
    // and fires before any connection is attempted, so a doomed reconnect loop is harmless here.
    // Its own retry warnings carry a different message and cannot satisfy the assertions below.
    let stderr = spawn_and_capture_stderr(
        dir.path(),
        &[
            ("ROCKET_MEM_REPLICAOF", "127.0.0.1:1"),
            ("ROCKET_MEM_TLS_RESP_ADDR", "127.0.0.1:0"),
            ("ROCKET_MEM_TLS_CERT_PATH", cert.to_str().unwrap()),
            ("ROCKET_MEM_TLS_KEY_PATH", key.to_str().unwrap()),
        ],
        &[],
        4, // metrics, RMP, RESP+TLS, RESP
    );

    let line = stderr
        .lines()
        .find(|l| l.contains("replica_announce_addr is unset"))
        .unwrap_or_else(|| {
            panic!("expected a plaintext-announce warning at startup, got:\n{stderr}")
        });

    assert!(
        line.contains("WARN"),
        "this is an operator-actionable misconfiguration, so it must be warn, not info: {line}"
    );
    // `spawn_and_capture_stderr` sets ROCKET_MEM_ADDR=127.0.0.1:0, and the warning reports the
    // configured `addr` -- the value that would be announced -- not the OS-assigned bound port.
    // Unquoted, because the field is rendered with `%` (Display); a `?` sigil would produce
    // `announced="127.0.0.1:0"` and this assertion would fail.
    assert!(
        line.contains("announced=127.0.0.1:0"),
        "the warning must name the plaintext address being announced, unquoted: {line}"
    );
    assert_eq!(
        stderr.matches("replica_announce_addr is unset").count(),
        1,
        "one line at startup, never a repeat, got:\n{stderr}"
    );
}

/// The fix must actually silence the warning -- otherwise an operator who sets the field keeps
/// seeing it and learns to ignore it. Identical config to the test above except for the one
/// field, so a pass here can only mean that field is what turned it off.
#[test]
fn setting_replica_announce_addr_silences_the_startup_warning() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let cert = fixtures.join("test-cert.pem");
    let key = fixtures.join("test-key.pem");

    let stderr = spawn_and_capture_stderr(
        dir.path(),
        &[
            ("ROCKET_MEM_REPLICAOF", "127.0.0.1:1"),
            ("ROCKET_MEM_TLS_RESP_ADDR", "127.0.0.1:0"),
            ("ROCKET_MEM_TLS_CERT_PATH", cert.to_str().unwrap()),
            ("ROCKET_MEM_TLS_KEY_PATH", key.to_str().unwrap()),
            ("ROCKET_MEM_REPLICA_ANNOUNCE_ADDR", "numericlabs.lxd:16479"),
        ],
        &[],
        4,
    );

    assert!(
        !stderr.contains("replica_announce_addr is unset"),
        "a node that announces an explicit address must not be warned, got:\n{stderr}"
    );
    // The canary for the assertion above: it must be looking at a real startup log, not at an
    // empty capture that would make any absence-assertion pass vacuously.
    assert_eq!(
        stderr.matches(LISTENER_EVENT).count(),
        4,
        "the capture must contain a real startup log, got:\n{stderr}"
    );
}
```

- [ ] **Step 2: Run the tests to verify the first one fails**

```bash
cargo test -p rocket-mem --test startup_logging announce
```

Expected: `a_tls_follower_with_no_announce_addr_is_warned_about_at_startup` **FAILS** with
"expected a plaintext-announce warning at startup", because nothing emits it yet.
`setting_replica_announce_addr_silences_the_startup_warning` passes already — it is the negative
control, and Step 5 mutation-checks it so its pass is not vacuous.

- [ ] **Step 3: Emit the warning from `main.rs`**

In `crates/server/src/main.rs`, directly after the three validator calls and **before**
`replication.start_replicating_from_config(&config);`:

```rust
    // The misconfiguration the announce-address spec exists to make visible: a follower serving
    // TLS still tells its leader to find it at `addr`, the plaintext RESP listen address, because
    // `announce_addr` is deliberately dumb rather than guessing at `tls_resp_addr`. One line, at
    // startup, above every bind -- never per-command. See
    // docs/superpowers/specs/2026-09-10-replica-announce-addr-spec.md's "Warn when the announced
    // address contradicts the transport".
    //
    // `config.addr` is rendered unescaped, matching the `addr = %config.addr` field in the
    // resolved-config summary above. It is an operator-supplied local config value, not the
    // network-supplied `PSYNC` bulk that `connection.rs` routes through `logging::escape_ident` --
    // no remote party can put bytes here.
    if rocket_mem::config::should_warn_plaintext_announce(&config) {
        tracing::warn!(
            announced = %config.addr,
            "replica_announce_addr is unset while a TLS listener is configured -- this node \
             advertises its plaintext address to its leader"
        );
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --test startup_logging
```

Expected: PASS, every test in the file.

- [ ] **Step 5: Mutation-check the silencing test**

`setting_replica_announce_addr_silences_the_startup_warning` is an absence assertion, and an
absence assertion is worthless unless it can fail. Prove it can.

In `crates/server/src/config.rs`, drop the third condition from
`should_warn_plaintext_announce` — that is, delete
`&& config.replica_announce_addr.is_none()`:

```bash
cargo test -p rocket-mem --test startup_logging setting_replica_announce_addr_silences
```

Expected: **FAIL** — "a node that announces an explicit address must not be warned". Revert the
mutation and re-run to confirm it passes again. If the mutated version still passes, the test is
not exercising the branch it claims to and must be fixed before continuing.

- [ ] **Step 6: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, **1007 passing**.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/main.rs crates/server/tests/startup_logging.rs
git commit -m "feat(server): warn when a TLS follower announces its plaintext address

One warn at startup, above every bind, when replicaof is set, a TLS
listener is configured, and replica_announce_addr is not -- the case
where announce_addr falls back to the plaintext addr on a deployment
that intended TLS. Silenced by setting the field."
```

---

## Next plan

[`04-documentation-and-spec-alignment.md`](04-documentation-and-spec-alignment.md) — document the
field across the four docs that describe configuration, and record the TLS gap in the
sentinel-failover spec.
