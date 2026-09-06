# Wire BGREWRITEAOF Into the Dispatcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** make `BGREWRITEAOF` reachable from a real client over RESP/RMP, ACL-gated exactly like `SAVE`, and correctly excluded from cluster-slot routing and AOF logging.

**Architecture:** mirrors `SAVE`'s existing wiring exactly (`is_save_command`/`handle_save`'s call site in `dispatch_and_log_inner`, `KNOWN_COMMANDS`, `key_spec`'s keyless list). ACL gating requires no new code at all — `auth_gate` (`dispatcher.rs:2210`) already checks any command name generically against the authenticated user's rules before `dispatch_and_log_inner` reaches the `SAVE`/`BGREWRITEAOF` interception, so a command merely needs to be recognized by name for its ACL grant (`+bgrewriteaof`) to be enforced.

**Tech Stack:** none new.

**Spec:** [`../../specs/2026-09-06-aof-compaction-design.md`](../../specs/2026-09-06-aof-compaction-design.md), "Decision: `BGREWRITEAOF` command" (ACL gating) and "Definition of done".

## Global Constraints

- `BGREWRITEAOF` must NOT be added to `crate::aof::WRITE_COMMANDS` — like `SAVE`, it doesn't mutate the keyspace, so logging it would append it into the very AOF generation it just rotated to.
- Depends on plan 03 (`handle_bgrewriteaof`) being merged first.

---

### Task 1: Recognize and dispatch `BGREWRITEAOF`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `handle_bgrewriteaof` (plan 03).
- Produces: nothing new for other plans — this is the last piece that makes the command reachable. Plan 06's integration tests depend on this.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`
#[test]
fn bgrewriteaof_is_reachable_through_dispatch_and_log() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

    let reply = dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"BGREWRITEAOF"]),
        &Session::new(),
        1,
    );

    assert_eq!(reply, Frame::Simple("OK".into()));
    assert_eq!(crate::aof::read_generation(&snapshot_path).unwrap(), 1);
}

#[test]
fn bgrewriteaof_is_not_appended_to_the_aof() {
    let engine = std::sync::Arc::new(Engine::new());
    let (dir, aof) = test_aof();
    let snapshot_path = dir.path().join("test.snapshot");
    let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path);

    dispatch_and_log(
        &engine,
        &aof,
        &replication,
        cmd(&[b"BGREWRITEAOF"]),
        &Session::new(),
        1,
    );
    aof.fsync().unwrap();

    // The rotated-to generation-1 AOF must be empty -- BGREWRITEAOF itself is never logged.
    assert_eq!(std::fs::read(aof.path()).unwrap(), b"");
}

#[test]
fn write_commands_excludes_bgrewriteaof() {
    assert!(!crate::aof::WRITE_COMMANDS.contains(&"BGREWRITEAOF"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::bgrewriteaof -- --nocapture`
Expected: FAIL — `BGREWRITEAOF` currently falls through to `dispatch`'s unknown-command error, so the first two tests get an error reply instead of `OK`/an empty AOF. The third test passes already (nothing to exclude yet) — that's fine, it exists to stay green after Step 3, proving the exclusion holds going forward.

- [ ] **Step 3: Implement**

Add a keyless-command interception function, mirroring `is_save_command` exactly:

```rust
// crates/server/src/dispatcher.rs — directly below `is_save_command`
fn is_bgrewriteaof_command(frame: &Frame) -> bool {
    let Frame::Array(items) = frame else {
        return false;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return false;
    };
    name.eq_ignore_ascii_case(b"BGREWRITEAOF")
}
```

Wire it into `dispatch_and_log_inner`, directly after the existing `SAVE` interception:

```rust
    if is_save_command(&frame) {
        return handle_save(aof, replication);
    }
    if is_bgrewriteaof_command(&frame) {
        return handle_bgrewriteaof(aof, replication);
    }
```

Add `"BGREWRITEAOF"` to `KNOWN_COMMANDS` (alphabetically, between `"AUTH"` and `"CLUSTER"`):

```rust
pub(crate) const KNOWN_COMMANDS: &[&str] = &[
    "ACL",
    "APPEND",
    "AUTH",
    "BGREWRITEAOF",
    "CLUSTER",
    // ... unchanged from here
```

Add `"BGREWRITEAOF"` to `key_spec`'s keyless match arm, alongside `"SAVE"`:

```rust
        "PING" | "ECHO" | "SELECT" | "COMMAND" | "INFO" | "HELLO" | "KEYS" | "SCAN"
        | "RANDOMKEY" | "CLUSTER" | "SAVE" | "BGREWRITEAOF" | "REPLICAOF" | "PSYNC"
        | "SLOWLOG" | "DEBUG" | "AUTH" | "ACL" => {
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher:: -- --nocapture`
Expected: all PASS, including the pre-existing `known_commands_is_sorted_so_binary_search_works` test — `"BGREWRITEAOF"` sorts correctly between `"AUTH"` and `"CLUSTER"` (if that test fails, the insertion point above is wrong; fix the position, don't touch the sort-check test).

- [ ] **Step 5: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p rocket-mem --lib dispatcher::`
Expected: all green.

Use the `1-git-commit` skill to commit `crates/server/src/dispatcher.rs`.

---

### Task 2: ACL denies `BGREWRITEAOF` without an explicit grant

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `auth_gate` (existing, unchanged), Task 1's `is_bgrewriteaof_command`/`KNOWN_COMMANDS` entry.
- Produces: nothing new — a regression test proving the "no code change needed" claim in this plan's Architecture section is actually true.

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/src/dispatcher.rs — inside `mod tests`, near
// `auth_gate_denies_a_command_the_authenticated_user_lacks_a_grant_for`
#[test]
fn auth_gate_denies_bgrewriteaof_to_a_user_without_that_grant() {
    let replication = ReplicationHandle::default();
    // `auth_gate` re-resolves the LIVE user by username, so this fabricated `acl_user(...)`
    // needs a matching real registration or it's treated as deleted (NOAUTH, not NOPERM).
    replication
        .acl
        .set_user(
            "app",
            &[
                Bytes::from_static(b"on"),
                Bytes::from_static(b"+get"),
                Bytes::from_static(b"~*"),
            ],
        )
        .unwrap();
    let session = Session::new();
    session.set_authenticated_user(Some(acl_user(vec![
        crate::acl::AclRule::AllowCommand("GET".to_string()),
        crate::acl::AclRule::AllKeys,
    ])));
    let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"BGREWRITEAOF"))]);
    let reply = auth_gate(&replication, &session, &frame).unwrap();
    assert_eq!(
        reply,
        Frame::Error("NOPERM this user has no permissions to run this command".into())
    );
}

#[test]
fn auth_gate_permits_bgrewriteaof_to_a_user_with_that_grant() {
    let replication = ReplicationHandle::default();
    replication
        .acl
        .set_user(
            "app",
            &[Bytes::from_static(b"on"), Bytes::from_static(b"+bgrewriteaof")],
        )
        .unwrap();
    let session = Session::new();
    session.set_authenticated_user(Some(acl_user(vec![crate::acl::AclRule::AllowCommand(
        "BGREWRITEAOF".to_string(),
    )])));
    let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"BGREWRITEAOF"))]);
    assert!(auth_gate(&replication, &session, &frame).is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib dispatcher::tests::auth_gate_denies_bgrewriteaof -- --nocapture && cargo test -p rocket-mem --lib dispatcher::tests::auth_gate_permits_bgrewriteaof -- --nocapture`
Expected: both should already PASS if Task 1 is complete and `auth_gate`'s generic name-based check is working as designed — this task exists to make that guarantee explicit and regression-tested, not to add new production code. If either fails, `is_bgrewriteaof_command`/`KNOWN_COMMANDS` from Task 1 is incomplete; fix Task 1 rather than adding special-case ACL code here.

- [ ] **Step 3: No production code change**

Nothing to implement — `auth_gate`'s existing `user.is_allowed(name, &keys)` check (`dispatcher.rs:2253`) already covers any command name, `BGREWRITEAOF` included, with zero special-casing. This task's tests exist purely to lock that guarantee in.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib dispatcher:: -- --nocapture`
Expected: all PASS.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill to commit `crates/server/src/dispatcher.rs`.

---

## Next plan

If [`05-generation-aware-recovery.md`](./05-generation-aware-recovery.md) hasn't been done yet (it only depends on `01`, not on this plan, so it may already be finished if worked in parallel), do it now — [`06-concurrency-and-end-to-end.md`](./06-concurrency-and-end-to-end.md) needs both this plan and `05` merged first, since its tests call `dispatch_and_log` with a real `BGREWRITEAOF` command and then `crate::aof::recover` to check the result.
