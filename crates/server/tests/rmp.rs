use bytes::Bytes;
use protocol::rmp::{MsgType, RmpCodec, RmpMessage};
use protocol::Frame;
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

fn command(args: &[&[u8]]) -> Frame {
    Frame::Array(
        args.iter()
            .map(|a| Frame::Bulk(Bytes::copy_from_slice(a)))
            .collect(),
    )
}
