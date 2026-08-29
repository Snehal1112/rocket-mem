use redis::AsyncCommands;
use std::sync::Arc;
use tokio::net::TcpListener;

async fn spawn_test_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(engine::Engine::new());
    tokio::spawn(rocket_mem::serve(listener, engine));
    format!("redis://{addr}")
}

#[tokio::test]
async fn redis_rs_client_can_set_and_get_over_real_tcp() {
    let url = spawn_test_server().await;
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    let _: () = con.set("foo", "bar").await.unwrap();
    let value: String = con.get("foo").await.unwrap();
    assert_eq!(value, "bar");
}

#[tokio::test]
async fn redis_rs_client_runs_a_mixed_workload_across_all_sprint_1_data_types() {
    let url = spawn_test_server().await;
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
    let url = spawn_test_server().await;
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    let _: () = con.set("k", "v").await.unwrap();
    let result: redis::RedisResult<()> = con.hset("k", "f", "v").await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("WRONGTYPE"));
}
