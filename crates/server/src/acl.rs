//! ACL rule types and `ACL SETUSER`-style token parsing.
//!
//! This module holds the whole access-control-list system: rule parsing (here), password
//! hashing, and permission-check logic (both added by later tasks in this sprint). Keeping them
//! in one module mirrors how Redis treats ACL as a single subsystem rather than splitting it
//! across the places that consume it.

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
}
