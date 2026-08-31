use bytes::Bytes;
use futures_util::SinkExt;
use protocol::rmp::{MsgType, RmpCodec, RmpMessage};
use protocol::Frame;
use redis::AsyncCommands;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::codec::Framed;

async fn spawn_dual_protocol_server() -> (tempfile::TempDir, String, std::net::SocketAddr) {
    let engine = Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().unwrap();
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("test.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::default());

    let resp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let resp_addr = resp_listener.local_addr().unwrap();
    let rmp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rmp_addr = rmp_listener.local_addr().unwrap();

    tokio::spawn(rocket_mem::serve(
        resp_listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));
    tokio::spawn(rocket_mem::rmp_connection::serve(
        rmp_listener,
        engine,
        aof,
        replication,
    ));

    (dir, format!("redis://{resp_addr}"), rmp_addr)
}

fn command(args: &[&[u8]]) -> Frame {
    Frame::Array(
        args.iter()
            .map(|a| Frame::Bulk(Bytes::copy_from_slice(a)))
            .collect(),
    )
}

#[tokio::test]
async fn resp_write_is_visible_to_a_read_over_rmp() {
    let (_dir, resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let redis_client = redis::Client::open(resp_url).unwrap();
    let mut resp_con = redis_client
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let _: () = resp_con.set("k", "v").await.unwrap();

    let rmp_client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();
    assert_eq!(
        rmp_client.get("k").await.unwrap(),
        Some(Bytes::from_static(b"v"))
    );
}

#[tokio::test]
async fn rmp_write_is_visible_to_a_read_over_resp() {
    let (_dir, resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let rmp_client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();
    rmp_client.set("k", "v").await.unwrap();

    let redis_client = redis::Client::open(resp_url).unwrap();
    let mut resp_con = redis_client
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let value: String = resp_con.get("k").await.unwrap();
    assert_eq!(value, "v");
}

#[tokio::test]
async fn rmp_correctly_multiplexes_concurrent_requests_on_one_connection() {
    let (_dir, _resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();
    client.set("a", "1").await.unwrap();

    // Fired concurrently on the same connection, without either awaiting the other first.
    let (get_result, set_result) = tokio::join!(client.get("a"), client.set("b", "2"));
    assert_eq!(get_result.unwrap(), Some(Bytes::from_static(b"1")));
    set_result.unwrap();
    assert_eq!(
        client.get("b").await.unwrap(),
        Some(Bytes::from_static(b"2"))
    );
}

#[tokio::test]
async fn rmp_reaches_info_and_cluster_commands() {
    let (_dir, _resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();

    let info = client
        .call(vec![Bytes::from_static(b"INFO")])
        .await
        .unwrap();
    match info {
        Frame::Bulk(b) => assert!(String::from_utf8_lossy(&b).contains("# Server")),
        other => panic!("expected INFO to reply Bulk, got {other:?}"),
    }

    // 12182 is the known reference value for key_slot(b"foo") (Sprint 6 spec), independent of
    // whether cluster mode is configured -- CLUSTER KEYSLOT is a pure function of the key.
    let slot = client
        .call(vec![
            Bytes::from_static(b"CLUSTER"),
            Bytes::from_static(b"KEYSLOT"),
            Bytes::from_static(b"foo"),
        ])
        .await
        .unwrap();
    assert_eq!(slot, Frame::Integer(12182));
}

#[tokio::test]
async fn the_server_survives_an_rmp_client_disconnecting_before_reading_its_reply() {
    let (_dir, _resp_url, rmp_addr) = spawn_dual_protocol_server().await;

    {
        let socket = tokio::net::TcpStream::connect(rmp_addr).await.unwrap();
        let mut framed = Framed::new(socket, RmpCodec);
        framed
            .send(RmpMessage {
                request_id: 1,
                msg_type: MsgType::Request,
                frame: command(&[b"PING"]),
            })
            .await
            .unwrap();
        // `framed` (and its socket) drops here, before the reply is ever read.
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // A second, independent connection must still work -- proves the dropped connection's
    // spawned tasks and writer loop didn't take the whole listener down.
    let client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();
    assert_eq!(
        client
            .call(vec![Bytes::from_static(b"PING")])
            .await
            .unwrap(),
        Frame::Simple("PONG".into())
    );
}

// Runs on a multi-threaded runtime deliberately: on the default single-threaded
// `#[tokio::test]` runtime, `DEBUG SLEEP`'s blocking `std::thread::sleep` blocks the
// *entire* executor -- including the read loop that would otherwise pick up PING while
// SLEEP is still in flight -- so PING only ever gets dispatched after SLEEP completes.
// The two replies then land on the wire within microseconds of each other, and simply
// recording *which reply's continuation runs first* is not a reliable signal at that
// point: Tokio's LIFO-slot scheduling optimization can (and, verified empirically while
// writing this test, reliably does) run the second-woken continuation before the first
// one queued, making a push-order assertion pass even against a fully sequential
// connection handler -- a tautology. Requiring `flavor = "multi_thread"` here lets PING
// actually get dispatched on a different worker thread while SLEEP's thread blocks, so
// this test's timing bound is measuring genuine concurrent dispatch.
#[tokio::test(flavor = "multi_thread")]
async fn rmp_genuinely_delivers_a_fast_reply_before_a_slower_concurrent_request_on_one_connection()
{
    let (_dir, _resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();
    let started = std::time::Instant::now();

    let slow = async {
        let reply = client
            .call(vec![
                Bytes::from_static(b"DEBUG"),
                Bytes::from_static(b"SLEEP"),
                Bytes::from_static(b"0.3"),
            ])
            .await;
        (reply, started.elapsed())
    };

    let fast = async {
        let reply = client.call(vec![Bytes::from_static(b"PING")]).await;
        (reply, started.elapsed())
    };

    let ((slow_result, slow_elapsed), (fast_result, fast_elapsed)) = tokio::join!(slow, fast);
    assert_eq!(slow_result.unwrap(), Frame::Simple("OK".into()));
    assert_eq!(fast_result.unwrap(), Frame::Simple("PONG".into()));

    // The real proof: PING's own round trip completed in a small fraction of the 300ms
    // DEBUG SLEEP is blocked for, even though slow's request was fired first in program
    // order -- i.e. the server genuinely dispatched PING *while* SLEEP was still running,
    // not merely "queued PING's reply ahead of SLEEP's by coincidence of scheduling." A
    // sequential connection handler would make fast_elapsed converge on slow_elapsed
    // (~300ms) instead of staying near zero.
    assert!(
        fast_elapsed < std::time::Duration::from_millis(150),
        "PING took {fast_elapsed:?} to complete alongside a 300ms DEBUG SLEEP -- \
         expected well under 150ms, which would mean it was NOT served concurrently"
    );
    assert!(
        slow_elapsed >= std::time::Duration::from_millis(295),
        "DEBUG SLEEP 0.3 returned after only {slow_elapsed:?}"
    );
}
