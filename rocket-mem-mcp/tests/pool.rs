mod support;

use redis::AsyncCommands;
use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn set_then_get_round_trips_through_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("pool-test.aof");
    let (_guard, addr) = support::spawn_rocket_mem(&aof_path);

    let pool = Pool::connect(&addr).await.expect("pool should connect");
    let mut conn = pool.connection();
    let _: () = conn.set("pool-test-key", "pool-test-value").await.unwrap();
    let value: String = conn.get("pool-test-key").await.unwrap();
    assert_eq!(value, "pool-test-value");
}
