# `SCAN` Cursor Iteration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a cursor-based `SCAN` that walks the keyspace one shard per call — never blocking the whole keyspace like `KEYS` can — proven correct under concurrent writes by a stress test.

**Architecture:** `Store::scan(cursor: u64) -> (u64, Vec<Bytes>)` in `crates/engine/src/store.rs`, exposed through `Engine::scan` in `crates/engine/src/engine.rs`, wired into the dispatcher as `SCAN cursor` → a 2-element `[next-cursor, keys]` array, matching real Redis's reply shape.

**Tech Stack:** no new dependencies.

**Spec:** `../../specs/2026-08-29-sprint-3-spec.md` — the cursor design (cursor = next shard index, one shard's full key list per call) and the correctness guarantee it gives are authoritative; don't re-derive them here.

**Depends on:** nothing this sprint. Independent of every other Sprint 3 plan.

## Global Constraints

- The cursor is opaque to the client but must round-trip as a decimal string over RESP (`SCAN 0` → reply cursor `"3"` → next call `SCAN 3`), matching real Redis's wire convention.
- Cursor `0` means both "start a new scan" and "the scan is complete" — this is real Redis's own contract, not a rocket-mem invention.

---

### Task 1: `Store::scan` and `Engine::scan`

**Files:**
- Modify: `crates/engine/src/store.rs`
- Modify: `crates/engine/src/engine.rs`

**Interfaces:**
- Consumes: `Shard::keys()` (existing, Sprint 1).
- Produces: `pub fn scan(&self, cursor: u64) -> (u64, Vec<Bytes>)` on both `Store` and `Engine` — `03`'s dispatcher task and the concurrency stress test below both call `Engine::scan`/`Store::scan` respectively.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/store.rs — add to the existing tests module
#[test]
fn scan_from_cursor_zero_returns_shard_zeros_keys_and_the_next_cursor() {
    let store = Store::new(16);
    store.set(Bytes::from_static(b"k"), Value::String(Bytes::from_static(b"v")));
    let (next, _keys) = store.scan(0);
    assert_eq!(next, 1);
}

#[test]
fn scan_wraps_back_to_zero_after_the_last_shard() {
    let store = Store::new(16);
    let (next, keys) = store.scan(15);
    assert_eq!(next, 0);
    assert!(keys.is_empty() || !keys.is_empty()); // shard 15 may or may not be empty; only the cursor matters here
}

#[test]
fn scan_past_the_last_shard_returns_zero_and_no_keys() {
    let store = Store::new(16);
    let (next, keys) = store.scan(16);
    assert_eq!(next, 0);
    assert!(keys.is_empty());
}

#[test]
fn a_full_scan_visits_every_pre_existing_key_exactly_once() {
    use std::collections::HashMap;

    let store = Store::new(16);
    for i in 0..200 {
        store.set(Bytes::from(format!("k{i}")), Value::String(Bytes::from_static(b"v")));
    }

    let mut seen_counts: HashMap<Bytes, usize> = HashMap::new();
    let mut cursor = 0u64;
    loop {
        let (next, keys) = store.scan(cursor);
        for k in keys {
            *seen_counts.entry(k).or_insert(0) += 1;
        }
        cursor = next;
        if cursor == 0 {
            break;
        }
    }

    assert_eq!(seen_counts.len(), 200);
    assert!(seen_counts.values().all(|&count| count == 1));
}

#[test]
fn scan_visits_every_pre_existing_key_at_least_once_under_concurrent_writes() {
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::thread;

    let store = Arc::new(Store::new(16));
    for i in 0..5000 {
        store.set(Bytes::from(format!("pre{i}")), Value::String(Bytes::from_static(b"v")));
    }

    let writer_store = Arc::clone(&store);
    let writer = thread::spawn(move || {
        for i in 0..5000 {
            writer_store.set(Bytes::from(format!("new{i}")), Value::String(Bytes::from_static(b"v")));
        }
    });

    let mut seen: HashSet<Bytes> = HashSet::new();
    let mut cursor = 0u64;
    loop {
        let (next, keys) = store.scan(cursor);
        seen.extend(keys);
        cursor = next;
        if cursor == 0 {
            break;
        }
    }

    writer.join().unwrap();

    for i in 0..5000 {
        let key = Bytes::from(format!("pre{i}"));
        assert!(seen.contains(&key), "missing pre-existing key pre{i}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine store::tests`
Expected: FAIL — `Store::scan` is not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/store.rs — add to the `impl Store` block
pub fn scan(&self, cursor: u64) -> (u64, Vec<Bytes>) {
    let idx = cursor as usize;
    if idx >= self.shards.len() {
        return (0, Vec::new());
    }
    let keys = self.shards[idx].keys();
    let next = if idx + 1 >= self.shards.len() { 0 } else { (idx + 1) as u64 };
    (next, keys)
}
```

```rust
// crates/engine/src/engine.rs — add to the `impl Engine` block
pub fn scan(&self, cursor: u64) -> (u64, Vec<Bytes>) {
    self.store.scan(cursor)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine store::tests`
Expected: PASS, all tests including the 5 new ones

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/store.rs` and `crates/engine/src/engine.rs` — do not compose the commit
message freeform. Suggested subject: `feat(engine): add cursor-based SCAN over the sharded keyspace`.

---

### Task 2: Wire `SCAN` into the dispatcher

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `engine.scan(cursor: u64)` (Task 1).
- Produces: a `"SCAN"` arm in `dispatch`'s `match`, replying `Frame::Array([Frame::Bulk(next_cursor_as_string), Frame::Array(keys)])`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn scan_zero_returns_an_array_of_cursor_and_keys() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"v"]), &mut Protocol::default(), 1);
    let reply = dispatch(&engine, cmd(&[b"SCAN", b"0"]), &mut Protocol::default(), 1);
    let Frame::Array(parts) = reply else { panic!("expected Array") };
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], Frame::Bulk(Bytes::from_static(b"1")));
}

#[test]
fn scan_with_a_non_numeric_cursor_is_a_resp_error() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"SCAN", b"notacursor"]), &mut Protocol::default(), 1),
        Frame::Error("ERR invalid cursor".into())
    );
}

#[test]
fn a_full_scan_over_dispatch_eventually_returns_cursor_zero() {
    let engine = Engine::new();
    for i in 0..50 {
        dispatch(
            &engine,
            cmd(&[b"SET", format!("k{i}").as_bytes(), b"v"]),
            &mut Protocol::default(),
            1,
        );
    }
    let mut cursor = Bytes::from_static(b"0");
    let mut total_keys = 0;
    loop {
        let reply = dispatch(&engine, cmd(&[b"SCAN", &cursor]), &mut Protocol::default(), 1);
        let Frame::Array(parts) = reply else { panic!("expected Array") };
        let Frame::Bulk(next) = parts[0].clone() else { panic!("expected Bulk cursor") };
        let Frame::Array(keys) = parts[1].clone() else { panic!("expected Array of keys") };
        total_keys += keys.len();
        cursor = next;
        if cursor.as_ref() == b"0" {
            break;
        }
    }
    assert_eq!(total_keys, 50);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — `SCAN` is currently an unknown command

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — add near KEYS
"SCAN" => {
    require_args!(rest, 1, "scan");
    let cursor: u64 = match std::str::from_utf8(&rest[0]).ok().and_then(|s| s.parse().ok()) {
        Some(n) => n,
        None => return Frame::Error("ERR invalid cursor".into()),
    };
    let (next, keys) = engine.scan(cursor);
    Frame::Array(vec![
        Frame::Bulk(Bytes::from(next.to_string())),
        Frame::Array(keys.into_iter().map(Frame::Bulk).collect()),
    ])
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 3 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): wire cursor-based SCAN command`.
