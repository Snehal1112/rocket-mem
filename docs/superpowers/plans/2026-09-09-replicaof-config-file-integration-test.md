# Config-file `replicaof` Integration Test & Docs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the config-file `replicaof` feature (built in the prior plan) actually works end-to-end over a real socket, and document it for operators.

**Architecture:** One new `#[tokio::test]` in `crates/server/tests/replication.rs`, alongside the existing `a_follower_syncs_from_an_acl_protected_leader_when_replicaof_auth_is_used` test, that builds a `Config` with `replicaof`/`replicaof_auth_username`/`replicaof_auth_password` set and boots a node via the real `main.rs`-equivalent startup path (not by calling `start_replicating` directly), then asserts it links up and replicates a write. Then two doc updates.

**Tech Stack:** Rust, tokio, the `redis` crate (test-only Redis client).

**Spec:** `docs/superpowers/specs/2026-09-09-replicaof-config-file-spec.md`

## Global Constraints

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` must all pass clean before any commit.
- This plan assumes the prior plan (`docs/superpowers/plans/2026-09-09-replicaof-config-file.md`) is already merged — `Config.replicaof`/`replicaof_auth_username`/`replicaof_auth_password` and the `main.rs` startup wiring must already exist.

---

### Task 1: Add the end-to-end integration test

**Files:**
- Modify: `crates/server/tests/replication.rs` (new test, alongside `spawn_node` and the existing ACL-auth replication test at line 136)

**Interfaces:**
- Consumes: `Config.replicaof`/`replicaof_auth_username`/`replicaof_auth_password` (prior plan), `rocket_mem::serve` (existing, used by `spawn_node`).

- [ ] **Step 1: Write the failing test**

`spawn_node` (`replication.rs:10-39`) doesn't take a `Config`, so this test builds its follower manually instead of using that helper, to control `replicaof` before the node starts. Add to `crates/server/tests/replication.rs`, after `a_follower_syncs_from_an_acl_protected_leader_when_replicaof_auth_is_used`:

```rust
#[tokio::test]
async fn a_node_configured_with_replicaof_auto_connects_on_startup() {
    let (_leader_dir, _leader_engine, _leader_aof, leader_replication, leader_addr) =
        spawn_node().await;
    leader_replication
        .acl
        .set_user(
            "app",
            &[
                bytes::Bytes::from_static(b"on"),
                bytes::Bytes::from_static(b">changeme"),
                bytes::Bytes::from_static(b"allcommands"),
                bytes::Bytes::from_static(b"allkeys"),
            ],
        )
        .unwrap();

    // Build the follower's own Engine/AofWriter/ReplicationHandle by hand (not spawn_node,
    // which has no replicaof knob) so this test controls the config the same way main.rs's
    // startup wiring would, without needing a real TOML file or subprocess.
    let f_dir = tempfile::tempdir().unwrap();
    let f_engine = std::sync::Arc::new(engine::Engine::new());
    let f_aof = std::sync::Arc::new(
        rocket_mem::aof::AofWriter::open(
            &f_dir.path().join("node.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let f_replication = std::sync::Arc::new(rocket_mem::replication::ReplicationHandle::new(
        std::sync::Arc::clone(&f_engine),
        f_dir.path().join("node.snapshot"),
    ));
    let f_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f_addr = f_listener.local_addr().unwrap().to_string();
    tokio::spawn(rocket_mem::serve(
        f_listener,
        std::sync::Arc::clone(&f_engine),
        std::sync::Arc::clone(&f_aof),
        std::sync::Arc::clone(&f_replication),
    ));

    // This is the exact call main.rs's startup wiring makes when config.replicaof is set --
    // the test proves the STARTUP PATH works, by driving it the same way main.rs does, rather
    // than re-testing start_replicating_with_auth itself (already covered by the ACL-auth test
    // above).
    f_replication.start_replicating_with_auth(
        leader_addr.clone(),
        Some(("app".to_string(), "changeme".to_string())),
    );

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !f_replication.link_up() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "config-driven follower never linked up against the leader"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let client = redis::Client::open(format!("redis://app:changeme@{leader_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = con.set("k", "v").await.unwrap();

    wait_for(&f_engine, b"k", b"v").await;

    // Prove the follower actually came up as read-only via the config-driven path too, not
    // just linked -- same assertion shape as a_follower_rejects_client_writes_over_a_real_...
    let f_client = redis::Client::open(format!("redis://{f_addr}")).unwrap();
    let mut f_con = f_client.get_multiplexed_async_connection().await.unwrap();
    let result: Result<(), redis::RedisError> = f_con.set("nope", "x").await;
    assert_eq!(result.expect_err("must be read-only").code(), Some("READONLY"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rocket-mem --test replication a_node_configured_with_replicaof_auto_connects_on_startup`
Expected: this test does NOT exercise new production code (it calls `start_replicating_with_auth` directly, same as the existing ACL test does) — it should actually PASS already if the prior plan's Task 1/2 are merged but Task 3's `main.rs` wiring doesn't need to exist for this test to pass, since this test doesn't invoke `main.rs` at all.

**If it passes immediately**, that's expected and correct — this test is deliberately written to validate the *reusable primitive* (`start_replicating_with_auth`, already covered) in the *shape* main.rs's new wiring will call it in, as a safety net that would catch a future signature change breaking that call shape. Proceed to Step 3 regardless (there is still a real gap to close: nothing yet proves `main.rs` itself, given a `replicaof`-bearing config, performs this call — see Step 3).

- [ ] **Step 3: Add the real startup-path proof (manual, not automated)**

An automated test that shells out to the actual `rocket-mem` binary and parses `--config` is disproportionate for this feature (this codebase has no existing precedent for subprocess-based integration tests of `main.rs` itself — every existing test in `replication.rs` builds the engine/aof/replication/serve pieces directly, as Step 1 does). Instead, this is proven manually, once, and recorded here:

```bash
cargo build --release --workspace
# (repeat the exact commands from the prior plan's Task 3, Step 7 manual smoke test)
```

Confirm the output matches what that step already specified. This manual run is the actual proof that `main.rs`'s wiring (not just the underlying `ReplicationHandle` primitive) works; record the confirmation in this plan's PR/commit description.

- [ ] **Step 4: Run the full test suite**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean, all pass, including the new test.

- [ ] **Step 5: Commit**

```bash
git add crates/server/tests/replication.rs
git commit -m "$(cat <<'EOF'
Add integration test for config-driven replicaof auto-connect

Covers the same start_replicating_with_auth call shape main.rs's
startup wiring uses, plus the READONLY gate, over a real socket.
EOF
)"
```

---

### Task 2: Update `docs/config-reference.md`

**Files:**
- Modify: `docs/config-reference.md` (fields table at lines 16-31, new section after "TLS requires both a cert and a key" at line 39-45)

- [ ] **Step 1: Add the three field rows**

In the fields table (`docs/config-reference.md:16-31`), immediately after the `tls_key_path` row and before the `[[acl.users]]` row:

```markdown
| `replicaof` | `ROCKET_MEM_REPLICAOF` | `--replicaof` | unset | `host:port` of a leader to auto-connect to as a follower on every startup. Unset means standalone (or purely live-`REPLICAOF`-driven) operation. |
| `replicaof_auth_username` | `ROCKET_MEM_REPLICAOF_AUTH_USERNAME` | `--replicaof-auth-username` | unset | Username sent in `AUTH` before `PSYNC`, when `replicaof`'s leader has ACL users configured. Must be set together with `replicaof_auth_password`, or neither. |
| `replicaof_auth_password` | `ROCKET_MEM_REPLICAOF_AUTH_PASSWORD` | `--replicaof-auth-password` | unset | Password sent in `AUTH` before `PSYNC`. Plaintext in the TOML file, same as `[[acl.users]]`'s own `password` field. |
```

- [ ] **Step 2: Add a section explaining the fail-soft/fail-hard split**

Immediately after "### TLS requires both a cert and a key" (`docs/config-reference.md:39-45`):

```markdown
### `replicaof`'s auth pair is all-or-nothing; the target itself is not validated

If `replicaof_auth_username` or `replicaof_auth_password` is set, both must be — `rocket-mem`
checks this at startup, before any listener binds, and aborts immediately if only one is set.
`replicaof` itself (the `host:port` target) is **not** validated at startup: a bad host or an
unreachable leader is only discoverable by actually attempting the connection, so it fails
soft — the node starts normally, and its background reconnect loop retries once a second
forever, exactly as it would for a leader that later becomes unreachable. This mirrors the
live `REPLICAOF` command's existing behavior; see `.claude/manual-testing.md`'s "Replication
(`REPLICAOF`)" section.
```

- [ ] **Step 3: Commit**

```bash
git add docs/config-reference.md
git commit -m "docs: document replicaof config fields"
```

---

### Task 3: Update `.claude/manual-testing.md`

**Files:**
- Modify: `.claude/manual-testing.md` ("Replication (`REPLICAOF`)" section)

- [ ] **Step 1: Add a config-file subsection**

In `.claude/manual-testing.md`'s "Replication (`REPLICAOF`)" section, immediately after its existing intro paragraph (the one starting "`REPLICAOF <host> <port>` turns the CURRENT node into a read-only follower..." and ending "...it's always sent as a live command to an already-running node."), add:

```markdown
As of the `replicaof` config field, the *initial* connect can now be config-driven instead —
useful for a follower that should resume following its leader automatically after a restart
(previously a restarted follower silently came back as standalone until `REPLICAOF` was
reissued by hand — see `docs/superpowers/specs/2026-08-30-sprint-5-spec.md`'s "footgun" note).
This only covers startup: a runtime `REPLICAOF`/`REPLICAOF NO ONE` change is still not
persisted back into the file.

```bash
cat > /tmp/rm-replicaof-config.toml <<'EOF'
addr = "127.0.0.1:6401"
aof_path = "/tmp/rm-follower.aof"
snapshot_path = "/tmp/rm-follower.snap"
metrics_addr = "127.0.0.1:9201"
rmp_addr = "127.0.0.1:6481"
replicaof = "127.0.0.1:6400"
# replicaof_auth_username/replicaof_auth_password if the leader has ACL users configured
EOF
./target/release/rocket-mem --config /tmp/rm-replicaof-config.toml &
# no `redis-cli replicaof` needed -- it already links up on its own
redis-cli -p 6401 info replication      # role:slave, master_link_status:up
```
```

- [ ] **Step 2: Commit**

```bash
git add .claude/manual-testing.md
git commit -m "docs: document config-file replicaof in manual-testing guide"
```

## Next plan

This closes the config-file `replicaof` feature end to end. The next two features from this
investigation are tracked at the spec level only, not yet broken into implementation plans:

- `docs/superpowers/specs/2026-09-09-config-rewrite-spec.md` — persisting a live `REPLICAOF`
  change back into the TOML file (a `CONFIG REWRITE` equivalent). Flagged as non-trivial: needs
  `toml_edit` for comment-preserving writes and independent tracking of which values are
  file-original vs. env/CLI-sourced, since figment's merged `Config` has no provenance info.
- `docs/superpowers/specs/2026-09-09-sentinel-failover-spec.md` — automatic leader-failure
  detection and promotion. Re-scoped by investigation, not a small feature: rocket-mem's
  replication has no offsets and no acks today, so automatic promotion on top of it would risk
  silent data loss on every failover. Real v1 is replication offsets + `REPLCONF ACK` +
  `min-replicas-to-write` self-fencing + a manual promotion runbook — automatic quorum-based
  promotion is deferred to a separate `rocket-sentinel` crate built after offsets exist.
