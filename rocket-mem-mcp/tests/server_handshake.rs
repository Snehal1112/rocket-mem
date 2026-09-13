mod support;

use rmcp::ServiceExt;
use rocket_mem_mcp::pool::Pool;
use rocket_mem_mcp::server::RocketMemMcpServer;

#[tokio::test]
async fn mcp_handshake_and_list_tools_succeed_over_a_duplex_transport() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("handshake-test.aof");
    let (mut child, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");

    let (server_io, client_io) = tokio::io::duplex(4096);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);

    let server = RocketMemMcpServer::new(pool);
    let server_handle = tokio::spawn(async move {
        let running = server
            .serve((server_read, server_write))
            .await
            .expect("server should complete the MCP handshake");
        running.waiting().await.ok();
    });

    let client =
        ().serve((client_read, client_write))
            .await
            .expect("client should complete the MCP handshake");
    let tools = client.peer().list_tools(Default::default()).await;
    assert!(tools.is_ok(), "list_tools should succeed: {tools:?}");

    server_handle.abort();
    child.kill().ok();
}
