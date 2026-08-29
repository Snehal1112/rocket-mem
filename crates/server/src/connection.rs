use crate::dispatcher;
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use protocol::codec::{Protocol, RespCodec};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::codec::Framed;

pub async fn serve(listener: TcpListener, engine: Arc<Engine>) {
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

async fn handle_connection(socket: tokio::net::TcpStream, engine: Arc<Engine>, client_id: u64) {
    let mut framed = Framed::new(socket, RespCodec::default());
    let mut protocol = Protocol::default();
    while let Some(result) = framed.next().await {
        let frame = match result {
            Ok(frame) => frame,
            Err(_) => return, // malformed input or a dropped connection — end this task quietly
        };
        let response = dispatcher::dispatch(&engine, frame, &mut protocol, client_id);
        framed.codec_mut().protocol = protocol; // sync BEFORE sending this reply
        if framed.send(response).await.is_err() {
            return; // client went away mid-response
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
}
