use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use protocol::rmp::{MsgType, RmpCodec, RmpMessage};
use protocol::Frame;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::codec::Framed;

#[derive(Debug)]
pub enum RmpError {
    Io(std::io::Error),
    ConnectionClosed,
    ServerError(String),
    UnexpectedReply(Frame),
}

impl std::fmt::Display for RmpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RmpError::Io(e) => write!(f, "rmp io error: {e}"),
            RmpError::ConnectionClosed => write!(f, "rmp connection closed"),
            RmpError::ServerError(msg) => write!(f, "{msg}"),
            RmpError::UnexpectedReply(frame) => write!(f, "unexpected rmp reply: {frame:?}"),
        }
    }
}

impl std::error::Error for RmpError {}

impl From<std::io::Error> for RmpError {
    fn from(e: std::io::Error) -> Self {
        RmpError::Io(e)
    }
}

/// State shared between `call` (which registers a pending reply) and the reader task
/// (which resolves or, on disconnect, drops it). The lock is only ever held for the
/// duration of a single insert/remove/clear -- never across an `.await` -- so it never
/// blocks either side for longer than a `HashMap` operation.
struct Shared {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Frame>>>,
}

/// A minimal async client for the RMP wire protocol. Opens one TCP connection and lets
/// callers issue requests concurrently on it: each `call` gets its own `request_id` and
/// its own reply channel, so replies can come back in any order -- the reader task
/// demultiplexes them by id instead of assuming request/response order line up.
pub struct RmpClient {
    write_tx: mpsc::UnboundedSender<RmpMessage>,
    shared: Arc<Shared>,
}

impl RmpClient {
    /// Opens the TCP connection and spawns the writer and reader tasks that drive it.
    pub async fn connect(addr: impl tokio::net::ToSocketAddrs) -> Result<Self, RmpError> {
        let socket = TcpStream::connect(addr).await?;
        let framed = Framed::new(socket, RmpCodec);
        let (mut sink, mut stream) = framed.split();

        let shared = Arc::new(Shared {
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
        });

        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<RmpMessage>();
        tokio::spawn(async move {
            while let Some(msg) = write_rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break; // the peer went away; nothing left to do but stop writing
                }
            }
        });

        let reader_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            while let Some(next) = stream.next().await {
                let Ok(msg) = next else { break };
                if msg.msg_type != MsgType::Response {
                    continue; // a stray Request from the server would be a protocol violation
                }
                if let Some(tx) = reader_shared
                    .pending
                    .lock()
                    .unwrap()
                    .remove(&msg.request_id)
                {
                    let _ = tx.send(msg.frame);
                }
            }
            // The connection ended: fail every reply still waiting instead of hanging forever.
            // Dropping each Sender fails its matching Receiver with a RecvError, which `call`
            // below maps to RmpError::ConnectionClosed.
            reader_shared.pending.lock().unwrap().clear();
        });

        Ok(RmpClient { write_tx, shared })
    }

    /// Sends `args` as a Request (encoded as an Array of Bulk strings) and awaits the
    /// matching Response, correlated by `request_id`. The primitive every higher-level
    /// command (GET, SET, DEL, ...) is built on.
    pub async fn call(&self, args: Vec<Bytes>) -> Result<Frame, RmpError> {
        let request_id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(request_id, tx);
        let command = Frame::Array(args.into_iter().map(Frame::Bulk).collect());
        self.write_tx
            .send(RmpMessage {
                request_id,
                msg_type: MsgType::Request,
                frame: command,
            })
            .map_err(|_| RmpError::ConnectionClosed)?;
        rx.await.map_err(|_| RmpError::ConnectionClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::{SinkExt, StreamExt};
    use protocol::rmp::{MsgType, RmpCodec, RmpMessage};
    use tokio::net::TcpListener;
    use tokio_util::codec::Framed;

    #[test]
    fn server_error_displays_its_message() {
        let err = RmpError::ServerError("WRONGTYPE bad".to_string());
        assert_eq!(err.to_string(), "WRONGTYPE bad");
    }

    #[test]
    fn connection_closed_has_a_stable_message() {
        assert_eq!(
            RmpError::ConnectionClosed.to_string(),
            "rmp connection closed"
        );
    }

    #[tokio::test]
    async fn call_sends_the_command_as_an_array_of_bulk_strings_and_returns_the_reply() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut framed = Framed::new(socket, RmpCodec);
            let request = framed.next().await.unwrap().unwrap();
            assert_eq!(request.msg_type, MsgType::Request);
            assert_eq!(
                request.frame,
                Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"GET")),
                    Frame::Bulk(Bytes::from_static(b"foo")),
                ])
            );
            framed
                .send(RmpMessage {
                    request_id: request.request_id,
                    msg_type: MsgType::Response,
                    frame: Frame::Bulk(Bytes::from_static(b"bar")),
                })
                .await
                .unwrap();
        });

        let client = RmpClient::connect(addr).await.unwrap();
        let reply = client
            .call(vec![Bytes::from_static(b"GET"), Bytes::from_static(b"foo")])
            .await
            .unwrap();
        assert_eq!(reply, Frame::Bulk(Bytes::from_static(b"bar")));
    }

    #[tokio::test]
    async fn call_correlates_replies_by_request_id_even_when_the_server_answers_out_of_order() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut framed = Framed::new(socket, RmpCodec);
            let r1 = framed.next().await.unwrap().unwrap();
            let r2 = framed.next().await.unwrap().unwrap();
            // Deliberately answer the second request first -- proves the client doesn't assume
            // reply order matches request order.
            framed
                .send(RmpMessage {
                    request_id: r2.request_id,
                    msg_type: MsgType::Response,
                    frame: Frame::Simple("OK".into()),
                })
                .await
                .unwrap();
            framed
                .send(RmpMessage {
                    request_id: r1.request_id,
                    msg_type: MsgType::Response,
                    frame: Frame::Integer(42),
                })
                .await
                .unwrap();
        });

        let client = RmpClient::connect(addr).await.unwrap();
        let (r1, r2) = tokio::join!(
            client.call(vec![Bytes::from_static(b"GET"), Bytes::from_static(b"a")]),
            client.call(vec![
                Bytes::from_static(b"SET"),
                Bytes::from_static(b"b"),
                Bytes::from_static(b"1")
            ]),
        );
        assert_eq!(r1.unwrap(), Frame::Integer(42));
        assert_eq!(r2.unwrap(), Frame::Simple("OK".into()));
    }

    #[tokio::test]
    async fn call_fails_with_connection_closed_once_the_peer_disconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket); // disconnect immediately, without ever replying
        });

        let client = RmpClient::connect(addr).await.unwrap();
        let result = client.call(vec![Bytes::from_static(b"PING")]).await;
        assert!(matches!(result, Err(RmpError::ConnectionClosed)));
    }
}
