use std::path::Path;

use redis::aio::ConnectionManager;
use redis::{Client, ConnectionAddr, IntoConnectionInfo, RedisResult, TlsCertificates};

/// A cheap-to-clone handle to a rocket-mem connection. `ConnectionManager` already multiplexes
/// concurrent requests over one underlying connection and reconnects automatically, so no
/// separate pooling crate (bb8/deadpool) is needed — every tool call just clones this and
/// issues its command.
#[derive(Clone)]
pub struct Pool {
    manager: ConnectionManager,
}

impl Pool {
    /// `target_addr` is `"host:port"`. `acl_username`/`acl_password` are sent as `AUTH`
    /// credentials when set — omit both against an instance with no ACLs configured.
    /// `tls_ca_path`, when set, is what turns TLS on for this connection: the file at that path
    /// is used as the trust root to verify the target's certificate. There is no insecure/
    /// skip-verification path.
    pub async fn connect(
        target_addr: &str,
        acl_username: Option<&str>,
        acl_password: Option<&str>,
        tls_ca_path: Option<&Path>,
    ) -> RedisResult<Self> {
        let (host, port) = target_addr.rsplit_once(':').ok_or_else(|| {
            redis::RedisError::from((
                redis::ErrorKind::InvalidClientConfig,
                "target address must be host:port",
            ))
        })?;
        let port: u16 = port.parse().map_err(|_| {
            redis::RedisError::from((
                redis::ErrorKind::InvalidClientConfig,
                "target address port is not a valid u16",
            ))
        })?;

        let addr = match tls_ca_path {
            Some(_) => ConnectionAddr::TcpTls {
                host: host.to_string(),
                port,
                insecure: false,
                tls_params: None,
            },
            None => ConnectionAddr::Tcp(host.to_string(), port),
        };

        let mut conn_info = addr.into_connection_info()?;
        let mut redis_settings = conn_info.redis_settings().clone();
        if let Some(username) = acl_username {
            redis_settings = redis_settings.set_username(username);
        }
        if let Some(password) = acl_password {
            redis_settings = redis_settings.set_password(password);
        }
        conn_info = conn_info.set_redis_settings(redis_settings);

        let client = match tls_ca_path {
            Some(ca_path) => {
                // rustls needs a process-wide default crypto provider installed before any TLS
                // connection; with more than one provider available in the dependency tree
                // (aws-lc-rs/ring) it won't pick one automatically. Matches
                // `crates/server/src/tls.rs`'s identical fix — `install_default` errors if a
                // provider is already installed (e.g. by an earlier call in this same process),
                // which is fine to ignore, hence `let _ =`.
                let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
                let root_cert = std::fs::read(ca_path).map_err(|e| {
                    redis::RedisError::from((
                        redis::ErrorKind::InvalidClientConfig,
                        "could not read TLS CA certificate",
                        e.to_string(),
                    ))
                })?;
                Client::build_with_tls(
                    conn_info,
                    TlsCertificates {
                        client_tls: None,
                        root_cert: Some(root_cert),
                    },
                )?
            }
            None => Client::open(conn_info)?,
        };

        let manager = client.get_connection_manager().await?;
        Ok(Self { manager })
    }

    pub fn connection(&self) -> ConnectionManager {
        self.manager.clone()
    }
}
