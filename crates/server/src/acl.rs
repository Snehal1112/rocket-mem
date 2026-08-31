//! ACL rule types, `ACL SETUSER`-style token parsing, and password hashing.
//!
//! This module holds the whole access-control-list system: rule parsing, password hashing, and
//! permission-check logic, all implemented here. Keeping them in one module mirrors how Redis
//! treats ACL as a single subsystem rather than splitting it across the places that consume it.
//! Wiring this into a live, store-backed `AclUser`/dispatcher path is a later sprint's job.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;

/// One permission grant or restriction parsed from an `ACL SETUSER` rule token. `on`/`off` and
/// password tokens live on `AclToken` instead -- they're user-level state, not permission rules,
/// so `AclUser::is_allowed` never needs to match on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclRule {
    AllCommands,
    NoCommands,
    AllowCommand(String), // uppercased
    DenyCommand(String),  // uppercased
    AllKeys,
    KeyPattern(String),
}

/// Every distinct thing an `ACL SETUSER` argument can mean. `Rule` wraps `AclRule` rather than
/// flattening its variants in here, since only the permission-rule subset ends up in
/// `AclUser::rules` -- the `Vec` that `is_allowed` folds -- while `On`/`Off`/`Password`/`NoPass`
/// carry separate user-level state, applied differently by whatever parses a full `ACL SETUSER`
/// command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclToken {
    On,
    Off,
    Password(String),
    NoPass,
    Rule(AclRule),
}

/// The one way `parse_token` can fail. A single variant (rather than one error per invalid
/// shape) because every failure mode -- empty command, empty pattern, unrecognized keyword --
/// reduces to the same Redis-compatible reply: `ERR syntax error at '<token>'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclError {
    SyntaxError(String), // the offending raw token, echoed in the error message
}

impl std::fmt::Display for AclError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The token is attacker-controlled (it comes straight off the wire), so CR/LF must
            // be scrubbed before it reaches a formatted reply -- otherwise it could split the
            // RESP reply stream for a client. This is a minimum, module-local fix; the broader
            // codec-level fix is out of scope here.
            AclError::SyntaxError(tok) => {
                let sanitized = tok.replace('\r', "\\r").replace('\n', "\\n");
                write!(f, "ERR syntax error at '{sanitized}'")
            }
        }
    }
}

/// Parses one `ACL SETUSER`-style token. Command names in `+CMD`/`-CMD` are uppercased here so
/// every later comparison (`AclUser::is_allowed`) is a plain string-equality check, never a
/// case-insensitive one. The fixed keywords (`on`/`off`/`nopass`/`allcommands`/`nocommands`/
/// `allkeys`) are matched case-insensitively, matching real Redis's `strcasecmp`; the `+CMD`/
/// `-CMD`/`~pattern`/`>password` prefix forms stay byte-exact past their prefix, since patterns
/// and passwords must not be case-folded.
pub fn parse_token(raw: &[u8]) -> Result<AclToken, AclError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| AclError::SyntaxError(String::from_utf8_lossy(raw).into_owned()))?;
    let lower = text.to_ascii_lowercase();
    match text {
        _ if lower == "on" => Ok(AclToken::On),
        _ if lower == "off" => Ok(AclToken::Off),
        _ if lower == "nopass" => Ok(AclToken::NoPass),
        _ if lower == "allcommands" => Ok(AclToken::Rule(AclRule::AllCommands)),
        _ if lower == "nocommands" => Ok(AclToken::Rule(AclRule::NoCommands)),
        _ if lower == "allkeys" => Ok(AclToken::Rule(AclRule::AllKeys)),
        // `+@all`/`-@all` are the spec's own accepted spellings of AllCommands/NoCommands.
        // Every OTHER `+@category`/`-@category` token is rejected below -- this plan's Global
        // Constraint is explicit `+CMDNAME`/`-CMDNAME`/`allcommands`/`nocommands` only, no
        // general `@category` grants.
        "+@all" => Ok(AclToken::Rule(AclRule::AllCommands)),
        "-@all" => Ok(AclToken::Rule(AclRule::NoCommands)),
        _ if text.starts_with("+@") || text.starts_with("-@") => {
            Err(AclError::SyntaxError(text.to_string()))
        }
        // `~*` is the spec's literal spelling of AllKeys. Parsing it as AllKeys (rather than
        // falling into the generic KeyPattern arm below) keeps the single most common key rule
        // off `engine::glob::glob_match`'s non-tail-recursive `*` handling.
        "~*" => Ok(AclToken::Rule(AclRule::AllKeys)),
        _ if text.starts_with('>') && text.len() > 1 => {
            Ok(AclToken::Password(text[1..].to_string()))
        }
        _ if text.starts_with('+') && text.len() > 1 => Ok(AclToken::Rule(AclRule::AllowCommand(
            text[1..].to_ascii_uppercase(),
        ))),
        _ if text.starts_with('-') && text.len() > 1 => Ok(AclToken::Rule(AclRule::DenyCommand(
            text[1..].to_ascii_uppercase(),
        ))),
        _ if text.starts_with('~') && text.len() > 1 => {
            Ok(AclToken::Rule(AclRule::KeyPattern(text[1..].to_string())))
        }
        _ => Err(AclError::SyntaxError(text.to_string())),
    }
}

/// Hashes `password` with a fresh random salt using argon2's own recommended default
/// parameters -- no manual tuning this sprint, per the spec's own scope note. Takes `&[u8]`
/// rather than `&str` because RESP passwords are binary-safe and arrive off the wire as
/// arbitrary bytes, not necessarily valid UTF-8.
pub fn hash_password(password: &[u8]) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password, &salt)
        .expect("argon2 hashing with a freshly generated salt cannot fail")
        .to_string()
}

/// `false` for both a genuine mismatch and a malformed `hash` string -- a caller never needs to
/// distinguish "wrong password" from "corrupt stored hash", and both must fail closed. Takes
/// `&[u8]` for the same binary-safety reason as `hash_password`.
pub fn verify_password(password: &[u8], hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default().verify_password(password, &parsed).is_ok()
}

/// One configured ACL user: its login state, password hash (`None` for `nopass`), and the
/// ordered list of rules `is_allowed` folds to answer permission questions.
#[derive(Debug, Clone)]
pub struct AclUser {
    pub username: String,
    pub password_hash: Option<String>,
    pub enabled: bool,
    pub rules: Vec<AclRule>,
}

impl AclUser {
    /// Folds `self.rules` left-to-right: the last rule that matches a given question (command
    /// allowed? this key allowed?) wins, matching real Redis's own rule-order semantics. A
    /// command with no keys needs only the command check; a command with keys additionally
    /// needs every one of them to match at least one key rule.
    ///
    /// `command` must already be uppercased by the caller -- this is a plain string-equality
    /// check against `AclRule::AllowCommand`/`DenyCommand`, never case-insensitive.
    pub fn is_allowed(&self, command: &str, keys: &[&bytes::Bytes]) -> bool {
        let mut command_allowed = false;
        for rule in &self.rules {
            match rule {
                AclRule::AllCommands => command_allowed = true,
                AclRule::NoCommands => command_allowed = false,
                AclRule::AllowCommand(c) if c == command => command_allowed = true,
                AclRule::DenyCommand(c) if c == command => command_allowed = false,
                _ => {}
            }
        }
        if !command_allowed {
            return false;
        }
        keys.iter().all(|key| self.key_allowed(key))
    }

    fn key_allowed(&self, key: &[u8]) -> bool {
        let mut allowed = false;
        for rule in &self.rules {
            match rule {
                AclRule::AllKeys => allowed = true,
                AclRule::KeyPattern(p) if engine::glob::glob_match(p.as_bytes(), key) => {
                    allowed = true
                }
                _ => {}
            }
        }
        allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn user(rules: Vec<AclRule>) -> AclUser {
        AclUser {
            username: "test".to_string(),
            password_hash: None,
            enabled: true,
            rules,
        }
    }

    #[test]
    fn a_user_with_no_rules_is_allowed_nothing() {
        let u = user(vec![]);
        assert!(!u.is_allowed("GET", &[]));
    }

    #[test]
    fn allcommands_and_allkeys_permits_any_command_and_key() {
        let u = user(vec![AclRule::AllCommands, AclRule::AllKeys]);
        let k = Bytes::from_static(b"anything");
        assert!(u.is_allowed("GET", &[&k]));
        assert!(u.is_allowed("FLUSHALL", &[]));
    }

    #[test]
    fn the_spec_example_plus_get_minus_set_with_a_key_pattern() {
        // ["on", ">pw", "~app:*", "+get", "-set"] from the spec's own worked example
        let u = user(vec![
            AclRule::KeyPattern("app:*".to_string()),
            AclRule::AllowCommand("GET".to_string()),
            AclRule::DenyCommand("SET".to_string()),
        ]);
        let allowed_key = Bytes::from_static(b"app:1");
        let other_key = Bytes::from_static(b"other:1");
        assert!(u.is_allowed("GET", &[&allowed_key]));
        assert!(!u.is_allowed("SET", &[&allowed_key]), "explicitly denied");
        assert!(
            !u.is_allowed("GET", &[&other_key]),
            "key outside the pattern"
        );
        assert!(!u.is_allowed("DEL", &[&allowed_key]), "never granted");
    }

    #[test]
    fn a_later_rule_overrides_an_earlier_one_for_the_same_command() {
        let u = user(vec![
            AclRule::AllowCommand("GET".to_string()),
            AclRule::DenyCommand("GET".to_string()), // revokes it again
            AclRule::AllKeys,
        ]);
        assert!(!u.is_allowed("GET", &[]));
    }

    #[test]
    fn allcommands_then_a_later_deny_still_denies_that_one_command() {
        let u = user(vec![
            AclRule::AllCommands,
            AclRule::DenyCommand("FLUSHALL".to_string()),
            AclRule::AllKeys,
        ]);
        assert!(!u.is_allowed("FLUSHALL", &[]));
        assert!(u.is_allowed("GET", &[]));
    }

    #[test]
    fn a_keyless_command_needs_no_key_rule_at_all() {
        let u = user(vec![AclRule::AllowCommand("PING".to_string())]); // no AllKeys/KeyPattern rule at all
        assert!(u.is_allowed("PING", &[]));
    }

    #[test]
    fn a_command_with_keys_needs_every_key_to_match_some_key_rule() {
        let u = user(vec![
            AclRule::AllCommands,
            AclRule::KeyPattern("app:*".to_string()),
        ]);
        let ok = Bytes::from_static(b"app:1");
        let bad = Bytes::from_static(b"other:1");
        assert!(u.is_allowed("MGET", &[&ok, &ok]));
        assert!(
            !u.is_allowed("MGET", &[&ok, &bad]),
            "one key outside the pattern denies the whole command"
        );
    }

    #[test]
    fn nocommands_last_overrides_allcommands_and_an_explicit_allow() {
        let u = user(vec![
            AclRule::AllCommands,
            AclRule::AllowCommand("GET".to_string()),
            AclRule::NoCommands,
        ]);
        assert!(!u.is_allowed("GET", &[]));
        assert!(!u.is_allowed("SET", &[]));
    }

    #[test]
    fn nocommands_then_an_explicit_allow_permits_only_that_command() {
        let u = user(vec![
            AclRule::NoCommands,
            AclRule::AllowCommand("GET".to_string()),
        ]);
        assert!(u.is_allowed("GET", &[]));
        assert!(!u.is_allowed("SET", &[]));
    }

    #[test]
    fn parses_on_and_off() {
        assert_eq!(parse_token(b"on").unwrap(), AclToken::On);
        assert_eq!(parse_token(b"off").unwrap(), AclToken::Off);
    }

    #[test]
    fn keyword_tokens_are_case_insensitive() {
        assert_eq!(parse_token(b"ON").unwrap(), AclToken::On);
        assert_eq!(
            parse_token(b"AllKeys").unwrap(),
            AclToken::Rule(AclRule::AllKeys)
        );
    }

    #[test]
    fn parses_nopass() {
        assert_eq!(parse_token(b"nopass").unwrap(), AclToken::NoPass);
    }

    #[test]
    fn parses_a_password_token() {
        assert_eq!(
            parse_token(b">hunter2").unwrap(),
            AclToken::Password("hunter2".to_string())
        );
    }

    #[test]
    fn parses_allcommands_and_nocommands() {
        assert_eq!(
            parse_token(b"allcommands").unwrap(),
            AclToken::Rule(AclRule::AllCommands)
        );
        assert_eq!(
            parse_token(b"nocommands").unwrap(),
            AclToken::Rule(AclRule::NoCommands)
        );
    }

    #[test]
    fn parses_allow_and_deny_command_rules_uppercased() {
        assert_eq!(
            parse_token(b"+get").unwrap(),
            AclToken::Rule(AclRule::AllowCommand("GET".to_string()))
        );
        assert_eq!(
            parse_token(b"-Set").unwrap(),
            AclToken::Rule(AclRule::DenyCommand("SET".to_string()))
        );
    }

    #[test]
    fn parses_allkeys_and_a_key_pattern() {
        assert_eq!(
            parse_token(b"allkeys").unwrap(),
            AclToken::Rule(AclRule::AllKeys)
        );
        assert_eq!(
            parse_token(b"~app:*").unwrap(),
            AclToken::Rule(AclRule::KeyPattern("app:*".to_string()))
        );
    }

    #[test]
    fn tilde_star_normalizes_to_allkeys_not_a_key_pattern() {
        assert_eq!(
            parse_token(b"~*").unwrap(),
            AclToken::Rule(AclRule::AllKeys)
        );
    }

    #[test]
    fn plus_at_all_and_minus_at_all_parse_as_allcommands_and_nocommands() {
        assert_eq!(
            parse_token(b"+@all").unwrap(),
            AclToken::Rule(AclRule::AllCommands)
        );
        assert_eq!(
            parse_token(b"-@all").unwrap(),
            AclToken::Rule(AclRule::NoCommands)
        );
    }

    #[test]
    fn any_other_at_category_token_is_a_syntax_error() {
        assert!(parse_token(b"+@read").is_err());
        assert!(parse_token(b"-@admin").is_err());
        assert!(parse_token(b"-@write").is_err());
    }

    #[test]
    fn an_empty_command_or_pattern_token_is_a_syntax_error() {
        assert!(parse_token(b"+").is_err());
        assert!(parse_token(b"~").is_err());
    }

    #[test]
    fn a_bare_minus_or_gt_token_is_a_syntax_error() {
        assert!(parse_token(b"-").is_err());
        assert!(parse_token(b">").is_err());
    }

    #[test]
    fn non_utf8_input_is_a_syntax_error_not_a_panic() {
        assert!(parse_token(&[0xFF, 0xFE]).is_err());
    }

    #[test]
    fn an_unrecognized_token_is_a_syntax_error() {
        assert!(parse_token(b"@read").is_err()); // @categories are out of scope, see Global Constraints
        assert!(parse_token(b"+@read").is_err());
        assert!(parse_token(b"-@write").is_err());
        assert!(parse_token(b"garbage").is_err());
    }

    #[test]
    fn syntax_error_display_sanitizes_embedded_crlf() {
        let err = parse_token(b"garbage\r\nSET evil 1").unwrap_err();
        let rendered = err.to_string();
        assert!(!rendered.contains('\r'));
        assert!(!rendered.contains('\n'));
    }

    #[test]
    fn a_password_verifies_against_its_own_hash() {
        let hash = hash_password(b"hunter2");
        assert!(verify_password(b"hunter2", &hash));
    }

    #[test]
    fn the_wrong_password_does_not_verify() {
        let hash = hash_password(b"hunter2");
        assert!(!verify_password(b"wrong", &hash));
    }

    #[test]
    fn two_hashes_of_the_same_password_differ_by_salt() {
        let a = hash_password(b"hunter2");
        let b = hash_password(b"hunter2");
        assert_ne!(a, b, "each hash must use a fresh random salt");
        assert!(verify_password(b"hunter2", &a));
        assert!(verify_password(b"hunter2", &b));
    }

    #[test]
    fn verify_against_a_malformed_hash_string_returns_false_not_a_panic() {
        assert!(!verify_password(b"anything", "not-a-real-argon2-hash"));
    }
}
