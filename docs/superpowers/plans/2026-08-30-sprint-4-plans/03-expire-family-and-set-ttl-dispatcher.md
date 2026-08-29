# EXPIRE Family & SET TTL Dispatcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT`/`TTL`/`PTTL`/`PERSIST` become real commands (replacing the Sprint 3 stub), and `SET`'s `EX`/`PX` flags become real (replacing the Sprint 2 stub).

**Architecture:** all seven `EXPIRE`-family commands and the two `SET` flags call straight into `Engine::expire_at`/`persist`/`ttl` from `01-ttl-passive-expiry-core.md`. `EXPIREAT`/`PEXPIREAT` need the `Instant`-from-`SystemTime` conversion the spec fixes; `EXPIRE`/`PEXPIRE` (relative) don't.

**Tech Stack:** `std::time::{Duration, Instant, SystemTime, UNIX_EPOCH}` (all std, no new dependency).

**Spec:** `../../specs/2026-08-30-sprint-4-spec.md` — the `Instant`-vs-`SystemTime` conversion for absolute timestamps is authoritative; don't re-derive it here.

**Depends on:** `01-ttl-passive-expiry-core.md` (`Engine::expire_at`/`persist`/`ttl`, `TtlStatus`).

## Global Constraints

- `EXPIRE`/`PEXPIRE` take a relative duration from *now*; `EXPIREAT`/`PEXPIREAT` take an absolute Unix timestamp (seconds/milliseconds since the epoch) — don't conflate the two conversions.
- A target timestamp already in the past means "expire immediately," not an error — matches real Redis.

---

### Task 1: `instant_from_unix_ms` helper and the `EXPIRE`-family dispatcher arms

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `Engine::{expire_at, persist, ttl}`, `TtlStatus` (from `01-ttl-passive-expiry-core.md`).
- Produces: a private `fn instant_from_unix_ms(target_unix_ms: i64) -> Instant` helper (also consumed, independently re-derived with `SystemTime` math, by `05-aof-dispatch-wiring.md`'s `EXPIRE`-family→`PEXPIREAT` rewrite — see that plan's own note on why it's a separate, `SystemTime`-only computation rather than calling this `Instant`-returning helper); seven new `match` arms replacing the existing stub arm.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn expire_sets_a_relative_ttl_and_ttl_reports_it_positive() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"EXPIRE", b"k", b"100"]), &mut Protocol::default(), 1),
        Frame::Integer(1)
    );
    let Frame::Integer(secs) = dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1)
    else {
        panic!("expected Integer")
    };
    assert!((1..=100).contains(&secs));
}

#[test]
fn expire_on_a_missing_key_returns_zero() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"EXPIRE", b"missing", b"100"]), &mut Protocol::default(), 1),
        Frame::Integer(0)
    );
}

#[test]
fn expire_with_a_non_integer_seconds_is_a_resp_error() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"EXPIRE", b"k", b"soon"]), &mut Protocol::default(), 1),
        Frame::Error("ERR value is not an integer or out of range".into())
    );
}

#[test]
fn pexpire_sets_a_millisecond_ttl() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"PEXPIRE", b"k", b"60000"]), &mut Protocol::default(), 1),
        Frame::Integer(1)
    );
    let Frame::Integer(ms) = dispatch(&engine, cmd(&[b"PTTL", b"k"]), &mut Protocol::default(), 1)
    else {
        panic!("expected Integer")
    };
    assert!((1..=60000).contains(&ms));
}

#[test]
fn expireat_with_a_past_timestamp_deletes_the_key_immediately() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"EXPIREAT", b"k", b"1"]), &mut Protocol::default(), 1),
        Frame::Integer(1)
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1),
        Frame::Null
    );
}

#[test]
fn pexpireat_with_a_future_timestamp_keeps_the_key_alive() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    let future_ms = (std::time::SystemTime::now() + std::time::Duration::from_secs(60))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    assert_eq!(
        dispatch(
            &engine,
            cmd(&[b"PEXPIREAT", b"k", future_ms.as_bytes()]),
            &mut Protocol::default(),
            1
        ),
        Frame::Integer(1)
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"v"))
    );
}

#[test]
fn ttl_on_a_missing_key_returns_negative_two() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"TTL", b"missing"]), &mut Protocol::default(), 1),
        Frame::Integer(-2)
    );
}

#[test]
fn ttl_on_a_key_with_no_expiry_returns_negative_one() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1),
        Frame::Integer(-1)
    );
}

#[test]
fn persist_removes_an_existing_ttl_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"EXPIRE", b"k", b"100"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"PERSIST", b"k"]), &mut Protocol::default(), 1),
        Frame::Integer(1)
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1),
        Frame::Integer(-1)
    );
}

#[test]
fn persist_on_a_key_with_no_ttl_returns_zero() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"PERSIST", b"k"]), &mut Protocol::default(), 1),
        Frame::Integer(0)
    );
}
```

In the same edit, **delete** the now-obsolete Sprint 3 test that asserts the stub error —
`dispatcher::tests::expire_family_returns_a_clear_not_implemented_error`, which loops over all
seven command names asserting `msg.contains("not supported yet")`. It is a direct assertion
that this task's feature does *not* exist, so it must go rather than be adapted; the ten tests
above replace its coverage. Leaving it in place turns Step 4 red for a reason unrelated to the
new work.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — the `EXPIRE`-family match arm still returns the Sprint 3 "not supported yet" stub for every one of these commands

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — add near the other private helpers, e.g. below parse_score
/// `EXPIREAT`/`PEXPIREAT` give an absolute Unix timestamp; `Instant` has no epoch relationship,
/// so the absolute target is first resolved via `SystemTime`, then re-expressed as a delta
/// applied to `Instant::now()`. A target already in the past collapses to `Duration::ZERO`,
/// which the very next expiry check reads as already-expired — see
/// ../../specs/2026-08-30-sprint-4-spec.md for why this two-step conversion is necessary.
fn instant_from_unix_ms(target_unix_ms: i64) -> std::time::Instant {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let target = UNIX_EPOCH + Duration::from_millis(target_unix_ms.max(0) as u64);
    let delta = target.duration_since(SystemTime::now()).unwrap_or(Duration::ZERO);
    std::time::Instant::now() + delta
}
```

```rust
// crates/server/src/dispatcher.rs — replace the existing stub arm entirely
"EXPIRE" | "PEXPIRE" => {
    require_args!(rest, 2, name.to_ascii_lowercase().as_str());
    let n: i64 = match std::str::from_utf8(&rest[1]).ok().and_then(|s| s.parse().ok()) {
        Some(n) => n,
        None => return Frame::Error("ERR value is not an integer or out of range".into()),
    };
    let delta = if name == "EXPIRE" {
        std::time::Duration::from_secs(n.max(0) as u64)
    } else {
        std::time::Duration::from_millis(n.max(0) as u64)
    };
    match engine.expire_at(&rest[0], std::time::Instant::now() + delta) {
        true => Frame::Integer(1),
        false => Frame::Integer(0),
    }
}
"EXPIREAT" | "PEXPIREAT" => {
    require_args!(rest, 2, name.to_ascii_lowercase().as_str());
    let n: i64 = match std::str::from_utf8(&rest[1]).ok().and_then(|s| s.parse().ok()) {
        Some(n) => n,
        None => return Frame::Error("ERR value is not an integer or out of range".into()),
    };
    let target_unix_ms = if name == "EXPIREAT" { n.saturating_mul(1000) } else { n };
    match engine.expire_at(&rest[0], instant_from_unix_ms(target_unix_ms)) {
        true => Frame::Integer(1),
        false => Frame::Integer(0),
    }
}
"TTL" => {
    require_args!(rest, 1, "ttl");
    match engine.ttl(&rest[0]) {
        engine::TtlStatus::NoSuchKey => Frame::Integer(-2),
        engine::TtlStatus::NoExpiry => Frame::Integer(-1),
        engine::TtlStatus::Remaining(d) => Frame::Integer(d.as_secs().max(1) as i64),
    }
}
"PTTL" => {
    require_args!(rest, 1, "pttl");
    match engine.ttl(&rest[0]) {
        engine::TtlStatus::NoSuchKey => Frame::Integer(-2),
        engine::TtlStatus::NoExpiry => Frame::Integer(-1),
        engine::TtlStatus::Remaining(d) => Frame::Integer(d.as_millis().max(1) as i64),
    }
}
"PERSIST" => {
    require_args!(rest, 1, "persist");
    match engine.persist(&rest[0]) {
        true => Frame::Integer(1),
        false => Frame::Integer(0),
    }
}
```

Note: `TTL`/`PTTL` round a sub-second/sub-millisecond `Remaining` up to `1` rather than `0` —
a key that hasn't *actually* expired yet must never report a TTL of `0` (real Redis's own
convention: `0` is indistinguishable from "about to be read as expired").

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 10 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): wire EXPIRE/PEXPIRE/EXPIREAT/PEXPIREAT/TTL/PTTL/PERSIST`.

---

### Task 2: `SET`'s `EX`/`PX` flags

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `Engine::expire_at` (from `01-ttl-passive-expiry-core.md`).
- Produces: the existing `"SET"` arm's `EX`/`PX` rejection replaced with real TTL application.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn set_with_ex_applies_a_relative_ttl_in_seconds() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v", b"EX", b"100"]), &mut Protocol::default(), 1);
    let Frame::Integer(secs) = dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1)
    else {
        panic!("expected Integer")
    };
    assert!((1..=100).contains(&secs));
}

#[test]
fn set_with_px_applies_a_relative_ttl_in_milliseconds() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v", b"PX", b"60000"]), &mut Protocol::default(), 1);
    let Frame::Integer(ms) = dispatch(&engine, cmd(&[b"PTTL", b"k"]), &mut Protocol::default(), 1)
    else {
        panic!("expected Integer")
    };
    assert!((1..=60000).contains(&ms));
}

#[test]
fn set_without_ex_or_px_leaves_no_ttl() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1),
        Frame::Integer(-1)
    );
}

#[test]
fn set_with_a_non_integer_ex_value_is_a_resp_error() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"SET", b"k", b"v", b"EX", b"soon"]), &mut Protocol::default(), 1),
        Frame::Error("ERR value is not an integer or out of range".into())
    );
}

#[test]
fn set_overwriting_an_existing_key_with_a_ttl_clears_the_old_ttl() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"old", b"EX", b"100"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"SET", b"k", b"new"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1),
        Frame::Integer(-1)
    );
}
```

In the same edit, **delete** the now-obsolete Sprint 2 test
`dispatcher::tests::set_with_ex_flag_returns_a_clear_not_implemented_error`, which asserts
`SET k v EX 10` replies
`Frame::Error("ERR syntax error: EX/PX are not supported yet (planned Sprint 4)")`. Same
reasoning as Task 1's deletion: it asserts the absence of exactly what this task adds, and the
five tests above replace its coverage.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — `SET ... EX ...`/`SET ... PX ...` still return `"ERR syntax error: EX/PX are not supported yet"`

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — replace the existing "SET" arm entirely
"SET" => {
    require_args!(rest, 2, "set");
    let key = rest[0].clone();
    let val = rest[1].clone();
    let flags: Vec<String> = rest[2..]
        .iter()
        .map(|b| String::from_utf8_lossy(b).to_ascii_uppercase())
        .collect();
    let ex_ms: Option<i64> = if let Some(pos) = flags.iter().position(|f| f == "EX") {
        match rest.get(2 + pos + 1).and_then(|b| std::str::from_utf8(b).ok()).and_then(|s| s.parse::<i64>().ok()) {
            Some(secs) => Some(secs.saturating_mul(1000)),
            None => return Frame::Error("ERR value is not an integer or out of range".into()),
        }
    } else if let Some(pos) = flags.iter().position(|f| f == "PX") {
        match rest.get(2 + pos + 1).and_then(|b| std::str::from_utf8(b).ok()).and_then(|s| s.parse::<i64>().ok()) {
            Some(ms) => Some(ms),
            None => return Frame::Error("ERR value is not an integer or out of range".into()),
        }
    } else {
        None
    };

    let applied = if flags.iter().any(|f| f == "NX") {
        commands::string::set_nx(engine, key.clone(), val)
    } else if flags.iter().any(|f| f == "XX") {
        commands::string::set_xx(engine, key.clone(), val)
    } else {
        engine.set(key.clone(), Value::String(val));
        true
    };
    if !applied {
        return Frame::Null;
    }
    if let Some(ms) = ex_ms {
        engine.expire_at(&key, std::time::Instant::now() + std::time::Duration::from_millis(ms.max(0) as u64));
    }
    Frame::Simple("OK".into())
}
```

Note: `EX`/`PX`'s value is looked up positionally as `rest[2 + pos + 1]` — the flag token's
own index plus one, since `flags` is built from `rest[2..]` (offset by 2 relative to `rest`)
and each flag's value is the very next argument. `SET k v EX 100`: `rest = [k, v, EX, 100]`,
`flags = [EX, 100]` (uppercased) — `EX` is at `flags` index 0, so its value is at
`rest[2 + 0 + 1] == rest[3] == 100`. This matches the existing `NX`/`XX` flag lookup pattern
already used elsewhere in this same arm (`flags.iter().any(...)`), just extended to also read
the flag's associated value rather than only checking presence.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 5 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): implement SET's EX/PX flags`.
