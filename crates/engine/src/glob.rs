/// Matches `text` against a Redis-style glob `pattern`. Supports `*` (any run, including
/// empty), `?` (exactly one character), `[abc]` (one character from the listed set),
/// `[a-z]` (one character from a range), `[^abc]`/`[!abc]` (negated set), and a top-level
/// `\` to match the next character literally. Escaping is not supported inside `[...]`
/// classes — see `docs/superpowers/specs/2026-08-30-tech-debt-cleanup-spec.md`.
pub fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some(b'\\') if pattern.len() > 1 => {
            let literal = pattern[1];
            !text.is_empty() && text[0] == literal && glob_match(&pattern[2..], &text[1..])
        }
        Some(b'*') => {
            glob_match(&pattern[1..], text) || (!text.is_empty() && glob_match(pattern, &text[1..]))
        }
        Some(b'?') => !text.is_empty() && glob_match(&pattern[1..], &text[1..]),
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
        Some(&c) => !text.is_empty() && text[0] == c && glob_match(&pattern[1..], &text[1..]),
    }
}

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

    #[test]
    fn combined_pattern_matches_realistically() {
        assert!(glob_match(b"user:???:[ab]*", b"user:123:a-session"));
        assert!(!glob_match(b"user:???:[ab]*", b"user:123:c-session"));
    }

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
}
