use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::path::Path;
use std::sync::Arc;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Accepts any server certificate -- test-only, matching this test's self-signed fixture cert,
/// which has no real-world trust chain to verify against. Never used outside `#[cfg(test)]`.
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn test_client_config() -> Arc<rustls::ClientConfig> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    Arc::new(
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
            .with_no_client_auth(),
    )
}

#[tokio::test]
async fn a_real_tls_client_completes_a_set_get_round_trip() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().unwrap();
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("test.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::default());
    let tls_config =
        rocket_mem::tls::load_server_config(&fixture("test-cert.pem"), &fixture("test-key.pem"))
            .unwrap();
    tokio::spawn(rocket_mem::serve_tls(
        listener,
        tls_config,
        engine,
        aof,
        replication,
    ));

    let connector = tokio_rustls::TlsConnector::from(test_client_config());
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let tls_stream = connector.connect(server_name, tcp).await.unwrap();

    let mut framed =
        tokio_util::codec::Framed::new(tls_stream, protocol::codec::RespCodec::default());
    framed
        .send(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(Bytes::from_static(b"SET")),
            protocol::Frame::Bulk(Bytes::from_static(b"foo")),
            protocol::Frame::Bulk(Bytes::from_static(b"bar")),
        ]))
        .await
        .unwrap();
    assert_eq!(
        framed.next().await.unwrap().unwrap(),
        protocol::Frame::Simple("OK".into())
    );

    framed
        .send(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(Bytes::from_static(b"GET")),
            protocol::Frame::Bulk(Bytes::from_static(b"foo")),
        ]))
        .await
        .unwrap();
    assert_eq!(
        framed.next().await.unwrap().unwrap(),
        protocol::Frame::Bulk(Bytes::from_static(b"bar"))
    );
}
