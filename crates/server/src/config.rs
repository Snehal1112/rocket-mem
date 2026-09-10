/// The server's full configuration, later loaded via `figment`'s TOML/env layering (see the
/// sprint 8 config plan). Every field name and type here is what later tasks and plans build
/// on: plan 04's `AclBootstrapConfig` reference and plan 10's TLS fields both assume this shape.
///
/// `#[serde(default)]` on the struct means a partial TOML file (or one missing entirely) still
/// deserializes -- any field it doesn't mention falls back to `Config::default()`'s value for it.
/// `Debug` is hand-written below, not derived -- this struct holds a plaintext leader password.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Config {
    pub addr: String,
    pub rmp_addr: String,
    pub metrics_addr: String,
    pub aof_path: String,
    pub snapshot_path: String,
    pub slowlog_threshold_micros: u64,
    pub cluster_config: Option<String>,
    pub cluster_node_id: Option<String>,
    pub tls_resp_addr: Option<String>,
    pub tls_rmp_addr: Option<String>,
    pub tls_cert_path: Option<String>,
    pub tls_key_path: Option<String>,
    /// The leader's certificate file, for a follower's replication connection to pin to via
    /// `tls::load_client_config`. Unset means plaintext replication, matching every deployment
    /// before this field existed.
    pub tls_ca_path: Option<String>,
    /// `host:port` of a leader this node should auto-connect to as a follower on every startup.
    /// Unset means standalone (or purely live-`REPLICAOF`-driven) operation, matching every
    /// deployment before this field existed. See `main.rs`'s startup wiring and
    /// `docs/superpowers/specs/2026-09-09-replicaof-config-file-spec.md`.
    pub replicaof: Option<String>,
    /// Username presented in `AUTH` before `PSYNC`, when `replicaof`'s leader has ACL users
    /// configured. Must be set together with `replicaof_auth_password`, or neither -- see
    /// `validate_replicaof`.
    pub replicaof_auth_username: Option<String>,
    /// Password presented in `AUTH` before `PSYNC`. Plaintext in the TOML file, same as
    /// `[[acl.users]]`'s own `password` field -- there is no encryption-at-rest for config
    /// secrets anywhere in this project today.
    pub replicaof_auth_password: Option<String>,
    /// The address this node advertises to its leader in its `PSYNC <addr>` frame, and which the
    /// leader then reports in `INFO REPLICATION`'s `slaveN:ip=...,port=...` lines. Unset means
    /// `addr` -- today's behaviour byte for byte, for every deployment that predates this field.
    ///
    /// Set it when the address a peer must dial differs from the address this node binds: a TLS
    /// deployment (announce `tls_resp_addr`, since `addr` is the plaintext port), or NAT and
    /// container port mapping (announce the externally reachable `host:port`). Shape-validated at
    /// startup by `validate_replica_announce_addr`; never checked for reachability, because this
    /// node cannot know how a peer routes to it. See
    /// `docs/superpowers/specs/2026-09-10-replica-announce-addr-spec.md`.
    pub replica_announce_addr: Option<String>,
    pub acl: AclBootstrapConfig,
    /// Log level filter, e.g. "info", "debug", "rocket_mem=debug,warn" -- same syntax as
    /// `RUST_LOG`. Overridden by the `RUST_LOG` env var when it's set (see
    /// `resolve_log_filter_directive` below); this field is the *default* for a deployment
    /// that doesn't set RUST_LOG, not a competing source of truth.
    pub log_level: String,
    /// Maximum bytes of a value or argument rendered into a `trace`-level log line before
    /// truncation. Only consulted at `trace` -- lower it to keep trace logs readable, raise
    /// it to see whole values. See `logging::fmt_value`.
    pub log_value_max_bytes: u64,
}

impl Default for Config {
    /// Matches today's hardcoded values in `main.rs` exactly -- this task only moves those
    /// values into one place, it doesn't change any of them.
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:6379".to_string(),
            rmp_addr: "127.0.0.1:6380".to_string(),
            metrics_addr: "127.0.0.1:9121".to_string(),
            aof_path: "./appendonly.aof".to_string(),
            snapshot_path: "./dump.snapshot".to_string(),
            slowlog_threshold_micros: 10_000,
            cluster_config: None,
            cluster_node_id: None,
            tls_resp_addr: None,
            tls_rmp_addr: None,
            tls_cert_path: None,
            tls_key_path: None,
            tls_ca_path: None,
            replicaof: None,
            replicaof_auth_username: None,
            replicaof_auth_password: None,
            replica_announce_addr: None,
            acl: AclBootstrapConfig::default(),
            log_level: "info".to_string(),
            log_value_max_bytes: 128,
        }
    }
}

impl std::fmt::Debug for Config {
    /// Hand-written rather than derived, because `replicaof_auth_password` is a plaintext leader
    /// credential (see its own doc comment) and a derived `Debug` would render it in full from any
    /// `?config` call site. The decision recorded in the verbose-logging spec used to be the
    /// opposite -- leave it derived -- on the reasoning that the only residue was ACL *usernames*
    /// and TLS *paths*, which are not key material. That reasoning stopped holding the moment
    /// `replicaof_auth_password` landed.
    ///
    /// **The destructuring is the point, not decoration.** `let Config { .. } = self` with no
    /// `..` rest pattern is exhaustive: adding a field to `Config` makes this function fail to
    /// compile with "pattern does not mention field `x`", forcing whoever adds it to decide here
    /// whether it renders or is redacted. That compile-time check is exactly what the earlier
    /// decision assumed a hand-written impl could not have -- "no compile-time check for a field
    /// someone forgets to add, so it would silently start omitting new configuration while looking
    /// exhaustive". It is available, and it also covers the failure mode that decision did not
    /// consider: a future field that is itself a secret. Never add a `..` to the pattern below.
    ///
    /// What still renders, deliberately: the ACL usernames (via `AclUserConfig`'s own redacting
    /// `Debug`), `replicaof_auth_username`, and the TLS cert/key/CA *paths*. A filename is not key
    /// material, and hiding it would only make a TLS misconfiguration harder to diagnose. That is
    /// the residue, and it is the same residue the spec has always named -- minus the password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Config {
            addr,
            rmp_addr,
            metrics_addr,
            aof_path,
            snapshot_path,
            slowlog_threshold_micros,
            cluster_config,
            cluster_node_id,
            tls_resp_addr,
            tls_rmp_addr,
            tls_cert_path,
            tls_key_path,
            tls_ca_path,
            replicaof,
            replicaof_auth_username,
            replicaof_auth_password,
            replica_announce_addr,
            acl,
            log_level,
            log_value_max_bytes,
        } = self;

        // `Option<&str>`, not a bare marker string, so the field keeps its `Some`/`None` shape:
        // "a leader password is configured" is a routine, non-secret operational fact, and
        // collapsing it with "none configured" would only make an auth misconfiguration harder to
        // spot -- the same reasoning `AclUserConfig`'s `<nopass>` rests on.
        let replicaof_auth_password = replicaof_auth_password.as_ref().map(|_| "<redacted>");

        f.debug_struct("Config")
            .field("addr", addr)
            .field("rmp_addr", rmp_addr)
            .field("metrics_addr", metrics_addr)
            .field("aof_path", aof_path)
            .field("snapshot_path", snapshot_path)
            .field("slowlog_threshold_micros", slowlog_threshold_micros)
            .field("cluster_config", cluster_config)
            .field("cluster_node_id", cluster_node_id)
            .field("tls_resp_addr", tls_resp_addr)
            .field("tls_rmp_addr", tls_rmp_addr)
            .field("tls_cert_path", tls_cert_path)
            .field("tls_key_path", tls_key_path)
            .field("tls_ca_path", tls_ca_path)
            .field("replicaof", replicaof)
            .field("replicaof_auth_username", replicaof_auth_username)
            .field("replicaof_auth_password", &replicaof_auth_password)
            .field("replica_announce_addr", replica_announce_addr)
            .field("acl", acl)
            .field("log_level", log_level)
            .field("log_value_max_bytes", log_value_max_bytes)
            .finish()
    }
}

/// ACL bootstrap users, read from the TOML config's `[[acl.users]]` array. Converted into real
/// `acl::AclUser`s by `ReplicationHandle::with_acl_bootstrap` — see
/// docs/superpowers/plans/2026-08-31-sprint-8-plans/04-acl-store-and-bootstrap-wiring.md.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct AclBootstrapConfig {
    pub users: Vec<AclUserConfig>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct AclUserConfig {
    pub username: String,
    /// Plaintext in the TOML file, hashed once at load time by plan 04's bootstrap conversion.
    /// `None` means `nopass` — the user authenticates with any password or none at all.
    pub password: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Raw rule tokens, parsed the same way `ACL SETUSER`'s tokens are (plan 03).
    #[serde(default)]
    pub rules: Vec<String>,
}

impl std::fmt::Debug for AclUserConfig {
    /// Hand-written rather than derived, for the same reason `acl::AclUser`'s `Debug` is: a
    /// derived one would route around `crate::logging`'s redaction policy from any
    /// `?`-formatted call site. This struct is the stronger case of the two -- `AclUser` holds
    /// only a password *hash*, while this one holds the operator's plaintext password straight
    /// out of the TOML file, and it is what `Config`'s own derived `Debug` reaches through.
    ///
    /// `None` still renders as `"<nopass>"` rather than the same marker as `Some(_)`, exactly as
    /// in `acl::AclUser`: knowing a user has no password at all is a routine ACL fact, not a
    /// secret, and hiding it would only make debugging auth issues harder.
    ///
    /// `rules` renders as a count, not verbatim -- unlike `AclUser::rules`, these are *raw*
    /// `ACL SETUSER` tokens, and a `>password` token is a plaintext credential. The count keeps
    /// the field's one operational use (did this user's rules load at all?) without rendering
    /// any token's contents. `username` and `enabled` are non-secret and stay readable, matching
    /// `acl::AclUser`; a deployment treating usernames as sensitive would have to redact both
    /// impls together.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let password: &dyn std::fmt::Debug = match &self.password {
            Some(_) => &"<redacted>",
            None => &"<nopass>",
        };
        f.debug_struct("AclUserConfig")
            .field("username", &self.username)
            .field("password", password)
            .field("enabled", &self.enabled)
            .field("rules", &self.rules.len())
            .finish()
    }
}

fn default_true() -> bool {
    true
}

/// Merges, in order (later wins): built-in defaults, an optional TOML file, then
/// `ROCKET_MEM_*` env vars. CLI-flag overrides are a further layer plan 02's `load()` applies
/// on top of this function's result -- kept separate so this layer stays testable without
/// needing to construct a `clap::Parser` in every test above.
// `figment::Error` is the standard error type for this ecosystem and is what plan 02's
// `load()` expects to propagate; boxing it here would just move the problem there.
#[allow(clippy::result_large_err)]
pub fn load_layered(toml_path: Option<&std::path::Path>) -> Result<Config, figment::Error> {
    use figment::providers::{Env, Format, Serialized, Toml};
    use figment::Figment;

    let mut figment = Figment::from(Serialized::defaults(Config::default()));
    if let Some(path) = toml_path {
        // Guard the merge on the resolved path actually existing, rather than letting
        // figment's own `Toml::file()` upward search silently pick up a same-named file
        // from a parent directory -- a missing `--config` path should fall back to
        // defaults, not load an unrelated file found higher up the tree.
        if path.exists() {
            figment = figment.merge(Toml::file(path));
        }
    }
    figment = figment.merge(Env::prefixed("ROCKET_MEM_"));
    figment.extract()
}

// Every field is `Option` -- an unset flag must not override a lower layer's value (see
// `cli_overrides`), so a required/defaulted field here would break that precedence chain.
//
// Adding a new field to `Config` also requires adding it here and to `cli_overrides`'s `set!`
// calls -- there's no compile-time check that catches a forgotten one.
/// A RESP-compatible in-memory data store.
//
// **No `Debug`, deliberately.** `--replicaof-auth-password` puts a plaintext leader credential in
// this struct, *earlier* than `Config` sees it, so a derived `Debug` here is the same hazard
// `Config`'s hand-written impl above exists to close -- and closing only `Config`'s half would
// leave the CLI layer leaking. Nothing in the workspace formats a `Cli`, and nothing should: it is
// a transient parse artifact whose every value ends up in `Config`, which *is* debuggable. Not
// having the impl at all is the stronger guard, because a `?cli` does not compile rather than
// merely being discouraged. `clap::Parser` does not require `Debug`; if a future need for one is
// real, hand-write it exhaustively the way `Config`'s is, never derive it.
#[derive(clap::Parser)]
#[command(name = "rocket-mem", version)]
pub struct Cli {
    /// Path to a TOML config file. Not read via env/CLI layering itself -- it names which file
    /// `load_layered` merges, so it's resolved before any other layer applies.
    /// [default: "rocket-mem.toml" if present, else skipped]
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
    /// TCP address for RESP clients [default: 127.0.0.1:6379]
    #[arg(long)]
    pub addr: Option<String>,
    /// TCP address for RMP (rocket-mem's custom protocol) clients [default: 127.0.0.1:6380]
    #[arg(long)]
    pub rmp_addr: Option<String>,
    /// TCP address the Prometheus metrics endpoint listens on [default: 127.0.0.1:9121]
    #[arg(long)]
    pub metrics_addr: Option<String>,
    /// Path to the append-only file [default: ./appendonly.aof]
    #[arg(long)]
    pub aof_path: Option<String>,
    /// Path to the point-in-time snapshot file [default: ./dump.snapshot]
    #[arg(long)]
    pub snapshot_path: Option<String>,
    /// Minimum command duration, in microseconds, logged to the slow log; 0 disables it [default: 10000]
    #[arg(long)]
    pub slowlog_threshold_micros: Option<u64>,
    /// Path to the cluster topology file; requires --cluster-node-id [default: unset, standalone mode]
    #[arg(long)]
    pub cluster_config: Option<String>,
    /// This node's id within --cluster-config's topology; requires --cluster-config [default: unset]
    #[arg(long)]
    pub cluster_node_id: Option<String>,
    /// TCP address for TLS-wrapped RESP clients [default: unset, TLS disabled]
    #[arg(long)]
    pub tls_resp_addr: Option<String>,
    /// TCP address for TLS-wrapped RMP clients [default: unset, TLS disabled]
    #[arg(long)]
    pub tls_rmp_addr: Option<String>,
    /// Path to the TLS certificate file [default: unset]
    #[arg(long)]
    pub tls_cert_path: Option<String>,
    /// Path to the TLS private key file [default: unset]
    #[arg(long)]
    pub tls_key_path: Option<String>,
    /// Path to the leader's certificate file, for a follower to pin its replication connection
    /// to over TLS [default: unset, replication stays plaintext]
    #[arg(long)]
    pub tls_ca_path: Option<String>,
    /// `host:port` of a leader to auto-connect to as a follower on startup [default: unset]
    #[arg(long)]
    pub replicaof: Option<String>,
    /// Username for the AUTH clause sent before PSYNC to --replicaof's leader [default: unset]
    #[arg(long)]
    pub replicaof_auth_username: Option<String>,
    /// Password for the AUTH clause sent before PSYNC to --replicaof's leader [default: unset]
    #[arg(long)]
    pub replicaof_auth_password: Option<String>,
    /// `host:port` this node advertises to its leader in PSYNC, when the address a peer must dial
    /// differs from --addr [default: unset, announces --addr]
    #[arg(long)]
    pub replica_announce_addr: Option<String>,
    /// Log level filter, e.g. "info", "debug", "rocket_mem=debug,warn" [default: info]
    #[arg(long)]
    pub log_level: Option<String>,
    /// Max bytes of a value rendered into a trace-level log line [default: 128]
    #[arg(long)]
    pub log_value_max_bytes: Option<u64>,
}

/// `Serialized::defaults` embeds every field including the unset `None`s, which would make an
/// unset CLI flag overwrite a lower layer's real value with `null` on merge -- exactly what
/// `cli_flags_left_unset_do_not_override_lower_layers` above guards against. Building a
/// `BTreeMap` by hand and only inserting `Some(_)` fields is what avoids that: an unset flag is
/// simply absent from the merged provider, so figment's merge leaves the lower layer's value
/// untouched.
///
/// The map's value type is `figment::value::Value`, not `String`. Figment only coerces a bare
/// string into a number for providers that do their own string parsing (like `Env`); a plain
/// serialized `String` value would deserialize as `Value::String` and fail extraction into a
/// `u64` field with `invalid type: found string, expected u64`. Using `Value::from(v)` for
/// `slowlog_threshold_micros` preserves its real numeric type through serialization instead.
fn cli_overrides(
    cli: &Cli,
) -> figment::providers::Serialized<std::collections::BTreeMap<&'static str, figment::value::Value>>
{
    use figment::providers::Serialized;
    use figment::value::Value;

    let mut map = std::collections::BTreeMap::new();
    macro_rules! set {
        ($field:ident) => {
            if let Some(v) = &cli.$field {
                map.insert(stringify!($field), Value::from(v.as_str()));
            }
        };
    }
    set!(addr);
    set!(rmp_addr);
    set!(metrics_addr);
    set!(aof_path);
    set!(snapshot_path);
    set!(cluster_config);
    set!(cluster_node_id);
    set!(tls_resp_addr);
    set!(tls_rmp_addr);
    set!(tls_cert_path);
    set!(tls_key_path);
    set!(tls_ca_path);
    set!(replicaof);
    set!(replicaof_auth_username);
    set!(replicaof_auth_password);
    set!(replica_announce_addr);
    set!(log_level);
    if let Some(v) = cli.slowlog_threshold_micros {
        map.insert("slowlog_threshold_micros", Value::from(v));
    }
    if let Some(v) = cli.log_value_max_bytes {
        map.insert("log_value_max_bytes", Value::from(v));
    }
    Serialized::defaults(map)
}

/// Merges `load_layered`'s result (defaults < TOML file < env vars) with CLI-flag overrides on
/// top, giving the full four-layer precedence: defaults < TOML file < `ROCKET_MEM_*` env vars <
/// CLI flags.
#[allow(clippy::result_large_err)]
pub fn load_with_cli(cli: Cli) -> Result<Config, figment::Error> {
    use figment::providers::Serialized;
    use figment::Figment;

    let base = load_layered(Some(
        cli.config
            .as_deref()
            .unwrap_or(std::path::Path::new("rocket-mem.toml")),
    ))?;
    Figment::from(Serialized::defaults(base))
        .merge(cli_overrides(&cli))
        .extract()
}

/// Parses `std::env::args()` and applies the full four-layer precedence: defaults < TOML file <
/// `ROCKET_MEM_*` env vars < CLI flags. This is what `main.rs` calls.
#[allow(clippy::result_large_err)]
pub fn load() -> Result<Config, figment::Error> {
    use clap::Parser;
    load_with_cli(Cli::parse())
}

/// Enforces the spec's "Required if either `tls_*_addr` is set" rule for `tls_cert_path` and
/// `tls_key_path` (see `docs/superpowers/specs/2026-08-31-sprint-8-spec.md`): an operator who
/// sets `tls_resp_addr`/`tls_rmp_addr` but forgets one or both of the cert/key paths must fail
/// startup loudly, not silently start with that TLS listener simply never bound. `main.rs` calls
/// this before wiring up either TLS listener.
pub fn validate_tls(config: &Config) -> Result<(), std::io::Error> {
    let have_cert_and_key = config.tls_cert_path.is_some() && config.tls_key_path.is_some();
    if config.tls_resp_addr.is_some() && !have_cert_and_key {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "tls_resp_addr is set but tls_cert_path/tls_key_path is not -- TLS requires both",
        ));
    }
    if config.tls_rmp_addr.is_some() && !have_cert_and_key {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "tls_rmp_addr is set but tls_cert_path/tls_key_path is not -- TLS requires both",
        ));
    }
    Ok(())
}

/// Enforces "replicaof_auth_username and replicaof_auth_password must both be set, or neither" --
/// see `docs/superpowers/specs/2026-09-09-replicaof-config-file-spec.md`. Does NOT validate
/// `replicaof` itself (a missing port, unresolvable host, etc.): that is only discoverable by
/// actually attempting the connection, exactly like the existing live `REPLICAOF` command already
/// behaves, so a bad `replicaof` value fails soft (the background reconnect loop retries forever)
/// rather than blocking startup. `main.rs` calls this before wiring up replication.
pub fn validate_replicaof(config: &Config) -> Result<(), std::io::Error> {
    let has_username = config.replicaof_auth_username.is_some();
    let has_password = config.replicaof_auth_password.is_some();
    if has_username != has_password {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "replicaof_auth_username and replicaof_auth_password must both be set, or neither",
        ));
    }
    Ok(())
}

/// Derives the AUTH tuple `start_replicating_with_auth` needs from a `Config`'s
/// `replicaof_auth_username`/`replicaof_auth_password` fields -- `None` unless both are set.
/// `validate_replicaof` guarantees these two fields are never partially set by the time startup
/// reaches this, but this stays a plain match (not an `unwrap`) so it's correct regardless of
/// call order. `ReplicationHandle::start_replicating_from_config` is the only production caller;
/// pulled out as its own function so it -- and the `Config` -> auth-tuple mapping it does -- can
/// be exercised directly from a test without hand-building `start_replicating_with_auth`'s args.
pub fn replicaof_auth(config: &Config) -> Option<(String, String)> {
    match (
        &config.replicaof_auth_username,
        &config.replicaof_auth_password,
    ) {
        (Some(u), Some(p)) => Some((u.clone(), p.clone())),
        _ => None,
    }
}

/// Resolves the log filter directive: `RUST_LOG`, when set, wins over `log_level` -- the
/// standard `tracing` convention of letting an operator's env var override any code- or
/// config-file-supplied default. Returns a plain `String` (not an `EnvFilter`) so this stays
/// unit-testable without constructing a filter or a subscriber. `main.rs` passes the result
/// straight to `tracing_subscriber::EnvFilter::new`.
pub fn resolve_log_filter_directive(log_level: &str) -> String {
    std::env::var("RUST_LOG").unwrap_or_else(|_| log_level.to_string())
}

#[cfg(test)]
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn default_config_matches_todays_hardcoded_main_rs_values() {
        let cfg = Config::default();
        assert_eq!(cfg.addr, "127.0.0.1:6379");
        assert_eq!(cfg.rmp_addr, "127.0.0.1:6380");
        assert_eq!(cfg.metrics_addr, "127.0.0.1:9121");
        assert_eq!(cfg.aof_path, "./appendonly.aof");
        assert_eq!(cfg.snapshot_path, "./dump.snapshot");
        assert_eq!(cfg.slowlog_threshold_micros, 10_000);
        assert_eq!(cfg.cluster_config, None);
        assert_eq!(cfg.cluster_node_id, None);
        assert_eq!(cfg.tls_resp_addr, None);
        assert_eq!(cfg.tls_rmp_addr, None);
        assert_eq!(cfg.tls_cert_path, None);
        assert_eq!(cfg.tls_key_path, None);
        assert_eq!(cfg.tls_ca_path, None);
        assert_eq!(cfg.replica_announce_addr, None);
        assert_eq!(cfg.log_level, "info");
        assert!(cfg.acl.users.is_empty());
    }

    #[test]
    fn load_layered_with_no_file_and_no_env_returns_defaults() {
        figment::Jail::expect_with(|_jail| {
            let cfg = load_layered(None).unwrap();
            assert_eq!(cfg.addr, "127.0.0.1:6379");
            Ok(())
        });
    }

    #[test]
    fn load_layered_reads_the_existing_rocket_mem_env_var_names() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("ROCKET_MEM_ADDR", "0.0.0.0:9999");
            jail.set_env("ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS", "5000");
            let cfg = load_layered(None).unwrap();
            assert_eq!(cfg.addr, "0.0.0.0:9999");
            assert_eq!(cfg.slowlog_threshold_micros, 5000);
            Ok(())
        });
    }

    #[test]
    fn load_layered_applies_a_toml_file_under_the_env_layer() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "rocket-mem.toml",
                "addr = \"127.0.0.1:1111\"\nrmp_addr = \"127.0.0.1:2222\"\nslowlog_threshold_micros = 7000\n",
            )?;
            jail.set_env("ROCKET_MEM_ADDR", "127.0.0.1:3333"); // env must win over the file
            jail.set_env("ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS", "9000"); // same, for a numeric field
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.addr, "127.0.0.1:3333", "env overrides file");
            assert_eq!(cfg.rmp_addr, "127.0.0.1:2222", "file overrides default");
            assert_eq!(
                cfg.slowlog_threshold_micros, 9000,
                "numeric field: env overrides file"
            );
            Ok(())
        });
    }

    #[test]
    fn load_layered_with_a_missing_toml_path_is_not_an_error() {
        figment::Jail::expect_with(|_jail| {
            let cfg = load_layered(Some(std::path::Path::new("does-not-exist.toml"))).unwrap();
            assert_eq!(cfg.addr, "127.0.0.1:6379"); // fell back to defaults, no error
            Ok(())
        });
    }

    #[test]
    fn load_layered_parses_acl_users_and_defaults_missing_rules_to_empty() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "rocket-mem.toml",
                r#"
                [[acl.users]]
                username = "admin"
                password = "hunter2"
                enabled = true
                rules = ["allcommands", "allkeys"]

                [[acl.users]]
                username = "readonly"
                "#,
            )?;
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.acl.users.len(), 2);

            let admin = &cfg.acl.users[0];
            assert_eq!(admin.username, "admin");
            assert_eq!(admin.password.as_deref(), Some("hunter2"));
            assert!(admin.enabled);
            assert_eq!(admin.rules, vec!["allcommands", "allkeys"]);

            let readonly = &cfg.acl.users[1];
            assert_eq!(readonly.username, "readonly");
            assert_eq!(readonly.password, None, "no password means nopass");
            assert!(readonly.enabled, "enabled defaults to true when omitted");
            assert!(
                readonly.rules.is_empty(),
                "a rules-less user must load with an empty Vec, not fail"
            );
            Ok(())
        });
    }

    /// Makes the leak structurally impossible rather than merely forbidden at the one call site
    /// that formats a `Config` today. `startup_logging.rs`'s
    /// `secret_bearing_config_is_never_rendered_into_the_summary` guards that call site; this
    /// guards every present and future one, since `Config`'s derived `Debug` reaches the
    /// plaintext password only through this struct.
    #[test]
    fn acl_user_config_debug_redacts_the_password_and_the_rule_tokens() {
        let user = AclUserConfig {
            username: "admin".to_string(),
            password: Some("zzsecret".to_string()),
            enabled: true,
            rules: vec!["allcommands".to_string(), ">zzrulepassword".to_string()],
        };
        let rendered = format!("{user:?}");

        assert!(
            !rendered.contains("zzsecret"),
            "the plaintext password must never render, got: {rendered}"
        );
        assert!(
            !rendered.contains("zzrulepassword") && !rendered.contains("allcommands"),
            "raw rule tokens can carry a >password, so none may render, got: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "got: {rendered}");
        assert!(
            rendered.contains("rules: 2"),
            "the rule count is the non-secret part worth keeping, got: {rendered}"
        );
        // Non-secret fields stay readable, matching `acl::AclUser`'s precedent.
        assert!(rendered.contains("admin") && rendered.contains("enabled: true"));
    }

    /// `None` is `nopass`, an operationally useful and non-secret fact, so it must stay
    /// distinguishable from a redacted real password -- same rule as `acl::AclUser`'s `Debug`.
    #[test]
    fn acl_user_config_debug_shows_nopass_distinguishably_from_a_redacted_password() {
        let user = AclUserConfig {
            username: "readonly".to_string(),
            password: None,
            enabled: true,
            rules: Vec::new(),
        };
        let rendered = format!("{user:?}");
        assert!(rendered.contains("<nopass>"), "got: {rendered}");
        assert!(!rendered.contains("<redacted>"), "got: {rendered}");
    }

    /// The reason the impl above matters: `Config` derives `Debug`, so anything formatting a
    /// whole config with `{:?}` reaches `acl.users` transitively.
    #[test]
    fn config_debug_does_not_leak_an_acl_password_through_the_nested_users() {
        let cfg = Config {
            acl: AclBootstrapConfig {
                users: vec![AclUserConfig {
                    username: "admin".to_string(),
                    password: Some("zzsecret".to_string()),
                    enabled: true,
                    rules: vec![">zzrulepassword".to_string()],
                }],
            },
            ..Config::default()
        };
        let rendered = format!("{cfg:?}");
        assert!(
            !rendered.contains("zzsecret") && !rendered.contains("zzrulepassword"),
            "got: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "got: {rendered}");
    }

    /// The hazard `Config`'s hand-written `Debug` closes: `replicaof_auth_password` is a plaintext
    /// leader credential, and until this impl existed the derived `Debug` rendered it in full.
    ///
    /// Mutation-checked, per the spec's "Redaction tests must be mutation-checked" rule: the
    /// fixture below genuinely carries `zzleaderpassword`, and replacing the `map(|_| ...)` in the
    /// impl with `.map(|p| p.as_str())` makes this test -- and only this test plus
    /// `config_debug_still_renders_every_non_secret_field` -- fail on the first assertion.
    #[test]
    fn config_debug_redacts_the_replicaof_auth_password() {
        let cfg = Config {
            replicaof: Some("127.0.0.1:6400".to_string()),
            replicaof_auth_username: Some("app".to_string()),
            replicaof_auth_password: Some("zzleaderpassword".to_string()),
            ..Config::default()
        };
        let rendered = format!("{cfg:?}");

        assert!(
            !rendered.contains("zzleaderpassword"),
            "the plaintext leader password must never render, got: {rendered}"
        );
        assert!(
            rendered.contains("replicaof_auth_password: Some(\"<redacted>\")"),
            "got: {rendered}"
        );
        // "a password is configured" is a non-secret operational fact and must stay
        // distinguishable from "none configured", so the `Option` shape survives redaction.
        let none = format!("{:?}", Config::default());
        assert!(
            none.contains("replicaof_auth_password: None"),
            "got: {none}"
        );
    }

    /// The other half of a hand-written `Debug`'s risk: silently *dropping* a field while looking
    /// exhaustive. The destructuring in the impl makes a forgotten field a compile error, but
    /// nothing stops a field being destructured and then not passed to `debug_struct`, so this
    /// pins the rendered output too. It also fixes the residue the spec records as still visible.
    #[test]
    fn config_debug_still_renders_every_non_secret_field() {
        let cfg = Config {
            addr: "1.1.1.1:1".to_string(),
            rmp_addr: "1.1.1.1:2".to_string(),
            metrics_addr: "1.1.1.1:3".to_string(),
            aof_path: "/zz/a.aof".to_string(),
            snapshot_path: "/zz/s.snap".to_string(),
            slowlog_threshold_micros: 4321,
            cluster_config: Some("/zz/cluster.conf".to_string()),
            cluster_node_id: Some("zznode".to_string()),
            tls_resp_addr: Some("1.1.1.1:4".to_string()),
            tls_rmp_addr: Some("1.1.1.1:5".to_string()),
            tls_cert_path: Some("/zz/cert.pem".to_string()),
            tls_key_path: Some("/zz/key.pem".to_string()),
            tls_ca_path: Some("/zz/ca.pem".to_string()),
            replicaof: Some("1.1.1.1:6".to_string()),
            replicaof_auth_username: Some("zzuser".to_string()),
            replicaof_auth_password: Some("zzleaderpassword".to_string()),
            replica_announce_addr: Some("1.1.1.1:7".to_string()),
            acl: AclBootstrapConfig::default(),
            log_level: "zzlevel".to_string(),
            log_value_max_bytes: 4322,
        };
        let rendered = format!("{cfg:?}");

        for expected in [
            "1.1.1.1:1",
            "1.1.1.1:2",
            "1.1.1.1:3",
            "/zz/a.aof",
            "/zz/s.snap",
            "4321",
            "/zz/cluster.conf",
            "zznode",
            "1.1.1.1:4",
            "1.1.1.1:5",
            // The TLS paths and the replication username are the residue this impl deliberately
            // keeps: a filename is not key material, and hiding it makes a TLS or auth
            // misconfiguration harder to diagnose, not safer.
            "/zz/cert.pem",
            "/zz/key.pem",
            "/zz/ca.pem",
            "1.1.1.1:6",
            "zzuser",
            "1.1.1.1:7",
            "acl",
            "zzlevel",
            "4322",
        ] {
            assert!(
                rendered.contains(expected),
                "a hand-written Debug must not silently drop {expected:?}, got: {rendered}"
            );
        }
        assert!(!rendered.contains("zzleaderpassword"), "got: {rendered}");
    }

    #[test]
    fn cli_flag_overrides_env_var_overrides_file_overrides_default() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "rocket-mem.toml",
                "addr = \"127.0.0.1:1111\"\nslowlog_threshold_micros = 2000\nrmp_addr = \"127.0.0.1:1234\"\n",
            )?;
            jail.set_env("ROCKET_MEM_ADDR", "127.0.0.1:2222"); // beats the file
            jail.set_env("ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS", "3000"); // beats the file, not overridden by CLI below

            let cli = Cli::parse_from([
                "rocket-mem",
                "--config",
                "rocket-mem.toml",
                "--addr",
                "127.0.0.1:4444", // beats the env var
            ]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(cfg.addr, "127.0.0.1:4444", "CLI beats env");
            assert_eq!(
                cfg.slowlog_threshold_micros, 3000,
                "env beats file when CLI doesn't set it"
            );
            assert_eq!(
                cfg.rmp_addr, "127.0.0.1:1234",
                "file-only value (no CLI flag, no env var) still reaches the final Config"
            );
            Ok(())
        });
    }

    #[test]
    fn cli_flag_overrides_an_optional_string_field() {
        figment::Jail::expect_with(|_jail| {
            let cli = Cli::parse_from(["rocket-mem", "--tls-cert-path", "/x"]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(
                cfg.tls_cert_path.as_deref(),
                Some("/x"),
                "CLI flag must be able to override an Option<String> field"
            );
            Ok(())
        });
    }

    #[test]
    fn default_config_has_no_replicaof_target() {
        let cfg = Config::default();
        assert_eq!(cfg.replicaof, None);
        assert_eq!(cfg.replicaof_auth_username, None);
        assert_eq!(cfg.replicaof_auth_password, None);
    }

    #[test]
    fn replicaof_is_layered_like_every_other_optional_string_field() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "rocket-mem.toml",
                "replicaof = \"127.0.0.1:6400\"\nreplicaof_auth_username = \"app\"\nreplicaof_auth_password = \"changeme\"\n",
            )?;
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.replicaof.as_deref(), Some("127.0.0.1:6400"));
            assert_eq!(cfg.replicaof_auth_username.as_deref(), Some("app"));
            assert_eq!(cfg.replicaof_auth_password.as_deref(), Some("changeme"));

            jail.set_env("ROCKET_MEM_REPLICAOF", "127.0.0.1:9999"); // env beats file
            jail.set_env("ROCKET_MEM_REPLICAOF_AUTH_USERNAME", "envuser"); // env beats file
            jail.set_env("ROCKET_MEM_REPLICAOF_AUTH_PASSWORD", "envpass"); // env beats file
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.replicaof.as_deref(), Some("127.0.0.1:9999"));
            assert_eq!(cfg.replicaof_auth_username.as_deref(), Some("envuser"));
            assert_eq!(cfg.replicaof_auth_password.as_deref(), Some("envpass"));

            let cli = Cli::parse_from([
                "rocket-mem",
                "--config",
                "rocket-mem.toml",
                "--replicaof",
                "127.0.0.1:1111", // CLI beats env
                "--replicaof-auth-username",
                "cliuser", // CLI beats env
                "--replicaof-auth-password",
                "clipass", // CLI beats env
            ]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(cfg.replicaof.as_deref(), Some("127.0.0.1:1111"));
            assert_eq!(cfg.replicaof_auth_username.as_deref(), Some("cliuser"));
            assert_eq!(cfg.replicaof_auth_password.as_deref(), Some("clipass"));
            Ok(())
        });
    }

    #[test]
    fn default_config_has_no_replica_announce_addr() {
        // Unset is the whole compatibility guarantee: a node with no `replica_announce_addr`
        // must announce `addr`, exactly as every deployment did before this field existed.
        assert_eq!(Config::default().replica_announce_addr, None);
    }

    #[test]
    fn replica_announce_addr_is_layered_like_every_other_optional_string_field() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "rocket-mem.toml",
                "replica_announce_addr = \"numericlabs.lxd:16479\"\n",
            )?;
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(
                cfg.replica_announce_addr.as_deref(),
                Some("numericlabs.lxd:16479"),
                "file overrides default"
            );

            jail.set_env("ROCKET_MEM_REPLICA_ANNOUNCE_ADDR", "numericlabs.lxd:26479");
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(
                cfg.replica_announce_addr.as_deref(),
                Some("numericlabs.lxd:26479"),
                "env overrides file"
            );

            let cli = Cli::parse_from([
                "rocket-mem",
                "--config",
                "rocket-mem.toml",
                "--replica-announce-addr",
                "numericlabs.lxd:36479",
            ]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(
                cfg.replica_announce_addr.as_deref(),
                Some("numericlabs.lxd:36479"),
                "CLI overrides env"
            );

            // The layer that is easiest to break by forgetting a `set!` line: an unset flag must
            // leave the env value alone rather than clobbering it with `None`.
            let cli = Cli::parse_from(["rocket-mem", "--config", "rocket-mem.toml"]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(
                cfg.replica_announce_addr.as_deref(),
                Some("numericlabs.lxd:26479"),
                "an unset CLI flag must not clobber the env value"
            );
            Ok(())
        });
    }

    #[test]
    fn log_level_is_layered_like_every_other_string_field() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("rocket-mem.toml", "log_level = \"debug\"\n")?;
            jail.set_env("ROCKET_MEM_LOG_LEVEL", "warn"); // env beats file
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.log_level, "warn");

            let cli = Cli::parse_from([
                "rocket-mem",
                "--config",
                "rocket-mem.toml",
                "--log-level",
                "error", // CLI beats env
            ]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(cfg.log_level, "error");
            Ok(())
        });
    }

    #[test]
    fn log_value_max_bytes_defaults_to_128() {
        figment::Jail::expect_with(|_jail| {
            let cfg = load_layered(None).unwrap();
            assert_eq!(cfg.log_value_max_bytes, 128);
            Ok(())
        });
    }

    #[test]
    fn log_value_max_bytes_is_layered_like_every_other_numeric_field() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("rocket-mem.toml", "log_value_max_bytes = 64\n")?;
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.log_value_max_bytes, 64);

            jail.set_env("ROCKET_MEM_LOG_VALUE_MAX_BYTES", "32"); // env beats file
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.log_value_max_bytes, 32);

            let cli = Cli::parse_from([
                "rocket-mem",
                "--config",
                "rocket-mem.toml",
                "--log-value-max-bytes",
                "16", // CLI beats env
            ]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(cfg.log_value_max_bytes, 16);
            Ok(())
        });
    }

    #[test]
    fn resolve_log_filter_directive_prefers_rust_log_env_over_config_value() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("RUST_LOG", "debug");
            assert_eq!(resolve_log_filter_directive("info"), "debug");
            Ok(())
        });
    }

    #[test]
    fn resolve_log_filter_directive_falls_back_to_config_value_when_unset() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            assert_eq!(resolve_log_filter_directive("warn"), "warn");
            Ok(())
        });
    }

    #[test]
    fn cli_flag_sets_the_numeric_slowlog_threshold_field() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("rocket-mem.toml", "slowlog_threshold_micros = 2000\n")?;
            jail.set_env("ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS", "3000"); // beats the file

            let cli = Cli::parse_from([
                "rocket-mem",
                "--config",
                "rocket-mem.toml",
                "--slowlog-threshold-micros",
                "4000", // beats the env var
            ]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(
                cfg.slowlog_threshold_micros, 4000,
                "CLI must be able to set a numeric field, not just string fields"
            );
            Ok(())
        });
    }

    #[test]
    fn cli_flags_left_unset_do_not_override_lower_layers() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:5555");
            let cli = Cli::parse_from(["rocket-mem"]); // no flags at all
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(
                cfg.rmp_addr, "127.0.0.1:5555",
                "unset CLI flag must not clobber the env value with None/default"
            );
            Ok(())
        });
    }

    #[test]
    fn validate_tls_rejects_tls_resp_addr_without_cert_and_key() {
        let mut cfg = Config {
            tls_resp_addr: Some("127.0.0.1:6443".to_string()),
            ..Config::default()
        };
        assert!(validate_tls(&cfg).is_err(), "cert and key both missing");

        cfg.tls_cert_path = Some("/certs/cert.pem".to_string());
        assert!(validate_tls(&cfg).is_err(), "key still missing");
    }

    #[test]
    fn validate_tls_rejects_tls_rmp_addr_without_cert_and_key() {
        let mut cfg = Config {
            tls_rmp_addr: Some("127.0.0.1:6444".to_string()),
            ..Config::default()
        };
        assert!(validate_tls(&cfg).is_err(), "cert and key both missing");

        cfg.tls_key_path = Some("/certs/key.pem".to_string());
        assert!(validate_tls(&cfg).is_err(), "cert still missing");
    }

    #[test]
    fn validate_tls_accepts_fully_configured_tls() {
        let cfg = Config {
            tls_resp_addr: Some("127.0.0.1:6443".to_string()),
            tls_rmp_addr: Some("127.0.0.1:6444".to_string()),
            tls_cert_path: Some("/certs/cert.pem".to_string()),
            tls_key_path: Some("/certs/key.pem".to_string()),
            ..Config::default()
        };
        assert!(validate_tls(&cfg).is_ok());
    }

    #[test]
    fn validate_tls_accepts_fully_unconfigured_tls() {
        let cfg = Config::default();
        assert!(validate_tls(&cfg).is_ok());
    }

    #[test]
    fn validate_replicaof_rejects_username_without_password() {
        let cfg = Config {
            replicaof_auth_username: Some("app".to_string()),
            ..Config::default()
        };
        assert!(validate_replicaof(&cfg).is_err());
    }

    #[test]
    fn validate_replicaof_rejects_password_without_username() {
        let cfg = Config {
            replicaof_auth_password: Some("changeme".to_string()),
            ..Config::default()
        };
        assert!(validate_replicaof(&cfg).is_err());
    }

    #[test]
    fn validate_replicaof_accepts_both_set_or_both_unset() {
        assert!(validate_replicaof(&Config::default()).is_ok());
        let cfg = Config {
            replicaof: Some("127.0.0.1:6400".to_string()),
            replicaof_auth_username: Some("app".to_string()),
            replicaof_auth_password: Some("changeme".to_string()),
            ..Config::default()
        };
        assert!(validate_replicaof(&cfg).is_ok());
    }

    #[test]
    fn replicaof_auth_is_none_when_neither_field_is_set() {
        assert_eq!(replicaof_auth(&Config::default()), None);
    }

    #[test]
    fn replicaof_auth_pairs_username_and_password_when_both_are_set() {
        let cfg = Config {
            replicaof_auth_username: Some("app".to_string()),
            replicaof_auth_password: Some("changeme".to_string()),
            ..Config::default()
        };
        assert_eq!(
            replicaof_auth(&cfg),
            Some(("app".to_string(), "changeme".to_string()))
        );
    }

    #[test]
    fn validate_replicaof_accepts_no_auth_at_all() {
        let cfg = Config {
            replicaof: Some("127.0.0.1:6400".to_string()),
            ..Config::default()
        };
        assert!(
            validate_replicaof(&cfg).is_ok(),
            "replicaof with no ACL-protected leader needs no auth fields at all"
        );
    }
}
