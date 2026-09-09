//! Pure formatting and redaction helpers for log lines.
//!
//! This module is deliberately separate from `dispatcher.rs` and free of side effects: it is
//! the single place where raw stored bytes are turned into text that reaches a log file, so it
//! must stay small enough to audit at a glance. See
//! ../../../docs/superpowers/specs/2026-09-09-verbose-logging-design.md.

use bytes::Bytes;

/// The escaper every client-controlled log field in this crate goes through, re-exported from
/// `common::log_escape` so that this module stays the one file a reviewer opens.
///
/// The implementation is in `common` rather than here for a reason spelled out in that
/// module's own header: `engine` logs key names too and cannot depend on `server`. No policy
/// moved with it -- `is_sensitive` and `redact_args`, the parts that decide what is a secret,
/// are still below and still only here.
pub(crate) use common::log_escape::escape_ident;

/// Renders `dispatcher::logged_key`'s key for a log field -- the `cmd` span's `key`, and
/// `SlowLog::maybe_record`'s `warn!` (see that fn's doc comment for why that event needs its own
/// copy of the key rather than leaning on the span).
///
/// Lives here rather than in `dispatcher.rs`, where it started, because it has two callers in
/// two modules and this module is the one the spec designates as *the single auditable place a
/// secret could reach a log*. Reuse over duplication was always right; the destination was not.
///
/// A `Bytes` must never reach a log line through `Debug`: that impl renders byte-by-byte, so a
/// key logged with `?` comes out unreadable *and* costs O(len) of formatting on the hottest
/// path in the project. Lossy UTF-8 is the right rendering instead -- and for a valid-UTF-8
/// key, which is essentially all of them, `from_utf8_lossy` returns a `Cow::Borrowed` and
/// copies nothing.
///
/// It is not lossy UTF-8 *alone*, though. A key is arbitrary client bytes, so it goes through
/// `common::log_escape::escape_key`: unescaped, a key containing `\n` forges a second log
/// record and one containing an ANSI escape repaints the operator's terminal, and both would
/// reach a `warn`-level slow-log line at the production default. The escaper still borrows for
/// an ordinary key, so the no-copy property above survives -- pinned by
/// `key_field_borrows_a_valid_utf8_key_instead_of_allocating` below.
///
/// `None` -- a keyless command such as `PING`, `ECHO` or `AUTH`, all of which
/// `dispatcher::logged_key` deliberately reports as keyless -- renders as the empty string
/// rather than a literal `"None"`, so a `key=` field is either a real key or visibly absent.
/// `pub(crate)`, not `pub` like this module's other helpers: it had that visibility in
/// `dispatcher.rs` and moving a function must not widen the crate's public surface.
pub(crate) fn key_field(key: Option<&Bytes>) -> std::borrow::Cow<'_, str> {
    match key {
        Some(k) => common::log_escape::escape_key(k),
        None => std::borrow::Cow::Borrowed(""),
    }
}

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
///
/// `REPLICAOF` was added after a review caught it missing: its six-token form,
/// `REPLICAOF <host> <port> AUTH <username> <password>`, carries a plaintext password (see
/// `dispatcher::handle_replicaof`), so it needs the same "only when an AUTH clause is
/// present" treatment as HELLO.
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
        // Only the six-token `REPLICAOF <host> <port> AUTH <username> <password>` form. A
        // bare `REPLICAOF <host> <port>` and `REPLICAOF NO ONE` carry no secret and stay
        // loggable. `args` excludes the command name, so the AUTH keyword -- when present --
        // sits at `args[2]`, matching `handle_replicaof`'s `items[3]` (which includes the
        // command name at `items[0]`).
        "REPLICAOF" => args.get(2).is_some_and(|a| a.eq_ignore_ascii_case(b"AUTH")),
        _ => false,
    }
}

/// Renders a stored value for a `trace`-level log line: `common::log_escape::escape_for_log` at
/// the operator-tunable `Config::log_value_max_bytes` cap, owned.
///
/// The escaping and truncation used to live here; they moved to `common` when `key_field`, the
/// `user`/`cmd`/`host_port` fields and `engine`'s own key fields all turned out to need the same
/// treatment. This wrapper stays because the *cap* is the distinction worth keeping: a value's
/// cap is a knob an operator turns to control how much user data reaches a log, while an
/// identifier's is the fixed `LOG_IDENT_MAX_BYTES`. See that constant's doc comment.
///
/// Returns an owned `String` rather than the escaper's `Cow`, giving up its borrow. That costs
/// nothing real: the only caller is `redact_args`, which is `trace`-only and allocates a joined
/// `String` regardless.
pub fn fmt_value(bytes: &[u8], cap: usize) -> String {
    common::log_escape::escape_for_log(bytes, cap).into_owned()
}

/// The text that replaces a sensitive command's entire argument list.
const REDACTED: &str = "<redacted>";

/// Renders a command's argument list for a `trace`-level log line, or `<redacted>` when the
/// command carries credential material.
///
/// The whole list is replaced, never just the argument believed to hold the secret: AUTH's
/// one-argument and two-argument forms put the password in different positions, so any
/// positional rule would leak one form while guarding the other.
///
/// Allocates, which is acceptable because every caller sits behind a `trace`-level check --
/// `tracing`'s macros only evaluate field expressions when the callsite is enabled, so this
/// never runs at the default `info` level.
pub fn redact_args(cmd: &str, args: &[Bytes], cap: usize) -> String {
    if is_sensitive(cmd, args) {
        return REDACTED.to_string();
    }
    args.iter()
        .map(|a| fmt_value(a, cap))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn args(items: &[&[u8]]) -> Vec<Bytes> {
        items.iter().map(|b| Bytes::copy_from_slice(b)).collect()
    }

    #[test]
    fn key_field_renders_a_key_as_text_never_as_debug_bytes() {
        let key = Bytes::from_static(b"mykey");
        assert_eq!(key_field(Some(&key)), "mykey");
        // The failure this guards: `Bytes`'s Debug impl renders byte-by-byte, so a key logged
        // with `?` comes out as `b"mykey"` or a numeric list. Neither is greppable, and both
        // are O(len) of formatting on the hottest path in the project.
        assert!(!key_field(Some(&key)).contains('['));
        assert!(!key_field(Some(&key)).contains("b\""));
    }

    #[test]
    fn key_field_renders_a_keyless_command_as_an_empty_string() {
        // PING, and every other command `dispatcher::logged_key` returns `None` for -- including
        // AUTH, which it deliberately reports as keyless so the password can never surface here.
        assert_eq!(key_field(None), "");
    }

    #[test]
    fn key_field_renders_a_non_utf8_key_lossily_without_panicking() {
        let key = Bytes::from_static(&[0x61, 0xff, 0x62]);
        assert_eq!(key_field(Some(&key)), "a\u{fffd}b");
    }

    #[test]
    fn key_field_borrows_a_valid_utf8_key_instead_of_allocating() {
        // This is the perf claim the span rests on: for a valid-UTF-8 key the Cow is Borrowed,
        // so entering the span copies no key bytes.
        let key = Bytes::from_static(b"mykey");
        assert!(matches!(
            key_field(Some(&key)),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn key_field_escapes_a_newline_so_a_key_cannot_forge_a_log_line() {
        // A key is arbitrary client bytes and reaches a `warn`-level slow-log line at the
        // production default level, so an unescaped `\n` here writes a second, forged record
        // into the operator's audit trail.
        let key = Bytes::from_static(b"a\nb");
        assert_eq!(key_field(Some(&key)), "a\\x0ab");
        assert!(!key_field(Some(&key)).contains('\n'));
    }

    #[test]
    fn key_field_escapes_an_ansi_escape_so_a_key_cannot_repaint_a_terminal() {
        let key = Bytes::from_static(b"a\x1b[31mb");
        assert_eq!(key_field(Some(&key)), "a\\x1b[31mb");
    }

    #[test]
    fn key_field_caps_an_unbounded_key() {
        // rocket-mem accepts keys far larger than any log line should carry; without a cap one
        // client can write an arbitrarily long record into the log.
        let key = Bytes::from(vec![b'k'; common::log_escape::LOG_IDENT_MAX_BYTES + 7]);
        assert!(key_field(Some(&key)).ends_with("…(7 more)"));
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
    fn replicaof_is_sensitive_only_when_it_carries_an_auth_clause() {
        // `REPLICAOF <host> <port> AUTH <user> <pass>` -- args excludes the command name, so
        // the AUTH keyword sits at args[2]. Verified against `handle_replicaof` in
        // dispatcher.rs, which parses `items[3]` (items includes the command name at
        // items[0]).
        assert!(is_sensitive(
            "REPLICAOF",
            &args(&[b"127.0.0.1", b"1", b"AUTH", b"app", b"changeme"])
        ));
        assert!(is_sensitive(
            "REPLICAOF",
            &args(&[b"127.0.0.1", b"1", b"auth", b"app", b"changeme"])
        ));
        // A bare host/port form carries no secret and stays loggable.
        assert!(!is_sensitive("REPLICAOF", &args(&[b"127.0.0.1", b"1"])));
        // `REPLICAOF NO ONE` carries no secret either.
        assert!(!is_sensitive("REPLICAOF", &args(&[b"NO", b"ONE"])));
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

    #[test]
    fn redact_args_renders_ordinary_arguments_space_separated() {
        assert_eq!(redact_args("SET", &args(&[b"k", b"v"]), 128), "k v");
    }

    #[test]
    fn redact_args_renders_no_arguments_as_an_empty_string() {
        assert_eq!(redact_args("PING", &args(&[]), 128), "");
    }

    #[test]
    fn redact_args_applies_the_cap_to_each_argument_independently() {
        assert_eq!(
            redact_args("SET", &args(&[b"k", b"abcdefghij"]), 4),
            "k abcd…(6 more)"
        );
    }

    #[test]
    fn redact_args_escapes_control_bytes_in_arguments() {
        assert_eq!(
            redact_args("SET", &args(&[b"k", b"a\nb"]), 128),
            "k a\\x0ab"
        );
    }

    #[test]
    fn redact_args_never_renders_an_auth_password() {
        let rendered = redact_args("AUTH", &args(&[b"alice", b"hunter2"]), 128);
        assert_eq!(rendered, "<redacted>");
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("alice"));
    }

    #[test]
    fn redact_args_never_renders_a_hello_auth_password() {
        let rendered = redact_args("HELLO", &args(&[b"3", b"AUTH", b"alice", b"hunter2"]), 128);
        assert_eq!(rendered, "<redacted>");
        assert!(!rendered.contains("hunter2"));
    }

    #[test]
    fn redact_args_never_renders_an_acl_setuser_rule_token() {
        let rendered = redact_args("ACL", &args(&[b"SETUSER", b"alice", b">hunter2"]), 128);
        assert_eq!(rendered, "<redacted>");
        assert!(!rendered.contains("hunter2"));
    }

    #[test]
    fn redact_args_redacts_the_whole_list_not_just_the_secret_argument() {
        // Partial redaction is a trap: the argument layout differs between AUTH's one- and
        // two-argument forms, so "redact the last one" would leak the password of the other.
        assert_eq!(redact_args("AUTH", &args(&[b"hunter2"]), 128), "<redacted>");
    }

    #[test]
    fn redact_args_still_renders_a_bare_hello() {
        assert_eq!(redact_args("HELLO", &args(&[b"3"]), 128), "3");
    }

    #[test]
    fn redact_args_ignores_the_cap_when_redacting() {
        // A tiny cap must not truncate the marker into something that looks like data.
        assert_eq!(redact_args("AUTH", &args(&[b"hunter2"]), 1), "<redacted>");
    }

    #[test]
    fn redact_args_never_renders_a_replicaof_auth_password() {
        let rendered = redact_args(
            "REPLICAOF",
            &args(&[b"127.0.0.1", b"1", b"AUTH", b"app", b"changeme"]),
            128,
        );
        assert_eq!(rendered, "<redacted>");
        assert!(!rendered.contains("changeme"));
    }

    #[test]
    fn fmt_value_escapes_unicode_line_and_paragraph_separators() {
        // U+2028 LINE SEPARATOR and U+2029 PARAGRAPH SEPARATOR are not covered by
        // `char::is_control()` (they are categories Zl/Zp, not Cc), but Unicode-aware log
        // consumers treat them as hard line breaks, so a stored value containing one could
        // forge a second log record downstream.
        assert_eq!(fmt_value("a\u{2028}b".as_bytes(), 128), "a\\x2028b");
        assert_eq!(fmt_value("a\u{2029}b".as_bytes(), 128), "a\\x2029b");
    }

    #[test]
    fn fmt_value_still_keeps_ordinary_multibyte_utf8_unescaped() {
        // Guards against an overbroad fix that escapes all non-ASCII characters instead of
        // just the line/paragraph separators.
        assert_eq!(fmt_value("kéy ok!".as_bytes(), 128), "kéy ok!");
    }
}

/// A log-capture harness for unit tests that need to assert on rendered log text.
///
/// Lives here, in the crate's own `src`, rather than in `tests/logging.rs`: Rust compiles each
/// file under `tests/` as a separate crate, so a helper there is not importable from a
/// `#[cfg(test)] mod tests` inside `src/`. The two cannot be merged.
///
/// `#[cfg(test)]` and introduced at its first use, because an unused test helper would trip
/// `clippy -D warnings`' dead-code lint.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    /// A `tracing_subscriber::fmt` writer backed by a shared buffer. Uses `tracing-subscriber`'s
    /// own public `MakeWriter` trait -- no new dependency.
    #[derive(Clone, Default)]
    pub(crate) struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            // `unwrap_or_else(|e| e.into_inner())` rather than `unwrap()`: a test that panics
            // while holding this lock would otherwise poison it and turn one real failure into
            // a cascade of unrelated ones.
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedLogs;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl CapturedLogs {
        /// The log text captured so far.
        pub(crate) fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
        }
    }
}
