# Verbose Logging Plan 02: `logging` Module & `fmt_value`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create `crates/server/src/logging.rs` and implement `fmt_value`, the function that renders arbitrary value bytes into a log line safely and with a length cap.

**Architecture:** A new leaf module in `crates/server` holding pure, side-effect-free formatting helpers. It depends on nothing but `bytes` and `std`, so it is trivially unit-testable without a subscriber, a socket, or an `Engine`. Keeping it separate from `dispatcher.rs` matters: `dispatcher.rs` is already 10,158 lines, and this logic must be easy to audit because it is what stands between raw stored values and the log file.

**Tech Stack:** Rust 2021, `bytes::Bytes`.

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the "Decision: log content" section.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting. The load-bearing ones here: `cargo clippy --workspace --all-targets -- -D warnings` must pass, every pre-existing test must pass unchanged, and redaction policy lives only in `crates/server`.

---

### Task 1: Create the module with `fmt_value` truncation

**Files:**
- Create: `crates/server/src/logging.rs`
- Modify: `crates/server/src/lib.rs`
- Test: `crates/server/src/logging.rs` (inline `#[cfg(test)] mod tests`, matching this codebase's convention)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn fmt_value(bytes: &[u8], cap: usize) -> String`. Plan 03's `redact_args` calls this per argument; every `trace`-level value site in plans 08 onward calls it too.

- [ ] **Step 1: Write the failing tests**

Create `crates/server/src/logging.rs` containing only the test module and a stub, so the tests compile and fail on behavior rather than on a missing symbol:

```rust
//! Pure formatting and redaction helpers for log lines.
//!
//! This module is deliberately separate from `dispatcher.rs` and free of side effects: it is
//! the single place where raw stored bytes are turned into text that reaches a log file, so it
//! must stay small enough to audit at a glance. See
//! ../../../docs/superpowers/specs/2026-09-09-verbose-logging-design.md.

/// Renders `bytes` for a log line, truncating at `cap` bytes.
pub fn fmt_value(_bytes: &[u8], _cap: usize) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_value_renders_a_short_utf8_value_unchanged() {
        assert_eq!(fmt_value(b"hello", 128), "hello");
    }

    #[test]
    fn fmt_value_truncates_at_the_cap_and_reports_how_much_it_dropped() {
        assert_eq!(fmt_value(b"abcdefghij", 4), "abcd…(6 more)");
    }

    #[test]
    fn fmt_value_leaves_a_value_exactly_at_the_cap_untruncated() {
        // Boundary: `cap` bytes is not "over" the cap, so no marker is appended.
        assert_eq!(fmt_value(b"abcd", 4), "abcd");
    }

    #[test]
    fn fmt_value_with_a_zero_cap_renders_only_the_marker() {
        assert_eq!(fmt_value(b"abc", 0), "…(3 more)");
    }

    #[test]
    fn fmt_value_renders_an_empty_value_as_an_empty_string() {
        assert_eq!(fmt_value(b"", 128), "");
    }
}
```

Wire the module into the crate. In `crates/server/src/lib.rs`, add `pub mod logging;` in alphabetical position:

```rust
pub mod acl;
pub mod aof;
pub mod cluster;
pub mod config;
pub mod connection;
pub mod dispatcher;
pub mod logging;
pub mod metrics;
pub mod replication;
pub mod rmp_connection;
pub mod slowlog;
pub mod tls;
pub use connection::{serve, serve_tls};
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem logging::tests
```

Expected: FAIL. Four of the five assertions fail on a left value of `""`; `fmt_value_renders_an_empty_value_as_an_empty_string` passes vacuously against the stub, which is fine — it is a boundary guard, not the driver.

- [ ] **Step 3: Implement truncation**

Replace the stub in `crates/server/src/logging.rs`:

```rust
/// Renders `bytes` for a log line, truncating at `cap` bytes and appending a `…(N more)`
/// marker naming how many bytes were dropped. The marker matters: without it a truncated
/// value is indistinguishable from a short one, which turns a log into a misleading record
/// of what was actually stored.
pub fn fmt_value(bytes: &[u8], cap: usize) -> String {
    if bytes.len() <= cap {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let head = String::from_utf8_lossy(&bytes[..cap]).into_owned();
    format!("{head}…({} more)", bytes.len() - cap)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem logging::tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all five tests PASS, fmt clean, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/logging.rs crates/server/src/lib.rs
git commit -m "feat(logging): add fmt_value with length-capped rendering"
```

---

### Task 2: Escape non-printable bytes

**Files:**
- Modify: `crates/server/src/logging.rs`
- Test: `crates/server/src/logging.rs` (same inline test module)

**Interfaces:**
- Consumes: `fmt_value` from Task 1.
- Produces: the same `fmt_value` signature, now binary-safe. No signature change, so nothing downstream needs updating.

- [ ] **Step 1: Write the failing tests**

rocket-mem stores arbitrary bytes, not text. A raw newline in a value would forge a second log line, and a raw ANSI escape would let a stored value repaint the operator's terminal — so control bytes must never reach the log verbatim. Add these tests to the existing `mod tests` in `crates/server/src/logging.rs`:

```rust
    #[test]
    fn fmt_value_escapes_control_bytes_so_a_value_cannot_forge_a_log_line() {
        assert_eq!(fmt_value(b"a\nb", 128), "a\\x0ab");
        assert_eq!(fmt_value(b"a\tb", 128), "a\\x09b");
        assert_eq!(fmt_value(b"a\x1b[31mb", 128), "a\\x1b[31mb");
    }

    #[test]
    fn fmt_value_escapes_the_del_byte() {
        assert_eq!(fmt_value(b"a\x7fb", 128), "a\\x7fb");
    }

    #[test]
    fn fmt_value_keeps_printable_ascii_and_multibyte_utf8_intact() {
        assert_eq!(fmt_value("kéy ok!".as_bytes(), 128), "kéy ok!");
    }

    #[test]
    fn fmt_value_counts_the_cap_in_bytes_before_escaping() {
        // The cap bounds how much of the *stored value* is shown, not how long the rendered
        // string ends up -- escaping expands bytes, and a cap that drifted with it would make
        // the truncation count meaningless.
        assert_eq!(fmt_value(b"\n\n\n\n\n\n", 2), "\\x0a\\x0a…(4 more)");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem logging::tests
```

Expected: FAIL. `fmt_value_escapes_control_bytes_so_a_value_cannot_forge_a_log_line` reports a left value containing a literal newline instead of `\x0a`.

- [ ] **Step 3: Implement escaping**

Replace `fmt_value` in `crates/server/src/logging.rs`:

```rust
/// Renders `bytes` for a log line, truncating at `cap` bytes and appending a `…(N more)`
/// marker naming how many bytes were dropped. The marker matters: without it a truncated
/// value is indistinguishable from a short one, which turns a log into a misleading record
/// of what was actually stored.
///
/// Control bytes are escaped as `\xNN`. rocket-mem stores arbitrary bytes, so a value is
/// fully capable of containing a newline (which would forge a second log line) or an ANSI
/// escape (which would repaint the operator's terminal) -- neither may reach the log
/// verbatim. `cap` is counted against the *stored* bytes, before escaping expands them, so
/// the dropped-byte count stays a true statement about the value.
pub fn fmt_value(bytes: &[u8], cap: usize) -> String {
    let shown = &bytes[..cap.min(bytes.len())];
    let mut out = String::with_capacity(shown.len());
    // Decoded lossily first so multi-byte UTF-8 survives as characters rather than being
    // escaped byte-by-byte; only genuine control characters are then expanded.
    for c in String::from_utf8_lossy(shown).chars() {
        if c.is_control() || c == '\x7f' {
            out.push_str(&format!("\\x{:02x}", c as u32));
        } else {
            out.push(c);
        }
    }
    if bytes.len() > cap {
        out.push_str(&format!("…({} more)", bytes.len() - cap));
    }
    out
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem logging::tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all nine tests PASS, fmt clean, clippy clean, and the full workspace suite still green.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/logging.rs
git commit -m "feat(logging): escape control bytes in fmt_value"
```

---

## Next plan

[`03-logging-module-redaction.md`](03-logging-module-redaction.md) — adds `is_sensitive` and `redact_args` on top of `fmt_value`, the credential guard every per-command log line depends on.
