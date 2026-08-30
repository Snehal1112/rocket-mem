// crates/server/tests/metrics.rs
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use protocol::codec::RespCodec;
use protocol::Frame;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

async fn scrape(addr: std::net::SocketAddr) -> String {
    let mut socket = TcpStream::connect(addr).await.unwrap();
    socket
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn command_counts_and_latencies_appear_in_the_prometheus_output() {
    let handle = rocket_mem::metrics::recorder_handle();
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("node.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&engine),
        dir.path().join("node.snapshot"),
    ));

    let resp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let resp_addr = resp_listener.local_addr().unwrap();
    tokio::spawn(rocket_mem::serve(
        resp_listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));

    let metrics_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let metrics_addr = metrics_listener.local_addr().unwrap();
    tokio::spawn(rocket_mem::metrics::serve_metrics(
        metrics_listener,
        handle,
        Arc::clone(&engine),
        Arc::clone(&replication),
    ));

    let mut client = Framed::new(
        TcpStream::connect(resp_addr).await.unwrap(),
        RespCodec::default(),
    );
    for command in [
        vec![b"SET".to_vec(), b"k".to_vec(), b"v".to_vec()],
        vec![b"GET".to_vec(), b"k".to_vec()],
        vec![b"GET".to_vec(), b"k".to_vec()],
        vec![b"NOSUCHCOMMAND".to_vec()],
    ] {
        client
            .send(Frame::Array(
                command
                    .into_iter()
                    .map(|p| Frame::Bulk(Bytes::from(p)))
                    .collect(),
            ))
            .await
            .unwrap();
        client.next().await.unwrap().unwrap();
    }

    let body = scrape(metrics_addr).await;
    assert!(
        body.contains(r#"rocket_mem_commands_total{cmd="set"} 1"#),
        "{body}"
    );
    assert!(
        body.contains(r#"rocket_mem_commands_total{cmd="get"} 2"#),
        "{body}"
    );
    // an unknown command is counted, but collapsed into the bounded `other` label
    assert!(
        body.contains(r#"rocket_mem_commands_total{cmd="other"} 1"#),
        "{body}"
    );
    assert!(
        body.contains(r#"rocket_mem_command_errors_total{cmd="other"} 1"#),
        "{body}"
    );
    assert!(
        body.contains(r#"rocket_mem_command_duration_seconds_bucket{cmd="set","#),
        "{body}"
    );
    assert!(body.contains("rocket_mem_keys 1"), "{body}");
    assert!(body.contains("rocket_mem_connected_clients 1"), "{body}");
    assert!(body.contains("rocket_mem_connected_replicas 0"), "{body}");
    assert!(body.contains("rocket_mem_connections_total 1"), "{body}");
}
