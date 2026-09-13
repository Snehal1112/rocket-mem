mod support;

use rmcp::model::CallToolRequestParams;
use rmcp::object;
use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn set_then_get_round_trips_through_the_mcp_tools() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("get-set-tool-test.aof");
    let (_guard, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set")
                .with_arguments(object!({"key": "mcp-key", "value": "mcp-value"})),
        )
        .await
        .expect("set should succeed");

    let get_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("get").with_arguments(object!({"key": "mcp-key"})))
        .await
        .expect("get should succeed");
    assert_ne!(get_result.is_error, Some(true));
}

#[tokio::test]
async fn get_on_a_missing_key_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("get-missing-key-test.aof");
    let (_guard, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");
    let client = support::connect_client_and_server(pool).await;

    let get_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("get")
                .with_arguments(object!({"key": "this-key-was-never-set"})),
        )
        .await
        .expect("get on a missing key should still succeed as a tool call");
    assert_ne!(get_result.is_error, Some(true));
    assert_eq!(
        get_result.structured_content,
        Some(serde_json::json!({ "found": false })),
        "a missing key should report found: false in structured_content"
    );
}

#[tokio::test]
async fn get_on_a_wrongtype_key_surfaces_the_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("get-wrongtype-test.aof");
    let (_guard, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");

    // Seed a list key directly, bypassing the tool layer — `get` has no way to create one.
    let mut raw_conn = pool.connection();
    let _: () = redis::cmd("RPUSH")
        .arg("a-list-key")
        .arg("x")
        .query_async(&mut raw_conn)
        .await
        .unwrap();

    let client = support::connect_client_and_server(pool).await;
    let get_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("get").with_arguments(object!({"key": "a-list-key"})))
        .await
        .expect("call_tool itself should succeed even though the tool reports an error");
    assert_eq!(get_result.is_error, Some(true));
    let text = format!("{:?}", get_result.content);
    assert!(
        text.contains("WRONGTYPE"),
        "expected the real WRONGTYPE message, got: {text}"
    );
}

/// A stored value that happens to equal the literal string `"(nil)"` must still be
/// distinguishable from a genuinely missing key. Both used to render identical
/// `ContentBlock::text("(nil)")` output; `structured_content`'s `found` flag is what makes them
/// tell apart programmatically now (see src/server.rs's `get`).
#[tokio::test]
async fn get_on_a_key_whose_value_is_literally_nil_reports_found_true() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("get-literal-nil-test.aof");
    let (_guard, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set")
                .with_arguments(object!({"key": "nil-literal-key", "value": "(nil)"})),
        )
        .await
        .expect("set should succeed");

    let get_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("get").with_arguments(object!({"key": "nil-literal-key"})),
        )
        .await
        .expect("get should succeed");
    assert_ne!(get_result.is_error, Some(true));
    assert_eq!(
        get_result.structured_content,
        Some(serde_json::json!({ "found": true, "value": "(nil)" })),
        "a key whose stored value is the literal string \"(nil)\" must report found: true, \
         not be confused with a genuinely missing key"
    );
}
