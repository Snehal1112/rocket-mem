# String & Key Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `GETSET`/`MSET`/`MGET`/`MSETNX`/`RENAME`/`RENAMENX`/`TYPE`/`RANDOMKEY` implemented at the engine level and wired through the dispatcher, plus an explicit stub response for the `EXPIRE` command family.

**Architecture:** `GETSET`/`MSET`/`MGET`/`MSETNX` extend `crates/engine/src/commands/string.rs` (they're String-type operations, same file as `SET`/`GET`/`APPEND`). `RENAME`/`RENAMENX`/`TYPE`/`RANDOMKEY` are key-space operations that don't belong to any one `Value` variant, so they get a new `crates/engine/src/commands/keys.rs`. All eight are wired into `crates/server/src/dispatcher.rs`'s existing `match` arms, following the exact `require_args!`/`engine_error_to_frame` pattern already there.

**Tech Stack:** `rand = "0.8"` (new workspace dependency, for `RANDOMKEY`'s uniform pick — also reused by `08-remaining-set-commands.md`).

**Spec:** `../../specs/2026-08-29-sprint-3-spec.md` — the `EXPIRE`-family stub decision, the `NoSuchKey` error variant, and the `MGET` wrongtype-exception note are authoritative; don't re-derive them here.

**Depends on:** Sprint 1's `engine` crate and Sprint 2's dispatcher. Independent of every other Sprint 3 plan.

## Global Constraints

- Every new dispatcher arm validates arg count with `require_args!` before indexing `rest`, per the panic-gap rule `../../specs/2026-08-29-sprint-2-spec.md`'s Sequencing section already established (see Sprint 2's `6ba2c1b` fix in `docs/phase-1-retro.md`).
- New `common::EngineError` variants get the same bare (no `"ERR "` prefix) `#[error(...)]` text style as `NotAnInteger` — see spec's note on this being a known, inherited wire-format gap, not something to fix here.
- Every new engine command gets a wrongtype case added to `crates/engine/src/commands/wrongtype_matrix_tests.rs` and a missing-key case added to `crates/engine/src/commands/missing_key_semantics_tests.rs` where applicable, per `CLAUDE.md`'s "Correctness conventions enforced across every command."

---

### Task 1: `common::EngineError::NoSuchKey`

**Files:**
- Modify: `crates/common/src/lib.rs`

**Interfaces:**
- Produces: `common::EngineError::NoSuchKey`, `Display` text `"no such key"` (no `ERR ` prefix, matching `NotAnInteger`'s existing style).

- [ ] **Step 1: Write the failing test**

```rust
// crates/common/src/lib.rs — add a tests module
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_such_key_has_the_expected_display_text() {
        assert_eq!(EngineError::NoSuchKey.to_string(), "no such key");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p common`
Expected: FAIL — `NoSuchKey` variant doesn't exist yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/common/src/lib.rs
#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum EngineError {
    #[error("WRONGTYPE Operation against a key holding the wrong kind of value")]
    WrongType,
    #[error("value is not an integer or out of range")]
    NotAnInteger,
    #[error("no such key")]
    NoSuchKey,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p common`
Expected: PASS

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/common/src/lib.rs` — do not compose the commit message freeform. Suggested
subject: `feat(common): add NoSuchKey error variant`.

---

### Task 2: `GETSET`/`MSET`/`MGET`/`MSETNX` (engine level)

**Files:**
- Modify: `crates/engine/src/commands/string.rs`

**Interfaces:**
- Consumes: `crate::{Engine, Value}` (existing), `common::EngineError::WrongType` (existing, via the already-defined `get`).
- Produces: `pub fn getset(engine: &Engine, key: Bytes, val: Bytes) -> Result<Option<Bytes>, common::EngineError>`, `pub fn mset(engine: &Engine, pairs: Vec<(Bytes, Bytes)>)`, `pub fn mget(engine: &Engine, keys: &[Bytes]) -> Vec<Option<Bytes>>`, `pub fn msetnx(engine: &Engine, pairs: Vec<(Bytes, Bytes)>) -> bool`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/commands/string.rs — add to the existing tests module
#[test]
fn getset_returns_old_value_and_sets_new_one() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"old")));
    let old = getset(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"new")).unwrap();
    assert_eq!(old, Some(Bytes::from_static(b"old")));
    assert_eq!(get(&engine, b"k").unwrap(), Some(Bytes::from_static(b"new")));
}

#[test]
fn getset_on_missing_key_returns_none_and_creates_it() {
    let engine = Engine::new();
    let old = getset(&engine, Bytes::from_static(b"k"), Bytes::from_static(b"v")).unwrap();
    assert_eq!(old, None);
    assert_eq!(get(&engine, b"k").unwrap(), Some(Bytes::from_static(b"v")));
}

#[test]
fn getset_on_hash_key_returns_wrongtype() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"h"), Value::Hash(Default::default()));
    let err = getset(&engine, Bytes::from_static(b"h"), Bytes::from_static(b"v")).unwrap_err();
    assert_eq!(err, common::EngineError::WrongType);
}

#[test]
fn mset_sets_every_pair() {
    let engine = Engine::new();
    mset(
        &engine,
        vec![
            (Bytes::from_static(b"a"), Bytes::from_static(b"1")),
            (Bytes::from_static(b"b"), Bytes::from_static(b"2")),
        ],
    );
    assert_eq!(get(&engine, b"a").unwrap(), Some(Bytes::from_static(b"1")));
    assert_eq!(get(&engine, b"b").unwrap(), Some(Bytes::from_static(b"2")));
}

#[test]
fn mget_returns_none_for_missing_keys_in_order() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"a"), Value::String(Bytes::from_static(b"1")));
    let result = mget(&engine, &[Bytes::from_static(b"a"), Bytes::from_static(b"missing")]);
    assert_eq!(result, vec![Some(Bytes::from_static(b"1")), None]);
}

#[test]
fn mget_returns_none_for_a_wrongtype_key_instead_of_erroring() {
    // MGET is Redis's documented exception to the WRONGTYPE convention: a non-string key
    // among the requested keys comes back nil for that key, not an error for the whole command.
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"h"), Value::Hash(Default::default()));
    let result = mget(&engine, &[Bytes::from_static(b"h")]);
    assert_eq!(result, vec![None]);
}

#[test]
fn msetnx_fails_and_sets_nothing_if_any_key_already_exists() {
    let engine = Engine::new();
    engine.set(Bytes::from_static(b"a"), Value::String(Bytes::from_static(b"existing")));
    let applied = msetnx(
        &engine,
        vec![
            (Bytes::from_static(b"a"), Bytes::from_static(b"new")),
            (Bytes::from_static(b"b"), Bytes::from_static(b"new")),
        ],
    );
    assert!(!applied);
    assert_eq!(get(&engine, b"a").unwrap(), Some(Bytes::from_static(b"existing")));
    assert_eq!(get(&engine, b"b").unwrap(), None);
}

#[test]
fn msetnx_succeeds_when_no_key_exists() {
    let engine = Engine::new();
    let applied = msetnx(&engine, vec![(Bytes::from_static(b"a"), Bytes::from_static(b"1"))]);
    assert!(applied);
    assert_eq!(get(&engine, b"a").unwrap(), Some(Bytes::from_static(b"1")));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine commands::string`
Expected: FAIL — `getset`/`mset`/`mget`/`msetnx` not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/commands/string.rs — add above the tests module
pub fn getset(engine: &Engine, key: Bytes, val: Bytes) -> Result<Option<Bytes>, common::EngineError> {
    let old = get(engine, &key)?;
    engine.set(key, Value::String(val));
    Ok(old)
}

pub fn mset(engine: &Engine, pairs: Vec<(Bytes, Bytes)>) {
    for (k, v) in pairs {
        engine.set(k, Value::String(v));
    }
}

/// A missing key and a wrong-type key are indistinguishable in the result — both come back
/// `None` — matching real Redis's documented MGET behavior of never erroring.
pub fn mget(engine: &Engine, keys: &[Bytes]) -> Vec<Option<Bytes>> {
    keys.iter()
        .map(|k| match engine.get(k) {
            Some(Value::String(b)) => Some(b),
            _ => None,
        })
        .collect()
}

pub fn msetnx(engine: &Engine, pairs: Vec<(Bytes, Bytes)>) -> bool {
    if pairs.iter().any(|(k, _)| engine.exists(k)) {
        return false;
    }
    for (k, v) in pairs {
        engine.set(k, Value::String(v));
    }
    true
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine commands::string`
Expected: PASS, all tests including the 7 new ones

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/commands/string.rs` — do not compose the commit message freeform.
Suggested subject: `feat(engine): add getset/mset/mget/msetnx string commands`.

---

### Task 3: `RENAME`/`RENAMENX`/`TYPE`/`RANDOMKEY` (engine level)

**Files:**
- Create: `crates/engine/src/commands/keys.rs`
- Modify: `crates/engine/src/commands/mod.rs`
- Modify: `crates/engine/Cargo.toml`
- Modify: `Cargo.toml` (workspace root)

**Interfaces:**
- Consumes: `crate::Engine` (`get`/`set`/`del`/`exists`/`keys`, all existing), `common::EngineError::NoSuchKey` (Task 1).
- Produces: `pub fn rename(engine: &Engine, src: &[u8], dst: Bytes) -> Result<(), common::EngineError>`, `pub fn renamenx(engine: &Engine, src: &[u8], dst: Bytes) -> Result<bool, common::EngineError>`, `pub fn key_type(engine: &Engine, key: &[u8]) -> &'static str`, `pub fn randomkey(engine: &Engine) -> Option<Bytes>`.

- [ ] **Step 1: Add the `rand` dependency**

```toml
# Cargo.toml — add to [workspace.dependencies]
rand = "0.8"
```

```toml
# crates/engine/Cargo.toml — add to [dependencies]
rand.workspace = true
```

- [ ] **Step 2: Write the failing tests**

```rust
// crates/engine/src/commands/keys.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, Value};
    use bytes::Bytes;

    #[test]
    fn rename_moves_the_value_and_removes_the_source() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"src"), Value::String(Bytes::from_static(b"v")));
        rename(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        assert!(!engine.exists(b"src"));
        assert_eq!(engine.get(b"dst"), Some(Value::String(Bytes::from_static(b"v"))));
    }

    #[test]
    fn rename_on_missing_source_returns_no_such_key() {
        let engine = Engine::new();
        let err = rename(&engine, b"missing", Bytes::from_static(b"dst")).unwrap_err();
        assert_eq!(err, common::EngineError::NoSuchKey);
    }

    #[test]
    fn rename_to_itself_is_a_no_op_success() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
        rename(&engine, b"k", Bytes::from_static(b"k")).unwrap();
        assert_eq!(engine.get(b"k"), Some(Value::String(Bytes::from_static(b"v"))));
    }

    #[test]
    fn rename_overwrites_an_existing_destination() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"src"), Value::String(Bytes::from_static(b"new")));
        engine.set(Bytes::from_static(b"dst"), Value::String(Bytes::from_static(b"old")));
        rename(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        assert_eq!(engine.get(b"dst"), Some(Value::String(Bytes::from_static(b"new"))));
    }

    #[test]
    fn renamenx_fails_without_error_when_destination_exists() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"src"), Value::String(Bytes::from_static(b"v")));
        engine.set(Bytes::from_static(b"dst"), Value::String(Bytes::from_static(b"existing")));
        let applied = renamenx(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        assert!(!applied);
        assert_eq!(engine.get(b"dst"), Some(Value::String(Bytes::from_static(b"existing"))));
        assert!(engine.exists(b"src"));
    }

    #[test]
    fn renamenx_succeeds_when_destination_is_free() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"src"), Value::String(Bytes::from_static(b"v")));
        let applied = renamenx(&engine, b"src", Bytes::from_static(b"dst")).unwrap();
        assert!(applied);
        assert!(!engine.exists(b"src"));
        assert_eq!(engine.get(b"dst"), Some(Value::String(Bytes::from_static(b"v"))));
    }

    #[test]
    fn renamenx_on_missing_source_returns_no_such_key() {
        let engine = Engine::new();
        let err = renamenx(&engine, b"missing", Bytes::from_static(b"dst")).unwrap_err();
        assert_eq!(err, common::EngineError::NoSuchKey);
    }

    #[test]
    fn key_type_reports_none_for_a_missing_key() {
        let engine = Engine::new();
        assert_eq!(key_type(&engine, b"missing"), "none");
    }

    #[test]
    fn key_type_reports_the_real_type_name_for_each_variant() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"s"), Value::String(Bytes::from_static(b"v")));
        engine.set(Bytes::from_static(b"h"), Value::Hash(Default::default()));
        assert_eq!(key_type(&engine, b"s"), "string");
        assert_eq!(key_type(&engine, b"h"), "hash");
    }

    #[test]
    fn randomkey_on_empty_keyspace_returns_none() {
        let engine = Engine::new();
        assert_eq!(randomkey(&engine), None);
    }

    #[test]
    fn randomkey_returns_one_of_the_existing_keys() {
        let engine = Engine::new();
        engine.set(Bytes::from_static(b"a"), Value::String(Bytes::from_static(b"1")));
        engine.set(Bytes::from_static(b"b"), Value::String(Bytes::from_static(b"2")));
        let picked = randomkey(&engine).unwrap();
        assert!(picked == Bytes::from_static(b"a") || picked == Bytes::from_static(b"b"));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p engine commands::keys`
Expected: FAIL — module doesn't exist yet

- [ ] **Step 4: Write the implementation**

```rust
// crates/engine/src/commands/keys.rs (above the tests module)
use crate::Engine;
use bytes::Bytes;

pub fn rename(engine: &Engine, src: &[u8], dst: Bytes) -> Result<(), common::EngineError> {
    let val = engine.get(src).ok_or(common::EngineError::NoSuchKey)?;
    if src == dst.as_ref() {
        return Ok(());
    }
    engine.set(dst, val);
    engine.del(src);
    Ok(())
}

pub fn renamenx(engine: &Engine, src: &[u8], dst: Bytes) -> Result<bool, common::EngineError> {
    let val = engine.get(src).ok_or(common::EngineError::NoSuchKey)?;
    if engine.exists(&dst) {
        return Ok(false);
    }
    engine.set(dst, val);
    engine.del(src);
    Ok(true)
}

pub fn key_type(engine: &Engine, key: &[u8]) -> &'static str {
    match engine.get(key) {
        None => "none",
        Some(v) => v.type_name(),
    }
}

pub fn randomkey(engine: &Engine) -> Option<Bytes> {
    use rand::Rng;
    let keys = engine.keys();
    if keys.is_empty() {
        return None;
    }
    let idx = rand::thread_rng().gen_range(0..keys.len());
    Some(keys[idx].clone())
}
```

```rust
// crates/engine/src/commands/mod.rs
pub mod hash;
pub mod keys;
pub mod list;
pub mod set;
pub mod string;

#[cfg(test)]
mod missing_key_semantics_tests;
#[cfg(test)]
mod wrongtype_matrix_tests;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p engine commands::keys`
Expected: PASS, 12/12

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/commands/keys.rs`, `crates/engine/src/commands/mod.rs`,
`crates/engine/Cargo.toml`, `Cargo.toml`, and `Cargo.lock` — do not compose the commit
message freeform. Suggested subject: `feat(engine): add rename/renamenx/type/randomkey key commands`.

---

### Task 4: Wire all eight commands, plus the `EXPIRE`-family stub, into the dispatcher

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `commands::string::{getset, mset, mget, msetnx}` (Task 2), `commands::keys::{rename, renamenx, key_type, randomkey}` (Task 3).
- Produces: nine new `match` arms in `dispatch` (`GETSET`, `MSET`, `MGET`, `MSETNX`, `RENAME`, `RENAMENX`, `TYPE`, `RANDOMKEY`, and one combined arm for `EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT`/`TTL`/`PTTL`/`PERSIST`).

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn getset_round_trips_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"old"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"GETSET", b"k", b"new"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"old"))
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"new"))
    );
}

#[test]
fn mset_then_mget_round_trips_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"MSET", b"a", b"1", b"b", b"2"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"MGET", b"a", b"b", b"missing"]), &mut Protocol::default(), 1),
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"1")),
            Frame::Bulk(Bytes::from_static(b"2")),
            Frame::Null,
        ])
    );
}

#[test]
fn mset_with_an_odd_number_of_args_is_a_resp_error() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"MSET", b"a", b"1", b"b"]), &mut Protocol::default(), 1),
        Frame::Error("ERR wrong number of arguments for 'mset' command".into())
    );
}

#[test]
fn msetnx_returns_zero_when_a_key_already_exists() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"a", b"1"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"MSETNX", b"a", b"2", b"b", b"2"]), &mut Protocol::default(), 1),
        Frame::Integer(0)
    );
}

#[test]
fn rename_then_get_round_trips_through_dispatch() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"src", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"RENAME", b"src", b"dst"]), &mut Protocol::default(), 1),
        Frame::Simple("OK".into())
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"GET", b"dst"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"v"))
    );
}

#[test]
fn rename_on_missing_key_returns_resp_error() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"RENAME", b"missing", b"dst"]), &mut Protocol::default(), 1),
        Frame::Error("no such key".into())
    );
}

#[test]
fn type_reports_none_for_a_missing_key() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"TYPE", b"missing"]), &mut Protocol::default(), 1),
        Frame::Simple("none".into())
    );
}

#[test]
fn randomkey_on_empty_keyspace_returns_null() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"RANDOMKEY"]), &mut Protocol::default(), 1),
        Frame::Null
    );
}

#[test]
fn expire_family_returns_a_clear_not_implemented_error() {
    let engine = Engine::new();
    for name in ["EXPIRE", "PEXPIRE", "EXPIREAT", "PEXPIREAT", "TTL", "PTTL", "PERSIST"] {
        let reply = dispatch(
            &engine,
            cmd(&[name.as_bytes(), b"k"]),
            &mut Protocol::default(),
            1,
        );
        let Frame::Error(msg) = reply else {
            panic!("expected Frame::Error for {name}, got something else")
        };
        assert!(msg.contains("not supported yet"), "unexpected message for {name}: {msg}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — none of the new commands are wired yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — add arms to the existing `match name.as_str()` block,
// anywhere after the SCARD arm and before the PING arm
"GETSET" => {
    require_args!(rest, 2, "getset");
    match commands::string::getset(engine, rest[0].clone(), rest[1].clone()) {
        Ok(Some(b)) => Frame::Bulk(b),
        Ok(None) => Frame::Null,
        Err(e) => engine_error_to_frame(e),
    }
}
"MSET" => {
    require_args!(rest, 2, "mset");
    if rest.len() % 2 != 0 {
        return Frame::Error("ERR wrong number of arguments for 'mset' command".into());
    }
    let pairs: Vec<(Bytes, Bytes)> = rest.chunks(2).map(|c| (c[0].clone(), c[1].clone())).collect();
    commands::string::mset(engine, pairs);
    Frame::Simple("OK".into())
}
"MGET" => {
    require_args!(rest, 1, "mget");
    let vals = commands::string::mget(engine, rest);
    Frame::Array(
        vals.into_iter()
            .map(|v| match v {
                Some(b) => Frame::Bulk(b),
                None => Frame::Null,
            })
            .collect(),
    )
}
"MSETNX" => {
    require_args!(rest, 2, "msetnx");
    if rest.len() % 2 != 0 {
        return Frame::Error("ERR wrong number of arguments for 'msetnx' command".into());
    }
    let pairs: Vec<(Bytes, Bytes)> = rest.chunks(2).map(|c| (c[0].clone(), c[1].clone())).collect();
    match commands::string::msetnx(engine, pairs) {
        true => Frame::Integer(1),
        false => Frame::Integer(0),
    }
}
"RENAME" => {
    require_args!(rest, 2, "rename");
    match commands::keys::rename(engine, &rest[0], rest[1].clone()) {
        Ok(()) => Frame::Simple("OK".into()),
        Err(e) => engine_error_to_frame(e),
    }
}
"RENAMENX" => {
    require_args!(rest, 2, "renamenx");
    match commands::keys::renamenx(engine, &rest[0], rest[1].clone()) {
        Ok(true) => Frame::Integer(1),
        Ok(false) => Frame::Integer(0),
        Err(e) => engine_error_to_frame(e),
    }
}
"TYPE" => {
    require_args!(rest, 1, "type");
    Frame::Simple(commands::keys::key_type(engine, &rest[0]).into())
}
"RANDOMKEY" => match commands::keys::randomkey(engine) {
    Some(k) => Frame::Bulk(k),
    None => Frame::Null,
},
"EXPIRE" | "PEXPIRE" | "EXPIREAT" | "PEXPIREAT" | "TTL" | "PTTL" | "PERSIST" => Frame::Error(
    format!("ERR {name} is not supported yet (planned Sprint 4 — no expiry reaper exists)"),
),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 9 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): wire getset/mset/mget/msetnx/rename/renamenx/type/randomkey and stub EXPIRE family`.
