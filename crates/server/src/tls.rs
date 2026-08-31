use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

/// Builds a server-auth-only `rustls::ServerConfig` (no client-certificate verification -- TLS
/// for encrypting/authenticating the server to clients, not mutual TLS) from a PEM certificate
/// chain and private key. Installs rustls's process-wide default crypto provider defensively --
/// rustls 0.23 requires one to exist before any `ServerConfig` is built, and ignoring the
/// "already installed" error is what lets this be called once per TLS listener (plan 10) without
/// every caller after the first needing to know it was already done.
pub fn load_server_config(
    cert_path: &Path,
    key_path: &Path,
) -> std::io::Result<Arc<rustls::ServerConfig>> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cert_file = std::fs::File::open(cert_path)?;
    let cert_chain: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(cert_file)).collect::<Result<Vec<_>, _>>()?;
    if cert_chain.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "no certificate found in cert file",
        ));
    }

    let key_file = std::fs::File::open(key_path)?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key_file))?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "no private key found in key file",
        )
    })?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn load_server_config_succeeds_with_the_test_fixture() {
        let config =
            load_server_config(&fixture("test-cert.pem"), &fixture("test-key.pem")).unwrap();
        assert_eq!(config.cert_resolver.only_raw_public_keys(), false); // sanity: a real cert-based config, not raw-key mode
    }

    #[test]
    fn load_server_config_fails_cleanly_on_a_missing_cert_file() {
        assert!(
            load_server_config(&fixture("does-not-exist.pem"), &fixture("test-key.pem")).is_err()
        );
    }

    #[test]
    fn load_server_config_fails_cleanly_on_a_malformed_key_file() {
        assert!(load_server_config(&fixture("test-cert.pem"), &fixture("test-cert.pem")).is_err());
        // cert file has no private key in it
    }
}
