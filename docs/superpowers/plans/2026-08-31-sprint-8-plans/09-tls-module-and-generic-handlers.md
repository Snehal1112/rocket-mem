# TLS Module & Generic Connection Handlers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a `tls.rs` that loads a `rustls::ServerConfig` from a PEM cert/key pair, and `connection.rs`/`rmp_connection.rs` made generic over any `AsyncRead + AsyncWrite` socket so the same connection-handling code serves both plaintext and TLS-wrapped sockets. Listener wiring (actually binding TLS ports) is plan 10's job.

**Architecture:** `handle_connection` in both files changes from a hardcoded `tokio::net::TcpStream` parameter to a generic `S: AsyncRead + AsyncWrite + Unpin + Send + 'static`. Nothing inside either function changes behaviorally — `Framed::new(socket, Codec)` and (in `connection.rs`) `serve_replica`'s raw-socket `write_all` calls already only need those two traits, not the concrete `TcpStream` type.

**Tech Stack:** `tokio-rustls = "0.26"`, `rustls-pemfile = "2"` (new dependencies, `server` crate only).

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: TLS — separate ports, no protocol sniffing" section.

## Global Constraints

- No behavior change for plaintext connections — genericizing `handle_connection` must not alter any existing test's outcome; `serve`/`rmp_connection::serve` (which call `handle_connection::<TcpStream>` implicitly via type inference) keep compiling and passing unchanged.
- rustls 0.23 (pulled in by `tokio-rustls` 0.26) requires a process-wide default `CryptoProvider` to be installed before any `ServerConfig` is built. `load_server_config` installs it defensively (ignoring the "already installed" error) so callers never need a separate setup step.
- One cert/key pair serves both the RESP-TLS and RMP-TLS listeners (plan 10) — `load_server_config` is called once per listener with the same paths, not once globally, since `rustls::ServerConfig` isn't `Clone` but is cheap to rebuild and each listener wants its own `Arc`.

---

### Task 1: `tls.rs` — `load_server_config`

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/server/Cargo.toml`
- Create: `crates/server/src/tls.rs`
- Modify: `crates/server/src/lib.rs` (add `pub mod tls;`)
- Create: `crates/server/tests/fixtures/test-cert.pem`, `crates/server/tests/fixtures/test-key.pem` (test-only self-signed fixture, generated once, committed)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn load_server_config(cert_path: &Path, key_path: &Path) -> std::io::Result<Arc<rustls::ServerConfig>>` — plan 10's listener wiring, and Task 2/3 here (indirectly, via the type import) depend on this.

- [ ] **Step 1: Add dependencies**

In `Cargo.toml`'s `[workspace.dependencies]` (alphabetically):

```toml
rustls = "0.23"
rustls-pemfile = "2"
tokio-rustls = "0.26"
```

(`tokio-rustls` 0.26 depends on `rustls` 0.23 — declaring `rustls` directly too, rather than only reaching its types through `tokio_rustls::rustls`, is what lets plan 10's test-only certificate-verifier code reference `rustls::client::danger::*` directly and keeps cargo's resolver unifying both to one version.)

In `crates/server/Cargo.toml`'s `[dependencies]`:

```toml
rustls.workspace = true
rustls-pemfile.workspace = true
tokio-rustls.workspace = true
```

- [ ] **Step 2: Generate the test fixture cert/key**

Run, from the repo root:

```bash
mkdir -p crates/server/tests/fixtures
openssl req -x509 -newkey rsa:2048 -keyout crates/server/tests/fixtures/test-key.pem \
  -out crates/server/tests/fixtures/test-cert.pem -days 3650 -nodes -subj "/CN=localhost"
```

This is a throwaway, publicly-known-to-be-a-test-fixture self-signed cert with no real-world trust value (10-year validity is fine precisely because it secures nothing) — commit both files. Confirm the key file starts with `-----BEGIN PRIVATE KEY-----` (PKCS#8, what modern OpenSSL emits by default with this invocation, and what `rustls-pemfile::private_key` auto-detects alongside PKCS#1/SEC1).

- [ ] **Step 3: Write the failing test**

```rust
// crates/server/src/tls.rs
#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    #[test]
    fn load_server_config_succeeds_with_the_test_fixture() {
        let config = load_server_config(&fixture("test-cert.pem"), &fixture("test-key.pem")).unwrap();
        assert_eq!(config.cert_resolver.only_raw_public_keys(), false); // sanity: a real cert-based config, not raw-key mode
    }

    #[test]
    fn load_server_config_fails_cleanly_on_a_missing_cert_file() {
        assert!(load_server_config(&fixture("does-not-exist.pem"), &fixture("test-key.pem")).is_err());
    }

    #[test]
    fn load_server_config_fails_cleanly_on_a_malformed_key_file() {
        assert!(load_server_config(&fixture("test-cert.pem"), &fixture("test-cert.pem")).is_err()); // cert file has no private key in it
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem --lib tls:: -- --nocapture`
Expected: FAIL to compile — `load_server_config` doesn't exist yet.

- [ ] **Step 5: Implement**

```rust
// crates/server/src/tls.rs
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

/// Builds a server-auth-only `rustls::ServerConfig` (no client-certificate verification -- TLS
/// for encrypting/authenticating the server to clients, not mutual TLS) from a PEM certificate
/// chain and private key. Installs rustls's process-wide default crypto provider defensively --
/// rustls 0.23 requires one to exist before any `ServerConfig` is built, and ignoring the
/// "already installed" error is what lets this be called once per TLS listener (plan 10) without
/// every caller after the first needing to know it was already done.
pub fn load_server_config(cert_path: &Path, key_path: &Path) -> std::io::Result<Arc<rustls::ServerConfig>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

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
        std::io::Error::new(std::io::ErrorKind::InvalidData, "no private key found in key file")
    })?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(Arc::new(config))
}
```

Add `use rustls;` is unnecessary (it's referenced fully-qualified above); add `pub mod tls;` to `crates/server/src/lib.rs` (alphabetically, near the end, after `slowlog`).

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --lib tls:: -- --nocapture`
Expected: all PASS. (If `cert_resolver.only_raw_public_keys()` isn't accessible on your `rustls` version, replace that sanity assertion with simply asserting `load_server_config(...).is_ok()` — the field-level check is a bonus, not the point of the test.)

- [ ] **Step 7: Full-crate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test -p rocket-mem --lib tls::`
Expected: all green.

Use the `1-git-commit` skill/command to commit `Cargo.toml`, `crates/server/Cargo.toml`, `crates/server/src/tls.rs`, `crates/server/src/lib.rs`, and the two fixture files.

---

### Task 2: Genericize `connection.rs`'s `handle_connection`

**Files:**
- Modify: `crates/server/src/connection.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `async fn handle_connection<S>(socket: S, ...)` where `S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static` — plan 10's TLS listener calls this with a `tokio_rustls::server::TlsStream<TcpStream>`; the existing plaintext `serve` keeps calling it with a plain `TcpStream`, inferred automatically.

- [ ] **Step 1: Change the signature**

In `crates/server/src/connection.rs`, change:

```rust
async fn handle_connection(
    socket: tokio::net::TcpStream,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) {
```

to:

```rust
async fn handle_connection<S>(
    socket: S,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
```

`serve_replica` (called from inside this function, taking `framed: Framed<tokio::net::TcpStream, RespCodec>` today) must become generic too, since it receives this same socket type:

```rust
async fn serve_replica<S>(
    framed: Framed<S, RespCodec>,
    aof: &AofWriter,
    replication: &crate::replication::ReplicationHandle,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
```

Its body's `framed.into_parts()` and subsequent `io.write_all(...)` calls (via `tokio::io::AsyncWriteExt`) already only need `AsyncWrite + Unpin`, so no further change is needed inside the function.

- [ ] **Step 2: Verify it compiles and existing tests pass**

Run: `cargo build -p rocket-mem && cargo test -p rocket-mem --lib connection::`
Expected: clean build (`serve`'s existing call to `handle_connection(socket, ...)` with a plain `TcpStream` still type-checks via inference — `S = TcpStream`), all pre-existing `connection::tests` PASS unchanged.

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `crates/server/src/connection.rs`.

---

### Task 3: Genericize `rmp_connection.rs`'s `handle_connection`

**Files:**
- Modify: `crates/server/src/rmp_connection.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `async fn handle_connection<S>(socket: S, ...)` — same generic shape as Task 2, for plan 10's RMP-TLS listener.

- [ ] **Step 1: Change the signature**

In `crates/server/src/rmp_connection.rs`, change:

```rust
async fn handle_connection(
    socket: tokio::net::TcpStream,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) {
```

to:

```rust
async fn handle_connection<S>(
    socket: S,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
```

Nothing else in the function body references `tokio::net::TcpStream` by name (`Framed::new(socket, RmpCodec)` is already generic over the trait bounds), so no further change is needed.

- [ ] **Step 2: Verify it compiles and existing tests pass**

Run: `cargo build -p rocket-mem && cargo test -p rocket-mem --lib rmp_connection::`
Expected: clean build, all pre-existing `rmp_connection::tests` PASS unchanged.

- [ ] **Step 3: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green.

Use the `1-git-commit` skill/command to commit `crates/server/src/rmp_connection.rs`.
