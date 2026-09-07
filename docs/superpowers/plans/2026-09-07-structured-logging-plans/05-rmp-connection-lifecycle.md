# Structured Logging Plan 5: RMP Connection Lifecycle Logging

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Mirror Plan 4's RESP connection-lifecycle logging for the RMP path (`rmp_connection.rs`) — connection accepted, decode-error close, TLS handshake failure/timeout.

**Architecture:** Same shape as Plan 4: `rmp_connection.rs`'s `serve`/`serve_tls`/`handle_connection` currently discard the peer address (`_addr`) the same way `connection.rs`'s did before Plan 4. This plan threads it through the same way, in one atomic task for the same reason as Plan 4 (`serve` and `serve_tls` both call `handle_connection`, so its signature and both call sites must change together to compile). One difference from Plan 4: `rmp_connection.rs:128` already has a decode-error `eprintln!` — Plan 2, Task 2 already converted it to `tracing::warn!(error = %e, "rmp decode error")` without a `peer` field (peer wasn't available yet at that point). This plan's Task 1 extends that same call to add `peer` now that it's threaded through — this is expected, not a conflict with Plan 2's earlier work.

**Tech Stack:** `tracing`, `std::net::SocketAddr`.

**Spec:** `docs/superpowers/specs/2026-09-07-structured-logging-design.md`

## Global Constraints

- Scope is `crates/server` only (this plan touches only `rmp_connection.rs`).
- New log fields per the spec: `peer`, `protocol` (`"rmp"`), `tls` (bool).
- A clean disconnect (`Ok(_) => break` for a stray Response, or the writer task's `sink.send` failing because the client went away) stays unlogged — only a genuine decode error or TLS failure gets logged, same rule as Plan 4.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` must all stay clean after every task.
- Requires Plans 1–4 already merged.

**Next plan:** `docs/superpowers/plans/2026-09-07-structured-logging-plans/06-final-verification.md`

---

### Task 1: Thread `peer`/`tls` through `serve`/`serve_tls`/`handle_connection`, log accept/decode-error/TLS-failure

**Files:**
- Modify: `crates/server/src/rmp_connection.rs` — `serve` (around line 22), `serve_tls` (around line 46), `handle_connection`'s signature (around line 89) and its read-loop decode-error arm (around line 128, already converted by Plan 2 Task 2)

**Interfaces:**
- Produces: `handle_connection`'s new signature `handle_connection<S>(socket: S, peer: std::net::SocketAddr, tls: bool, engine: Arc<Engine>, aof: Arc<AofWriter>, replication: Arc<ReplicationHandle>, client_id: u64)` — same shape as `connection.rs`'s equivalent from Plan 4.

- [ ] **Step 1: Capture the peer address in `serve` and pass it through**

In `crates/server/src/rmp_connection.rs`, in `serve` (around line 29), change:

```rust
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        let client_id = next_client_id;
        next_client_id += 1;
        tokio::spawn(handle_connection(
            socket,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
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
        tokio::spawn(handle_connection(
            socket,
            peer,
            false, // plaintext listener
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            client_id,
        ));
```

- [ ] **Step 2: Update `handle_connection`'s signature and log the accept**

Change the function signature (around line 89):

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
    tracing::info!(%peer, protocol = "rmp", tls, "connection accepted");
    replication.connection_opened();
```

- [ ] **Step 3: Add `peer` to the existing decode-error warning**

Find the read loop's decode-error arm (around line 128, already converted from `eprintln!` by Plan 2 Task 2):

```rust
            Err(e) => {
                tracing::warn!(error = %e, "rmp decode error");
                break;
            }
```

Change it to:

```rust
            Err(e) => {
                tracing::warn!(%peer, error = %e, "rmp decode error");
                break;
            }
```

- [ ] **Step 4: Capture the peer address in `serve_tls` and split the handshake-failure arm**

In the same file, in `serve_tls` (around line 57), change:

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
                // A failed handshake, or a timed-out one, ends this connection the same way.
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
Expected: all pass unchanged. `serve`'s and `serve_tls`'s *public* signatures are untouched — only their internal call to `handle_connection` changed — so `crates/server/src/rmp_connection.rs`'s own `#[cfg(test)]` module (which calls `serve` directly, per `spawn_test_server`) needs no changes. `crates/server/tests/rmp.rs` also calls `rmp_connection::serve` (public signature, unaffected). If it turns out some test does call `handle_connection` directly, update that call site to the new 7-argument signature.

- [ ] **Step 7: Lint and format**

Run: `cargo clippy -p rocket-mem --all-targets -- -D warnings && cargo fmt --all -- --check`

- [ ] **Step 8: Commit**

```bash
git add crates/server/src/rmp_connection.rs
git commit -m "$(cat <<'EOF'
Log RMP connection lifecycle: accept, decode error, TLS failure

Mirrors the RESP-path lifecycle logging from the previous plan for
the RMP path: threads the peer SocketAddr through handle_connection/
serve/serve_tls, adds an accepted info! log, adds the peer field to
the existing rmp-decode-error warn!, and adds TLS handshake
failure/timeout warn! logs. See
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
  --addr 127.0.0.1:17201 --rmp-addr 127.0.0.1:17202 --metrics-addr 127.0.0.1:17203 \
  --tls-rmp-addr 127.0.0.1:17204 --tls-cert-path certs/cert.pem --tls-key-path certs/key.pem \
  --aof-path /tmp/plan5-verify.aof --snapshot-path /tmp/plan5-verify.snapshot &
sleep 1
```

(Same certificate caveat as Plan 4 Task 2 Step 1 if `certs/cert.pem`/`certs/key.pem` aren't present.)

- [ ] **Step 2: Verify the "connection accepted" log via `rmp-client`**

Write and run a throwaway program using the `rmp-client` crate (path dependency `{ path = "crates/rmp-client" }`) that does `RmpClient::connect("127.0.0.1:17202").await?` and then drops the client. Delete the throwaway program after this step.
Expected server-side log line: `INFO ... connection accepted peer=127.0.0.1:<some-port> protocol="rmp" tls=false`.

- [ ] **Step 3: Verify the decode-error warning**

Run: `printf 'not an rmp frame' | timeout 2 nc 127.0.0.1 17202`
Expected server-side log line: `WARN ... rmp decode error peer=127.0.0.1:<some-port> error="..."`.

- [ ] **Step 4: Verify the TLS handshake-failure warning**

Run: `printf 'not a tls clienthello' | timeout 2 nc 127.0.0.1 17204`
Expected server-side log line: `WARN ... tls handshake failed peer=127.0.0.1:<some-port> error="..."`.

- [ ] **Step 5: Tear down and clean up**

```bash
kill %1
rm -f /tmp/plan5-verify.aof /tmp/plan5-verify.snapshot
```

- [ ] **Step 6: No commit for this task**

This task is verification-only.

**On completion of this plan:** proceed automatically to `docs/superpowers/plans/2026-09-07-structured-logging-plans/06-final-verification.md` without waiting for further confirmation.
