mod support;

use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn mcp_handshake_and_list_tools_succeed_over_a_duplex_transport() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("handshake-test.aof");
    let (_guard, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr, None, None, None)
        .await
        .expect("pool should connect");
    let client = support::connect_client_and_server(pool).await;

    let tools = client.peer().list_tools(Default::default()).await;
    assert!(tools.is_ok(), "list_tools should succeed: {tools:?}");

    let tools = tools.unwrap();
    let names: Vec<&str> = tools.tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert!(
        names.contains(&"get") && names.contains(&"set"),
        "expected the tool list to include \"get\" and \"set\", got: {names:?}"
    );
}
