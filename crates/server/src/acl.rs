//! ACL rule types, `ACL SETUSER`-style token parsing, and password hashing.
//!
//! This module holds the whole access-control-list system: rule parsing, password hashing, and
//! permission-check logic, all implemented here. Keeping them in one module mirrors how Redis
//! treats ACL as a single subsystem rather than splitting it across the places that consume it.
//! This module wires all of that into a live, store-backed `AclUser`/dispatcher path: `AclStore`
//! below is what `ReplicationHandle::acl` (see `replication.rs`) holds and what `main.rs` seeds
//! from the TOML config's `[[acl.users]]` bootstrap list.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

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

/// A fixed argon2 hash of a placeholder password, computed once (on first use, not at process
/// start) and reused by every `AclStore::authenticate` call that rejects on a path which would
/// otherwise skip argon2 entirely -- see `authenticate`'s own doc comment for why that matters.
static DUMMY_PASSWORD_HASH: OnceLock<String> = OnceLock::new();

fn dummy_password_hash() -> &'static str {
    DUMMY_PASSWORD_HASH
        .get_or_init(|| hash_password(b"dummy-password-for-constant-time-comparison"))
}

/// Rejects a username that would corrupt plan 08's future `ACL LIST` output -- one
/// space-separated line per user (`user <name> on nopass ~* +@all`), which a username
/// containing whitespace or CRLF would break -- or that otherwise makes no sense as an identity:
/// empty. Shared by both ways a username enters the store: `AclStore::set_user` (`ACL SETUSER`)
/// and `from_bootstrap_config` (the TOML `[[acl.users]]` list).
fn validate_username(username: &str) -> Result<(), AclError> {
    if username.is_empty()
        || username.contains(' ')
        || username.contains('\r')
        || username.contains('\n')
    {
        return Err(AclError::SyntaxError(username.to_string()));
    }
    Ok(())
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

/// In-memory ACL users, keyed by username. A plain `std::sync::RwLock`, matching `SlowLog`'s and
/// `ReplicaRegistry`'s existing choice in this codebase: every access here is a quick map
/// read/write, never held across an `.await`. Never persisted to the AOF or snapshot -- see
/// this plan's Global Constraints:
/// ../../../docs/superpowers/plans/2026-08-31-sprint-8-plans/04-acl-store-and-bootstrap-wiring.md
#[derive(Default)]
pub struct AclStore {
    users: RwLock<HashMap<String, Arc<AclUser>>>,
}

impl AclStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The fast-path check plan 06's auth gate uses to skip enforcement entirely.
    pub fn is_empty(&self) -> bool {
        self.users
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// Applies `raw_tokens` (parsed via `parse_token`) on top of `username`'s existing user, or a
    /// fresh `enabled: false, password_hash: None, rules: []` default if it doesn't exist yet --
    /// real-Redis-style incremental `ACL SETUSER`. Parses every token before applying any of
    /// them, so a malformed token in the middle of the list leaves the store unchanged rather
    /// than half-applying the earlier tokens.
    pub fn set_user(&self, username: &str, raw_tokens: &[bytes::Bytes]) -> Result<(), AclError> {
        validate_username(username)?;
        let tokens = raw_tokens
            .iter()
            .map(|t| parse_token(t))
            .collect::<Result<Vec<_>, _>>()?;
        // Pre-hash any password token now, still outside the write lock: argon2 hashing costs
        // ~20ms, and that cost must never run while holding the exclusive lock every reader and
        // writer of this store contends on. From here down, a `Password` token's payload is
        // already a hash, not plaintext -- see `apply_tokens`.
        let tokens: Vec<AclToken> = tokens
            .into_iter()
            .map(|t| match t {
                AclToken::Password(pw) => AclToken::Password(hash_password(pw.as_bytes())),
                other => other,
            })
            .collect();
        let mut users = self.users.write().unwrap_or_else(|e| e.into_inner());
        let base = users
            .get(username)
            .map(|u| (**u).clone())
            .unwrap_or_else(|| AclUser {
                username: username.to_string(),
                password_hash: None,
                enabled: false,
                rules: Vec::new(),
            });
        let updated = apply_tokens(base, &tokens);
        users.insert(username.to_string(), Arc::new(updated));
        Ok(())
    }

    pub fn del_user(&self, username: &str) -> bool {
        self.users
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(username)
            .is_some()
    }

    pub fn get_user(&self, username: &str) -> Option<Arc<AclUser>> {
        self.users
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(username)
            .cloned()
    }

    pub fn list(&self) -> Vec<Arc<AclUser>> {
        self.users
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// `None` for: unknown username, a disabled user, or a wrong password. A `nopass` user
    /// (`password_hash: None`) authenticates with any password, including an empty one.
    ///
    /// Clones the `Arc<AclUser>` out of the lock and drops it *before* calling
    /// `verify_password`: `std::sync::RwLock` on Linux does not starve writers, so a pending
    /// `ACL SETUSER` writer queues, and subsequent readers queue behind that writer. Holding the
    /// read guard across argon2 verification (~20ms) would mean one slow `AUTH` plus one
    /// concurrent `ACL SETUSER` stalls every other command's ACL check for that whole window.
    /// `password` is `&[u8]`, not `&str`: RESP passwords are binary-safe and arrive off the wire
    /// as arbitrary bytes, so narrowing to `&str` here would force a lossy UTF-8 conversion at
    /// the call site (`String::from_utf8_lossy` collapses distinct invalid byte sequences to the
    /// same string -- a narrow auth-bypass shape).
    ///
    /// Both the "no such user" and "user disabled" paths call `verify_password` against a fixed
    /// dummy hash before returning `None`, even though the result is discarded. Without this, an
    /// unknown or disabled username returns in well under a millisecond while a known, enabled
    /// user with the wrong password takes argon2's ~10-20ms -- a response-latency side channel
    /// that lets a caller enumerate valid usernames without ever guessing a password. Paying the
    /// same argon2 cost on every rejection path closes that timing oracle.
    pub fn authenticate(&self, username: &str, password: &[u8]) -> Option<Arc<AclUser>> {
        let user = {
            self.users
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(username)
                .cloned()
        };
        let Some(user) = user else {
            verify_password(password, dummy_password_hash());
            return None;
        };
        if !user.enabled {
            verify_password(password, dummy_password_hash());
            return None;
        }
        match &user.password_hash {
            None => Some(user),
            Some(hash) if verify_password(password, hash) => Some(user),
            Some(_) => None,
        }
    }

    /// Inserts an already-fully-formed `AclUser` directly, bypassing token parsing/incremental
    /// application -- used only by bootstrap loading (Task 3), which builds a complete `AclUser`
    /// from `AclUserConfig` in one step.
    pub fn insert_bootstrap(&self, user: AclUser) {
        self.users
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(user.username.clone(), Arc::new(user));
    }
}

/// Applies `tokens` on top of `base`. By the time a `Password` token reaches here, its payload
/// is already an argon2 hash, not plaintext -- `set_user` hashes it before acquiring the write
/// lock this runs under, so this function itself never pays argon2's cost while holding it.
fn apply_tokens(mut base: AclUser, tokens: &[AclToken]) -> AclUser {
    for token in tokens {
        match token {
            AclToken::On => base.enabled = true,
            AclToken::Off => base.enabled = false,
            AclToken::NoPass => base.password_hash = None,
            AclToken::Password(hash) => base.password_hash = Some(hash.clone()),
            AclToken::Rule(r) => base.rules.push(r.clone()),
        }
    }
    base
}

/// Converts one TOML `[[acl.users]]` entry into a fully-formed `AclUser`. `cfg.rules` must
/// contain only rule tokens (`+CMD`/`-CMD`/`~pattern`/`allcommands`/`nocommands`/`allkeys`) --
/// `enabled` and `password` are `AclUserConfig`'s own fields precisely so they don't also need
/// to appear as `on`/`off`/`>pw` tokens inside `rules`, and a `rules` entry that parses as one of
/// those (or fails to parse at all) is rejected rather than silently ignored.
pub fn from_bootstrap_config(cfg: &crate::config::AclUserConfig) -> Result<AclUser, AclError> {
    validate_username(&cfg.username)?;
    let mut rules = Vec::with_capacity(cfg.rules.len());
    for raw in &cfg.rules {
        match parse_token(raw.as_bytes())? {
            AclToken::Rule(r) => rules.push(r),
            AclToken::On | AclToken::Off | AclToken::NoPass => {
                return Err(AclError::SyntaxError(raw.clone()))
            }
            // Don't echo the token's content here: unlike on/off/nopass, this one carries a
            // secret (the raw text is literally `>plaintext-password`), and `AclError`'s
            // `Display` impl is what `main.rs` prints to stderr on a bootstrap failure --
            // journald, container logs, CI output. A fixed placeholder keeps the error useful
            // without leaking the password.
            AclToken::Password(_) => {
                return Err(AclError::SyntaxError("<password token>".to_string()))
            }
        }
    }
    Ok(AclUser {
        username: cfg.username.clone(),
        password_hash: cfg
            .password
            .as_deref()
            .map(|pw| hash_password(pw.as_bytes())),
        enabled: cfg.enabled,
        rules,
    })
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

    fn tokens(strs: &[&str]) -> Vec<Bytes> {
        strs.iter().map(|s| Bytes::from(s.to_string())).collect()
    }

    #[test]
    fn a_new_store_is_empty() {
        let store = AclStore::default();
        assert!(store.is_empty());
        assert!(store.list().is_empty());
    }

    #[test]
    fn set_user_creates_a_user_disabled_and_closed_by_default_then_applies_tokens() {
        let store = AclStore::default();
        store
            .set_user("app", &tokens(&["on", ">pw", "~app:*", "+get", "-set"]))
            .unwrap();
        let user = store.get_user("app").unwrap();
        assert!(user.enabled);
        assert!(!store.is_empty());
    }

    #[test]
    fn set_user_hashes_the_password_not_stores_it_in_plaintext() {
        let store = AclStore::default();
        store.set_user("app", &tokens(&["on", ">hunter2"])).unwrap();
        let user = store.get_user("app").unwrap();
        let hash = user.password_hash.as_deref().unwrap();
        assert_ne!(hash, "hunter2");
        assert!(verify_password(b"hunter2", hash));
    }

    #[test]
    fn set_user_is_incremental_not_replace_whole_user() {
        let store = AclStore::default();
        store.set_user("app", &tokens(&["on", "+get"])).unwrap();
        store.set_user("app", &tokens(&["+set"])).unwrap(); // adds to, doesn't reset, the existing rules
        let user = store.get_user("app").unwrap();
        // AllKeys was never granted, so this only proves both command grants survived, not key access.
        assert!(user
            .rules
            .contains(&AclRule::AllowCommand("GET".to_string())));
        assert!(user
            .rules
            .contains(&AclRule::AllowCommand("SET".to_string())));
    }

    #[test]
    fn set_user_with_a_malformed_token_returns_a_syntax_error_and_does_not_partially_apply() {
        let store = AclStore::default();
        let result = store.set_user("app", &tokens(&["on", "garbage-token"]));
        assert!(result.is_err());
        assert!(
            store.get_user("app").is_none(),
            "a failed SETUSER must not create a half-applied user"
        );
    }

    #[test]
    fn set_user_rejects_an_empty_or_whitespace_containing_username() {
        assert!(AclStore::default().set_user("", &tokens(&["on"])).is_err());
        assert!(AclStore::default()
            .set_user("bad name", &tokens(&["on"]))
            .is_err());
        assert!(AclStore::default()
            .set_user("bad\r\nname", &tokens(&["on"]))
            .is_err());
    }

    #[test]
    fn del_user_removes_an_existing_user_and_returns_false_for_an_unknown_one() {
        let store = AclStore::default();
        store.set_user("app", &tokens(&["on"])).unwrap();
        assert!(store.del_user("app"));
        assert!(store.get_user("app").is_none());
        assert!(!store.del_user("app"));
    }

    #[test]
    fn authenticate_succeeds_with_the_right_password_and_fails_with_the_wrong_one() {
        let store = AclStore::default();
        store.set_user("app", &tokens(&["on", ">hunter2"])).unwrap();
        assert!(store.authenticate("app", b"hunter2").is_some());
        assert!(store.authenticate("app", b"wrong").is_none());
    }

    #[test]
    fn authenticate_a_nopass_user_accepts_any_password() {
        let store = AclStore::default();
        store.set_user("app", &tokens(&["on", "nopass"])).unwrap();
        assert!(store.authenticate("app", b"literally-anything").is_some());
    }

    #[test]
    fn authenticate_a_disabled_user_always_fails() {
        let store = AclStore::default();
        store.set_user("app", &tokens(&["off", "nopass"])).unwrap();
        assert!(store.authenticate("app", b"anything").is_none());
    }

    #[test]
    fn authenticate_an_unknown_username_fails() {
        let store = AclStore::default();
        assert!(store.authenticate("nobody", b"anything").is_none());
    }

    #[test]
    fn authenticate_still_returns_none_on_the_paths_that_now_pay_the_dummy_argon2_cost() {
        // Timing-oracle fix: an unknown username or a disabled user now calls `verify_password`
        // against a fixed dummy hash before returning `None`, so both rejection paths take
        // comparable time to a wrong-password rejection below (which always ran argon2). This
        // test can't assert the timing itself -- real timing-equalization checks are flaky in a
        // unit test -- it only documents that the fix left the `Some`/`None` return shape
        // unchanged for both paths.
        let store = AclStore::default();
        assert!(store.authenticate("nobody", b"anything").is_none());

        store
            .set_user("app", &tokens(&["off", ">hunter2"]))
            .unwrap();
        assert!(store.authenticate("app", b"hunter2").is_none());
    }

    #[test]
    fn authenticate_accepts_a_non_utf8_password() {
        // Binary-safety regression (plan 03 / fix #5): `authenticate` takes `&[u8]`, not `&str`,
        // so a password containing invalid UTF-8 must still round-trip through hash/verify
        // without a lossy conversion collapsing it into some other input. `>token` parsing
        // itself requires UTF-8 (a separate, pre-existing constraint on `ACL SETUSER`/bootstrap
        // token text), so this builds the `AclUser` directly rather than through `set_user`.
        let store = AclStore::default();
        let raw_password: &[u8] = &[0xFF, 0xFE, b'x'];
        store.insert_bootstrap(AclUser {
            username: "app".to_string(),
            password_hash: Some(hash_password(raw_password)),
            enabled: true,
            rules: vec![],
        });
        assert!(store.authenticate("app", raw_password).is_some());
        assert!(store.authenticate("app", b"\xFF\xFEy").is_none());
    }

    #[test]
    fn list_returns_every_user() {
        let store = AclStore::default();
        store.set_user("a", &tokens(&["on"])).unwrap();
        store.set_user("b", &tokens(&["on"])).unwrap();
        let mut names: Vec<String> = store.list().iter().map(|u| u.username.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn insert_bootstrap_adds_an_already_formed_user_directly() {
        let store = AclStore::default();
        store.insert_bootstrap(AclUser {
            username: "seed".to_string(),
            password_hash: None,
            enabled: true,
            rules: vec![AclRule::AllCommands, AclRule::AllKeys],
        });
        assert!(!store.is_empty());
        assert!(store.get_user("seed").unwrap().enabled);
    }

    fn cfg(
        username: &str,
        password: Option<&str>,
        enabled: bool,
        rules: &[&str],
    ) -> crate::config::AclUserConfig {
        crate::config::AclUserConfig {
            username: username.to_string(),
            password: password.map(|p| p.to_string()),
            enabled,
            rules: rules.iter().map(|r| r.to_string()).collect(),
        }
    }

    #[test]
    fn from_bootstrap_config_builds_a_matching_acl_user() {
        let user =
            from_bootstrap_config(&cfg("app", Some("pw"), true, &["~app:*", "+get", "-set"]))
                .unwrap();
        assert_eq!(user.username, "app");
        assert!(user.enabled);
        assert!(user.password_hash.is_some());
        assert!(verify_password(
            b"pw",
            user.password_hash.as_deref().unwrap()
        ));
        assert_eq!(
            user.rules,
            vec![
                AclRule::KeyPattern("app:*".to_string()),
                AclRule::AllowCommand("GET".to_string()),
                AclRule::DenyCommand("SET".to_string()),
            ]
        );
    }

    #[test]
    fn from_bootstrap_config_with_no_password_is_nopass() {
        let user =
            from_bootstrap_config(&cfg("nopass-user", None, true, &["allcommands", "allkeys"]))
                .unwrap();
        assert_eq!(user.password_hash, None);
    }

    #[test]
    fn from_bootstrap_config_rejects_an_on_off_or_password_token_inside_rules() {
        // `enabled`/`password` are their own AclUserConfig fields; the `rules` list must contain
        // only rule tokens (+CMD/-CMD/~pattern/allcommands/.../allkeys), not "on"/"off"/">pw".
        assert!(from_bootstrap_config(&cfg("bad", None, true, &["on"])).is_err());
        assert!(from_bootstrap_config(&cfg("bad", None, true, &[">oops"])).is_err());
    }

    #[test]
    fn from_bootstrap_config_rejects_a_malformed_rule_token() {
        assert!(from_bootstrap_config(&cfg("bad", None, true, &["garbage"])).is_err());
    }

    #[test]
    fn from_bootstrap_config_rejects_an_empty_or_whitespace_containing_username() {
        assert!(from_bootstrap_config(&cfg("", None, true, &[])).is_err());
        assert!(from_bootstrap_config(&cfg("bad name", None, true, &[])).is_err());
    }

    #[test]
    fn from_bootstrap_config_does_not_leak_a_misplaced_password_into_the_error_message() {
        let err = from_bootstrap_config(&cfg("bad", None, true, &[">hunter2"])).unwrap_err();
        let rendered = err.to_string();
        assert!(
            !rendered.contains("hunter2"),
            "the error must not echo the rejected password's content, got: {rendered}"
        );
    }

    #[test]
    fn from_bootstrap_config_rejects_a_category_grant_rule() {
        // Plan 03's Global Constraint: only explicit +CMD/-CMD/allcommands/nocommands, never a
        // general +@category grant (+@all/-@all excepted, covered elsewhere).
        assert!(from_bootstrap_config(&cfg("app", None, true, &["+@read"])).is_err());
    }

    #[test]
    fn from_bootstrap_config_normalizes_tilde_star_to_allkeys() {
        let user = from_bootstrap_config(&cfg("app", None, true, &["~*"])).unwrap();
        assert_eq!(user.rules, vec![AclRule::AllKeys]);
    }

    /// End-to-end: TOML-shaped config -> `from_bootstrap_config` -> `AclStore::insert_bootstrap`
    /// -> `authenticate`, proving the whole chain works together, not just each piece alone.
    #[test]
    fn the_full_bootstrap_to_authenticate_chain_works_end_to_end() {
        let toml = r#"
            [[acl.users]]
            username = "app"
            password = "hunter2"
            enabled = true
            rules = ["~app:*", "+get", "-set"]
        "#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, toml).unwrap();
        let config = crate::config::load_layered(Some(&path)).unwrap();

        let store = AclStore::default();
        for cfg in &config.acl.users {
            let user = from_bootstrap_config(cfg).unwrap();
            store.insert_bootstrap(user);
        }

        let authenticated = store.authenticate("app", b"hunter2").unwrap();
        assert!(authenticated.is_allowed("GET", &[&Bytes::from_static(b"app:1")]));
        assert!(!authenticated.is_allowed("SET", &[&Bytes::from_static(b"app:1")]));
        assert!(store.authenticate("app", b"wrong").is_none());
    }
}
