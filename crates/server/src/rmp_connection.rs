use crate::aof::AofWriter;
use crate::connection::ClientGuard;
use crate::dispatcher;
use crate::replication::ReplicationHandle;
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use protocol::codec::Protocol;
use protocol::rmp::{MsgType, RmpCodec, RmpMessage};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::Semaphore;
use tokio_util::codec::Framed;

/// Caps how many requests on one RMP connection can be mid-dispatch at once. Once the
/// cap is hit, the read loop's next `semaphore.acquire_owned().await` blocks -- it stops
/// reading more requests off the socket, which applies ordinary TCP backpressure to
/// whatever sent them, mirroring how RESP's sequential loop already gets backpressure
/// for free. Without this, a client that pipelines aggressively and never reads its
/// replies could make the server spawn unbounded tasks and queue unbounded encoded
/// replies in memory.
const MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION: usize = 256;

pub async fn serve(
    listener: TcpListener,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
) {
    let mut next_client_id: u64 = 1;
    loop {
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
    let framed = Framed::new(socket, RmpCodec);
    let (mut sink, mut stream) = framed.split();

    // Every spawned request-handling task below gets its own clone of `tx`; this loop's own
    // clone is dropped when the read loop ends. The writer task's `rx.recv()` only returns
    // `None` once every clone has dropped -- i.e. once every in-flight task has also finished
    // and sent (or failed to send) its reply -- so a client disconnecting mid-flight still gets
    // every reply that was already in progress written out before the connection fully closes.
    let (tx, mut rx) = mpsc::channel::<RmpMessage>(MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION);
    let semaphore = Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION));

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(msg).await.is_err() {
                break; // the client went away; nothing left to do but stop writing
            }
        }
    });

    while let Some(next) = stream.next().await {
        let request = match next {
            Ok(msg) if msg.msg_type == MsgType::Request => msg,
            Ok(_) => break, // a stray Response from a misbehaving client
            Err(e) => {
                eprintln!("rmp decode error: {e}");
                break;
            }
        };
        // Blocks once MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION tasks are already mid-dispatch --
        // that's the backpressure: the read loop stops pulling more requests off the socket
        // until one finishes and its permit is released.
        let permit = match Arc::clone(&semaphore).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break, // semaphore closed -- only happens if it were explicitly closed, which nothing here does
        };
        let engine = Arc::clone(&engine);
        let aof = Arc::clone(&aof);
        let replication = Arc::clone(&replication);
        let tx = tx.clone();
        // Spawned, not awaited inline: the read loop must go straight back to decoding the next
        // request without waiting for this one's reply -- that's what makes multiple in-flight
        // requests on one connection possible at all.
        tokio::spawn(async move {
            let _permit = permit; // released (dropped) when this task ends, freeing a slot
            let mut protocol = Protocol::default(); // RMP has no negotiation state to persist
            let reply = dispatcher::dispatch_and_log(
                &engine,
                &aof,
                &replication,
                request.frame,
                &mut protocol,
                client_id,
            );
            let _ = tx
                .send(RmpMessage {
                    request_id: request.request_id,
                    msg_type: MsgType::Response,
                    frame: reply,
                })
                .await; // bounded channel: send is now async
        });
    }
    drop(tx);
    let _ = writer.await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use protocol::Frame;
    use tokio::net::TcpStream;

    fn test_aof() -> (tempfile::TempDir, Arc<AofWriter>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, crate::aof::FsyncPolicy::Never).unwrap();
        (dir, Arc::new(writer))
    }

    async fn spawn_test_server() -> (tempfile::TempDir, std::net::SocketAddr, Arc<Engine>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (dir, aof) = test_aof();
        let replication = Arc::new(ReplicationHandle::default());
        tokio::spawn(serve(listener, Arc::clone(&engine), aof, replication));
        (dir, addr, engine)
    }

    async fn connect(addr: std::net::SocketAddr) -> Framed<TcpStream, RmpCodec> {
        Framed::new(TcpStream::connect(addr).await.unwrap(), RmpCodec)
    }

    fn command(args: &[&[u8]]) -> Frame {
        Frame::Array(
            args.iter()
                .map(|a| Frame::Bulk(Bytes::copy_from_slice(a)))
                .collect(),
        )
    }

    #[tokio::test]
    async fn set_then_get_round_trips_over_a_real_socket() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut con = connect(addr).await;

        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"foo", b"bar"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(reply.request_id, 1);
        assert_eq!(reply.frame, Frame::Simple("OK".into()));

        con.send(RmpMessage {
            request_id: 2,
            msg_type: MsgType::Request,
            frame: command(&[b"GET", b"foo"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(reply.request_id, 2);
        assert_eq!(reply.frame, Frame::Bulk(Bytes::from_static(b"bar")));
    }

    #[tokio::test]
    async fn a_write_over_rmp_updates_the_same_engine_a_second_rmp_connection_reads_from() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut a = connect(addr).await;
        let mut b = connect(addr).await;

        a.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"k", b"v"]),
        })
        .await
        .unwrap();
        assert_eq!(
            a.next().await.unwrap().unwrap().frame,
            Frame::Simple("OK".into())
        );

        b.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"GET", b"k"]),
        })
        .await
        .unwrap();
        assert_eq!(
            b.next().await.unwrap().unwrap().frame,
            Frame::Bulk(Bytes::from_static(b"v"))
        );
    }

    #[tokio::test]
    async fn two_requests_sent_without_waiting_both_get_correct_replies_regardless_of_order() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut con = connect(addr).await;

        con.send(RmpMessage {
            request_id: 10,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"a", b"1"]),
        })
        .await
        .unwrap();
        con.send(RmpMessage {
            request_id: 20,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"b", b"2"]),
        })
        .await
        .unwrap();

        let mut replies = std::collections::HashMap::new();
        for _ in 0..2 {
            let reply = con.next().await.unwrap().unwrap();
            replies.insert(reply.request_id, reply.frame);
        }
        assert_eq!(replies[&10], Frame::Simple("OK".into()));
        assert_eq!(replies[&20], Frame::Simple("OK".into()));
    }

    #[tokio::test]
    async fn an_unknown_command_gets_the_same_error_shape_resp_would() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut con = connect(addr).await;

        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"NOTACOMMAND"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert!(matches!(reply.frame, Frame::Error(_)));
    }

    #[tokio::test]
    async fn serve_tracks_connected_rmp_clients_and_drops_the_count_on_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(ReplicationHandle::default());
        tokio::spawn(serve(listener, engine, aof, Arc::clone(&replication)));

        let mut con = connect(addr).await;
        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"PING"]),
        })
        .await
        .unwrap();
        con.next().await.unwrap().unwrap();
        assert_eq!(replication.connected_clients(), 1);
        assert_eq!(replication.total_connections(), 1);

        drop(con);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(replication.connected_clients(), 0);
        assert_eq!(replication.total_connections(), 1); // the lifetime total never drops
    }

    #[tokio::test]
    async fn more_than_the_in_flight_cap_concurrent_requests_all_still_succeed() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let con = std::sync::Arc::new(tokio::sync::Mutex::new(connect(addr).await));

        // 2x the cap, all fired without waiting for any individual reply first -- proves
        // the semaphore-based cap throttles (pauses reading more requests) rather than
        // ever dropping, corrupting, or deadlocking a request once the cap is in play.
        let total = MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION * 2;
        for i in 0..total as u64 {
            let mut con = con.lock().await;
            con.send(RmpMessage {
                request_id: i,
                msg_type: MsgType::Request,
                frame: command(&[b"PING"]),
            })
            .await
            .unwrap();
        }

        let mut seen = std::collections::HashSet::new();
        for _ in 0..total {
            let mut con = con.lock().await;
            let reply = con.next().await.unwrap().unwrap();
            assert_eq!(reply.frame, Frame::Simple("PONG".into()));
            seen.insert(reply.request_id);
        }
        assert_eq!(seen.len(), total);
    }

    #[tokio::test]
    async fn the_server_survives_a_client_disconnecting_immediately() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let stream = TcpStream::connect(addr).await.unwrap();
        drop(stream);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // a second, independent connection must still work
        let mut con = connect(addr).await;
        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"PING"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(reply.frame, Frame::Simple("PONG".into()));
    }
}
