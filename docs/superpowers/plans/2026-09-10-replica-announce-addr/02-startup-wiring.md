# Replica Announce Address — Plan 02: Startup Wiring

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve the announced address from the config (`replica_announce_addr`, falling back to
`addr`), make a malformed value abort startup before anything binds, and hand the resolved value
to `ReplicationHandle::with_own_addr` so it reaches the leader's `INFO REPLICATION`.

**Architecture:** One more pure `fn(&Config) -> String` in `config.rs` — same shape and same reason
as the existing `replicaof_auth`, so the `Config` → announced-address mapping is testable without
standing up `main.rs`'s whole startup path. `main.rs` then changes by exactly two lines: one
validator call next to the other two, and `config.addr.clone()` becoming
`config::announce_addr(&config)` at the `.with_own_addr(...)` builder call.

**Tech Stack:** Rust 2021, `tokio`, `redis` 0.27 (test client), `tempfile`, `figment`, `clap`.

**Spec:** [`../../specs/2026-09-10-replica-announce-addr-spec.md`](../../specs/2026-09-10-replica-announce-addr-spec.md)

**Global Constraints:** see
[`01-config-field-and-validation.md` § Global Constraints](01-config-field-and-validation.md#global-constraints).
They apply to every task below unchanged — in particular the working-directory rule, the three
gates, the deadline rule for anything reading a spawned process, and the two pinned tests that
must keep passing untouched.

---

### Task 1: `config::announce_addr` — the `Config` → announced-address mapping

**Files:**
- Modify: `crates/server/src/config.rs` — new `pub fn` next to `replicaof_auth`
- Test: `crates/server/src/config.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Config::replica_announce_addr` and `Config::addr` (plan 01, Task 1).
- Produces: `pub fn announce_addr(config: &Config) -> String`. Tasks 2 and 3 below and plan 03's
  `main.rs` changes all call it; nothing else does.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/server/src/config.rs`, directly after
`replicaof_auth_pairs_username_and_password_when_both_are_set`:

```rust
    /// The compatibility guarantee in one assertion: unset means `addr`, so a deployment that
    /// never sets the new field announces exactly what it announced before the field existed.
    #[test]
    fn announce_addr_falls_back_to_addr_when_the_field_is_unset() {
        let cfg = Config {
            addr: "127.0.0.1:6479".to_string(),
            ..Config::default()
        };
        assert_eq!(announce_addr(&cfg), "127.0.0.1:6479");
    }

    #[test]
    fn announce_addr_prefers_the_configured_announce_address_over_addr() {
        let cfg = Config {
            addr: "127.0.0.1:6479".to_string(),
            replica_announce_addr: Some("numericlabs.lxd:16479".to_string()),
            ..Config::default()
        };
        assert_eq!(
            announce_addr(&cfg),
            "numericlabs.lxd:16479",
            "the announced address must be independent of the bound one -- the whole point of the \
             field is a NAT, container, or TLS deployment where they differ"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem --lib config::tests::announce_addr
```

Expected: **compile error** — `error[E0425]: cannot find function 'announce_addr' in this scope`.

- [ ] **Step 3: Write the implementation**

Add to `crates/server/src/config.rs` immediately after `replicaof_auth`:

```rust
/// The address this node announces to its leader in `PSYNC`: `replica_announce_addr` when set,
/// otherwise `addr`. Unset therefore means today's behaviour byte for byte -- see the field's own
/// doc comment for when to set it.
///
/// Pulled out as its own function for the same reason `replicaof_auth` is: it makes the `Config`
/// -> announced-address mapping exercisable from a test without hand-building `main.rs`'s whole
/// startup path, and it keeps the fallback in one place instead of inline at the single builder
/// call site. `main.rs`'s `.with_own_addr(...)` is the only production caller.
///
/// Deliberately dumb: it never consults `tls_resp_addr`, `tls_rmp_addr`, or the cluster topology.
/// The spec rejected both of those as defaults -- deriving from the cluster config announces the
/// *leader's* address on a replica whose `cluster_node_id` names its leader, and defaulting to
/// `tls_resp_addr` silently changes what every existing follower reports the moment TLS is
/// switched on, while still assuming the reachable address is one this node binds locally (false
/// under NAT, container port mapping, or a load balancer). The misconfiguration those defaults
/// would have papered over is surfaced by `should_warn_plaintext_announce` instead.
pub fn announce_addr(config: &Config) -> String {
    config
        .replica_announce_addr
        .clone()
        .unwrap_or_else(|| config.addr.clone())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --lib config::tests::announce_addr
```

Expected: PASS, both tests.

- [ ] **Step 5: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, **1000 passing** (998 after plan 01, plus 2).

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/config.rs
git commit -m "feat(config): add announce_addr, the Config -> announced-address mapping

replica_announce_addr when set, else addr. Mirrors replicaof_auth's
shape so the mapping is testable without main.rs's startup path. No
caller yet."
```

---

### Task 2: A malformed `replica_announce_addr` aborts startup

**Files:**
- Modify: `crates/server/src/main.rs:318-319` — add the third validator call
- Test: `crates/server/tests/startup_logging.rs` — one new helper and one new test

**Interfaces:**
- Consumes: `config::validate_replica_announce_addr` (plan 01, Task 2).
- Produces: a `spawn_and_wait_for_exit(dir, extra_env) -> (bool, String)` test helper in
  `startup_logging.rs`, returning `(exit_status.success(), captured_stderr)`. Plan 03 does not
  use it; nothing else does.

- [ ] **Step 1: Write the failing test**

Add to the end of `crates/server/tests/startup_logging.rs`. Note the deadline: without it, a
missing validator call leaves the child running in its accept loop forever, and `child.wait()`
would **hang the whole test suite instead of failing this test**.

```rust
/// How long to wait for the child to exit on its own before declaring the test failed. Only paid
/// when the expectation is already going to fail -- a config the binary correctly rejects exits
/// in milliseconds.
const EXIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawns the real binary and waits for it to **exit**, returning whether it exited successfully
/// and everything it wrote to stderr. The counterpart to `spawn_and_capture_stderr` above, for
/// the configs that must abort startup rather than reach the accept loop.
///
/// `try_wait` in a bounded poll loop, never a bare `wait()` or `Command::output()`: the failure
/// mode this test exists to catch is a binary that does NOT reject the config, and such a binary
/// blocks forever in `rocket_mem::serve`. An unbounded wait would hang the suite instead of
/// failing the test. The child is killed and reaped before any assertion runs, so a panicking
/// test cannot leak a server process. Same `:0` addressing and same `RUST_LOG` scrub as
/// `spawn_and_capture_stderr`, and likewise no `--config`, so the repo-root `rocket-mem.toml` is
/// never loaded.
fn spawn_and_wait_for_exit(
    dir: &std::path::Path,
    extra_env: &[(&str, &str)],
) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rocket-mem"));
    cmd.env_remove("RUST_LOG")
        .env("ROCKET_MEM_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_AOF_PATH", dir.join("startup-exit-test.aof"))
        .env(
            "ROCKET_MEM_SNAPSHOT_PATH",
            dir.join("startup-exit-test.snapshot"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("failed to spawn the rocket-mem binary");

    let stderr = child.stderr.take().expect("child stderr was not piped");
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&captured);
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break, // EOF, or the pipe died with the child
                Ok(_) => sink.lock().unwrap().push(line),
            }
        }
    });

    let deadline = std::time::Instant::now() + EXIT_DEADLINE;
    let status = loop {
        match child.try_wait().expect("failed to poll the child") {
            Some(status) => break Some(status),
            None if std::time::Instant::now() >= deadline => break None,
            None => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    };

    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    let output = captured.lock().unwrap().concat();

    let Some(status) = status else {
        panic!(
            "the binary was still running after {EXIT_DEADLINE:?}; it must have rejected the \
             config and exited. stderr so far:\n{output}"
        );
    };
    (status.success(), output)
}

/// `validate_replica_announce_addr` is only useful if `main.rs` actually calls it, and calls it
/// early. This asserts both: a non-zero exit, and no `listener bound` line -- the validator sits
/// with `validate_replicaof`/`validate_tls`, above every `TcpListener::bind` in `main`, so a
/// rejected config must never have opened a port.
#[test]
fn a_malformed_replica_announce_addr_aborts_startup_before_any_listener_binds() {
    let dir = tempfile::tempdir().unwrap();

    let (success, stderr) = spawn_and_wait_for_exit(
        dir.path(),
        &[("ROCKET_MEM_REPLICA_ANNOUNCE_ADDR", "numericlabs.lxd")],
    );

    assert!(
        !success,
        "a replica_announce_addr with no port must fail startup, got a clean exit and:\n{stderr}"
    );
    assert!(
        stderr.contains("replica_announce_addr"),
        "the error must name the field so an operator has something to grep for, got:\n{stderr}"
    );
    assert!(
        stderr.contains("numericlabs.lxd"),
        "the error must echo the offending value, got:\n{stderr}"
    );
    assert!(
        !stderr.contains(LISTENER_EVENT),
        "validation must run before anything binds, got:\n{stderr}"
    );
}

/// The other half: a well-shaped value must not be rejected. Without this, deleting the
/// `Ok(())` arm and rejecting everything would still pass the test above.
#[test]
fn a_well_shaped_replica_announce_addr_starts_normally() {
    let dir = tempfile::tempdir().unwrap();

    let stderr = spawn_and_capture_stderr(
        dir.path(),
        &[("ROCKET_MEM_REPLICA_ANNOUNCE_ADDR", "numericlabs.lxd:16479")],
        &[],
        3,
    );

    assert_eq!(
        stderr.matches(LISTENER_EVENT).count(),
        3,
        "a valid announce address must not stop the three always-on listeners, got:\n{stderr}"
    );
}
```

- [ ] **Step 2: Run the tests to verify the first one fails**

```bash
cargo test -p rocket-mem --test startup_logging replica_announce_addr
```

Expected: `a_malformed_replica_announce_addr_aborts_startup_before_any_listener_binds` **FAILS**
— and it fails within `EXIT_DEADLINE` (about 10 seconds) with the
`"the binary was still running after 10s"` panic, because nothing calls the validator yet. It must
**fail**, not hang; if the run does not terminate, the deadline is wrong and must be fixed before
going on. `a_well_shaped_replica_announce_addr_starts_normally` passes already — that is expected,
it is the negative control.

- [ ] **Step 3: Call the validator from `main.rs`**

In `crates/server/src/main.rs`, extend the existing validator block (the two calls after the
comment beginning "Must run before the auto-connect block below"):

```rust
    rocket_mem::config::validate_replicaof(&config)?;
    rocket_mem::config::validate_tls(&config)?;
    // Same reasoning as the two above, and the same placement: a pure function of `&Config` whose
    // failure must abort before anything binds. An unparseable announced address would otherwise
    // survive to the leader's `INFO REPLICATION`, which renders it as `ip=?,port=0` -- an operator
    // sees a broken-looking replica and has nothing to grep for.
    rocket_mem::config::validate_replica_announce_addr(&config)?;
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem --test startup_logging
```

Expected: PASS, every test in the file. The malformed-address test should now complete in well
under a second, not near the deadline.

- [ ] **Step 5: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, **1002 passing**.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/main.rs crates/server/tests/startup_logging.rs
git commit -m "feat(server): reject a malformed replica_announce_addr at startup

Called alongside validate_replicaof/validate_tls, above every bind, so a
value INFO REPLICATION could only render as ip=?,port=0 fails loudly
instead. Covered by a deadline-bounded spawn-and-wait-for-exit test."
```

---

### Task 3: The announced address reaches the leader's `INFO REPLICATION`

**Files:**
- Modify: `crates/server/src/main.rs:297` — `.with_own_addr(config.addr.clone())` becomes
  `.with_own_addr(rocket_mem::config::announce_addr(&config))`
- Test: `crates/server/tests/replication.rs` — one new test

**Interfaces:**
- Consumes: `config::announce_addr` (Task 1), `ReplicationHandle::with_own_addr`
  (`crates/server/src/replication.rs:289`), `ReplicationHandle::start_replicating_from_config`.
- Produces: nothing new. This is the last code change on the resolution path.

- [ ] **Step 1: Write the regression test**

Add to the end of `crates/server/tests/replication.rs`. It drives the same composed expression
`main.rs` builds its handle from, in the same style as the file's existing
`a_node_configured_with_replicaof_auto_connects_on_startup`.

**This one is not a red-first test, and Step 2 says why.** `main` is
`#[tokio::main] async fn main` with no unit-testable seam, so the honest red case is a mutation
check on the helper it calls, run in Step 2. Do not skip it.

```rust
/// The whole path the announce-address spec cares about, end to end: a `Config` carrying
/// `replica_announce_addr` -> `config::announce_addr` -> `with_own_addr` -> the follower's
/// `PSYNC <addr>` frame -> the leader's `ReplicaRegistry` -> the leader's `INFO REPLICATION`
/// `slaveN:` line. Driving the composed expression `main.rs` uses, rather than re-testing
/// `announce_addr` (unit-tested in config.rs) or `sync_once`'s outgoing PSYNC frame (pinned in
/// replication.rs) in isolation -- neither of those would catch `main.rs` passing `config.addr`.
#[tokio::test]
async fn a_configured_replica_announce_addr_is_what_the_leader_reports_in_info_replication() {
    let (_leader_dir, _leader_engine, _leader_aof, _leader_replication, leader_addr) =
        spawn_node().await;

    let follower_dir = tempfile::tempdir().unwrap();
    let follower_engine = Arc::new(engine::Engine::new());
    let config = rocket_mem::config::Config {
        // Deliberately three different values. `addr` is what a pre-this-feature node would have
        // announced; `replica_announce_addr` is what it must announce now; neither is the
        // ephemeral source port of the connection the leader actually sees. Nothing binds either
        // one -- the announced address is informational, so an unbound value is a legitimate
        // configuration and makes the assertion below unambiguous.
        addr: "127.0.0.1:6479".to_string(),
        replicaof: Some(leader_addr.clone()),
        replica_announce_addr: Some("announced.example:16479".to_string()),
        ..rocket_mem::config::Config::default()
    };
    let follower_replication = Arc::new(
        rocket_mem::replication::ReplicationHandle::new(
            Arc::clone(&follower_engine),
            follower_dir.path().join("follower.snapshot"),
        )
        .with_own_addr(rocket_mem::config::announce_addr(&config)),
    );
    follower_replication.start_replicating_from_config(&config);

    let client = redis::Client::open(format!("redis://{leader_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    // A bounded poll, not a fixed sleep: registration happens when the leader handles the PSYNC,
    // which is a scheduling race with this connection. `let info = loop { ... break info; }`
    // rather than a `let mut` seeded with an empty String, which would trip rustc's
    // `unused_assignments` lint and so fail the `-D warnings` gate.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let info = loop {
        let info: String = redis::cmd("INFO")
            .arg("replication")
            .query_async(&mut con)
            .await
            .unwrap();
        if info.contains("slave0:ip=announced.example,port=16479,state=online") {
            break info;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the leader never reported the follower's announced address, last INFO was:\n{info}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert!(
        !info.contains("port=6479"),
        "the bound `addr` must not be what gets announced once the field is set, got:\n{info}"
    );
}
```

- [ ] **Step 2: Run the test, then mutation-check it**

```bash
cargo test -p rocket-mem --test replication a_configured_replica_announce_addr
```

Expected: **PASS**, before `main.rs` is touched. That is correct and expected — the test builds
the handle itself, so it is the regression guard for the whole resolution path, not the red case
for Task 3's one-line `main.rs` change, which has no unit-testable seam.

The red case is a mutation check, and it is mandatory. In `crates/server/src/config.rs`, change
`announce_addr`'s body to `config.addr.clone()`:

```bash
cargo test -p rocket-mem --test replication a_configured_replica_announce_addr
```

Expected: **FAIL**, with `port=6479` in the reported INFO body. Then revert the mutation and
re-run to confirm it passes again. If the mutated version still passes, the test proves nothing
and must be fixed before continuing. `main.rs`'s own line is then confirmed by hand in Step 5.

- [ ] **Step 3: Change `main.rs`'s builder call**

In `crates/server/src/main.rs`, in the `ReplicationHandle::new(...)` builder chain, replace:

```rust
    .with_own_addr(config.addr.clone())
```

with:

```rust
    // Not `config.addr`: that is the *plaintext* RESP listen address unconditionally, so a TLS
    // deployment used to advertise a port a TLS peer must not dial. `announce_addr` falls back to
    // `config.addr` when `replica_announce_addr` is unset, so this is byte-for-byte the old
    // behaviour for every deployment that does not set the new field. See
    // docs/superpowers/specs/2026-09-10-replica-announce-addr-spec.md.
    .with_own_addr(rocket_mem::config::announce_addr(&config))
```

Also update `ReplicationHandle::with_own_addr`'s doc comment in
`crates/server/src/replication.rs`, which currently ends "Only `main.rs` calls this, with
`config.addr`" — that sentence is now false:

```rust
    /// Sets the address this node advertises on every `PSYNC` its follower loop sends -- see the
    /// `own_addr` field's doc comment. Only `main.rs` calls this, with
    /// `config::announce_addr(&config)`: `replica_announce_addr` when the operator set one, else
    /// `config.addr`.
```

- [ ] **Step 4: Run the tests to verify nothing regressed**

```bash
cargo test -p rocket-mem --test replication
cargo test -p rocket-mem --lib replication::tests::sync_once_advertises_its_own_address_in_the_psync_frame_when_configured
cargo test -p rocket-mem --lib dispatcher::tests::info_lists_each_connected_slaves_advertised_address
```

Expected: PASS, all of them, with **no edit** to either of the two pinned tests.

- [ ] **Step 5: Verify `main.rs`'s own line by hand**

Build and run a leader and a follower, and read the leader's `INFO`. Run these from the root of
the checkout you are editing (see Global Constraints), not from any hardcoded path.

```bash
cargo build --release
rm -rf /tmp/rm-announce && mkdir -p /tmp/rm-announce

ROCKET_MEM_ADDR=127.0.0.1:6400 ROCKET_MEM_RMP_ADDR=127.0.0.1:6480 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9200 \
ROCKET_MEM_AOF_PATH=/tmp/rm-announce/leader.aof \
ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-announce/leader.snap \
  ./target/release/rocket-mem &

cat > /tmp/rm-announce/follower.toml <<'EOF'
addr = "127.0.0.1:6401"
rmp_addr = "127.0.0.1:6481"
metrics_addr = "127.0.0.1:9201"
aof_path = "/tmp/rm-announce/follower.aof"
snapshot_path = "/tmp/rm-announce/follower.snap"
replicaof = "127.0.0.1:6400"
replica_announce_addr = "127.0.0.1:16401"
EOF
./target/release/rocket-mem --config /tmp/rm-announce/follower.toml &

sleep 1
redis-cli -p 6400 info replication | grep '^slave0:'
# EXPECT: slave0:ip=127.0.0.1,port=16401,state=online     <- the announced port, not 6401

kill %2
sed -i '/^replica_announce_addr/d' /tmp/rm-announce/follower.toml
./target/release/rocket-mem --config /tmp/rm-announce/follower.toml &
sleep 1
redis-cli -p 6400 info replication | grep '^slave0:'
# EXPECT: an entry with port=6401 -- the `addr` fallback, i.e. today's behaviour unchanged.
# The stale 16401 entry may still be listed until the dropped connection is pruned; what
# matters is that a port=6401 entry now exists and did not before.

kill %1 %2
```

`--config` points at a file written into `/tmp`, never the repo-root `rocket-mem.toml`, which is a
live deployment's config. Neither node has ACL users, so no `AUTH` is needed and each `redis-cli`
invocation is self-contained.

- [ ] **Step 6: Run the three gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean, **1003 passing**.

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/main.rs crates/server/src/replication.rs crates/server/tests/replication.rs
git commit -m "feat(server): announce replica_announce_addr instead of addr in PSYNC

main.rs handed with_own_addr the plaintext RESP listen address
unconditionally, so a TLS node advertised a port a TLS peer must not
dial. announce_addr falls back to addr, so an unset field is byte-for-byte
the old behaviour and both pinned tests are untouched."
```

---

## Next plan

[`03-plaintext-announce-warning.md`](03-plaintext-announce-warning.md) — warn at startup when a
TLS follower is about to announce its plaintext address.
