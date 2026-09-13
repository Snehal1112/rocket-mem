mod support;

use rmcp::model::CallToolRequestParams;
use rmcp::object;
use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn getset_returns_the_old_value_and_writes_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("getset.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool.clone()).await;

    let mut conn = pool.connection();
    let _: () = redis::cmd("SET")
        .arg("k")
        .arg("old")
        .query_async(&mut conn)
        .await
        .unwrap();

    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("getset")
                .with_arguments(object!({"key": "k", "value": "new"})),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let text = format!("{:?}", result.content);
    assert!(
        text.contains("old"),
        "expected the old value in the reply, got: {text}"
    );

    child.kill();
}

#[tokio::test]
async fn append_extends_an_existing_value_and_reports_the_new_length() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("append.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set")
                .with_arguments(object!({"key": "k", "value": "hello"})),
        )
        .await
        .unwrap();
    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("append")
                .with_arguments(object!({"key": "k", "value": " world"})),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let text = format!("{:?}", result.content);
    assert!(text.contains("11"), "expected new length 11, got: {text}");

    child.kill();
}

#[tokio::test]
async fn strlen_on_a_missing_key_is_zero_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("strlen.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("strlen").with_arguments(object!({"key": "missing"})))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let text = format!("{:?}", result.content);
    assert!(text.contains('0'), "expected 0, got: {text}");

    child.kill();
}

#[tokio::test]
async fn incr_decr_and_incr_by_move_a_counter() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("incr.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let r1 = client
        .peer()
        .call_tool(CallToolRequestParams::new("incr").with_arguments(object!({"key": "counter"})))
        .await
        .unwrap();
    assert!(format!("{:?}", r1.content).contains('1'));

    let r2 = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("incr_by")
                .with_arguments(object!({"key": "counter", "delta": 9})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", r2.content).contains("10"));

    let r3 = client
        .peer()
        .call_tool(CallToolRequestParams::new("decr").with_arguments(object!({"key": "counter"})))
        .await
        .unwrap();
    assert!(format!("{:?}", r3.content).contains('9'));

    child.kill();
}

#[tokio::test]
async fn incr_on_a_non_integer_string_surfaces_not_an_integer() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("incr-bad.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "k", "value": "abc"})),
        )
        .await
        .unwrap();
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("incr").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));

    child.kill();
}

#[tokio::test]
async fn getrange_and_setrange_slice_and_patch_a_string() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("range.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool.clone()).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set")
                .with_arguments(object!({"key": "k", "value": "Hello World"})),
        )
        .await
        .unwrap();

    let getrange_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("get_range")
                .with_arguments(object!({"key": "k", "start": 0, "end": 4})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", getrange_result.content).contains("Hello"));

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set_range")
                .with_arguments(object!({"key": "k", "offset": 6, "value": "Redis!"})),
        )
        .await
        .unwrap();
    let mut conn = pool.connection();
    let final_value: String = redis::cmd("GET")
        .arg("k")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(final_value, "Hello Redis!");

    child.kill();
}

#[tokio::test]
async fn setrange_with_an_empty_value_on_a_missing_key_does_not_create_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("setrange-noop.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool.clone()).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set_range")
                .with_arguments(object!({"key": "never-set", "offset": 0, "value": ""})),
        )
        .await
        .unwrap();
    let mut conn = pool.connection();
    let exists: i64 = redis::cmd("EXISTS")
        .arg("never-set")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(exists, 0);

    child.kill();
}

#[tokio::test]
async fn mset_mget_and_msetnx_round_trip_multiple_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("mset.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool.clone()).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("mset").with_arguments(object!({
            "pairs": [{"key": "a", "value": "1"}, {"key": "b", "value": "2"}]
        })))
        .await
        .unwrap();

    let mget_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("mget")
                .with_arguments(object!({"keys": ["a", "b", "missing"]})),
        )
        .await
        .unwrap();
    let text = format!("{:?}", mget_result.content);
    assert!(text.contains('1') && text.contains('2'));

    let msetnx_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("msetnx").with_arguments(object!({
                "pairs": [{"key": "a", "value": "should-not-apply"}, {"key": "c", "value": "3"}]
            })),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", msetnx_result.content).contains('0'));
    let mut conn = pool.connection();
    let c_exists: i64 = redis::cmd("EXISTS")
        .arg("c")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(
        c_exists, 0,
        "msetnx must apply nothing when any key already exists"
    );

    child.kill();
}
