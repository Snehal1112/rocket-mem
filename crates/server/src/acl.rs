//! ACL rule types, `ACL SETUSER`-style token parsing, and password hashing.
//!
//! This module holds the whole access-control-list system: rule parsing, password hashing
//! (here), and permission-check logic (added by a later task in this sprint). Keeping them in
//! one module mirrors how Redis treats ACL as a single subsystem rather than splitting it across
//! the places that consume it.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;

/// One permission grant or restriction parsed from an `ACL SETUSER` rule token. `on`/`off` and
/// password tokens live on `AclToken` instead -- they're user-level state, not permission rules,
/// so `AclUser::is_allowed` (added by a later task) never needs to match on them.
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
/// flattening its variants in here, since `AclUser::apply` (a later task) only ever needs the
/// permission-rule subset, not the enable/disable/password state alongside it.
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
            AclError::SyntaxError(tok) => write!(f, "ERR syntax error at '{tok}'"),
        }
    }
}

/// Parses one `ACL SETUSER`-style token. Command names in `+CMD`/`-CMD` are uppercased here so
/// every later comparison (`AclUser::is_allowed`) is a plain string-equality check, never a
/// case-insensitive one.
pub fn parse_token(raw: &[u8]) -> Result<AclToken, AclError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| AclError::SyntaxError(String::from_utf8_lossy(raw).into_owned()))?;
    match text {
        "on" => Ok(AclToken::On),
        "off" => Ok(AclToken::Off),
        "nopass" => Ok(AclToken::NoPass),
        "allcommands" => Ok(AclToken::Rule(AclRule::AllCommands)),
        "nocommands" => Ok(AclToken::Rule(AclRule::NoCommands)),
        "allkeys" => Ok(AclToken::Rule(AclRule::AllKeys)),
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
/// parameters -- no manual tuning this sprint, per the spec's own scope note.
pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing with a freshly generated salt cannot fail")
        .to_string()
}

/// `false` for both a genuine mismatch and a malformed `hash` string -- a caller never needs to
/// distinguish "wrong password" from "corrupt stored hash", and both must fail closed.
pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_on_and_off() {
        assert_eq!(parse_token(b"on").unwrap(), AclToken::On);
        assert_eq!(parse_token(b"off").unwrap(), AclToken::Off);
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
    fn an_empty_command_or_pattern_token_is_a_syntax_error() {
        assert!(parse_token(b"+").is_err());
        assert!(parse_token(b"~").is_err());
    }

    #[test]
    fn an_unrecognized_token_is_a_syntax_error() {
        assert!(parse_token(b"@read").is_err()); // @categories are out of scope, see Global Constraints
        assert!(parse_token(b"garbage").is_err());
    }

    #[test]
    fn a_password_verifies_against_its_own_hash() {
        let hash = hash_password("hunter2");
        assert!(verify_password("hunter2", &hash));
    }

    #[test]
    fn the_wrong_password_does_not_verify() {
        let hash = hash_password("hunter2");
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn two_hashes_of_the_same_password_differ_by_salt() {
        let a = hash_password("hunter2");
        let b = hash_password("hunter2");
        assert_ne!(a, b, "each hash must use a fresh random salt");
        assert!(verify_password("hunter2", &a));
        assert!(verify_password("hunter2", &b));
    }

    #[test]
    fn verify_against_a_malformed_hash_string_returns_false_not_a_panic() {
        assert!(!verify_password("anything", "not-a-real-argon2-hash"));
    }
}
