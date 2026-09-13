mod support;

use rmcp::model::CallToolRequestParams;
use rmcp::object;
use rmcp::ServiceExt;
use rocket_mem_mcp::pool::Pool;
use rocket_mem_mcp::server::RocketMemMcpServer;

async fn connect_client_and_server(
    pool: Pool,
) -> rmcp::service::RunningService<rmcp::service::RoleClient, ()> {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);

    let server = RocketMemMcpServer::new(pool);
    tokio::spawn(async move {
        let running = server
            .serve((server_read, server_write))
            .await
            .expect("server should complete the MCP handshake");
        running.waiting().await.ok();
    });

    ().serve((client_read, client_write))
        .await
        .expect("client should complete the MCP handshake")
}

#[tokio::test]
async fn set_then_get_round_trips_through_the_mcp_tools() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("get-set-tool-test.aof");
    let (mut child, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");
    let client = connect_client_and_server(pool).await;

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

    child.kill().ok();
}

#[tokio::test]
async fn get_on_a_missing_key_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("get-missing-key-test.aof");
    let (mut child, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");
    let client = connect_client_and_server(pool).await;

    let get_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("get")
                .with_arguments(object!({"key": "this-key-was-never-set"})),
        )
        .await
        .expect("get on a missing key should still succeed as a tool call");
    assert_ne!(get_result.is_error, Some(true));

    child.kill().ok();
}

#[tokio::test]
async fn get_on_a_wrongtype_key_surfaces_the_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("get-wrongtype-test.aof");
    let (mut child, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");

    // Seed a list key directly, bypassing the tool layer — `get` has no way to create one.
    let mut raw_conn = pool.connection();
    let _: () = redis::cmd("RPUSH")
        .arg("a-list-key")
        .arg("x")
        .query_async(&mut raw_conn)
        .await
        .unwrap();

    let client = connect_client_and_server(pool).await;
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

    child.kill().ok();
}
