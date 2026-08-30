use redis::AsyncCommands;
use std::sync::Arc;
use tokio::net::TcpListener;

async fn spawn_test_server() -> (tempfile::TempDir, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().unwrap();
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("test.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    tokio::spawn(rocket_mem::serve(
        listener,
        engine,
        aof,
        Arc::new(rocket_mem::replication::ReplicationHandle::default()),
    ));
    (dir, format!("redis://{addr}"))
}

#[tokio::test]
async fn redis_rs_client_can_set_and_get_over_real_tcp() {
    let (_dir, url) = spawn_test_server().await;
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    let _: () = con.set("foo", "bar").await.unwrap();
    let value: String = con.get("foo").await.unwrap();
    assert_eq!(value, "bar");
}

#[tokio::test]
async fn redis_rs_client_runs_a_mixed_workload_across_all_sprint_1_data_types() {
    let (_dir, url) = spawn_test_server().await;
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    let _: () = con.set("str", "hello").await.unwrap();
    let _: i64 = con.incr("counter", 5).await.unwrap();
    let _: () = con.hset("hash", "field", "value").await.unwrap();
    let _: () = con.rpush("list", "a").await.unwrap();
    let _: () = con.sadd("set", "member").await.unwrap();

    let str_val: String = con.get("str").await.unwrap();
    let hash_val: String = con.hget("hash", "field").await.unwrap();
    let is_member: bool = con.sismember("set", "member").await.unwrap();

    assert_eq!(str_val, "hello");
    assert_eq!(hash_val, "value");
    assert!(is_member);
}

#[tokio::test]
async fn redis_rs_client_gets_a_real_error_on_wrongtype() {
    let (_dir, url) = spawn_test_server().await;
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    let _: () = con.set("k", "v").await.unwrap();
    let result: redis::RedisResult<()> = con.hset("k", "f", "v").await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("WRONGTYPE"));
}

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[tokio::test]
async fn malformed_resp_input_gets_a_graceful_disconnect_not_a_crash() {
    let (_dir, url) = spawn_test_server().await;
    let addr = url.strip_prefix("redis://").unwrap();

    let mut raw = TcpStream::connect(addr).await.unwrap();
    raw.write_all(b"@this is not RESP\r\n").await.unwrap();
    // the connection should close (EOF), not hang or echo garbage back
    let mut buf = [0u8; 16];
    let n = raw.read(&mut buf).await.unwrap();
    assert_eq!(n, 0, "expected EOF on malformed input, got {n} bytes back");

    // the server itself must still be up — a fresh, well-formed connection works fine
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = redis::AsyncCommands::set(&mut con, "still-alive", "yes")
        .await
        .unwrap();
}
