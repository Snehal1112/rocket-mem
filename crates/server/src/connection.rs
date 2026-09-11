use crate::aof::AofWriter;
use crate::dispatcher;
use crate::replication::ReplicationHandle;
use engine::Engine;
use futures_util::{FutureExt, SinkExt, StreamExt};
use protocol::codec::RespCodec;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::codec::{Framed, FramedRead};

pub async fn serve(
    listener: TcpListener,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    node_id: Arc<str>,
) {
    tokio::spawn(active_expire_loop(
        Arc::clone(&engine),
        Arc::clone(&replication),
    ));
    tokio::spawn(periodic_fsync_loop(Arc::clone(&aof), Arc::clone(&node_id)));

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
            Arc::clone(&node_id),
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
async fn periodic_fsync_loop(aof: Arc<AofWriter>, node_id: Arc<str>) {
    if aof.policy() == crate::aof::FsyncPolicy::Never {
        return;
    }
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        interval.tick().await;
        if let Err(e) = aof.fsync() {
            tracing::error!(%node_id, error = %e, "aof fsync failed");
        }
        check_aof_intact(&aof, &node_id);
    }
}

/// Polled once a second by `periodic_fsync_loop`, right after the fsync tick -- see
/// `AofWriter::is_file_intact`'s doc comment for the failure mode this catches: something
/// deletes or replaces the AOF while this process keeps writing into the now-orphaned fd,
/// which otherwise stays completely silent (writes keep succeeding) until the next restart
/// discards everything written since. A 1-second detection window turns that into a logged
/// error and a Prometheus gauge flip instead.
fn check_aof_intact(aof: &AofWriter, node_id: &str) {
    match aof.is_file_intact() {
        Ok(true) => ::metrics::gauge!("rocket_mem_aof_file_intact").set(1.0),
        Ok(false) => {
            ::metrics::gauge!("rocket_mem_aof_file_intact").set(0.0);
            // `aof_path`, not `path`: this names the same file every event in `aof.rs` calls
            // `aof_path`, and the same file the `aof_path` config key configures. A lone `path`
            // here made `grep aof_path` miss the one event that says the file is gone.
            tracing::error!(
                %node_id,
                aof_path = %aof.path().display(),
                "AOF file has no directory entry at its configured path -- it was deleted or \
                 replaced while this process is still writing to it; every byte written since \
                 will be lost on the next restart unless this is fixed now"
            );
        }
        Err(e) => tracing::error!(%node_id, error = %e, "aof integrity check failed"),
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
    node_id: Arc<str>,
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
        let node_id = Arc::clone(&node_id);
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
            handle_connection(
                tls_socket,
                peer,
                true,
                engine,
                aof,
                replication,
                client_id,
                node_id,
            )
            .await;
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

// `protocol = %"RESP"`, not `protocol = "resp"`. A bare `&str` field records through `Debug`, so
// the plain literal rendered `protocol="resp"` -- quoted, and the only quoted field on a line
// whose neighbours read `conn_id=1 peer=127.0.0.1:60768 tls=false`. `main.rs`'s "listener bound"
// events already spell the same field `protocol=RESP`, unquoted and uppercase, and the spec's
// reason for a fixed field vocabulary is that a single `grep` follows an activity end to end --
// which `protocol=RESP` here and `protocol="resp"` there defeated.
// `name = "conn"` is not cosmetic: without it the span takes the function's name, so every log
// line on a connection renders as `handle_connection{...}` and the spec's three-span vocabulary
// (`conn`/`cmd`/`repl`) matches only two of its three names. `serve_replica` below already names
// its span `repl` for the same reason.
// 8 arguments, one over clippy's default threshold, since `node_id` (2026-09-11) joined the
// existing seven: `socket`/`peer`/`tls` describe this one connection, `engine`/`aof`/`replication`
// are the three handles every command needs, and `client_id`/`node_id` are two independent
// correlation ids (per-connection, per-process). Bundling the latter into a context struct would
// need a name for every call site to construct and would still have to be destructured right back
// out for the `#[instrument]` fields below -- not a real reduction, just moved indirection.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    name = "conn",
    skip_all,
    fields(conn_id = client_id, %peer, protocol = %"RESP", %tls, %node_id)
)]
async fn handle_connection<S>(
    socket: S,
    peer: std::net::SocketAddr,
    tls: bool,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
    node_id: Arc<str>,
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

/// Pulls the offset out of a follower's `REPLCONF ACK <offset>` frame -- the ack shape fixed by
/// the failover-safety design contract's §2.3, a plain RESP array on the existing replication
/// socket. `None` for anything else: a different frame type, too few elements, a `REPLCONF`
/// subcommand this leader does not implement, or an offset that does not parse as a `u64`. The
/// caller logs those at `debug` and ignores them -- an unrecognised inbound frame must never draw
/// an error reply and must never cost the follower its connection.
///
/// Three elements *or more*, deliberately. A real `redis-server` replica from 7.4 onward sends
/// `REPLCONF ACK <offset> FACK <offset>`, appending its fsynced offset. Requiring exactly three
/// would read that as "not an ack" and leave a genuine Redis replica looking like it had never
/// acked at all -- which write fencing would later treat as a replica that does not count. The
/// trailing fields are ignored rather than parsed: this leader has no use for a fsync offset, and
/// guessing at fields it does not implement is how a parser starts lying.
fn parse_replconf_ack(frame: &protocol::Frame) -> Option<u64> {
    let protocol::Frame::Array(items) = frame else {
        return None;
    };
    if items.len() < 3 {
        return None;
    }
    let protocol::Frame::Bulk(name) = &items[0] else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"REPLCONF") {
        return None;
    }
    let protocol::Frame::Bulk(subcommand) = &items[1] else {
        return None;
    };
    if !subcommand.eq_ignore_ascii_case(b"ACK") {
        return None;
    }
    let protocol::Frame::Bulk(offset) = &items[2] else {
        return None;
    };
    std::str::from_utf8(offset).ok()?.parse().ok()
}

/// Takes ownership of `framed`'s underlying socket and never returns until the replica
/// connection dies. `PSYNC` has no reply frame of its own — the length-prefixed snapshot blob
/// (not a RESP value) stands in for one.
///
/// The connection is bidirectional. The leader streams replicated writes down it and reads
/// frames back up it, which is what lets a follower report how caught up it is via `REPLCONF
/// ACK <offset>` on this same socket -- no second port and no second connection. See the
/// failover-safety design contract's §2.3.
///
/// The `repl` span is the correlation backbone for every replication log line on the leader
/// side, matching `replication.rs`'s follower-side span of the same name. `host_port` is the
/// address this replica advertised in its own `PSYNC <addr>` frame, or the fixed sentinel
/// `"unknown"` for a bare `PSYNC` (an old client, or a test) — see `psync_advertised_addr`'s
/// doc comment. This span is reached from `handle_connection`'s own already-open span (which
/// carries `conn_id`/`peer`/`protocol`/`tls`), so it deliberately does not re-log any of those --
/// only the one field neither ancestor span already has. See
/// ../../../docs/superpowers/specs/2026-09-09-verbose-logging-design.md's span table.
///
/// `host_port` goes through `logging::escape_ident`. It is a raw client bulk, and `PSYNC` clears
/// `auth_gate` even on a server with no ACLs configured, which makes this the one field an
/// *unauthenticated* remote party can put arbitrary bytes into at `info` -- the level every
/// production node runs at. Unescaped it forges log records; uncapped it forges long ones.
#[tracing::instrument(
    name = "repl",
    skip_all,
    fields(host_port = %crate::logging::escape_ident(
        advertised_addr.as_deref().unwrap_or("unknown")
    ))
)]
async fn serve_replica<S>(
    framed: Framed<S, RespCodec>,
    aof: &AofWriter,
    replication: &crate::replication::ReplicationHandle,
    advertised_addr: Option<String>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // ONE critical section: read the offset, snapshot, and register, so no write can slip
    // between them. Taken separately, a write committing after the snapshot walk but before
    // registration would reach neither the blob nor the stream -- lost permanently,
    // unrepairable by reconnect, since a reconnect just snapshots a leader that has already
    // moved past it. Reading `master_repl_offset` inside the same section is what makes the
    // header the follower is about to seed itself from describe exactly this blob: the fan-out
    // site advances that counter while holding the same AOF ordering guard, so nothing can
    // advance it between this read and the registration below. Lock ordering:
    // lock_for_ordering() before the registry's own mutex, matching this plan's Global
    // Constraints and the fan-out hook in dispatcher.rs, the only other place both are taken --
    // there, the order guard for a write's shard(s) is held across both the AOF append and the
    // registry broadcast, for the same reason: neither critical section may release the order
    // guard before it has finished touching the registry.
    let (snapshot_bytes, mut rx, entry) = {
        let _order_guard = aof.lock_all_shards();
        // The header's stream position, for a PSYNC image, is the leader's replication offset --
        // not an AOF length. See `Engine::snapshot`'s doc comment for the parameter's two
        // meanings.
        let handoff_offset = replication.master_repl_offset();
        let bytes = replication.engine().snapshot(handoff_offset);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        let host_port = advertised_addr
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        // The entry, not just a registration: acks arriving below are recorded straight through
        // this handle, with no registry lookup and no registry lock.
        let entry = replication.registry.register(advertised_addr, tx);
        tracing::info!(host_port = %crate::logging::escape_ident(&host_port), "replica registered");
        (bytes, rx, entry)
    };

    // Reclaim the raw socket and split it, because this connection now reads as well as
    // writes. The write half does exactly what the single `parts.io` handle used to; the read
    // half becomes a RESP frame stream.
    let parts = framed.into_parts();
    let (rd, mut wr) = tokio::io::split(parts.io);
    // Replay whatever the codec had already read ahead of `PSYNC` before touching the socket
    // again. Those bytes are gone from the socket, so reading it straight away would silently
    // swallow anything the follower pipelined behind its PSYNC -- its first ack, most likely.
    //
    // Chained in front of the read half, not poked into `FramedRead`'s own buffer with
    // `read_buffer_mut`. Seeding that buffer keeps the bytes but leaves `FramedRead`'s internal
    // `is_readable` flag false, so its very first poll reads the socket *before* it decodes what
    // is already buffered (tokio-util 0.7.19 `codec/framed_impl.rs:183-248`). A pipelined ack
    // would then sit undecoded until the follower happened to send something else, or until it
    // hung up. Chaining makes the leftovers ordinary bytes to read, which is what the decoder is
    // built for. `an_ack_pipelined_behind_psync_is_not_lost` is what pins this down.
    let rd = std::io::Cursor::new(parts.read_buf).chain(rd);
    let mut inbound = FramedRead::new(rd, RespCodec::default());

    // Any bytes already buffered for a reply this connection never got to send (there
    // shouldn't be any at this point -- PSYNC is answered with the blob below, not a normal
    // `feed`/`flush` reply -- but flushing defensively costs nothing) are written out first so
    // nothing already-queued is silently dropped.
    if !parts.write_buf.is_empty() && wr.write_all(&parts.write_buf).await.is_err() {
        return;
    }

    if wr
        .write_all(&(snapshot_bytes.len() as u64).to_le_bytes())
        .await
        .is_err()
    {
        return;
    }
    if wr.write_all(&snapshot_bytes).await.is_err() {
        return;
    }

    // Drain replicated writes onto the socket while simultaneously reading whatever the
    // follower sends back. Both branch futures are cancel-safe, which is what makes them legal
    // `select!` arms; the writes themselves happen in the arm bodies, after the other future
    // has been dropped.
    // Cleared by a decode error, which disables the inbound branch for the rest of this
    // connection's life. The write stream is untouched either way.
    let mut inbound_open = true;
    loop {
        tokio::select! {
            outbound = rx.recv() => match outbound {
                Some(bytes) => {
                    // A write error means the replica disconnected, which
                    // `ReplicaRegistry::broadcast`'s retain-based pruning already handles from
                    // the registry's side on its next send -- this loop returning is this
                    // connection's own half of that same cleanup.
                    if wr.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                // Every sender for this replica has been dropped: the registry pruned this
                // entry, or the process is shutting down. Nothing will ever be streamed here
                // again, so end the connection rather than parking on it forever.
                None => return,
            },
            incoming = inbound.next(), if inbound_open => match incoming {
                // A `REPLCONF ACK <offset>` is recorded straight onto this replica's own entry,
                // so the ack path takes no registry lock at all.
                Some(Ok(frame)) => match parse_replconf_ack(&frame) {
                    Some(offset) => {
                        entry.record_ack(offset);
                        // Logged so a stuck offset can be told apart from a missing one. Without
                        // this, an operator watching a replica that stops advancing cannot see
                        // whether acks are arriving and repeating, or not arriving at all.
                        tracing::debug!(offset, "recorded replica ack");
                    }
                    // Logged and dropped, never answered: an unrecognised frame must not draw an
                    // error reply and must not cost the follower its connection. A follower that
                    // never sends a recognisable ack stays a replica with no ack information.
                    // Only the frame's kind and length are logged, never its contents: a
                    // replica's frames are arbitrary client bytes, and `logging.rs` is the one
                    // place that decides what may be rendered.
                    None => tracing::debug!(
                        kind = frame.kind(),
                        len = frame.log_len(),
                        "ignoring inbound frame from a replica"
                    ),
                },
                // The follower sent bytes this codec cannot parse, so the read side is desynced
                // and can never resynchronise -- `RespCodec::decode` errors on an unknown type
                // byte without consuming it, so that byte stays at the head of the buffer.
                // Clearing the flag is not the same as `continue`: `FramedRead` fuses after a
                // decoder error, so the next poll would yield `None`, and `None` is handled
                // below as end-of-stream. Continuing would therefore drop this replica by a
                // roundabout route, which is the one thing this arm exists to prevent.
                // Stop reading and keep streaming: the leader loses this follower's ack
                // information, exactly as it would for a follower that never acks, and the
                // follower keeps replicating. Dropping the connection instead would punish a
                // replica for speaking a dialect this leader has not learned yet. The cost is
                // that this connection also stops noticing a disconnect promptly, falling back
                // to the write-failure pruning that predated the bidirectional split.
                Some(Err(e)) => {
                    tracing::debug!(
                        error = %e,
                        "unparseable inbound bytes from a replica; no longer reading this \
                         connection, but still streaming to it"
                    );
                    inbound_open = false;
                }
                // The follower closed its side. Return so this task's `ClientGuard` drops and
                // its sender goes with it, instead of waiting for some later broadcast to fail.
                None => return,
            },
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
            Arc::from("test-node"),
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
            Arc::from("test-node"),
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
            Arc::from("test-node"),
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
            Arc::from("test-node"),
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
            Arc::from("test-node"),
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
            Arc::from("test-node"),
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
            Arc::from("test-node"),
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
            Arc::from("test-node"),
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
            Arc::from("test-node"),
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
            Arc::from("test-node"),
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

    /// The one thing a write-only replica connection cannot do: notice its follower is gone.
    /// Before this plan, `serve_replica` parked on `rx.recv()` forever after handing over the
    /// snapshot, so a departed replica's connection task stayed alive -- and its `ClientGuard`
    /// undropped -- until some later broadcast happened to fail on the dead socket. That is why
    /// `a_registered_replica_is_pruned_after_its_connection_drops` has to drive two writes to
    /// observe pruning at all. Reading the connection makes the disconnect observable with no
    /// write traffic whatsoever, which is what this test pins.
    #[tokio::test]
    async fn a_replica_connection_is_released_as_soon_as_the_follower_disconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-disconnect-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            Arc::from("test-node"),
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

        // Drain the whole handshake first, so `serve_replica` is provably past its three
        // `write_all` calls and parked in its main loop. Dropping the socket before that could
        // end the connection through a failed write instead, which would prove nothing.
        use tokio::io::AsyncReadExt;
        let mut len_buf = [0u8; 8];
        parts.io.read_exact(&mut len_buf).await.unwrap();
        let mut blob = vec![0u8; u64::from_le_bytes(len_buf) as usize];
        parts.io.read_exact(&mut blob).await.unwrap();
        assert_eq!(replication.connected_clients(), 1);

        drop(parts.io); // the follower goes away, and nothing is ever written to it again

        // Bounded poll, not a fixed sleep: the deadline is what makes the old write-only
        // behavior (which never notices, ever) a failure rather than a slow pass.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while replication.connected_clients() != 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the leader never released the replica connection after the follower \
                 disconnected; it is still write-only"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// A follower is allowed to say things this leader does not understand. A well-formed frame
    /// with no handler is ignored, and bytes the RESP codec cannot parse at all cost the leader
    /// this follower's ack information -- nothing more. Neither may draw a reply, and neither
    /// may take the replication stream down: an older follower build that speaks a dialect this
    /// leader has never heard of must keep replicating.
    #[tokio::test]
    async fn a_replica_sending_unparseable_bytes_keeps_receiving_the_write_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-garbage-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            Arc::from("test-node"),
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

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut len_buf = [0u8; 8];
        parts.io.read_exact(&mut len_buf).await.unwrap();
        let mut blob = vec![0u8; u64::from_le_bytes(len_buf) as usize];
        parts.io.read_exact(&mut blob).await.unwrap();

        // A well-formed frame with no handler, then bytes that are not RESP at all. `g` is not
        // a RESP type byte, so `parse_frame` errors on it and consumes nothing.
        parts
            .io
            .write_all(b"*1\r\n$4\r\nPING\r\ngarbage\r\n")
            .await
            .unwrap();

        // Drive a write through an ordinary client. It must arrive on the replica socket, and
        // it must be the very next bytes on it -- proving the leader neither replied to
        // anything above nor tore the connection down over it.
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

        let expected = b"*3\r\n$3\r\nSET\r\n$3\r\nnew\r\n$5\r\nvalue\r\n";
        let mut streamed = vec![0u8; expected.len()];
        parts
            .io
            .read_exact(&mut streamed)
            .await
            .expect("the replica must still be receiving the write stream");
        assert_eq!(streamed, expected);
    }

    /// The parser's accepted shapes, pinned directly rather than through a socket. The
    /// five-element case is the one that matters: a real `redis-server` replica from 7.4 onward
    /// appends `FACK <offset>`, and rejecting that would leave a genuine Redis replica looking
    /// like it had never acked -- which write fencing would later read as a replica that does not
    /// count.
    #[test]
    fn parse_replconf_ack_accepts_a_real_redis_replicas_longer_ack() {
        fn ack(parts: &[&[u8]]) -> protocol::Frame {
            protocol::Frame::Array(
                parts
                    .iter()
                    .map(|p| protocol::Frame::Bulk(Bytes::copy_from_slice(p)))
                    .collect(),
            )
        }

        assert_eq!(
            parse_replconf_ack(&ack(&[b"REPLCONF", b"ACK", b"7"])),
            Some(7)
        );
        // Redis >= 7.4 appends its fsynced offset. The leading offset is still ours.
        assert_eq!(
            parse_replconf_ack(&ack(&[b"REPLCONF", b"ACK", b"7", b"FACK", b"4"])),
            Some(7)
        );
        // Case-insensitive on both words, as real clients vary.
        assert_eq!(
            parse_replconf_ack(&ack(&[b"replconf", b"ack", b"9"])),
            Some(9)
        );

        // Still rejected, and none of these may panic.
        assert_eq!(parse_replconf_ack(&ack(&[b"REPLCONF", b"ACK"])), None);
        assert_eq!(
            parse_replconf_ack(&ack(&[b"REPLCONF", b"GETACK", b"7"])),
            None
        );
        assert_eq!(parse_replconf_ack(&ack(&[b"PING", b"ACK", b"7"])), None);
        assert_eq!(
            parse_replconf_ack(&ack(&[b"REPLCONF", b"ACK", b"-1"])),
            None
        );
        assert_eq!(
            parse_replconf_ack(&ack(&[b"REPLCONF", b"ACK", b"nope"])),
            None
        );
        assert_eq!(
            parse_replconf_ack(&ack(&[b"REPLCONF", b"ACK", b"99999999999999999999999"])),
            None
        );
        assert_eq!(parse_replconf_ack(&protocol::Frame::Null), None);
    }

    /// The point of making the connection bidirectional: an ack a follower sends up the same
    /// socket lands on that replica's registry entry, where `INFO` and (later) fencing can read
    /// it.
    #[tokio::test]
    async fn a_replconf_ack_from_a_replica_is_recorded_on_its_registry_entry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-ack-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            Arc::from("test-node"),
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
        let mut parts = framed.into_parts();

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut len_buf = [0u8; 8];
        parts.io.read_exact(&mut len_buf).await.unwrap();
        let mut blob = vec![0u8; u64::from_le_bytes(len_buf) as usize];
        parts.io.read_exact(&mut blob).await.unwrap();

        parts
            .io
            .write_all(b"*3\r\n$8\r\nREPLCONF\r\n$3\r\nACK\r\n$4\r\n4096\r\n")
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let states = replication.registry.states();
            if states.len() == 1 && states[0].ack_offset == 4096 {
                assert!(
                    states[0].last_ack_unix > 1_700_000_000,
                    "an ack must stamp a real timestamp, got {}",
                    states[0].last_ack_unix
                );
                assert_eq!(states[0].addr.as_deref(), Some("127.0.0.1:6480"));
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the leader never recorded the replica's ack: {states:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// The `read_buf` hand-off, made observable. A follower is free to pipeline its first ack
    /// into the same write as its `PSYNC`, in which case the RESP codec has already pulled those
    /// bytes off the socket while decoding `PSYNC` -- they live in `FramedParts::read_buf` and
    /// nowhere else. `serve_replica` seeds its inbound reader with that buffer; if it ever stops
    /// doing so, the ack is gone with no error anywhere and this test is what catches it.
    ///
    /// One `write_all` of both frames, so on loopback they land in a single read and the codec
    /// genuinely reads ahead. Sending them as two writes would usually put the ack in a separate
    /// read, where a dropped `read_buf` would not show up.
    #[tokio::test]
    async fn an_ack_pipelined_behind_psync_is_not_lost() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-pipelined-ack-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            Arc::from("test-node"),
        ));

        use tokio::io::AsyncWriteExt;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"*1\r\n$5\r\nPSYNC\r\n\
                  *3\r\n$8\r\nREPLCONF\r\n$3\r\nACK\r\n$1\r\n7\r\n",
            )
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let states = replication.registry.states();
            if states.len() == 1 && states[0].ack_offset == 7 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the ack pipelined behind PSYNC was dropped -- read_buf was not carried into \
                 the inbound reader: {states:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(stream);
    }

    /// A follower that never acks -- an older build, or any test that just PSYNCs -- stays a
    /// perfectly good replica with no ack information. `PING` here stands in for any well-formed
    /// frame this leader has no handler for: it must be ignored, not answered, and not fatal.
    #[tokio::test]
    async fn an_unrecognised_inbound_frame_leaves_the_replica_registered_and_unacked() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-unknown-frame-unused.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            Arc::from("test-node"),
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

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut len_buf = [0u8; 8];
        parts.io.read_exact(&mut len_buf).await.unwrap();
        let mut blob = vec![0u8; u64::from_le_bytes(len_buf) as usize];
        parts.io.read_exact(&mut blob).await.unwrap();

        // A frame with no handler, a REPLCONF subcommand this leader does not implement, and an
        // ack whose offset is not a number. None of the three may be recorded or answered.
        //
        // The fourth frame is a real ack, and it is what makes the assertions below mean
        // anything. Without it, "this replica has never acked" would also hold if the leader had
        // never read the socket at all -- the test would pass while proving nothing. `FramedRead`
        // decodes in arrival order, so observing this ack recorded proves the three ahead of it
        // were decoded first and deliberately ignored.
        parts
            .io
            .write_all(
                b"*1\r\n$4\r\nPING\r\n\
                  *3\r\n$8\r\nREPLCONF\r\n$14\r\nlistening-port\r\n$4\r\n6480\r\n\
                  *3\r\n$8\r\nREPLCONF\r\n$3\r\nACK\r\n$3\r\nabc\r\n\
                  *3\r\n$8\r\nREPLCONF\r\n$3\r\nACK\r\n$1\r\n1\r\n",
            )
            .await
            .unwrap();

        // A real write, driven after them, must still arrive -- and be the very next bytes on
        // this socket, proving nothing above drew a reply.
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

        let expected = b"*3\r\n$3\r\nSET\r\n$3\r\nnew\r\n$5\r\nvalue\r\n";
        let mut streamed = vec![0u8; expected.len()];
        parts.io.read_exact(&mut streamed).await.unwrap();
        assert_eq!(streamed, expected);

        // Bounded poll: the sentinel is recorded by the inbound arm, which races the outbound
        // arm the write above drove.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let states = replication.registry.states();
            assert_eq!(states.len(), 1);
            if states[0].ack_offset != 0 {
                // Exactly the sentinel's offset. Had any of the three malformed frames been
                // recorded instead of ignored, this would carry some other value -- and had the
                // leader answered one of them, the `SET` above would not have been the very next
                // bytes on this socket.
                assert_eq!(
                    states[0].ack_offset, 1,
                    "only the trailing sentinel may be recorded; the three frames before it are \
                     unrecognised and must be ignored"
                );
                assert!(states[0].last_ack_unix > 0);
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the sentinel ack was never recorded, so nothing proves the leader ever decoded \
                 the three frames ahead of it: {states:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    use crate::logging::test_support::CapturedLogs;

    /// The snapshot's own 8-byte header carries the leader's live replication offset to a
    /// newly-attaching follower. This is not a wire-format change: the field has always been
    /// transmitted on this path, it was just always zero.
    #[tokio::test]
    async fn psync_stamps_the_leaders_replication_offset_into_the_snapshot_header() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(crate::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            std::env::temp_dir().join("psync-test-unused-4.snapshot"),
        ));
        tokio::spawn(serve(
            listener,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            Arc::from("test-node"),
        ));

        // Take a write first, so the leader's offset is non-zero before any follower attaches.
        // A header that is still 0 here would be indistinguishable from the old hardcoded value.
        let mut client = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            RespCodec::default(),
        );
        client
            .send(Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"k")),
                Frame::Bulk(Bytes::from_static(b"v")),
            ]))
            .await
            .unwrap();
        assert_eq!(
            client.next().await.unwrap().unwrap(),
            Frame::Simple("OK".into())
        );
        let leader_offset = replication.master_repl_offset();
        assert!(
            leader_offset > 0,
            "the write should have advanced the leader offset"
        );

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

        // The blob's own first 8 bytes are the snapshot header, little-endian.
        let mut header = [0u8; 8];
        header.copy_from_slice(&blob[..8]);
        assert_eq!(
            u64::from_le_bytes(header),
            leader_offset,
            "the PSYNC snapshot header must carry the leader's live replication offset"
        );
    }

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
            Arc::from("test-node"),
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
            // `repl{host_port=`, not `repl` and `host_port` separately: `serve_replica`'s own
            // name contains `repl`, so the separate form would have passed under
            // `#[instrument]`'s default name too. Same reasoning as the span-name section at
            // the bottom of `tests/logging.rs`.
            text.contains("repl{host_port=") && text.contains("127.0.0.1:9999"),
            "expected a new `repl` span carrying host_port=\"127.0.0.1:9999\", got:\n{text}"
        );
    }

    // `a_replica_registering_and_being_pruned_are_both_logged_at_info` used to live here,
    // asserting on captured `INFO` output from a real PSYNC round-trip. It flaked: `tracing`
    // caches callsite `Interest` per callsite, process-globally, the first time a callsite is
    // reached -- and this unit-test binary runs ~575 tests whose subscribers install and drop
    // constantly, so whichever test hit the register/prune callsites first (often with no
    // subscriber at all) could poison them for the rest of the process, including this test's
    // own later `INFO` subscriber. It had no assertions beyond the captured text --
    // `psync_with_an_advertised_address_registers_it_on_the_leader` and
    // `a_registered_replica_is_pruned_after_its_connection_drops` above already cover the
    // behavioural side (registration observable via `replication.registry.addrs()`, and the
    // server staying alive and answering after a prune) -- so it was moved wholesale to
    // `crates/server/tests/logging.rs`, a separate integration-test binary with far fewer
    // tests/callsites where capture assertions have never flaked, driving the same scenario
    // through the public `rocket_mem::serve`. Do not re-add a capture assertion here.
}
