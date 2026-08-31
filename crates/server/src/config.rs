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
/// ../../docs/superpowers/plans/2026-08-31-sprint-8-plans/04-acl-store-and-bootstrap-wiring.md.
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
    pub rules: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
