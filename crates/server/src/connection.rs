use crate::aof::AofWriter;
use crate::dispatcher;
use crate::replication::ReplicationHandle;
use engine::Engine;
use futures_util::{FutureExt, SinkExt, StreamExt};
use protocol::codec::RespCodec;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::codec::Framed;

pub async fn serve(
    listener: TcpListener,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
) {
    tokio::spawn(active_expire_loop(
        Arc::clone(&engine),
        Arc::clone(&replication),
    ));
    tokio::spawn(periodic_fsync_loop(Arc::clone(&aof)));

    let mut next_client_id: u64 = 1;
    loop {
        let (socket, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        disable_nagle(&socket, peer);
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
    }
}

/// Sweeps one shard per tick, rotating through all 16 — see
/// ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md's active-expiry decision for why a
/// whole-shard sweep (not per-key sampling) is the deliberate simplification here.
async fn active_expire_loop(engine: Arc<Engine>, replication: Arc<ReplicationHandle>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut shard_idx: usize = 0;
    loop {
        interval.tick().await;
        // The only writer of the recency clock every get/set reads. One tick per 100ms is what
        // keeps that clock off the per-operation hot path -- see `Store::advance_clock`.
        engine.advance_recency_clock();
        replication.record_expired(engine.active_expire_cycle(shard_idx));
        shard_idx = shard_idx.wrapping_add(1);
    }
}

/// `FsyncPolicy::EverySecond`'s periodic fsync — `Always` already fsyncs inline inside
/// `AofWriter::append`, so this loop firing harmlessly for that policy too (fsync is
/// idempotent and cheap when there's nothing new to flush) is fine. `Never` is different:
/// it's meant to defer entirely to the OS, so this loop must skip calling `fsync` for it —
/// otherwise `Never` degrades into `EverySecond` in practice, which is what `AofWriter::policy`
/// exists to let this loop check.
async fn periodic_fsync_loop(aof: Arc<AofWriter>) {
    if aof.policy() == crate::aof::FsyncPolicy::Never {
        return;
    }
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        interval.tick().await;
        if let Err(e) = aof.fsync() {
            tracing::error!(error = %e, "aof fsync failed");
        }
        check_aof_intact(&aof);
    }
}

/// Polled once a second by `periodic_fsync_loop`, right after the fsync tick -- see
/// `AofWriter::is_file_intact`'s doc comment for the failure mode this catches: something
/// deletes or replaces the AOF while this process keeps writing into the now-orphaned fd,
/// which otherwise stays completely silent (writes keep succeeding) until the next restart
/// discards everything written since. A 1-second detection window turns that into a logged
/// error and a Prometheus gauge flip instead.
fn check_aof_intact(aof: &AofWriter) {
    match aof.is_file_intact() {
        Ok(true) => ::metrics::gauge!("rocket_mem_aof_file_intact").set(1.0),
        Ok(false) => {
            ::metrics::gauge!("rocket_mem_aof_file_intact").set(0.0);
            tracing::error!(
                path = %aof.path().display(),
                "AOF file has no directory entry at its configured path -- it was deleted or \
                 replaced while this process is still writing to it; every byte written since \
                 will be lost on the next restart unless this is fixed now"
            );
        }
        Err(e) => tracing::error!(error = %e, "aof integrity check failed"),
    }
}

/// Turns off Nagle's algorithm for one accepted connection.
///
/// `Framed` force-flushes inside `feed` once its write buffer passes `backpressure_boundary`
/// (8 KiB), so any pipelined batch of replies larger than that leaves the socket as two writes
/// rather than one. The second is smaller than a loopback MSS and goes out while the first is
/// still unacknowledged, which is exactly what Nagle holds back -- and a client waiting on the
/// rest of the batch sends nothing that would acknowledge it, so the write sits until the 40ms
/// delayed-ACK timer fires. That capped 16-deep pipelined 1KB `GET` at ~20,000 req/s.
///
/// Failure is logged and non-fatal: this is a latency optimisation, never a reason to drop an
/// otherwise-good connection.
pub(crate) fn disable_nagle(socket: &tokio::net::TcpStream, peer: std::net::SocketAddr) {
    if let Err(e) = socket.set_nodelay(true) {
        tracing::warn!(%peer, error = %e, "could not set TCP_NODELAY; pipelined replies may stall");
    }
}

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
        let (socket, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        // Set on the underlying TcpStream before the handshake; the TLS layer wrapping it later
        // does not affect the socket option.
        disable_nagle(&socket, peer);
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
    }
}

/// Decrements the live-connection count on drop, so every one of `handle_connection`'s early
/// returns -- and the `serve_replica` path, which never returns normally -- is covered without
/// each of them having to remember.
pub(crate) struct ClientGuard(pub(crate) Arc<ReplicationHandle>);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.connection_closed();
    }
}

/// Tracks one RESP connection's lifetime state for the connection-closed log event: how long
/// it was open and how many commands it served. Emits that event from `Drop` so every one of
/// `handle_connection`'s several return paths -- decode error, clean EOF, feed failure, flush
/// failure, and the `serve_replica` hand-off, which never returns normally -- gets it exactly
/// once, without each of them having to remember. Mirrors `ClientGuard` just above, which
/// solves the identical problem for the connected-clients counter.
struct ConnectionStats {
    started_at: std::time::Instant,
    commands_served: u64,
}

impl ConnectionStats {
    fn new() -> Self {
        Self {
            started_at: std::time::Instant::now(),
            commands_served: 0,
        }
    }

    fn record_command(&mut self) {
        self.commands_served += 1;
    }
}

impl Drop for ConnectionStats {
    fn drop(&mut self) {
        tracing::info!(
            elapsed_us = self.started_at.elapsed().as_micros().min(u64::MAX as u128) as u64,
            commands_served = self.commands_served,
            "connection closed"
        );
    }
}

#[tracing::instrument(skip_all, fields(conn_id = client_id, %peer, protocol = "resp", %tls))]
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
    tracing::info!("connection accepted");
    replication.connection_opened();
    let _client_guard = ClientGuard(Arc::clone(&replication));
    let mut conn_stats = ConnectionStats::new();
    let mut framed = Framed::new(socket, RespCodec::default());
    let session = dispatcher::Session::with_peer_addr(peer);
    // Carries a frame pulled ahead by the pipelining peek below, so it isn't re-read.
    let mut pending: Option<Option<std::io::Result<protocol::Frame>>> = None;
    loop {
        let next = match pending.take() {
            Some(n) => n,
            None => framed.next().await,
        };
        let frame = match next {
            Some(Ok(frame)) => frame,
            Some(Err(e)) => {
                tracing::warn!(%peer, error = %e, "connection closed: decode error");
                return;
            }
            None => return, // client disconnected cleanly — not worth logging
        };
        if is_psync_command(&frame) {
            // PSYNC never reaches `dispatch_and_log` (it's intercepted here, before the frame
            // loop even calls it), so it must run the same `auth_gate` every other command goes
            // through -- both the NOAUTH check and, critically, the NOPERM check: without the
            // latter, any authenticated user regardless of their ACL grants could PSYNC and
            // receive a full snapshot of the entire keyspace plus a live stream of every
            // subsequent write, bypassing per-user ACL isolation entirely.
            if let Some(reply) = dispatcher::auth_gate(&replication, &session, &frame) {
                if framed.send(reply).await.is_err() {
                    return; // client went away
                }
                continue; // let the client retry after AUTH/HELLO ... AUTH, or give up
            }
            // Chaining (leader -> follower -> sub-replica) doesn't work: a follower applies
            // replicated frames via plain `dispatch`, never `dispatch_and_log`, so it never
            // calls `ReplicaRegistry::broadcast` -- a sub-replica would get a one-time snapshot
            // here and then silently never see another write. Refuse outright instead of
            // leaving that trap in place.
            if replication
                .is_replica
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                let reply = protocol::Frame::Error(
                    "ERR PSYNC refused: this node is itself a replica; chaining is not supported"
                        .into(),
                );
                if framed.send(reply).await.is_err() {
                    return; // client went away
                }
                continue;
            }
            let advertised_addr = psync_advertised_addr(&frame);
            serve_replica(framed, &aof, &replication, advertised_addr).await;
            return; // serve_replica never returns until the replica connection dies
        }
        let response =
            dispatcher::dispatch_and_log(&engine, &aof, &replication, frame, &session, client_id);
        conn_stats.record_command();
        framed.codec_mut().protocol = session.protocol(); // sync BEFORE sending this reply
                                                          // Buffer without flushing -- a flush is a write syscall, and flushing after
                                                          // every single response is what turned client-side pipelining into a
                                                          // regression instead of a speedup (each pipelined request paid for its own
                                                          // syscall despite arriving in the same TCP read as its neighbors).
        if framed.feed(response).await.is_err() {
            return; // client went away mid-response
        }
        // Peek whether the next request is already buffered from that same read
        // (i.e. genuinely pipelined) without blocking on the network. If so, keep
        // batching via feed(); only flush once nothing more is immediately ready.
        match framed.next().now_or_never() {
            Some(n) => pending = Some(n),
            None => {
                if framed.flush().await.is_err() {
                    return;
                }
            }
        }
    }
}

fn is_psync_command(frame: &protocol::Frame) -> bool {
    let protocol::Frame::Array(items) = frame else {
        return false;
    };
    let Some(protocol::Frame::Bulk(name)) = items.first() else {
        return false;
    };
    name.eq_ignore_ascii_case(b"PSYNC")
}

/// Pulls the follower's advertised RESP listen address from its `PSYNC` frame, when present --
/// `PSYNC <addr>`, the second array element. `None` for a bare `PSYNC` (an old client, or any
/// test that doesn't send one), which is exactly what pre-this-feature behavior was. Feeds
/// `ReplicaRegistry::register`, which in turn feeds `INFO REPLICATION`'s `slaveN:` lines.
fn psync_advertised_addr(frame: &protocol::Frame) -> Option<String> {
    let protocol::Frame::Array(items) = frame else {
        return None;
    };
    let protocol::Frame::Bulk(bytes) = items.get(1)? else {
        return None;
    };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Takes ownership of `framed`'s underlying socket and never returns until the replica
/// connection dies. `PSYNC` has no reply frame of its own — the length-prefixed snapshot blob
/// (not a RESP value) stands in for one.
///
/// The `repl` span is the correlation backbone for every replication log line on the leader
/// side, matching `replication.rs`'s follower-side span of the same name. `host_port` is the
/// address this replica advertised in its own `PSYNC <addr>` frame, or the fixed sentinel
/// `"unknown"` for a bare `PSYNC` (an old client, or a test) — see `psync_advertised_addr`'s
/// doc comment. This span is reached from `handle_connection`'s own already-open span (which
/// carries `conn_id`/`peer`/`protocol`/`tls`), so it deliberately does not re-log any of those --
/// only the one field neither ancestor span already has. See
/// ../../../docs/superpowers/specs/2026-09-09-verbose-logging-design.md's span table.
#[tracing::instrument(
    name = "repl",
    skip_all,
    fields(host_port = %advertised_addr.clone().unwrap_or_else(|| "unknown".to_string()))
)]
async fn serve_replica<S>(
    framed: Framed<S, RespCodec>,
    aof: &AofWriter,
    replication: &crate::replication::ReplicationHandle,
    advertised_addr: Option<String>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;

    // ONE critical section: snapshot + register, so no write can slip between them. Taken
    // separately, a write committing after the snapshot walk but before registration would
    // reach neither the blob nor the stream -- lost permanently, unrepairable by reconnect,
    // since a reconnect just snapshots a leader that has already moved past it. Lock
    // ordering: lock_for_ordering() before the registry's own mutex, matching this plan's
    // Global Constraints and the fan-out hook in dispatcher.rs, the only other place both are
    // taken -- there, the order guard for a write's shard(s) is held across both the AOF
    // append and the registry broadcast, for the same reason: neither critical section may
    // release the order guard before it has finished touching the registry.
    let (snapshot_bytes, mut rx) = {
        let _order_guard = aof.lock_all_shards();
        let bytes = replication.engine().snapshot(0); // 0: a follower keeps no AOF, so the header is moot
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        replication.registry.register(advertised_addr, tx);
        (bytes, rx)
    };

    // Reclaim the raw socket. Any bytes already buffered for a reply this connection never
    // got to send (there shouldn't be any at this point -- PSYNC is answered with the blob
    // below, not a normal `feed`/`flush` reply -- but flushing defensively costs nothing) are
    // written out first so nothing already-queued is silently dropped.
    let mut parts = framed.into_parts();
    if !parts.write_buf.is_empty() && parts.io.write_all(&parts.write_buf).await.is_err() {
        return;
    }
    let io = &mut parts.io;

    if io
        .write_all(&(snapshot_bytes.len() as u64).to_le_bytes())
        .await
        .is_err()
    {
        return;
    }
    if io.write_all(&snapshot_bytes).await.is_err() {
        return;
    }

    // Drain replicated writes onto the raw socket forever -- this connection never reads
    // again once PSYNC has been handled. A closed channel (this task's own sender side was
    // dropped, e.g. the process is shutting down) ends the loop cleanly; a write error means
    // the replica disconnected, which `ReplicaRegistry::broadcast`'s retain-based pruning
    // already handles from the registry's side on its next send -- this loop returning is
    // this connection's own half of that same cleanup.
    while let Some(bytes) = rx.recv().await {
        if io.write_all(&bytes).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use engine::Engine;
    use futures_util::{SinkExt, StreamExt};
    use protocol::{codec::RespCodec, Frame};
    use std::sync::Arc;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::codec::Framed;

    fn test_aof() -> (tempfile::TempDir, Arc<crate::aof::AofWriter>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = crate::aof::AofWriter::open(&path, crate::aof::FsyncPolicy::Never).unwrap();
        (dir, Arc::new(writer))
    }

    /// Covers the helper all four accept loops call (plaintext and TLS, for both RESP and RMP).
    /// It does not prove those loops call it: each moves its accepted socket straight into a
    /// connection task, so no test holds a handle on the server side of the connection. Nagle is
    /// on by default, so observing `nodelay() == true` here means the call really flipped it.
    #[test]
    fn connection_stats_counts_each_recorded_command() {
        let mut stats = ConnectionStats::new();
        stats.record_command();
        stats.record_command();
        stats.record_command();
        assert_eq!(stats.commands_served, 3);
    }

    #[tokio::test]
    async fn disable_nagle_sets_tcp_nodelay_on_an_accepted_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::spawn(async move { TcpStream::connect(addr).await.unwrap() });
        let (socket, peer) = listener.accept().await.unwrap();

        super::disable_nagle(&socket, peer);

        assert!(socket.nodelay().unwrap());
        drop(client.await.unwrap());
    }

    #[tokio::test]
    async fn serve_tracks_connected_clients_and_drops_the_count_on_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::default());
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let mut framed = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        framed
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))]))
            .await
            .unwrap();
        framed.next().await.unwrap().unwrap();
        assert_eq!(replication.connected_clients(), 1);
        assert_eq!(replication.total_connections(), 1);

        drop(framed);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(replication.connected_clients(), 0);
        assert_eq!(replication.total_connections(), 1); // the lifetime total never drops
    }

    #[tokio::test]
    async fn the_active_expiry_sweep_counts_the_keys_it_removes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let engine = Arc::new(Engine::new());
        engine.set(
            Bytes::from_static(b"k"),
            engine::Value::String(Bytes::from_static(b"v")),
        );
        engine.expire_at(
            b"k",
            std::time::Instant::now() + std::time::Duration::from_millis(20),
        );
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::default());
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        // one shard per 100ms tick, 16 shards -- 2s covers a full rotation with headroom, the
        // same bound `serve_actively_expires_a_key_even_without_any_read_touching_it` uses.
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
        assert_eq!(replication.expired_keys(), 1);
    }

    #[tokio::test]
    async fn serve_appends_write_commands_to_the_aof() {
        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("test.aof");
        let aof = Arc::new(
            crate::aof::AofWriter::open(&aof_path, crate::aof::FsyncPolicy::Always).unwrap(),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        tokio::spawn(serve(
            listener,
            engine,
            aof,
            Arc::new(crate::replication::ReplicationHandle::default()),
        ));

        let mut framed = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        framed
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"k")),
                Frame::Bulk(Bytes::from_static(b"v")),
            ]))
            .await
            .unwrap();
        assert_eq!(
            framed.next().await.unwrap().unwrap(),
            Frame::Simple("OK".into())
        );

        // give the (Always-policy, synchronous-fsync) append a moment to land on disk.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let contents = std::fs::read_to_string(&aof_path).unwrap();
        assert_eq!(contents, "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
    }

    #[tokio::test]
    async fn serve_handles_a_full_set_get_round_trip_over_a_real_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        tokio::spawn(serve(
            listener,
            engine,
            aof,
            Arc::new(crate::replication::ReplicationHandle::default()),
        ));

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec::default());

        framed
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"foo")),
                Frame::Bulk(Bytes::from_static(b"bar")),
            ]))
            .await
            .unwrap();
        assert_eq!(
            framed.next().await.unwrap().unwrap(),
            Frame::Simple("OK".into())
        );

        framed
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"GET")),
                Frame::Bulk(Bytes::from_static(b"foo")),
            ]))
            .await
            .unwrap();
        assert_eq!(
            framed.next().await.unwrap().unwrap(),
            Frame::Bulk(Bytes::from_static(b"bar"))
        );
    }

    #[tokio::test]
    async fn serve_handles_two_concurrent_connections_independently() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        tokio::spawn(serve(
            listener,
            engine,
            aof,
            Arc::new(crate::replication::ReplicationHandle::default()),
        ));

        let mut a = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        let mut b = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );

        a.send(Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Bulk(Bytes::from_static(b"k")),
            Frame::Bulk(Bytes::from_static(b"a")),
        ]))
        .await
        .unwrap();
        assert_eq!(a.next().await.unwrap().unwrap(), Frame::Simple("OK".into()));

        // same key, both connections share the one Engine — b sees a's write
        b.send(Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"GET")),
            Frame::Bulk(Bytes::from_static(b"k")),
        ]))
        .await
        .unwrap();
        assert_eq!(
            b.next().await.unwrap().unwrap(),
            Frame::Bulk(Bytes::from_static(b"a"))
        );
    }

    #[tokio::test]
    async fn serve_closes_the_connection_cleanly_when_the_client_disconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        tokio::spawn(serve(
            listener,
            engine,
            aof,
            Arc::new(crate::replication::ReplicationHandle::default()),
        ));

        let stream = TcpStream::connect(addr).await.unwrap();
        drop(stream); // disconnect immediately, before sending anything

        // give the server task a moment to observe the disconnect and return,
        // rather than panicking or looping forever
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // a second, independent connection must still work — proves the
        // dropped connection's task didn't take the whole server down with it
        let mut framed = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        framed
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))]))
            .await
            .unwrap();
        assert_eq!(
            framed.next().await.unwrap().unwrap(),
            Frame::Simple("PONG".into())
        );
    }

    #[tokio::test]
    async fn serve_actively_expires_a_key_even_without_any_read_touching_it() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        engine.set(
            Bytes::from_static(b"k"),
            engine::Value::String(Bytes::from_static(b"v")),
        );
        engine.expire_at(
            b"k",
            std::time::Instant::now() + std::time::Duration::from_millis(20),
        );
        let (_dir, aof) = test_aof();
        tokio::spawn(serve(
            listener,
            engine.clone(),
            aof,
            Arc::new(crate::replication::ReplicationHandle::default()),
        ));

        // Wait for a *full* rotation, not just a few ticks: the loop sweeps one shard per
        // 100ms tick, so all 16 shards are only guaranteed covered after ~1.6s — and which
        // shard `k` landed in depends on DefaultHasher, which this test can't predict. 2s
        // leaves headroom over that 1.6s floor. Real (unpaused) time is required here:
        // tokio's clock doesn't advance `std::time::Instant`, which is what `Entry`'s expiry
        // is measured against, so `tokio::time::pause()` would tick the loop without ever
        // making the key expired.
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;

        // sweeping every shard now should find nothing left to remove — the loop already did it
        let total_removed: usize = (0..16).map(|i| engine.active_expire_cycle(i)).sum();
        assert_eq!(total_removed, 0);

        // the server is still alive and serving other requests, proving the loop didn't crash it
        let mut framed = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        framed
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))]))
            .await
            .unwrap();
        assert_eq!(
            framed.next().await.unwrap().unwrap(),
            Frame::Simple("PONG".into())
        );
    }

    #[tokio::test]
    async fn psync_sends_a_length_prefixed_snapshot_then_streams_subsequent_writes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        engine.set(
            Bytes::from_static(b"k"),
            engine::Value::String(Bytes::from_static(b"v")),
        );
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-test-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec::default());
        framed
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(
                b"PSYNC",
            ))]))
            .await
            .unwrap();
        let mut parts = framed.into_parts();

        use tokio::io::AsyncReadExt;
        let mut len_buf = [0u8; 8];
        parts.io.read_exact(&mut len_buf).await.unwrap();
        let len = u64::from_le_bytes(len_buf) as usize;
        let mut blob = vec![0u8; len];
        parts.io.read_exact(&mut blob).await.unwrap();

        let loaded = Engine::new();
        loaded.load_snapshot(&blob).unwrap();
        assert_eq!(
            loaded.get(b"k"),
            Some(engine::Value::String(Bytes::from_static(b"v")))
        );

        // now drive a write through the *real* engine via a second, ordinary client connection,
        // and prove it arrives on the replica connection's raw socket
        let mut client = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        client
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"new")),
                Frame::Bulk(Bytes::from_static(b"value")),
            ]))
            .await
            .unwrap();
        assert_eq!(
            client.next().await.unwrap().unwrap(),
            Frame::Simple("OK".into())
        );

        let mut streamed = vec![0u8; b"*3\r\n$3\r\nSET\r\n$3\r\nnew\r\n$5\r\nvalue\r\n".len()];
        parts.io.read_exact(&mut streamed).await.unwrap();
        assert_eq!(streamed, b"*3\r\n$3\r\nSET\r\n$3\r\nnew\r\n$5\r\nvalue\r\n");
    }

    #[tokio::test]
    async fn psync_with_an_advertised_address_registers_it_on_the_leader() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-test-unused-3.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec::default());
        framed
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"PSYNC")),
                Frame::Bulk(Bytes::from_static(b"127.0.0.1:6480")),
            ]))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await; // let serve_replica register

        assert_eq!(
            replication.registry.addrs(),
            vec![Some("127.0.0.1:6480".to_string())]
        );
    }

    #[tokio::test]
    async fn a_registered_replica_is_pruned_after_its_connection_drops() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-test-unused-2.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec::default());
        framed
            .send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(
                b"PSYNC",
            ))]))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await; // let serve_replica register
        drop(framed); // disconnect the replica

        // two broadcasts: the first send after a drop can still succeed on some platforms before
        // the OS notices the close, so prune is only guaranteed observable after a second attempt
        let mut client = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        for _ in 0..2 {
            client
                .send(Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"SET")),
                    Frame::Bulk(Bytes::from_static(b"k")),
                    Frame::Bulk(Bytes::from_static(b"v")),
                ]))
                .await
                .unwrap();
            client.next().await.unwrap().unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // no assertion beyond "the server is still alive and answering" -- proves broadcast's
        // retain-based pruning didn't panic or wedge on the dropped connection
        let mut ping = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        ping.send(Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))]))
            .await
            .unwrap();
        assert_eq!(
            ping.next().await.unwrap().unwrap(),
            Frame::Simple("PONG".into())
        );
    }

    use crate::logging::test_support::CapturedLogs;

    #[tokio::test]
    async fn serve_replica_opens_a_repl_span_naming_the_advertised_host_port() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("repl-span-test.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));

        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = Framed::new(stream, RespCodec::default());
        framed
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"PSYNC")),
                Frame::Bulk(Bytes::from_static(b"127.0.0.1:9999")),
            ]))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await; // let serve_replica open its span

        drop(_guard);
        let text = captured.text();
        assert!(
            text.contains("repl") && text.contains("host_port") && text.contains("127.0.0.1:9999"),
            "expected a new `repl` span carrying host_port=\"127.0.0.1:9999\", got:\n{text}"
        );
    }
}
