/// Matches `text` against a Redis-style glob `pattern`. Supports `*` (any run, including
/// empty), `?` (exactly one character), and `[abc]` (exactly one character from the listed
/// set). Character ranges (`[a-z]`), negation (`[^abc]`), and escaping are not supported —
/// see `docs/superpowers/specs/2026-08-29-sprint-3-spec.md` for why.
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
