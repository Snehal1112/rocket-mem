mod support;

use std::time::Duration;

use rmcp::model::CallToolRequestParams;
use rmcp::object;
use rocket_mem_mcp::pool::Pool;

/// Exercises `redis_error_to_tool_result`'s *protocol-level* branch (src/errors.rs) — the one
/// existing tests don't cover. `get_on_a_wrongtype_key_surfaces_the_real_error` (tests/
/// get_set_tool.rs) already covers the tool-level branch (`Ok(CallToolResult::error(..))`); this
/// test needs the connection to rocket-mem itself to be gone, which requires a real process to
/// kill, not something a WRONGTYPE reply can trigger.
#[tokio::test]
async fn get_after_the_target_process_dies_is_a_protocol_level_error() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("errors-test.aof");
    let (mut guard, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");
    let client = support::connect_client_and_server(pool).await;

    // Confirm the tool genuinely works first, so a failure below is attributable to killing
    // rocket-mem, not to some unrelated setup problem.
    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set")
                .with_arguments(object!({"key": "errors-test-key", "value": "v"})),
        )
        .await
        .expect("set should succeed while rocket-mem is alive");

    guard.kill();

    // `redis::aio::ConnectionManager` returns a failed command's own I/O error to the caller
    // immediately — it only reconnects in the background afterwards (see redis-rs's
    // `send_packed_command`/`reconnect_if_io_error!`) — so a single `get` right after the kill
    // should already observe the failure. The retry loop below exists purely to absorb OS-level
    // timing between the `kill()` syscall returning and the socket actually reporting closed on
    // this machine, not because we expect the background reconnect to race us; each attempt
    // also gets a short timeout so a genuine hang fails fast instead of blocking the suite.
    let mut outcome = None;
    for _ in 0..10 {
        let attempt = tokio::time::timeout(
            Duration::from_secs(2),
            client.peer().call_tool(
                CallToolRequestParams::new("get")
                    .with_arguments(object!({"key": "errors-test-key"})),
            ),
        )
        .await;

        match attempt {
            // The call itself failed at the MCP/JSON-RPC layer — this is the protocol-level
            // error we're looking for.
            Ok(Err(err)) => {
                outcome = Some(Ok(err));
                break;
            }
            // The call round-tripped fine — either rocket-mem hasn't noticed the death yet, or
            // we hit a lucky reconnect window. Retry.
            Ok(Ok(result)) => {
                outcome = Some(Err(result));
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            // The call didn't resolve within the per-attempt timeout at all.
            Err(_) => {
                outcome = Some(Err(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::ContentBlock::text("attempt timed out".to_string()),
                ])));
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    match outcome {
        Some(Ok(_protocol_error)) => {} // success — got the protocol-level Err we wanted.
        Some(Err(result)) => panic!(
            "expected `get` to eventually fail at the MCP protocol level after rocket-mem was \
             killed, but every attempt over ~1s either succeeded or timed out without a \
             protocol-level Err; last outcome: {result:?}"
        ),
        None => unreachable!("loop always runs at least once"),
    }
}
