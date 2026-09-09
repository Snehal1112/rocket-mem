//! Escaping and length-capping for client-controlled text on its way into a log line.
//!
//! # Why this is in `common` and not in `crates/server/src/logging.rs`
//!
//! The verbose-logging spec designates `crates/server/src/logging.rs` as the single auditable
//! place a *secret* could reach a log, and that stays true: redaction policy (which commands
//! carry credentials, what replaces their arguments) lives there and only there. This module
//! holds no policy. It is the mechanical escaper, and it sits in `common` for one reason: the
//! `engine` crate logs key names too (`engine.rs`'s shard-routing, byte-delta and eviction
//! events), `server` depends on `engine` rather than the other way round, so a helper in
//! `server` is simply unreachable from the sites that need it. `common` is the one crate every
//! other crate already depends on. `crates/server/src/logging.rs` re-exports everything here,
//! so every server-side call site still reads `crate::logging::…` and `logging.rs` remains the
//! one file a reviewer opens to see what reaches a log.
//!
//! # What the escaping is for
//!
//! It is a log-*integrity* guard, not a confidentiality one. rocket-mem accepts arbitrary bytes
//! as keys, usernames, command names and advertised replica addresses, and `tracing`'s `%`
//! (Display) fields reach the writer verbatim. A key containing `\n` therefore forges a second,
//! indistinguishable log record -- including a fake `auth success` line -- and an ANSI escape
//! repaints the operator's terminal. Both are reachable by an unauthenticated remote party:
//! `AUTH "<newline>…" x` on any ACL-configured server, and `PSYNC "<newline>…"` on a default
//! one, which is why the cap and the escaping apply at every level rather than only at `debug`.

use std::borrow::Cow;

/// The cap applied to identifier-shaped log fields: keys, usernames, command names, advertised
/// `host:port` strings.
///
/// Deliberately a compile-time constant rather than the operator-tunable
/// `Config::log_value_max_bytes`, for three reasons:
///
/// * That knob documents itself as the cap on *stored values* in the `trace`-level argument
///   line -- an operator lowers it to keep user data out of the log. Reusing it would mean
///   tightening a privacy setting silently truncated the keys in `warn`-level slow-log lines,
///   and that raising it (to debug a large value) silently re-opened the unbounded-record
///   problem on the pre-auth username path.
/// * It is a runtime `Config` field, plumbed through `ReplicationHandle`. Neither `engine` --
///   which has no configuration at all -- nor `SlowLog::maybe_record` can reach it, so reusing
///   it would fix some sites and not others.
/// * A bound on an identifier is not a tuning decision. 256 bytes renders every realistic key,
///   username and address in full while keeping one forged-log-record attempt bounded.
pub const LOG_IDENT_MAX_BYTES: usize = 256;

/// Whether `c` must not reach a log line verbatim.
///
/// Unicode category `Cc` (`char::is_control()`: C0 controls, DEL, and C1 controls -- this
/// already covers NEL U+0085 and CSI U+009B) plus `\x7f` DEL itself, plus categories `Zl`/`Zp`
/// (U+2028 LINE SEPARATOR and U+2029 PARAGRAPH SEPARATOR). `Zl`/`Zp` are not controls and
/// cannot move a terminal cursor, but Unicode-aware log consumers (log shipper multiline
/// filters, Python's `str.splitlines()`, PCRE's `\R`) treat them as hard line breaks, so text
/// containing one could forge a second log record downstream just as `\n` would.
fn is_log_unsafe(c: char) -> bool {
    c.is_control() || c == '\x7f' || c == '\u{2028}' || c == '\u{2029}'
}

/// Renders `bytes` for a log field: truncated at `cap` *stored* bytes with a `…(N more)` marker
/// naming how many were dropped, and with every log-unsafe character escaped as `\xNN`.
///
/// The marker matters: without it a truncated field is indistinguishable from a short one,
/// which turns a log into a misleading record of what was actually there. `cap` is counted
/// against the stored bytes, before escaping expands them, so the dropped-byte count stays a
/// true statement about the input.
///
/// Returns a `Cow` and borrows in the common case -- valid UTF-8, under the cap, nothing to
/// escape -- so the fields on the hot path (`logging::key_field`, and the `user` fields that do
/// run at the production default of `info`) copy nothing. Escaping the borrowed case would be
/// the easy mistake: it would put an allocation on every logged command.
///
/// Both `Zl`/`Zp` code points exceed 0xFF, so they render as `\x2028`/`\x2029` under the same
/// `\xNN` format -- unambiguous, so no second escape format is needed.
pub fn escape_for_log(bytes: &[u8], cap: usize) -> Cow<'_, str> {
    let shown = &bytes[..cap.min(bytes.len())];
    let dropped = bytes.len() - shown.len();
    // Decoded lossily first so multi-byte UTF-8 survives as characters rather than being
    // escaped byte-by-byte; only genuine control characters (plus the Zl/Zp line-breaking
    // separators) are then expanded.
    let decoded = String::from_utf8_lossy(shown);
    if dropped == 0 && !decoded.contains(is_log_unsafe) {
        return decoded;
    }
    let mut out = String::with_capacity(decoded.len());
    for c in decoded.chars() {
        if is_log_unsafe(c) {
            out.push_str(&format!("\\x{:02x}", c as u32));
        } else {
            out.push(c);
        }
    }
    if dropped > 0 {
        out.push_str(&format!("…({dropped} more)"));
    }
    Cow::Owned(out)
}

/// `escape_for_log` at the identifier cap, for raw bytes -- a key.
pub fn escape_key(bytes: &[u8]) -> Cow<'_, str> {
    escape_for_log(bytes, LOG_IDENT_MAX_BYTES)
}

/// `escape_for_log` at the identifier cap, for text that has already been decoded -- a
/// username, a command name, an advertised `host:port`.
///
/// Goes through the byte path deliberately rather than iterating `text`'s chars directly: one
/// escaper is one thing to audit, and re-validating a short identifier's UTF-8 is a scan of a
/// handful of bytes. It also keeps the byte cap meaning the same thing for both entry points,
/// including the char-boundary case -- a cut that lands mid-character renders as U+FFFD rather
/// than panicking, which slicing a `&str` at `cap` would do.
pub fn escape_ident(text: &str) -> Cow<'_, str> {
    escape_for_log(text.as_bytes(), LOG_IDENT_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_clean_text_is_returned_unchanged_and_borrowed() {
        // The perf claim every hot-path caller rests on: nothing is copied for an ordinary key.
        let out = escape_for_log(b"mykey", 128);
        assert_eq!(out, "mykey");
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn a_newline_is_escaped_so_text_cannot_forge_a_log_line() {
        assert_eq!(escape_for_log(b"a\nb", 128), "a\\x0ab");
        assert_eq!(escape_for_log(b"a\rb", 128), "a\\x0db");
        assert_eq!(escape_for_log(b"a\tb", 128), "a\\x09b");
    }

    #[test]
    fn an_ansi_escape_is_escaped_so_text_cannot_repaint_a_terminal() {
        assert_eq!(escape_for_log(b"a\x1b[31mb", 128), "a\\x1b[31mb");
    }

    #[test]
    fn the_del_byte_is_escaped() {
        assert_eq!(escape_for_log(b"a\x7fb", 128), "a\\x7fb");
    }

    #[test]
    fn unicode_line_and_paragraph_separators_are_escaped() {
        assert_eq!(escape_for_log("a\u{2028}b".as_bytes(), 128), "a\\x2028b");
        assert_eq!(escape_for_log("a\u{2029}b".as_bytes(), 128), "a\\x2029b");
    }

    #[test]
    fn ordinary_multibyte_utf8_stays_unescaped() {
        // Guards against an overbroad fix that escapes all non-ASCII characters instead of
        // just the line/paragraph separators.
        assert_eq!(escape_for_log("kéy ok!".as_bytes(), 128), "kéy ok!");
    }

    #[test]
    fn invalid_utf8_is_rendered_lossily_without_panicking() {
        assert_eq!(escape_for_log(&[0x61, 0xff, 0x62], 128), "a\u{fffd}b");
    }

    #[test]
    fn text_over_the_cap_is_truncated_with_a_marker() {
        assert_eq!(escape_for_log(b"abcdefghij", 4), "abcd…(6 more)");
    }

    #[test]
    fn text_exactly_at_the_cap_is_not_truncated() {
        assert_eq!(escape_for_log(b"abcd", 4), "abcd");
    }

    #[test]
    fn a_zero_cap_renders_only_the_marker() {
        assert_eq!(escape_for_log(b"abc", 0), "…(3 more)");
    }

    #[test]
    fn the_cap_is_counted_in_bytes_before_escaping() {
        // The cap bounds how much of the input is shown, not how long the rendered string ends
        // up -- escaping expands bytes, and a cap that drifted with it would make the
        // truncation count meaningless.
        assert_eq!(escape_for_log(b"\n\n\n\n\n\n", 2), "\\x0a\\x0a…(4 more)");
    }

    #[test]
    fn escape_key_caps_an_unbounded_key_at_the_identifier_limit() {
        let key = vec![b'k'; LOG_IDENT_MAX_BYTES + 10];
        let out = escape_key(&key);
        assert!(out.ends_with("…(10 more)"), "got: {out}");
    }

    #[test]
    fn escape_ident_caps_an_unbounded_username_at_the_identifier_limit() {
        let user = "u".repeat(LOG_IDENT_MAX_BYTES + 1);
        let out = escape_ident(&user);
        assert!(out.ends_with("…(1 more)"), "got: {out}");
    }

    #[test]
    fn escape_ident_borrows_an_ordinary_username() {
        assert!(matches!(escape_ident("alice"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_cap_landing_mid_character_renders_lossily_rather_than_panicking() {
        // Slicing a `&str` at an arbitrary byte offset panics; going through the byte path
        // does not. "é" is two bytes, so a cap of 1 cuts it in half.
        assert_eq!(escape_for_log("é".as_bytes(), 1), "\u{fffd}…(1 more)");
    }
}
