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
}
