mod support;

use rmcp::model::CallToolRequestParams;
use rmcp::object;
use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn rpush_lpush_and_lrange_order_values_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("push.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let rpush_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b", "c"]
        })))
        .await
        .unwrap();
    assert_eq!(rpush_result.structured_content.unwrap()["length"], 3);

    let lpush_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lpush").with_arguments(object!({
            "key": "l", "values": ["x", "y", "z"]
        })))
        .await
        .unwrap();
    assert_eq!(lpush_result.structured_content.unwrap()["length"], 6);

    let lrange_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("lrange")
                .with_arguments(object!({"key": "l", "start": 0, "stop": -1})),
        )
        .await
        .unwrap();
    let values = lrange_result.structured_content.unwrap()["values"].clone();
    let values: Vec<String> = values
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    // LPUSH with multiple values prepends each in argument order, so the *last* argument
    // ("z") ends up first.
    assert_eq!(values, vec!["z", "y", "x", "a", "b", "c"]);

    guard.kill();
}

#[tokio::test]
async fn lpop_and_rpop_remove_from_the_correct_end_and_report_found() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("pop.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b", "c"]
        })))
        .await
        .unwrap();

    let lpop_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lpop").with_arguments(object!({"key": "l"})))
        .await
        .unwrap();
    let structured = lpop_result.structured_content.unwrap();
    assert_eq!(structured["found"], true);
    assert_eq!(structured["value"], "a");

    let rpop_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("rpop").with_arguments(object!({"key": "l"})))
        .await
        .unwrap();
    let structured = rpop_result.structured_content.unwrap();
    assert_eq!(structured["found"], true);
    assert_eq!(structured["value"], "c");

    // Only "b" is left. Pop it, then pop again on the now-missing key.
    client
        .peer()
        .call_tool(CallToolRequestParams::new("lpop").with_arguments(object!({"key": "l"})))
        .await
        .unwrap();
    let empty_pop_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lpop").with_arguments(object!({"key": "l"})))
        .await
        .unwrap();
    assert_ne!(empty_pop_result.is_error, Some(true));
    let structured = empty_pop_result.structured_content.unwrap();
    assert_eq!(structured["found"], false);
    assert!(structured["value"].is_null());

    guard.kill();
}

#[tokio::test]
async fn llen_reports_length_and_zero_for_a_missing_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("llen.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b"]
        })))
        .await
        .unwrap();
    let llen_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("llen").with_arguments(object!({"key": "l"})))
        .await
        .unwrap();
    assert_eq!(llen_result.structured_content.unwrap()["length"], 2);

    let missing_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("llen").with_arguments(object!({"key": "missing"})))
        .await
        .unwrap();
    assert_eq!(missing_result.structured_content.unwrap()["length"], 0);

    guard.kill();
}

#[tokio::test]
async fn lindex_supports_negative_indices_and_reports_not_found_out_of_range() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("lindex.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b", "c"]
        })))
        .await
        .unwrap();

    let last_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("lindex").with_arguments(object!({"key": "l", "index": -1})),
        )
        .await
        .unwrap();
    let structured = last_result.structured_content.unwrap();
    assert_eq!(structured["found"], true);
    assert_eq!(structured["value"], "c");

    let out_of_range_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("lindex").with_arguments(object!({"key": "l", "index": 99})),
        )
        .await
        .unwrap();
    assert_ne!(out_of_range_result.is_error, Some(true));
    assert_eq!(
        out_of_range_result.structured_content.unwrap()["found"],
        false
    );

    guard.kill();
}

#[tokio::test]
async fn rpush_on_a_wrongtype_key_surfaces_the_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("wrongtype.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();

    let mut raw_conn = pool.connection();
    let _: () = redis::cmd("SET")
        .arg("string-key")
        .arg("x")
        .query_async(&mut raw_conn)
        .await
        .unwrap();

    let client = support::connect_client_and_server(pool).await;
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "string-key", "values": ["y"]
        })))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    let text = format!("{:?}", result.content);
    assert!(
        text.contains("WRONGTYPE"),
        "expected WRONGTYPE, got: {text}"
    );

    guard.kill();
}

#[tokio::test]
async fn lset_updates_an_element_and_distinguishes_missing_key_from_bad_index() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("lset.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool.clone()).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b", "c"]
        })))
        .await
        .unwrap();

    let ok_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lset").with_arguments(object!({
            "key": "l", "index": 1, "value": "z"
        })))
        .await
        .unwrap();
    assert_ne!(ok_result.is_error, Some(true));
    let mut conn = pool.connection();
    let updated: String = redis::cmd("LINDEX")
        .arg("l")
        .arg(1)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(updated, "z");

    let bad_index_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lset").with_arguments(object!({
            "key": "l", "index": 99, "value": "z"
        })))
        .await
        .unwrap();
    assert_eq!(bad_index_result.is_error, Some(true));
    let bad_index_text = format!("{:?}", bad_index_result.content);

    let missing_key_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lset").with_arguments(object!({
            "key": "does-not-exist", "index": 0, "value": "z"
        })))
        .await
        .unwrap();
    assert_eq!(missing_key_result.is_error, Some(true));
    let missing_key_text = format!("{:?}", missing_key_result.content);

    // The two error messages must be genuinely distinct, not the same generic text.
    assert_ne!(bad_index_text, missing_key_text);

    guard.kill();
}

#[tokio::test]
async fn ltrim_keeps_only_the_requested_range_and_is_a_noop_on_a_missing_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("ltrim.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool.clone()).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b", "c", "d"]
        })))
        .await
        .unwrap();
    let trim_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("ltrim").with_arguments(object!({
            "key": "l", "start": 1, "stop": 2
        })))
        .await
        .unwrap();
    assert_ne!(trim_result.is_error, Some(true));
    let mut conn = pool.connection();
    let remaining: Vec<String> = redis::cmd("LRANGE")
        .arg("l")
        .arg(0)
        .arg(-1)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(remaining, vec!["b".to_string(), "c".to_string()]);

    let noop_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("ltrim").with_arguments(object!({
            "key": "never-existed", "start": 0, "stop": -1
        })))
        .await
        .unwrap();
    assert_ne!(noop_result.is_error, Some(true));
    let exists: i64 = redis::cmd("EXISTS")
        .arg("never-existed")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(exists, 0);

    guard.kill();
}

#[tokio::test]
async fn lrem_removes_by_count_direction() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("lrem.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "x", "b", "x", "c"]
        })))
        .await
        .unwrap();

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lrem").with_arguments(object!({
            "key": "l", "count": 0, "value": "x"
        })))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["removed"], 2);

    guard.kill();
}

#[tokio::test]
async fn linsert_inserts_relative_to_a_pivot_and_reports_sentinels() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("linsert.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "c"]
        })))
        .await
        .unwrap();

    let insert_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("linsert").with_arguments(object!({
                "key": "l", "before": true, "pivot": "c", "value": "b"
            })),
        )
        .await
        .unwrap();
    assert_eq!(insert_result.structured_content.unwrap()["length"], 3);

    let missing_pivot_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("linsert").with_arguments(object!({
                "key": "l", "before": true, "pivot": "not-there", "value": "z"
            })),
        )
        .await
        .unwrap();
    assert_eq!(
        missing_pivot_result.structured_content.unwrap()["length"],
        -1
    );

    let missing_key_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("linsert").with_arguments(object!({
                "key": "does-not-exist", "before": true, "pivot": "p", "value": "z"
            })),
        )
        .await
        .unwrap();
    assert_eq!(missing_key_result.structured_content.unwrap()["length"], 0);

    guard.kill();
}
