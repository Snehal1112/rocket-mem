# MEMORY USAGE & OBJECT ENCODING Stubs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `MEMORY USAGE key` and `OBJECT ENCODING key` respond usefully (not "unknown command") for tooling that probes them — the sprint backlog's explicit P2 stub item.

**Architecture:** both are dispatcher-only additions with no new engine surface: `MEMORY USAGE` reports `Value::approx_size()` (from `07-lru-eviction-maxmemory.md`), read in place through the existing `Engine::with_ref`; `OBJECT ENCODING` reports `Value::type_name()` (existing, Sprint 1) as a stand-in encoding name — clearly not real Redis's actual internal encodings (`embstr`/`listpack`/etc.), which this engine doesn't implement.

**Tech Stack:** none new.

**Spec:** `../../specs/2026-08-30-sprint-4-spec.md` — no `MEMORY`/`OBJECT`-specific decisions; this plan follows the established "clear stub, not silent inaccuracy" convention already used for `KEYS`'s partial glob support and `EXPIRE`'s prior stub error.

**Depends on:** `07-lru-eviction-maxmemory.md` (`Value::approx_size`).

---

### Task 1: `MEMORY USAGE` and `OBJECT ENCODING`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `Value::approx_size` (from `07-lru-eviction-maxmemory.md`), `Value::type_name` (existing, Sprint 1).
- Produces: `"MEMORY"` and `"OBJECT"` match arms, each with one supported subcommand.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn memory_usage_reports_the_approximate_size_of_an_existing_key() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    let Frame::Integer(n) =
        dispatch(&engine, cmd(&[b"MEMORY", b"USAGE", b"k"]), &mut Protocol::default(), 1)
    else {
        panic!("expected Integer")
    };
    assert!(n > 0);
}

#[test]
fn memory_usage_on_a_missing_key_returns_null() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"MEMORY", b"USAGE", b"missing"]), &mut Protocol::default(), 1),
        Frame::Null
    );
}

#[test]
fn memory_with_an_unknown_subcommand_is_a_resp_error() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"MEMORY", b"NOPE"]), &mut Protocol::default(), 1),
        Frame::Error("ERR unknown MEMORY subcommand 'NOPE'".into())
    );
}

#[test]
fn object_encoding_reports_a_type_derived_name_for_each_value_type() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"s", b"v"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"RPUSH", b"l", b"v"]), &mut Protocol::default(), 1);
    assert_eq!(
        dispatch(&engine, cmd(&[b"OBJECT", b"ENCODING", b"s"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"string"))
    );
    assert_eq!(
        dispatch(&engine, cmd(&[b"OBJECT", b"ENCODING", b"l"]), &mut Protocol::default(), 1),
        Frame::Bulk(Bytes::from_static(b"list"))
    );
}

#[test]
fn object_encoding_on_a_missing_key_is_a_resp_error() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"OBJECT", b"ENCODING", b"missing"]), &mut Protocol::default(), 1),
        Frame::Error("ERR no such key".into())
    );
}

#[test]
fn object_with_an_unknown_subcommand_is_a_resp_error() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"OBJECT", b"NOPE", b"k"]), &mut Protocol::default(), 1),
        Frame::Error("ERR unknown OBJECT subcommand 'NOPE'".into())
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — `MEMORY`/`OBJECT` are currently unknown commands

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — add near the other administrative commands (e.g. near INFO)
"MEMORY" => {
    require_args!(rest, 1, "memory");
    let subcommand = String::from_utf8_lossy(&rest[0]).to_ascii_uppercase();
    match subcommand.as_str() {
        "USAGE" => {
            require_args!(rest, 2, "memory usage");
            // with_ref, not get: sizing a value must not first clone the whole thing out
            match engine.with_ref(&rest[1], |v| v.map(|v| v.approx_size())) {
                Some(n) => Frame::Integer(n as i64),
                None => Frame::Null,
            }
        }
        _ => Frame::Error(format!("ERR unknown MEMORY subcommand '{subcommand}'")),
    }
}
"OBJECT" => {
    require_args!(rest, 1, "object");
    let subcommand = String::from_utf8_lossy(&rest[0]).to_ascii_uppercase();
    match subcommand.as_str() {
        "ENCODING" => {
            require_args!(rest, 2, "object encoding");
            // `type_name` returns &'static str, so nothing borrows past the closure
            match engine.with_ref(&rest[1], |v| v.map(|v| v.type_name())) {
                Some(name) => Frame::Bulk(Bytes::from(name)),
                None => Frame::Error("ERR no such key".into()),
            }
        }
        _ => Frame::Error(format!("ERR unknown OBJECT subcommand '{subcommand}'")),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 6 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): add MEMORY USAGE and OBJECT ENCODING stubs`.
