/// The server's full configuration, later loaded via `figment`'s TOML/env layering (see the
/// sprint 8 config plan). Every field name and type here is what later tasks and plans build
/// on: plan 04's `AclBootstrapConfig` reference and plan 10's TLS fields both assume this shape.
///
/// `#[serde(default)]` on the struct means a partial TOML file (or one missing entirely) still
/// deserializes -- any field it doesn't mention falls back to `Config::default()`'s value for it.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
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
    pub acl: AclBootstrapConfig,
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
            acl: AclBootstrapConfig::default(),
        }
    }
}

/// ACL bootstrap users, read from the TOML config's `[[acl.users]]` array. Converted into real
/// `acl::AclUser`s by `ReplicationHandle::with_acl_bootstrap` — see
/// docs/superpowers/plans/2026-08-31-sprint-8-plans/04-acl-store-and-bootstrap-wiring.md.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct AclBootstrapConfig {
    pub users: Vec<AclUserConfig>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
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
#[derive(clap::Parser, Debug)]
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
    if let Some(v) = cli.slowlog_threshold_micros {
        map.insert("slowlog_threshold_micros", Value::from(v));
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
}
