# Remaining Set Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `SINTER`/`SUNION`/`SDIFF` (plus their `STORE` variants), `SPOP`, `SRANDMEMBER`, rounding out Set command coverage per `../../rocket-mem-production-plan.md`'s Week 6 detail.

**Architecture:** all extend `crates/engine/src/commands/set.rs`, reusing the existing private `get_set` helper. `SPOP`/`SRANDMEMBER` reuse the `rand` dependency `01-string-key-commands.md` already added for `RANDOMKEY` — no new dependency here.

**Tech Stack:** `rand` (already a workspace dependency as of `01-string-key-commands.md`; add `rand.workspace = true` to `crates/engine/Cargo.toml` only if `01` hasn't landed yet — check the manifest first).

**Spec:** `../../specs/2026-08-29-sprint-3-spec.md` — no set-specific decisions beyond the `rand` reuse note; this plan follows Sprint 1's established `set.rs` conventions directly.

**Depends on:** `01-string-key-commands.md` for the `rand` workspace dependency. Otherwise independent of every other Sprint 3 plan.

---

### Task 1: `SINTER`/`SUNION`/`SDIFF` (+ `STORE` variants), `SPOP`, `SRANDMEMBER` (engine level)

**Files:**
- Modify: `crates/engine/src/commands/set.rs`
- Modify: `crates/engine/src/commands/wrongtype_matrix_tests.rs`
- Modify: `crates/engine/src/commands/missing_key_semantics_tests.rs`

**Interfaces:**
- Consumes: the existing private `get_set` helper (in `set.rs`).
- Produces: `pub fn sinter(engine: &Engine, keys: &[Bytes]) -> Result<HashSet<Bytes>, common::EngineError>`, `pub fn sunion(engine: &Engine, keys: &[Bytes]) -> Result<HashSet<Bytes>, common::EngineError>`, `pub fn sdiff(engine: &Engine, keys: &[Bytes]) -> Result<HashSet<Bytes>, common::EngineError>`, `pub fn sinterstore(engine: &Engine, dest: Bytes, keys: &[Bytes]) -> Result<usize, common::EngineError>`, `pub fn sunionstore(engine: &Engine, dest: Bytes, keys: &[Bytes]) -> Result<usize, common::EngineError>`, `pub fn sdiffstore(engine: &Engine, dest: Bytes, keys: &[Bytes]) -> Result<usize, common::EngineError>`, `pub fn spop(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError>`, `pub fn srandmember(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError>`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/commands/set.rs — add to the existing tests module
#[test]
fn sinter_returns_only_members_present_in_every_set() {
    let engine = Engine::new();
    sadd(&engine, Bytes::from_static(b"a"), Bytes::from_static(b"x")).unwrap();
    sadd(&engine, Bytes::from_static(b"a"), Bytes::from_static(b"y")).unwrap();
    sadd(&engine, Bytes::from_static(b"b"), Bytes::from_static(b"y")).unwrap();
    sadd(&engine, Bytes::from_static(b"b"), Bytes::from_static(b"z")).unwrap();
    let result = sinter(&engine, &[Bytes::from_static(b"a"), Bytes::from_static(b"b")]).unwrap();
    assert_eq!(result, HashSet::from([Bytes::from_static(b"y")]));
}

#[test]
fn sinter_with_a_missing_key_is_empty() {
    let engine = Engine::new();
    sadd(&engine, Bytes::from_static(b"a"), Bytes::from_static(b"x")).unwrap();
    let result = sinter(&engine, &[Bytes::from_static(b"a"), Bytes::from_static(b"missing")]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn sunion_returns_every_member_from_every_set() {
    let engine = Engine::new();
    sadd(&engine, Bytes::from_static(b"a"), Bytes::from_static(b"x")).unwrap();
    sadd(&engine, Bytes::from_static(b"b"), Bytes::from_static(b"y")).unwrap();
    let result = sunion(&engine, &[Bytes::from_static(b"a"), Bytes::from_static(b"b")]).unwrap();
    assert_eq!(result, HashSet::from([Bytes::from_static(b"x"), Bytes::from_static(b"y")]));
}

#[test]
fn sdiff_returns_members_of_the_first_set_absent_from_the_rest() {
    let engine = Engine::new();
    sadd(&engine, Bytes::from_static(b"a"), Bytes::from_static(b"x")).unwrap();
    sadd(&engine, Bytes::from_static(b"a"), Bytes::from_static(b"y")).unwrap();
    sadd(&engine, Bytes::from_static(b"b"), Bytes::from_static(b"y")).unwrap();
    let result = sdiff(&engine, &[Bytes::from_static(b"a"), Bytes::from_static(b"b")]).unwrap();
    assert_eq!(result, HashSet::from([Bytes::from_static(b"x")]));
}

#[test]
fn sinterstore_stores_the_result_and_returns_its_size() {
    let engine = Engine::new();
    sadd(&engine, Bytes::from_static(b"a"), Bytes::from_static(b"x")).unwrap();
    sadd(&engine, Bytes::from_static(b"b"), Bytes::from_static(b"x")).unwrap();
    let len = sinterstore(
        &engine,
        Bytes::from_static(b"dest"),
        &[Bytes::from_static(b"a"), Bytes::from_static(b"b")],
    )
    .unwrap();
    assert_eq!(len, 1);
    assert_eq!(smembers(&engine, b"dest").unwrap(), HashSet::from([Bytes::from_static(b"x")]));
}

#[test]
fn spop_removes_and_returns_a_member() {
    let engine = Engine::new();
    sadd(&engine, Bytes::from_static(b"s"), Bytes::from_static(b"x")).unwrap();
    let popped = spop(&engine, b"s").unwrap();
    assert_eq!(popped, Some(Bytes::from_static(b"x")));
    assert_eq!(scard(&engine, b"s").unwrap(), 0);
}

#[test]
fn spop_on_missing_key_returns_none_not_an_error() {
    let engine = Engine::new();
    assert_eq!(spop(&engine, b"missing").unwrap(), None);
}

#[test]
fn srandmember_returns_a_member_without_removing_it() {
    let engine = Engine::new();
    sadd(&engine, Bytes::from_static(b"s"), Bytes::from_static(b"x")).unwrap();
    let picked = srandmember(&engine, b"s").unwrap();
    assert_eq!(picked, Some(Bytes::from_static(b"x")));
    assert_eq!(scard(&engine, b"s").unwrap(), 1);
}

#[test]
fn srandmember_on_missing_key_returns_none_not_an_error() {
    let engine = Engine::new();
    assert_eq!(srandmember(&engine, b"missing").unwrap(), None);
}

#[test]
fn sinter_on_string_key_returns_wrongtype() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    assert_eq!(
        sinter(&engine, &[Bytes::from_static(b"k")]).unwrap_err(),
        common::EngineError::WrongType
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine commands::set`
Expected: FAIL — the new functions don't exist yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/commands/set.rs — add below the existing `scard`
pub fn sinter(engine: &Engine, keys: &[Bytes]) -> Result<HashSet<Bytes>, common::EngineError> {
    let mut sets = Vec::with_capacity(keys.len());
    for k in keys {
        sets.push(get_set(engine, k)?);
    }
    let mut iter = sets.into_iter();
    let Some(first) = iter.next() else {
        return Ok(HashSet::new());
    };
    Ok(iter.fold(first, |acc, s| acc.intersection(&s).cloned().collect()))
}

pub fn sunion(engine: &Engine, keys: &[Bytes]) -> Result<HashSet<Bytes>, common::EngineError> {
    let mut result = HashSet::new();
    for k in keys {
        result.extend(get_set(engine, k)?);
    }
    Ok(result)
}

pub fn sdiff(engine: &Engine, keys: &[Bytes]) -> Result<HashSet<Bytes>, common::EngineError> {
    let mut iter = keys.iter();
    let Some(first_key) = iter.next() else {
        return Ok(HashSet::new());
    };
    let mut result = get_set(engine, first_key)?;
    for k in iter {
        let other = get_set(engine, k)?;
        result.retain(|m| !other.contains(m));
    }
    Ok(result)
}

pub fn sinterstore(engine: &Engine, dest: Bytes, keys: &[Bytes]) -> Result<usize, common::EngineError> {
    let result = sinter(engine, keys)?;
    let len = result.len();
    engine.set(dest, Value::Set(result));
    Ok(len)
}

pub fn sunionstore(engine: &Engine, dest: Bytes, keys: &[Bytes]) -> Result<usize, common::EngineError> {
    let result = sunion(engine, keys)?;
    let len = result.len();
    engine.set(dest, Value::Set(result));
    Ok(len)
}

pub fn sdiffstore(engine: &Engine, dest: Bytes, keys: &[Bytes]) -> Result<usize, common::EngineError> {
    let result = sdiff(engine, keys)?;
    let len = result.len();
    engine.set(dest, Value::Set(result));
    Ok(len)
}

pub fn spop(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    use rand::seq::IteratorRandom;
    let mut set = match engine.get(key) {
        None => return Ok(None),
        Some(Value::Set(s)) => s,
        Some(_) => return Err(common::EngineError::WrongType),
    };
    let Some(member) = set.iter().choose(&mut rand::thread_rng()).cloned() else {
        return Ok(None);
    };
    set.remove(&member);
    engine.set(Bytes::copy_from_slice(key), Value::Set(set));
    Ok(Some(member))
}

pub fn srandmember(engine: &Engine, key: &[u8]) -> Result<Option<Bytes>, common::EngineError> {
    use rand::seq::IteratorRandom;
    let set = get_set(engine, key)?;
    Ok(set.into_iter().choose(&mut rand::thread_rng()))
}
```

- [ ] **Step 4: Add wrongtype and missing-key coverage**

```rust
// crates/engine/src/commands/wrongtype_matrix_tests.rs — extend set_commands_reject_non_set_keys
let e3 = engine_with_string_key();
assert_wrongtype!(set::sinter(&e3, &[Bytes::from_static(b"k")]));
let e4 = engine_with_string_key();
assert_wrongtype!(set::spop(&e4, b"k"));
```

```rust
// crates/engine/src/commands/missing_key_semantics_tests.rs — extend missing_key_reads_return_empty_or_none_not_errors
assert_eq!(set::spop(&engine, b"missing").unwrap(), None);
assert_eq!(set::srandmember(&engine, b"missing").unwrap(), None);
assert!(set::sinter(&engine, &[Bytes::from_static(b"missing")]).unwrap().is_empty());
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p engine commands::set`
Expected: PASS, all tests including the 10 new ones

- [ ] **Step 6: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 7: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/commands/set.rs`, `crates/engine/src/commands/wrongtype_matrix_tests.rs`,
and `crates/engine/src/commands/missing_key_semantics_tests.rs` — do not compose the commit
message freeform. Suggested subject:
`feat(engine): add sinter/sunion/sdiff(+store)/spop/srandmember set commands`.

---

### Task 2: Wire the eight commands into the dispatcher

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `commands::set::{sinter, sunion, sdiff, sinterstore, sunionstore, sdiffstore, spop, srandmember}` (Task 1).
- Produces: eight new `match` arms.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn sinter_sunion_sdiff_round_trip_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SADD", b"a", b"x"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"SADD", b"a", b"y"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"SADD", b"b", b"y"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"SINTER", b"a", b"b"]), &mut Protocol::default(), 1),
        Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"y"))])
    );
    let Frame::Array(mut union) = dispatch(&engine, cmd(&[b"SUNION", b"a", b"b"]), &mut Protocol::default(), 1)
    else {
        panic!("expected Array")
    };
    union.sort_by_key(|f| format!("{f:?}"));
    assert_eq!(
        union,
        vec![Frame::Bulk(Bytes::from_static(b"x")), Frame::Bulk(Bytes::from_static(b"y"))]
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"SDIFF", b"a", b"b"]), &mut Protocol::default(), 1),
        Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"x"))])
    );
}

#[test]
fn sinterstore_stores_the_result_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SADD", b"a", b"x"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"SADD", b"b", b"x"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"SINTERSTORE", b"dest", b"a", b"b"]), &mut Protocol::default(), 1),
        Frame::Integer(1)
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"SMEMBERS", b"dest"]), &mut Protocol::default(), 1),
        Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"x"))])
    );
}

#[test]
fn spop_removes_a_member_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SADD", b"s", b"x"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"SPOP", b"s"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"x"))
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"SCARD", b"s"]), &mut Protocol::default(), 1),
        Frame::Integer(0)
    );
}

#[test]
fn spop_on_missing_key_returns_null() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"SPOP", b"missing"]), &mut Protocol::default(), 1),
        Frame::Null
    );
}

#[test]
fn srandmember_does_not_remove_the_member_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SADD", b"s", b"x"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"SRANDMEMBER", b"s"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"x"))
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"SCARD", b"s"]), &mut Protocol::default(), 1),
        Frame::Integer(1)
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — none of the eight commands are wired yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — add near the other Set commands
"SINTER" => {
    require_args!(rest, 1, "sinter");
    match commands::set::sinter(engine, rest) {
        Ok(members) => Frame::Array(members.into_iter().map(Frame::Bulk).collect()),
        Err(e) => engine_error_to_frame(e),
    }
}
"SUNION" => {
    require_args!(rest, 1, "sunion");
    match commands::set::sunion(engine, rest) {
        Ok(members) => Frame::Array(members.into_iter().map(Frame::Bulk).collect()),
        Err(e) => engine_error_to_frame(e),
    }
}
"SDIFF" => {
    require_args!(rest, 1, "sdiff");
    match commands::set::sdiff(engine, rest) {
        Ok(members) => Frame::Array(members.into_iter().map(Frame::Bulk).collect()),
        Err(e) => engine_error_to_frame(e),
    }
}
"SINTERSTORE" => {
    require_args!(rest, 2, "sinterstore");
    match commands::set::sinterstore(engine, rest[0].clone(), &rest[1..]) {
        Ok(n) => Frame::Integer(n as i64),
        Err(e) => engine_error_to_frame(e),
    }
}
"SUNIONSTORE" => {
    require_args!(rest, 2, "sunionstore");
    match commands::set::sunionstore(engine, rest[0].clone(), &rest[1..]) {
        Ok(n) => Frame::Integer(n as i64),
        Err(e) => engine_error_to_frame(e),
    }
}
"SDIFFSTORE" => {
    require_args!(rest, 2, "sdiffstore");
    match commands::set::sdiffstore(engine, rest[0].clone(), &rest[1..]) {
        Ok(n) => Frame::Integer(n as i64),
        Err(e) => engine_error_to_frame(e),
    }
}
"SPOP" => {
    require_args!(rest, 1, "spop");
    match commands::set::spop(engine, &rest[0]) {
        Ok(Some(b)) => Frame::Bulk(b),
        Ok(None) => Frame::Null,
        Err(e) => engine_error_to_frame(e),
    }
}
"SRANDMEMBER" => {
    require_args!(rest, 1, "srandmember");
    match commands::set::srandmember(engine, &rest[0]) {
        Ok(Some(b)) => Frame::Bulk(b),
        Ok(None) => Frame::Null,
        Err(e) => engine_error_to_frame(e),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 5 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): wire sinter/sunion/sdiff(+store)/spop/srandmember set commands`.
