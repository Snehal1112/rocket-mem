# Glob Pattern Matching & `KEYS` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a `glob_match(pattern, text) -> bool` function supporting `*`/`?`/`[abc]`, and `KEYS pattern` wired into the dispatcher for the first time (it's existed at the `Engine` level, pattern-free, since Sprint 1, but was never reachable over RESP).

**Architecture:** new `crates/engine/src/glob.rs`, declared `pub mod glob;` from `lib.rs` alongside the existing `commands` module. `KEYS` filters `engine.keys()` (existing, Sprint 1) through `glob_match` in the dispatcher.

**Tech Stack:** no new dependencies — pure byte-slice matching, no regex crate.

**Spec:** `../../specs/2026-08-29-sprint-3-spec.md` — the supported-syntax table (`*`, `?`, `[abc]` only; no ranges, no negation, no escaping) is authoritative.

**Depends on:** nothing this sprint. Independent of every other Sprint 3 plan.

## Global Constraints

- `glob_match` operates on `&[u8]`, not `&str` — Redis keys are arbitrary bytes, not guaranteed UTF-8.
- Unsupported syntax (ranges, negation, escaping) is out of scope — don't add it "while you're in there."

---

### Task 1: `glob_match`

**Files:**
- Create: `crates/engine/src/glob.rs`
- Modify: `crates/engine/src/lib.rs`

**Interfaces:**
- Produces: `pub fn glob_match(pattern: &[u8], text: &[u8]) -> bool`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/engine/src/glob.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pattern_matches_only_empty_text() {
        assert!(glob_match(b"", b""));
        assert!(!glob_match(b"", b"x"));
    }

    #[test]
    fn literal_pattern_matches_only_identical_text() {
        assert!(glob_match(b"foo", b"foo"));
        assert!(!glob_match(b"foo", b"bar"));
        assert!(!glob_match(b"foo", b"foobar"));
    }

    #[test]
    fn star_matches_any_run_including_empty() {
        assert!(glob_match(b"user:*", b"user:123"));
        assert!(glob_match(b"user:*", b"user:"));
        assert!(!glob_match(b"user:*", b"session:123"));
    }

    #[test]
    fn star_at_both_ends_matches_a_substring_anywhere() {
        assert!(glob_match(b"*mid*", b"a mid b"));
        assert!(!glob_match(b"*mid*", b"no match here"));
    }

    #[test]
    fn question_mark_matches_exactly_one_character() {
        assert!(glob_match(b"h?llo", b"hello"));
        assert!(!glob_match(b"h?llo", b"hllo"));
        assert!(!glob_match(b"h?llo", b"heello"));
    }

    #[test]
    fn bracket_class_matches_one_of_the_listed_characters() {
        assert!(glob_match(b"[abc]", b"a"));
        assert!(glob_match(b"[abc]", b"b"));
        assert!(!glob_match(b"[abc]", b"d"));
        assert!(!glob_match(b"[abc]", b"ab"));
    }

    #[test]
    fn combined_pattern_matches_realistically() {
        assert!(glob_match(b"user:???:[ab]*", b"user:123:a-session"));
        assert!(!glob_match(b"user:???:[ab]*", b"user:123:c-session"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine glob::tests`
Expected: FAIL — `glob_match` is not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/glob.rs (above the tests module)
/// Matches `text` against a Redis-style glob `pattern`. Supports `*` (any run, including
/// empty), `?` (exactly one character), and `[abc]` (exactly one character from the listed
/// set). Character ranges (`[a-z]`), negation (`[^abc]`), and escaping are not supported —
/// see `../../docs/superpowers/specs/2026-08-29-sprint-3-spec.md` for why.
pub fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some(b'*') => {
            glob_match(&pattern[1..], text) || (!text.is_empty() && glob_match(pattern, &text[1..]))
        }
        Some(b'?') => !text.is_empty() && glob_match(&pattern[1..], &text[1..]),
        Some(b'[') => match pattern.iter().position(|&b| b == b']') {
            Some(close) => {
                if text.is_empty() {
                    return false;
                }
                let class = &pattern[1..close];
                class.contains(&text[0]) && glob_match(&pattern[close + 1..], &text[1..])
            }
            // Unterminated class: treat the '[' as a literal character.
            None => !text.is_empty() && text[0] == b'[' && glob_match(&pattern[1..], &text[1..]),
        },
        Some(&c) => !text.is_empty() && text[0] == c && glob_match(&pattern[1..], &text[1..]),
    }
}
```

```rust
// crates/engine/src/lib.rs
pub mod commands;
pub mod glob;
mod engine;
mod shard;
mod store;
mod value;
pub use engine::Engine;
pub use store::Store;
pub use value::Value;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine glob::tests`
Expected: PASS, 7/7

- [ ] **Step 5: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/engine/src/glob.rs` and `crates/engine/src/lib.rs` — do not compose the commit
message freeform. Suggested subject: `feat(engine): add glob_match for KEYS pattern support`.

---

### Task 2: Wire `KEYS` into the dispatcher

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `engine::glob::glob_match` (Task 1), `engine.keys()` (existing, Sprint 1).
- Produces: a `"KEYS"` arm in `dispatch`'s `match`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
#[test]
fn keys_returns_only_matching_keys() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"user:1", b"a"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"SET", b"user:2", b"b"]), &mut Protocol::default(), 1);
    dispatch(&engine, cmd(&[b"SET", b"session:1", b"c"]), &mut Protocol::default(), 1);
    let Frame::Array(mut items) = dispatch(&engine, cmd(&[b"KEYS", b"user:*"]), &mut Protocol::default(), 1)
    else {
        panic!("expected Array")
    };
    items.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(
        items,
        vec![
            Frame::Bulk(Bytes::from_static(b"user:1")),
            Frame::Bulk(Bytes::from_static(b"user:2")),
        ]
    );
}

#[test]
fn keys_on_empty_keyspace_returns_empty_array() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"KEYS", b"*"]), &mut Protocol::default(), 1),
        Frame::Array(vec![])
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — `KEYS` is currently an unknown command

- [ ] **Step 3: Write the implementation**

```rust
// crates/server/src/dispatcher.rs — add near the other read commands
"KEYS" => {
    require_args!(rest, 1, "keys");
    let pattern = &rest[0];
    Frame::Array(
        engine
            .keys()
            .into_iter()
            .filter(|k| engine::glob::glob_match(pattern, k))
            .map(Frame::Bulk)
            .collect(),
    )
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 2 new ones

- [ ] **Step 5: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 6: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/src/dispatcher.rs` — do not compose the commit message freeform. Suggested
subject: `feat(server): wire KEYS with glob pattern matching`.
