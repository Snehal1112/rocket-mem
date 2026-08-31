# TLS Listeners & Integration Tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** real TLS listeners for both RESP and RMP, bound only when configured, plus the production plan's own named test (a plaintext connection to a TLS port connects at the TCP layer but never gets a valid reply).

**Architecture:** `serve_tls` functions added to `connection.rs` and `rmp_connection.rs` (accept loops that wrap each socket in `TlsAcceptor::accept` before delegating to the already-generic `handle_connection` from plan 09), wired into `main.rs` behind `Config::tls_resp_addr`/`tls_rmp_addr`/`tls_cert_path`/`tls_key_path`.

**Tech Stack:** nothing new — builds on plan 09's `tls::load_server_config` and generic `handle_connection`.

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: TLS — separate ports, no protocol sniffing" section.

## Global Constraints

- `serve_tls` does **not** spawn `active_expire_loop`/`periodic_fsync_loop` (the plaintext `serve` already does, unconditionally, and `main.rs` still always binds the plaintext listener this sprint) — duplicating them per-listener would run the expiry sweep and fsync loop multiple times over the same shared `Engine`/`AofWriter`, which is redundant, not incorrect, but pointless. A future "genuinely disable the plaintext listener" config knob is out of this plan's scope; `main.rs` always binds `addr` as it does today, with TLS listeners purely additive.
- Both TLS listeners share one cert/key pair (`tls_cert_path`/`tls_key_path`) — `main.rs` calls `tls::load_server_config` once per listener it binds (up to twice), not once globally, since `rustls::ServerConfig` isn't `Clone`.

---

### Task 1: `serve_tls` in both connection modules + `main.rs` wiring

**Files:**
- Modify: `crates/server/src/connection.rs`
- Modify: `crates/server/src/rmp_connection.rs`
- Modify: `crates/server/src/lib.rs`
- Modify: `crates/server/src/main.rs`

**Interfaces:**
- Consumes: `tls::load_server_config` (plan 09), the generic `handle_connection<S>` (plan 09).
- Produces: `pub async fn serve_tls(listener: TcpListener, tls_config: Arc<rustls::ServerConfig>, engine: Arc<Engine>, aof: Arc<AofWriter>, replication: Arc<ReplicationHandle>)` in both `connection.rs` and `rmp_connection.rs`.

- [ ] **Step 1: Add `serve_tls` to `connection.rs`**

```rust
// crates/server/src/connection.rs
/// A second accept loop for the RESP-over-TLS listener, alongside `serve`'s plaintext one.
/// Wraps each accepted socket in a TLS handshake before handing it to the same
/// `handle_connection` `serve` already uses -- see plan 09's genericization of that function.
/// Deliberately does not spawn `active_expire_loop`/`periodic_fsync_loop`: `serve` already does,
/// unconditionally, and this project's TLS listener is additive to the plaintext one, not a
/// replacement for it -- see this plan's Global Constraints.
pub async fn serve_tls(
    listener: TcpListener,
    tls_config: Arc<rustls::ServerConfig>,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
) {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls_config);
    let mut next_client_id: u64 = 1;
    loop {
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let acceptor = acceptor.clone();
        let client_id = next_client_id;
        next_client_id += 1;
        let engine = Arc::clone(&engine);
        let aof = Arc::clone(&aof);
        let replication = Arc::clone(&replication);
        tokio::spawn(async move {
            let tls_socket = match acceptor.accept(socket).await {
                Ok(s) => s,
                // A failed handshake -- including a plaintext client whose raw bytes don't parse
                // as a TLS ClientHello -- simply ends this connection, exactly like any other
                // malformed-input path elsewhere in this codebase.
                Err(_) => return,
            };
            handle_connection(tls_socket, engine, aof, replication, client_id).await;
        });
    }
}
```

Add `use std::sync::Arc;` and `use rustls;`/`use tokio_rustls;` imports as needed at the top of the file if not already present (`Arc` almost certainly already is; `rustls`/`tokio_rustls` are new).

- [ ] **Step 2: Add `serve_tls` to `rmp_connection.rs`**

Same shape, without the RESP-specific doc comment about `active_expire_loop` (RMP's `serve` never spawned those loops either, so there's nothing to avoid duplicating):

```rust
// crates/server/src/rmp_connection.rs
pub async fn serve_tls(
    listener: TcpListener,
    tls_config: Arc<rustls::ServerConfig>,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
) {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls_config);
    let mut next_client_id: u64 = 1;
    loop {
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let acceptor = acceptor.clone();
        let client_id = next_client_id;
        next_client_id += 1;
        let engine = Arc::clone(&engine);
        let aof = Arc::clone(&aof);
        let replication = Arc::clone(&replication);
        tokio::spawn(async move {
            let tls_socket = match acceptor.accept(socket).await {
                Ok(s) => s,
                Err(_) => return,
            };
            handle_connection(tls_socket, engine, aof, replication, client_id).await;
        });
    }
}
```

- [ ] **Step 3: Re-export and wire into `main.rs`**

In `crates/server/src/lib.rs`, change `pub use connection::serve;` to `pub use connection::{serve, serve_tls};`.

In `crates/server/src/main.rs`, after the existing RMP listener block and before the final plaintext `listener`/`serve` call, add:

```rust
if let (Some(tls_addr), Some(cert), Some(key)) =
    (&config.tls_resp_addr, &config.tls_cert_path, &config.tls_key_path)
{
    let tls_config = rocket_mem::tls::load_server_config(
        std::path::Path::new(cert),
        std::path::Path::new(key),
    )?;
    let tls_listener = tokio::net::TcpListener::bind(tls_addr).await?;
    println!("TLS listening on {}", tls_listener.local_addr()?);
    tokio::spawn(rocket_mem::serve_tls(
        tls_listener,
        tls_config,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));
}

if let (Some(tls_rmp_addr), Some(cert), Some(key)) =
    (&config.tls_rmp_addr, &config.tls_cert_path, &config.tls_key_path)
{
    let tls_config = rocket_mem::tls::load_server_config(
        std::path::Path::new(cert),
        std::path::Path::new(key),
    )?;
    let tls_rmp_listener = tokio::net::TcpListener::bind(tls_rmp_addr).await?;
    println!("RMP TLS listening on {}", tls_rmp_listener.local_addr()?);
    tokio::spawn(rocket_mem::rmp_connection::serve_tls(
        tls_rmp_listener,
        tls_config,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));
}
```

- [ ] **Step 4: Verify a plain build (no TLS configured) is unaffected**

Run: `cargo build -p rocket-mem && cargo test --workspace`
Expected: clean build, all green — with no `tls_*` config set, both `if let` blocks above are simply skipped, so this is purely additive.

- [ ] **Step 5: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/src/connection.rs`, `crates/server/src/rmp_connection.rs`, `crates/server/src/lib.rs`, `crates/server/src/main.rs`.

---

### Task 2: Real `tokio-rustls` client round trip against the TLS listener

**Files:**
- Create: `crates/server/tests/tls.rs`

**Interfaces:**
- Consumes: `rocket_mem::serve_tls`, `rocket_mem::tls::load_server_config`.
- Produces: nothing new — an end-to-end proof over a real TLS-wrapped socket.

- [ ] **Step 1: Write the test**

```rust
// crates/server/tests/tls.rs
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::path::Path;
use std::sync::Arc;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
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
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn test_client_config() -> Arc<rustls::ClientConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
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
        rocket_mem::aof::AofWriter::open(&dir.path().join("test.aof"), rocket_mem::aof::FsyncPolicy::Never)
            .unwrap(),
    );
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::default());
    let tls_config =
        rocket_mem::tls::load_server_config(&fixture("test-cert.pem"), &fixture("test-key.pem")).unwrap();
    tokio::spawn(rocket_mem::serve_tls(listener, tls_config, engine, aof, replication));

    let connector = tokio_rustls::TlsConnector::from(test_client_config());
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let tls_stream = connector.connect(server_name, tcp).await.unwrap();

    let mut framed = tokio_util::codec::Framed::new(tls_stream, protocol::codec::RespCodec::default());
    framed
        .send(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(Bytes::from_static(b"SET")),
            protocol::Frame::Bulk(Bytes::from_static(b"foo")),
            protocol::Frame::Bulk(Bytes::from_static(b"bar")),
        ]))
        .await
        .unwrap();
    assert_eq!(framed.next().await.unwrap().unwrap(), protocol::Frame::Simple("OK".into()));

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
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p rocket-mem --test tls a_real_tls_client_completes -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/tests/tls.rs`.

---

### Task 3: Plaintext-rejected-at-a-TLS-port test

**Files:**
- Modify: `crates/server/tests/tls.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing new — the production plan's own named test.

- [ ] **Step 1: Write the test**

```rust
// crates/server/tests/tls.rs
#[tokio::test]
async fn a_plaintext_client_connects_at_tcp_but_never_gets_a_valid_reply() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().unwrap();
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(&dir.path().join("test.aof"), rocket_mem::aof::FsyncPolicy::Never)
            .unwrap(),
    );
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::default());
    let tls_config =
        rocket_mem::tls::load_server_config(&fixture("test-cert.pem"), &fixture("test-key.pem")).unwrap();
    tokio::spawn(rocket_mem::serve_tls(listener, tls_config, engine, aof, replication));

    // The TCP layer connects fine -- the port is open and accepting.
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();

    // Send raw RESP bytes, as a real (non-TLS) redis-cli would, straight at the TLS port.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream.write_all(b"*1\r\n$4\r\nPING\r\n").await.unwrap();

    // The server's TLS handshake fails to parse these bytes as a ClientHello and drops the
    // connection -- read_to_end must observe EOF (a closed connection), never a real RESP reply.
    let mut buf = Vec::new();
    let read_result =
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.read_to_end(&mut buf)).await;
    assert!(read_result.is_ok(), "the connection must be closed, not hang forever");
    assert!(
        buf.is_empty() || !buf.starts_with(b"+PONG"),
        "a plaintext client must never receive a valid RESP reply from a TLS-only port, got: {buf:?}"
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p rocket-mem --test tls a_plaintext_client_connects -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/tests/tls.rs`.
