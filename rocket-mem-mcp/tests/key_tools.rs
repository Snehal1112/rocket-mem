mod support;

use rmcp::model::CallToolRequestParams;
use rmcp::object;
use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn del_and_exists_count_matching_keys_not_args() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("del.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("mset").with_arguments(object!({
            "pairs": [{"key": "a", "value": "1"}, {"key": "b", "value": "2"}]
        })))
        .await
        .unwrap();

    let exists_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("exists")
                .with_arguments(object!({"keys": ["a", "a", "missing"]})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", exists_result.content).contains('2'));

    let del_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("del")
                .with_arguments(object!({"keys": ["a", "b", "missing"]})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", del_result.content).contains('2'));

    child.kill();
}

#[tokio::test]
async fn keys_and_scan_find_the_same_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("keys.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("mset").with_arguments(object!({
            "pairs": [{"key": "alpha", "value": "1"}, {"key": "beta", "value": "2"}]
        })))
        .await
        .unwrap();

    let keys_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("keys").with_arguments(object!({"pattern": "*"})))
        .await
        .unwrap();
    let keys_text = format!("{:?}", keys_result.content);
    assert!(keys_text.contains("alpha") && keys_text.contains("beta"));

    // SCAN is cursor-based: loop until the returned cursor comes back 0.
    let mut cursor: u64 = 0;
    let mut found = Vec::new();
    loop {
        let scan_result = client
            .peer()
            .call_tool(
                CallToolRequestParams::new("scan").with_arguments(object!({"cursor": cursor})),
            )
            .await
            .unwrap();
        let structured = scan_result
            .structured_content
            .expect("scan must return structured_content");
        let next_cursor = structured["cursor"].as_u64().unwrap();
        for k in structured["keys"].as_array().unwrap() {
            found.push(k.as_str().unwrap().to_string());
        }
        if next_cursor == 0 {
            break;
        }
        cursor = next_cursor;
    }
    assert!(found.contains(&"alpha".to_string()));
    assert!(found.contains(&"beta".to_string()));

    child.kill();
}

#[tokio::test]
async fn rename_moves_a_value_and_renamenx_refuses_an_existing_destination() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("rename.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool.clone()).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "src", "value": "v"})),
        )
        .await
        .unwrap();
    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("rename")
                .with_arguments(object!({"source": "src", "destination": "dst"})),
        )
        .await
        .unwrap();
    let mut conn = pool.connection();
    let moved: String = redis::cmd("GET")
        .arg("dst")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(moved, "v");

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set")
                .with_arguments(object!({"key": "other", "value": "x"})),
        )
        .await
        .unwrap();
    let renamenx_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("rename_nx")
                .with_arguments(object!({"source": "other", "destination": "dst"})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", renamenx_result.content).contains('0'));

    let missing_rename = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("rename")
                .with_arguments(object!({"source": "does-not-exist", "destination": "z"})),
        )
        .await
        .unwrap();
    assert_eq!(missing_rename.is_error, Some(true));

    child.kill();
}

#[tokio::test]
async fn type_and_randomkey_report_real_state() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("type.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "k", "value": "v"})),
        )
        .await
        .unwrap();
    let type_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("type").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    assert!(format!("{:?}", type_result.content).contains("string"));

    let randomkey_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("randomkey").with_arguments(object!({})))
        .await
        .unwrap();
    assert!(format!("{:?}", randomkey_result.content).contains('k'));

    child.kill();
}

#[tokio::test]
async fn expire_family_and_ttl_family_and_persist_agree() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("ttl.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "k", "value": "v"})),
        )
        .await
        .unwrap();

    let missing_ttl = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("ttl").with_arguments(object!({"key": "no-expiry-yet"})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", missing_ttl.content).contains("-2"));

    let expire_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("expire")
                .with_arguments(object!({"key": "k", "seconds": 100})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", expire_result.content).contains('1'));

    let ttl_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("ttl").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    let ttl_text = format!("{:?}", ttl_result.content);
    assert!(
        !ttl_text.contains("-1"),
        "key should have a real TTL now, got: {ttl_text}"
    );

    let persist_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("persist").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    assert!(format!("{:?}", persist_result.content).contains('1'));

    let after_persist_ttl = client
        .peer()
        .call_tool(CallToolRequestParams::new("ttl").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    assert!(format!("{:?}", after_persist_ttl.content).contains("-1"));

    child.kill();
}

#[tokio::test]
async fn memory_usage_and_object_encoding_report_real_state_and_error_on_missing_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("memory.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "k", "value": "v"})),
        )
        .await
        .unwrap();

    let usage_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("memory_usage").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    assert_ne!(usage_result.is_error, Some(true));

    let encoding_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("object_encoding").with_arguments(object!({"key": "k"})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", encoding_result.content).contains("string"));

    let missing_encoding = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("object_encoding")
                .with_arguments(object!({"key": "does-not-exist"})),
        )
        .await
        .unwrap();
    assert_eq!(missing_encoding.is_error, Some(true));

    child.kill();
}
