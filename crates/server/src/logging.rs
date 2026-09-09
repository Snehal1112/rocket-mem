//! Pure formatting and redaction helpers for log lines.
//!
//! This module is deliberately separate from `dispatcher.rs` and free of side effects: it is
//! the single place where raw stored bytes are turned into text that reaches a log file, so it
//! must stay small enough to audit at a glance. See
//! ../../../docs/superpowers/specs/2026-09-09-verbose-logging-design.md.

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
