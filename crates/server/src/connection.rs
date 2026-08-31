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
            eprintln!("aof fsync failed: {e}");
        }
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

async fn handle_connection(
    socket: tokio::net::TcpStream,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) {
    replication.connection_opened();
    let _client_guard = ClientGuard(Arc::clone(&replication));
    let mut framed = Framed::new(socket, RespCodec::default());
    let session = dispatcher::Session::new();
    // Carries a frame pulled ahead by the pipelining peek below, so it isn't re-read.
    let mut pending: Option<Option<std::io::Result<protocol::Frame>>> = None;
    loop {
        let next = match pending.take() {
            Some(n) => n,
            None => framed.next().await,
        };
        let frame = match next {
            Some(Ok(frame)) => frame,
            Some(Err(_)) | None => return, // malformed input or a dropped connection — end this task quietly
        };
        if is_psync_command(&frame) {
            // Same auth condition `dispatcher::auth_gate` enforces for every other command --
            // PSYNC never reaches `dispatch_and_log` (it's intercepted here, before the frame
            // loop even calls it), so without this check an unauthenticated client could send
            // PSYNC first and receive a full snapshot of the entire keyspace plus a live stream
            // of every subsequent write, bypassing the auth gate entirely.
            if !replication.acl.is_empty() && session.authenticated_user().is_none() {
                if framed
                    .send(protocol::Frame::Error(
                        "NOAUTH Authentication required.".into(),
                    ))
                    .await
                    .is_err()
                {
                    return; // client went away
                }
                continue; // let the client retry after AUTH/HELLO ... AUTH
            }
            serve_replica(framed, &aof, &replication).await;
            return; // serve_replica never returns until the replica connection dies
        }
        let response =
            dispatcher::dispatch_and_log(&engine, &aof, &replication, frame, &session, client_id);
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

/// Takes ownership of `framed`'s underlying socket and never returns until the replica
/// connection dies. `PSYNC` has no reply frame of its own — the length-prefixed snapshot blob
/// (not a RESP value) stands in for one.
async fn serve_replica(
    framed: Framed<tokio::net::TcpStream, RespCodec>,
    aof: &AofWriter,
    replication: &crate::replication::ReplicationHandle,
) {
    use tokio::io::AsyncWriteExt;

    // ONE critical section: snapshot + register, so no write can slip between them. Taken
    // separately, a write committing after the snapshot walk but before registration would
    // reach neither the blob nor the stream -- lost permanently, unrepairable by reconnect,
    // since a reconnect just snapshots a leader that has already moved past it. Lock
    // ordering: lock_for_ordering() before the registry's own mutex, matching this plan's
    // Global Constraints and the fan-out hook in dispatcher.rs, the only other place both
    // are taken.
    let (snapshot_bytes, mut rx) = {
        let _order_guard = aof.lock_for_ordering();
        let bytes = replication.engine().snapshot(0); // 0: a follower keeps no AOF, so the header is moot
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        replication.registry.register(tx);
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
}
