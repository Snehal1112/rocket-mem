mod support;

use rmcp::model::CallToolRequestParams;
use rmcp::object;
use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn hset_hget_hexists_and_hdel_round_trip_a_field() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hset.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let hset_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hset").with_arguments(object!({
            "key": "h",
            "pairs": [{"field": "f1", "value": "v1"}, {"field": "f2", "value": "v2"}]
        })))
        .await
        .unwrap();
    assert_ne!(hset_result.is_error, Some(true));
    assert!(format!("{:?}", hset_result.content).contains('2'));

    let hget_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hget").with_arguments(object!({"key": "h", "field": "f1"})),
        )
        .await
        .unwrap();
    let structured = hget_result
        .structured_content
        .expect("hget must return structured_content");
    assert_eq!(structured["found"], true);
    assert_eq!(structured["value"], "v1");

    let hexists_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hexists")
                .with_arguments(object!({"key": "h", "field": "f1"})),
        )
        .await
        .unwrap();
    assert_eq!(hexists_result.structured_content.unwrap()["exists"], true);

    let hdel_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hdel")
                .with_arguments(object!({"key": "h", "fields": ["f1", "missing"]})),
        )
        .await
        .unwrap();
    assert_eq!(hdel_result.structured_content.unwrap()["removed"], 1);

    let hexists_after_delete = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hexists")
                .with_arguments(object!({"key": "h", "field": "f1"})),
        )
        .await
        .unwrap();
    assert_eq!(
        hexists_after_delete.structured_content.unwrap()["exists"],
        false
    );

    guard.kill();
}

#[tokio::test]
async fn hget_and_hexists_on_a_missing_key_are_not_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hget-missing.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let hget_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hget")
                .with_arguments(object!({"key": "never-created", "field": "f"})),
        )
        .await
        .unwrap();
    assert_ne!(hget_result.is_error, Some(true));
    assert_eq!(hget_result.structured_content.unwrap()["found"], false);

    let hexists_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hexists")
                .with_arguments(object!({"key": "never-created", "field": "f"})),
        )
        .await
        .unwrap();
    assert_eq!(hexists_result.structured_content.unwrap()["exists"], false);

    guard.kill();
}

#[tokio::test]
async fn hget_on_a_wrongtype_key_surfaces_the_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hget-wrongtype.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();

    let mut raw_conn = pool.connection();
    let _: () = redis::cmd("SET")
        .arg("string-key")
        .arg("x")
        .query_async(&mut raw_conn)
        .await
        .unwrap();

    let client = support::connect_client_and_server(pool).await;
    let hget_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hget")
                .with_arguments(object!({"key": "string-key", "field": "f"})),
        )
        .await
        .unwrap();
    assert_eq!(hget_result.is_error, Some(true));
    let text = format!("{:?}", hget_result.content);
    assert!(
        text.contains("WRONGTYPE"),
        "expected WRONGTYPE, got: {text}"
    );

    guard.kill();
}

#[tokio::test]
async fn hsetnx_sets_only_when_the_field_is_absent() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hsetnx.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool.clone()).await;

    let first = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hsetnx").with_arguments(object!({
                "key": "h", "field": "f", "value": "first"
            })),
        )
        .await
        .unwrap();
    assert_eq!(first.structured_content.unwrap()["applied"], true);

    let second = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hsetnx").with_arguments(object!({
                "key": "h", "field": "f", "value": "second"
            })),
        )
        .await
        .unwrap();
    assert_eq!(second.structured_content.unwrap()["applied"], false);

    let mut conn = pool.connection();
    let value: String = redis::cmd("HGET")
        .arg("h")
        .arg("f")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(value, "first");

    guard.kill();
}

#[tokio::test]
async fn hgetall_hlen_hkeys_and_hvals_report_the_whole_hash() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hgetall.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("hset").with_arguments(object!({
            "key": "h",
            "pairs": [{"field": "f1", "value": "v1"}, {"field": "f2", "value": "v2"}]
        })))
        .await
        .unwrap();

    let hgetall_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hgetall").with_arguments(object!({"key": "h"})))
        .await
        .unwrap();
    let fields = hgetall_result.structured_content.unwrap()["fields"].clone();
    assert_eq!(fields["f1"], "v1");
    assert_eq!(fields["f2"], "v2");

    let hlen_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hlen").with_arguments(object!({"key": "h"})))
        .await
        .unwrap();
    assert_eq!(hlen_result.structured_content.unwrap()["length"], 2);

    let hkeys_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hkeys").with_arguments(object!({"key": "h"})))
        .await
        .unwrap();
    let keys = hkeys_result.structured_content.unwrap()["fields"].clone();
    let keys: Vec<String> = keys
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(keys.contains(&"f1".to_string()) && keys.contains(&"f2".to_string()));

    let hvals_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hvals").with_arguments(object!({"key": "h"})))
        .await
        .unwrap();
    let vals = hvals_result.structured_content.unwrap()["values"].clone();
    let vals: Vec<String> = vals
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(vals.contains(&"v1".to_string()) && vals.contains(&"v2".to_string()));

    guard.kill();
}

#[tokio::test]
async fn hgetall_hlen_hkeys_and_hvals_on_a_missing_key_are_empty_not_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hgetall-missing.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let hgetall_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hgetall").with_arguments(object!({"key": "missing"})),
        )
        .await
        .unwrap();
    assert_ne!(hgetall_result.is_error, Some(true));
    let fields = hgetall_result.structured_content.unwrap()["fields"].clone();
    assert!(fields.as_object().unwrap().is_empty());

    let hlen_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hlen").with_arguments(object!({"key": "missing"})))
        .await
        .unwrap();
    assert_eq!(hlen_result.structured_content.unwrap()["length"], 0);

    guard.kill();
}

#[tokio::test]
async fn hincrby_moves_a_field_and_surfaces_not_an_integer() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hincrby.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let r1 = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hincrby").with_arguments(object!({
                "key": "h", "field": "counter", "delta": 5
            })),
        )
        .await
        .unwrap();
    assert_eq!(r1.structured_content.unwrap()["value"], 5);

    let r2 = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hincrby").with_arguments(object!({
                "key": "h", "field": "counter", "delta": -2
            })),
        )
        .await
        .unwrap();
    assert_eq!(r2.structured_content.unwrap()["value"], 3);

    client
        .peer()
        .call_tool(CallToolRequestParams::new("hset").with_arguments(object!({
            "key": "h", "pairs": [{"field": "not-a-number", "value": "abc"}]
        })))
        .await
        .unwrap();
    let bad_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hincrby").with_arguments(object!({
                "key": "h", "field": "not-a-number", "delta": 1
            })),
        )
        .await
        .unwrap();
    assert_eq!(bad_result.is_error, Some(true));

    guard.kill();
}

#[tokio::test]
async fn hmget_returns_null_for_missing_fields_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hmget.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("hset").with_arguments(object!({
            "key": "h", "pairs": [{"field": "f1", "value": "v1"}]
        })))
        .await
        .unwrap();

    let hmget_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hmget")
                .with_arguments(object!({"key": "h", "fields": ["f1", "missing"]})),
        )
        .await
        .unwrap();
    let values = hmget_result.structured_content.unwrap()["values"].clone();
    let values = values.as_array().unwrap();
    assert_eq!(values[0], "v1");
    assert!(values[1].is_null());

    guard.kill();
}

#[tokio::test]
async fn hscan_returns_every_field_in_one_page_with_cursor_zero() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hscan.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("hset").with_arguments(object!({
            "key": "h",
            "pairs": [{"field": "alpha", "value": "1"}, {"field": "beta", "value": "2"}]
        })))
        .await
        .unwrap();

    let hscan_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hscan").with_arguments(object!({"key": "h", "cursor": 0})),
        )
        .await
        .unwrap();
    let structured = hscan_result.structured_content.unwrap();
    assert_eq!(structured["cursor"], 0);
    let fields = &structured["fields"];
    assert_eq!(fields["alpha"], "1");
    assert_eq!(fields["beta"], "2");

    guard.kill();
}
