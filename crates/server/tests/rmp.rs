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
