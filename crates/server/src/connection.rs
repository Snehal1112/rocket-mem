use crate::dispatcher;
use engine::Engine;
use futures_util::{FutureExt, SinkExt, StreamExt};
use protocol::codec::{Protocol, RespCodec};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::codec::Framed;

pub async fn serve(listener: TcpListener, engine: Arc<Engine>) {
    tokio::spawn(active_expire_loop(Arc::clone(&engine)));

    let mut next_client_id: u64 = 1;
    loop {
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        let client_id = next_client_id;
        next_client_id += 1;
        let engine = Arc::clone(&engine);
        tokio::spawn(handle_connection(socket, engine, client_id));
    }
}

/// Sweeps one shard per tick, rotating through all 16 — see
/// ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md's active-expiry decision for why a
/// whole-shard sweep (not per-key sampling) is the deliberate simplification here.
async fn active_expire_loop(engine: Arc<Engine>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut shard_idx: usize = 0;
    loop {
        interval.tick().await;
        engine.active_expire_cycle(shard_idx);
        shard_idx = shard_idx.wrapping_add(1);
    }
}

async fn handle_connection(socket: tokio::net::TcpStream, engine: Arc<Engine>, client_id: u64) {
    let mut framed = Framed::new(socket, RespCodec::default());
    let mut protocol = Protocol::default();
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
        let response = dispatcher::dispatch(&engine, frame, &mut protocol, client_id);
        framed.codec_mut().protocol = protocol; // sync BEFORE sending this reply
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

    #[tokio::test]
    async fn serve_handles_a_full_set_get_round_trip_over_a_real_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        tokio::spawn(serve(listener, engine));

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
        tokio::spawn(serve(listener, engine));

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
        tokio::spawn(serve(listener, engine));

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
        tokio::spawn(serve(listener, engine.clone()));

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
}
