//! Pure formatting and redaction helpers for log lines.
//!
//! This module is deliberately separate from `dispatcher.rs` and free of side effects: it is
//! the single place where raw stored bytes are turned into text that reaches a log file, so it
//! must stay small enough to audit at a glance. See
//! ../../../docs/superpowers/specs/2026-09-09-verbose-logging-design.md.

use bytes::Bytes;

/// Whether `cmd`'s argument list carries credential material and must never be rendered into
/// a log line at any level.
///
/// `cmd` is the uppercased command name (`dispatcher::command_name_upper`'s output); `args`
/// excludes the command name itself, matching `dispatcher::dispatch`'s `rest` slice.
///
/// Classification is by command name, never by argument contents: keying off contents would
/// make `SET auth v` mysteriously unloggable, and would still miss a credential passed under
/// any other shape. `dispatcher.rs` already carries the same command-shape knowledge -- see
/// `command_key_and_arity`'s AUTH special case and `key_spec`'s comment naming the AUTH
/// plaintext password and the ACL SETUSER rule token as non-key bytes.
///
/// `CONFIG` is deliberately absent: rocket-mem's CONFIG surface exposes no credential
/// parameter (there is no `requirepass` or `masterauth`), so redacting it would guard
/// nothing. Add an arm here if that ever changes.
pub fn is_sensitive(cmd: &str, args: &[Bytes]) -> bool {
    match cmd {
        // Every form of AUTH: the one-argument password form and the two-argument
        // user+password form both carry the secret.
        "AUTH" => true,
        // Only the `HELLO <proto> AUTH <user> <pass>` form. A bare protocol negotiation is
        // the common case and stays loggable.
        "HELLO" => args.iter().any(|a| a.eq_ignore_ascii_case(b"AUTH")),
        // SETUSER carries rule tokens including `>password`; GETUSER echoes a user's rules
        // back. ACL LIST/WHOAMI/CAT expose no credential material.
        "ACL" => matches!(
            args.first(),
            Some(sub) if sub.eq_ignore_ascii_case(b"SETUSER") || sub.eq_ignore_ascii_case(b"GETUSER")
        ),
        _ => false,
    }
}

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
    use bytes::Bytes;

    fn args(items: &[&[u8]]) -> Vec<Bytes> {
        items.iter().map(|b| Bytes::copy_from_slice(b)).collect()
    }

    #[test]
    fn auth_is_always_sensitive() {
        assert!(is_sensitive("AUTH", &args(&[b"hunter2"])));
        assert!(is_sensitive("AUTH", &args(&[b"alice", b"hunter2"])));
        // Even a malformed AUTH with no arguments stays sensitive -- the classification is
        // by command, never by whether this particular call looks well-formed.
        assert!(is_sensitive("AUTH", &args(&[])));
    }

    #[test]
    fn hello_is_sensitive_only_when_it_carries_an_auth_clause() {
        assert!(is_sensitive(
            "HELLO",
            &args(&[b"3", b"AUTH", b"alice", b"hunter2"])
        ));
        assert!(is_sensitive(
            "HELLO",
            &args(&[b"3", b"auth", b"alice", b"hunter2"])
        ));
        // A bare protocol negotiation carries no secret and stays loggable -- this is the
        // common case and losing it would blind the RESP3 upgrade log in plan 11.
        assert!(!is_sensitive("HELLO", &args(&[b"3"])));
        assert!(!is_sensitive("HELLO", &args(&[])));
    }

    #[test]
    fn acl_is_sensitive_for_setuser_and_getuser_only() {
        assert!(is_sensitive(
            "ACL",
            &args(&[b"SETUSER", b"alice", b">hunter2"])
        ));
        assert!(is_sensitive("ACL", &args(&[b"setuser", b"alice"])));
        assert!(is_sensitive("ACL", &args(&[b"GETUSER", b"alice"])));
        // ACL LIST / WHOAMI / CAT expose no credential material.
        assert!(!is_sensitive("ACL", &args(&[b"WHOAMI"])));
        assert!(!is_sensitive("ACL", &args(&[b"LIST"])));
        assert!(!is_sensitive("ACL", &args(&[])));
    }

    #[test]
    fn ordinary_data_commands_are_not_sensitive() {
        assert!(!is_sensitive("SET", &args(&[b"k", b"v"])));
        assert!(!is_sensitive("GET", &args(&[b"k"])));
        assert!(!is_sensitive(
            "CONFIG",
            &args(&[b"SET", b"maxmemory", b"100"])
        ));
    }

    #[test]
    fn a_key_named_auth_does_not_make_an_ordinary_command_sensitive() {
        // Classification keys off the command name, never off argument contents -- otherwise
        // `SET auth v` would silently stop being loggable.
        assert!(!is_sensitive("SET", &args(&[b"AUTH", b"v"])));
    }

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
