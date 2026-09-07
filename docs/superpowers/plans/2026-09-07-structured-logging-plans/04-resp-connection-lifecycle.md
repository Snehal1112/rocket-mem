# Structured Logging Plan 4: RESP Connection Lifecycle Logging

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the connection-lifecycle logging identified in the spec's Part B for the RESP path (`connection.rs`) — connection accepted, decode-error close, TLS handshake failure/timeout. This closes the exact observability gap that made a real debugging session (RedisInsight/rocketvault silently failing to connect) require packet captures instead of a log line.

**Architecture:** `handle_connection` currently has no idea what peer it's talking to (`serve`/`serve_tls` both discard the accepted socket's address into `_addr`) and its single `Some(Err(_)) | None => return` match arm treats a decode error identically to a clean disconnect. This plan threads the peer address through both accept loops and `handle_connection` in one atomic change (splitting it across tasks would leave an intermediate task that doesn't compile — `serve` and `serve_tls` both call `handle_connection`, so its signature and both call sites must change together), then splits the error-handling arms so a decode error / TLS failure gets a `warn!` while a clean disconnect stays silent.

**Tech Stack:** `tracing`, `std::net::SocketAddr`.

**Spec:** `docs/superpowers/specs/2026-09-07-structured-logging-design.md`

## Global Constraints

- Scope is `crates/server` only (this plan touches only `connection.rs`).
- New log fields per the spec: `peer` (the client's `SocketAddr`), `protocol` (`"resp"`), `tls` (bool).
- A clean EOF / client-initiated close stays unlogged at `warn`/`error` — only a genuine decode error or TLS failure gets logged.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` must all stay clean after every task.
- Requires Plans 1–3 already merged.

**Next plan:** `docs/superpowers/plans/2026-09-07-structured-logging-plans/05-rmp-connection-lifecycle.md`

---

### Task 1: Thread `peer`/`tls` through `serve`/`serve_tls`/`handle_connection`, log accept/decode-error/TLS-failure

**Files:**
- Modify: `crates/server/src/connection.rs` — `serve` (around line 11), `serve_tls` (around line 82), `handle_connection`'s signature (around line 136) and its read-loop match arm (around line 154)

**Interfaces:**
- Produces: `handle_connection`'s new signature `handle_connection<S>(socket: S, peer: std::net::SocketAddr, tls: bool, engine: Arc<Engine>, aof: Arc<AofWriter>, replication: Arc<ReplicationHandle>, client_id: u64)` — Plan 5's RMP equivalent mirrors this exact parameter order.

- [ ] **Step 1: Capture the peer address in `serve` and pass it through**

In `crates/server/src/connection.rs`, in `serve` (around line 24), change:

```rust
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        let client_id = next_client_id;
        next_client_id += 1;
        let engine = Arc::clone(&engine);
        let aof = Arc::clone(&aof);
        let replication = Arc::clone(&replication);
        tokio::spawn(handle_connection(
            socket,
            engine,
            aof,
            replication,
            client_id,
        ));
```

to:

```rust
        let (socket, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        let client_id = next_client_id;
        next_client_id += 1;
        let engine = Arc::clone(&engine);
        let aof = Arc::clone(&aof);
        let replication = Arc::clone(&replication);
        tokio::spawn(handle_connection(
            socket,
            peer,
            false, // plaintext listener
            engine,
            aof,
            replication,
            client_id,
        ));
```

- [ ] **Step 2: Update `handle_connection`'s signature and log the accept**

Change the function signature (around line 136):

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
    replication.connection_opened();
```

to:

```rust
async fn handle_connection<S>(
    socket: S,
    peer: std::net::SocketAddr,
    tls: bool,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tracing::info!(%peer, protocol = "resp", tls, "connection accepted");
    replication.connection_opened();
```

- [ ] **Step 3: Split the decode-error arm from the clean-disconnect arm**

In the same function's read loop (around line 154), change:

```rust
        let frame = match next {
            Some(Ok(frame)) => frame,
            Some(Err(_)) | None => return, // malformed input or a dropped connection — end this task quietly
        };
```

to:

```rust
        let frame = match next {
            Some(Ok(frame)) => frame,
            Some(Err(e)) => {
                tracing::warn!(%peer, error = %e, "connection closed: decode error");
                return;
            }
            None => return, // client disconnected cleanly — not worth logging
        };
```

- [ ] **Step 4: Capture the peer address in `serve_tls` and split the handshake-failure arm**

In the same file, in `serve_tls` (around line 92), change:

```rust
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
            // Bounded so a client that completes the TCP handshake and then sends nothing --
            // or an incomplete ClientHello -- can't hold this task alive forever. 10 seconds is
            // generous: a normal handshake is sub-millisecond locally and well under a second
            // over a real network.
            let tls_socket = match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                acceptor.accept(socket),
            )
            .await
            {
                Ok(Ok(s)) => s,
                // A failed handshake -- including a plaintext client whose raw bytes don't parse
                // as a TLS ClientHello -- simply ends this connection, exactly like any other
                // malformed-input path elsewhere in this codebase. A timed-out handshake ends
                // the connection the same way.
                Ok(Err(_)) | Err(_) => return,
            };
            handle_connection(tls_socket, engine, aof, replication, client_id).await;
        });
```

to:

```rust
        let (socket, peer) = match listener.accept().await {
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
            // Bounded so a client that completes the TCP handshake and then sends nothing --
            // or an incomplete ClientHello -- can't hold this task alive forever. 10 seconds is
            // generous: a normal handshake is sub-millisecond locally and well under a second
            // over a real network.
            let tls_socket = match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                acceptor.accept(socket),
            )
            .await
            {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::warn!(%peer, error = %e, "tls handshake failed");
                    return;
                }
                Err(_) => {
                    tracing::warn!(%peer, "tls handshake timed out");
                    return;
                }
            };
            handle_connection(tls_socket, peer, true, engine, aof, replication, client_id).await;
        });
```

- [ ] **Step 5: Build**

Run: `cargo build -p rocket-mem`
Expected: succeeds — both `handle_connection` call sites (in `serve` and `serve_tls`) now match its new signature.

- [ ] **Step 6: Confirm existing tests still pass**

Run: `cargo test -p rocket-mem`
Expected: all pass unchanged. `serve`'s and `serve_tls`'s *public* signatures (`listener, engine, aof, replication`) are untouched by this task — only their internal call to `handle_connection` changed — so `crates/server/src/connection.rs`'s own `#[cfg(test)]` module (which calls `serve` directly) needs no changes. If it turns out some test does call `handle_connection` directly, update that call site to the new 7-argument signature.

- [ ] **Step 7: Lint and format**

Run: `cargo clippy -p rocket-mem --all-targets -- -D warnings && cargo fmt --all -- --check`

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/connection.rs
git commit -m "$(cat <<'EOF'
Log RESP connection lifecycle: accept, decode error, TLS failure

Threads the peer SocketAddr through handle_connection/serve/serve_tls
and adds info!/warn! logs for connection accept, a decode-error close,
and TLS handshake failure/timeout. This is the exact gap that made
diagnosing RedisInsight's/rocketvault's silently-dropped connections
require a packet capture instead of a log line -- see
docs/superpowers/specs/2026-09-07-structured-logging-design.md.
EOF
)"
```

---

### Task 2: Manual verification against a live server

**Files:** none (verification only).

- [ ] **Step 1: Build and start the server with info-level logging**

```bash
cargo build --release -p rocket-mem
RUST_LOG=info ./target/release/rocket-mem \
  --addr 127.0.0.1:17101 --rmp-addr 127.0.0.1:17102 --metrics-addr 127.0.0.1:17103 \
  --tls-resp-addr 127.0.0.1:17104 --tls-cert-path certs/cert.pem --tls-key-path certs/key.pem \
  --aof-path /tmp/plan4-verify.aof --snapshot-path /tmp/plan4-verify.snapshot &
sleep 1
```

(If `certs/cert.pem`/`certs/key.pem` don't exist in the working tree at execution time, generate a throwaway self-signed pair first: `openssl req -x509 -newkey rsa:2048 -keyout /tmp/plan4-key.pem -out /tmp/plan4-cert.pem -days 1 -nodes -subj "/CN=localhost" -addext "subjectAltName=IP:127.0.0.1"` and use `/tmp/plan4-cert.pem`/`/tmp/plan4-key.pem` as the `--tls-cert-path`/`--tls-key-path` values instead.)

- [ ] **Step 2: Verify the "connection accepted" log**

Run: `redis-cli -h 127.0.0.1 -p 17101 PING`
Expected client output: `PONG`.
Expected server-side log line: `INFO ... connection accepted peer=127.0.0.1:<some-port> protocol="resp" tls=false`.

- [ ] **Step 3: Verify the decode-error warning**

Run: `printf 'not a resp frame\r\n' | timeout 2 nc 127.0.0.1 17101`
Expected client output: connection closes with no reply (same behavior as before this plan — only the server-side observability changed, not client-visible behavior).
Expected server-side log line: `WARN ... connection closed: decode error peer=127.0.0.1:<some-port> error="unknown RESP type byte: ..."`.

- [ ] **Step 4: Verify the TLS handshake-failure warning**

Run: `printf 'not a tls clienthello' | timeout 2 nc 127.0.0.1 17104`
Expected server-side log line: `WARN ... tls handshake failed peer=127.0.0.1:<some-port> error="..."`.

- [ ] **Step 5: Tear down and clean up**

```bash
kill %1
rm -f /tmp/plan4-verify.aof /tmp/plan4-verify.snapshot /tmp/plan4-cert.pem /tmp/plan4-key.pem
```

- [ ] **Step 6: No commit for this task**

This task is verification-only; nothing in the working tree changes.

**On completion of this plan:** proceed automatically to `docs/superpowers/plans/2026-09-07-structured-logging-plans/05-rmp-connection-lifecycle.md` without waiting for further confirmation.
