use bytes::Bytes;
use redis::AsyncCommands;
use std::sync::Arc;
use tokio::net::TcpListener;

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

#[tokio::test]
async fn resp_write_is_visible_to_a_read_over_rmp() {
    let (_dir, resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let redis_client = redis::Client::open(resp_url).unwrap();
    let mut resp_con = redis_client.get_multiplexed_async_connection().await.unwrap();
    let _: () = resp_con.set("k", "v").await.unwrap();

    let rmp_client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();
    assert_eq!(rmp_client.get("k").await.unwrap(), Some(Bytes::from_static(b"v")));
}

#[tokio::test]
async fn rmp_write_is_visible_to_a_read_over_resp() {
    let (_dir, resp_url, rmp_addr) = spawn_dual_protocol_server().await;
    let rmp_client = rmp_client::RmpClient::connect(rmp_addr).await.unwrap();
    rmp_client.set("k", "v").await.unwrap();

    let redis_client = redis::Client::open(resp_url).unwrap();
    let mut resp_con = redis_client.get_multiplexed_async_connection().await.unwrap();
    let value: String = resp_con.get("k").await.unwrap();
    assert_eq!(value, "v");
}
