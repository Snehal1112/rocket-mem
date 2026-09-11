/// One parsed pattern element. `pattern` is tokenized once up front so the matcher below can
/// walk it with plain indices instead of re-parsing variable-width syntax (`\X`, `[...]`) on
/// every backtrack.
enum Token {
    /// A single literal byte, from either a bare character or a `\`-escape.
    Literal(u8),
    /// `?` — matches exactly one byte.
    Any,
    /// `*` — matches any run of bytes, including empty.
    Star,
    /// `[...]`/`[^...]`/`[!...]` — `body` is the class content with any leading negation
    /// marker already stripped, exactly as `class_matches` expects.
    Class { body: Vec<u8>, negate: bool },
}

/// Tokenizes a glob `pattern` into `Token`s, using the same escape and bracket-class parsing
/// rules `glob_match` has always used (see its doc comment).
fn tokenize(pattern: &[u8]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < pattern.len() {
        match pattern[i] {
            b'\\' if i + 1 < pattern.len() => {
                tokens.push(Token::Literal(pattern[i + 1]));
                i += 2;
            }
            b'*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            b'?' => {
                tokens.push(Token::Any);
                i += 1;
            }
            b'[' => match pattern[i..].iter().position(|&b| b == b']') {
                Some(close) => {
                    let mut body = &pattern[i + 1..i + close];
                    let negate = matches!(body.first(), Some(b'^') | Some(b'!'));
                    if negate {
                        body = &body[1..];
                    }
                    tokens.push(Token::Class {
                        body: body.to_vec(),
                        negate,
                    });
                    i += close + 1;
                }
                // Unterminated class: treat the '[' as a literal character.
                None => {
                    tokens.push(Token::Literal(b'['));
                    i += 1;
                }
            },
            c => {
                tokens.push(Token::Literal(c));
                i += 1;
            }
        }
    }
    tokens
}

/// Does `token` match the single byte `c`? `Star` never matches a byte directly — it is
/// handled by the caller's backtracking loop instead.
fn token_matches(token: &Token, c: u8) -> bool {
    match token {
        Token::Literal(l) => *l == c,
        Token::Any => true,
        Token::Class { body, negate } => class_matches(body, c) != *negate,
        Token::Star => false,
    }
}

/// Matches `text` against a Redis-style glob `pattern`. Supports `*` (any run, including
/// empty), `?` (exactly one character), `[abc]` (one character from the listed set),
/// `[a-z]` (one character from a range), `[^abc]`/`[!abc]` (negated set), and a top-level
/// `\` to match the next character literally. Escaping is not supported inside `[...]`
/// classes — see `docs/superpowers/specs/2026-08-30-tech-debt-cleanup-spec.md`.
///
/// Uses the standard linear-time two-pointer wildcard-matching algorithm (tokenizing `pattern`
/// once, then walking token and text indices together, remembering the most recent `*` as a
/// backtrack point) rather than naive recursive backtracking — the latter re-explores both
/// branches of every `*`, which is exponential in the number of `*`s in the pattern when the
/// match ultimately fails.
pub fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let tokens = tokenize(pattern);

    let (mut ti, mut si) = (0usize, 0usize);
    let mut star_ti: Option<usize> = None;
    let mut star_si = 0usize;

    while si < text.len() {
        if ti < tokens.len() && token_matches(&tokens[ti], text[si]) {
            ti += 1;
            si += 1;
        } else if ti < tokens.len() && matches!(tokens[ti], Token::Star) {
            // Record the backtrack point: try matching zero characters with this '*' for now.
            star_ti = Some(ti);
            star_si = si;
            ti += 1;
        } else if let Some(st) = star_ti {
            // The last '*' needs to absorb one more character; retry from just after it.
            star_si += 1;
            ti = st + 1;
            si = star_si;
        } else {
            return false;
        }
    }

    // Any tokens left must all be '*' -- they match the empty remainder.
    while ti < tokens.len() && matches!(tokens[ti], Token::Star) {
        ti += 1;
    }
    ti == tokens.len()
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

    #[test]
    fn many_stars_against_a_non_matching_text_completes_quickly() {
        // Regression test for a DoS: the old naive recursive `*` handling
        // (`glob_match(&pattern[1..], text) || glob_match(pattern, &text[1..])`)
        // branches twice per '*', giving exponential worst-case time when the
        // match ultimately fails. A pattern with many '*'s against a
        // non-matching text used to hang for a very long time; it must now
        // finish comfortably within a tight wall-clock bound.
        let pattern = b"*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*b";
        let text = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; // 35 'a's, no trailing 'b'.

        let start = std::time::Instant::now();
        let matched = glob_match(pattern, text);
        let elapsed = start.elapsed();

        assert!(!matched);
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "glob_match took {elapsed:?}, expected well under 100ms"
        );
    }
}
