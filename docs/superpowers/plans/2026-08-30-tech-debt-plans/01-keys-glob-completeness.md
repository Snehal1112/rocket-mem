# KEYS Glob Completeness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `glob_match` in `crates/engine/src/glob.rs` to support bracket-class ranges (`[a-z]`), negation (`[^abc]` / `[!abc]`), and top-level backslash escaping (`\*`, `\?`, `\[`, `\\`).

**Architecture:** Pure additions to the existing single-function recursive matcher — no new files, no signature change, no call-site change (`KEYS`'s dispatcher arm keeps calling `glob_match(pattern, text)` exactly as today).

**Tech Stack:** Rust, existing `engine` crate, no new dependencies.

**Spec:** `../../specs/2026-08-30-tech-debt-cleanup-spec.md` (Item 3)

## Global Constraints

- No new dependencies in `crates/engine/Cargo.toml`.
- Escaping applies only at the top level of the pattern, not inside `[...]` classes — this is an explicit scope boundary from the spec, not an oversight.
- `cargo fmt --all -- --check` and `cargo clippy --workspace -- -D warnings` must both pass before any commit in this plan.

---

### Task 1: Bracket-class ranges and negation

**Files:**
- Modify: `crates/engine/src/glob.rs`
- Test: `crates/engine/src/glob.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: a new private helper `fn class_matches(class: &[u8], c: u8) -> bool` used only within `glob.rs`.
- `glob_match`'s public signature (`fn glob_match(pattern: &[u8], text: &[u8]) -> bool`) is unchanged.

- [ ] **Step 1: Write the failing tests**

Add these to the existing `#[cfg(test)] mod tests` block in `crates/engine/src/glob.rs`, after `bracket_class_matches_one_of_the_listed_characters`:

```rust
    #[test]
    fn bracket_class_range_matches_any_byte_in_the_range() {
        assert!(glob_match(b"[a-c]", b"a"));
        assert!(glob_match(b"[a-c]", b"b"));
        assert!(glob_match(b"[a-c]", b"c"));
        assert!(!glob_match(b"[a-c]", b"d"));
    }

    #[test]
    fn bracket_class_range_combines_with_literal_members() {
        assert!(glob_match(b"[a-cz]", b"z"));
        assert!(glob_match(b"[a-cz]", b"b"));
        assert!(!glob_match(b"[a-cz]", b"y"));
    }

    #[test]
    fn bracket_class_trailing_hyphen_is_a_literal_hyphen() {
        assert!(glob_match(b"[a-]", b"a"));
        assert!(glob_match(b"[a-]", b"-"));
        assert!(!glob_match(b"[a-]", b"b"));
    }

    #[test]
    fn bracket_class_caret_negates_the_set() {
        assert!(glob_match(b"[^abc]", b"d"));
        assert!(!glob_match(b"[^abc]", b"a"));
    }

    #[test]
    fn bracket_class_bang_negates_the_set() {
        assert!(glob_match(b"[!abc]", b"d"));
        assert!(!glob_match(b"[!abc]", b"a"));
    }

    #[test]
    fn bracket_class_negated_range_excludes_the_whole_range() {
        assert!(glob_match(b"[^a-c]", b"d"));
        assert!(!glob_match(b"[^a-c]", b"b"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p engine glob::tests -- --nocapture`
Expected: the new range/negation tests FAIL (current bracket matching treats `-`, `^`, and `!` as plain literal members of the set — e.g. `[a-c]` today only matches `a`, `-`, or `c`, not `b`).

- [ ] **Step 3: Implement range and negation support**

Replace the `Some(b'[') => ...` arm and add the new helper in `crates/engine/src/glob.rs`:

```rust
        Some(b'[') => match pattern.iter().position(|&b| b == b']') {
            Some(close) => {
                if text.is_empty() {
                    return false;
                }
                let mut class = &pattern[1..close];
                let negate = matches!(class.first(), Some(b'^') | Some(b'!'));
                if negate {
                    class = &class[1..];
                }
                let matched = class_matches(class, text[0]);
                (matched != negate) && glob_match(&pattern[close + 1..], &text[1..])
            }
            // Unterminated class: treat the '[' as a literal character.
            None => !text.is_empty() && text[0] == b'[' && glob_match(&pattern[1..], &text[1..]),
        },
```

Add the helper function below `glob_match` (before the `#[cfg(test)]` module):

```rust
/// Matches `c` against a bracket-class body (with any leading `^`/`!` negation marker already
/// stripped by the caller). `lo-hi` in the middle of the class expands to a range; a lone
/// trailing `-` (no byte after it to complete a range) is a literal hyphen.
fn class_matches(class: &[u8], c: u8) -> bool {
    let mut i = 0;
    while i < class.len() {
        if i + 2 < class.len() && class[i + 1] == b'-' {
            let (lo, hi) = (class[i], class[i + 2]);
            if lo <= c && c <= hi {
                return true;
            }
            i += 3;
        } else {
            if class[i] == c {
                return true;
            }
            i += 1;
        }
    }
    false
}
```

Also update the module doc comment at the top of `glob.rs` to reflect the new support:

```rust
/// Matches `text` against a Redis-style glob `pattern`. Supports `*` (any run, including
/// empty), `?` (exactly one character), `[abc]` (one character from the listed set),
/// `[a-z]` (one character from a range), `[^abc]`/`[!abc]` (negated set), and a top-level
/// `\` to match the next character literally. Escaping is not supported inside `[...]`
/// classes — see `docs/superpowers/specs/2026-08-30-tech-debt-cleanup-spec.md`.
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p engine glob::tests -- --nocapture`
Expected: PASS, including all pre-existing glob tests (unchanged behavior for plain `[abc]` classes, since a class with no `-` in a middle position falls entirely into the one-byte-at-a-time branch).

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/glob.rs
git commit -m "feat(engine): support bracket-class ranges and negation in KEYS glob"
```

---

### Task 2: Top-level backslash escaping

**Files:**
- Modify: `crates/engine/src/glob.rs`
- Test: `crates/engine/src/glob.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `class_matches` and the updated `Some(b'[')` arm from Task 1 (unchanged by this task).
- Produces: no new public items — `glob_match`'s signature stays the same.

- [ ] **Step 1: Write the failing tests**

Add to the same test module:

```rust
    #[test]
    fn backslash_escapes_a_star_to_match_it_literally() {
        assert!(glob_match(b"a\\*b", b"a*b"));
        assert!(!glob_match(b"a\\*b", b"axb"));
    }

    #[test]
    fn backslash_escapes_a_question_mark_to_match_it_literally() {
        assert!(glob_match(b"a\\?b", b"a?b"));
        assert!(!glob_match(b"a\\?b", b"axb"));
    }

    #[test]
    fn backslash_escapes_an_open_bracket_to_match_it_literally() {
        assert!(glob_match(b"a\\[b", b"a[b"));
        assert!(!glob_match(b"a\\[b", b"axb"));
    }

    #[test]
    fn backslash_escapes_itself_to_match_a_literal_backslash() {
        assert!(glob_match(b"a\\\\b", b"a\\b"));
        assert!(!glob_match(b"a\\\\b", b"axb"));
    }

    #[test]
    fn trailing_backslash_with_nothing_after_it_matches_as_a_literal_backslash() {
        // No second byte to escape -- falls through to matching '\' itself literally.
        assert!(glob_match(b"a\\", b"a\\"));
        assert!(!glob_match(b"a\\", b"a"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p engine glob::tests -- --nocapture`
Expected: FAIL — today `\` is matched only as a literal character via the catch-all `Some(&c)` arm, so `a\*b` requires the text to contain a literal `\` followed by `*`, not treat `*` as literal.

- [ ] **Step 3: Implement escaping**

Add a new match arm to `glob_match`, immediately before the `Some(b'*')` arm (order matters: it must be checked before the generic `Some(&c)` catch-all, and before `*`/`?`/`[` so an escaped one of those doesn't take the special-character path):

```rust
        Some(b'\\') if pattern.len() > 1 => {
            let literal = pattern[1];
            !text.is_empty() && text[0] == literal && glob_match(&pattern[2..], &text[1..])
        }
```

The existing `Some(&c) => ...` catch-all arm already handles a trailing lone `\` (pattern length 1, so the guard `pattern.len() > 1` is false and match falls through to the catch-all, treating `\` as a literal character) — no separate case needed for that.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p engine glob::tests -- --nocapture`
Expected: PASS, all glob tests including Task 1's.

- [ ] **Step 5: Run the full engine test suite and lints**

Run: `cargo test -p engine && cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/glob.rs
git commit -m "feat(engine): support backslash escaping in KEYS glob"
```
