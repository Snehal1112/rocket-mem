mod support;

use rmcp::model::CallToolRequestParams;
use rmcp::object;
use rmcp::service::{RoleClient, RunningService};
use rocket_mem_mcp::pool::Pool;

async fn seed_two_sets(client: &RunningService<RoleClient, ()>) {
    client
        .peer()
        .call_tool(CallToolRequestParams::new("sadd").with_arguments(object!({
            "key": "a", "members": ["x", "y"]
        })))
        .await
        .unwrap();
    client
        .peer()
        .call_tool(CallToolRequestParams::new("sadd").with_arguments(object!({
            "key": "b", "members": ["y", "z"]
        })))
        .await
        .unwrap();
}

fn sorted_members(value: &serde_json::Value) -> Vec<String> {
    let mut members: Vec<String> = value["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    members.sort();
    members
}

#[tokio::test]
async fn sinter_returns_only_members_present_in_every_set() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("sinter.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;
    seed_two_sets(&client).await;

    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("sinter").with_arguments(object!({"keys": ["a", "b"]})),
        )
        .await
        .unwrap();
    assert_eq!(
        sorted_members(&result.structured_content.unwrap()),
        vec!["y"]
    );

    guard.kill();
}

#[tokio::test]
async fn sunion_returns_every_member_from_every_set() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("sunion.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;
    seed_two_sets(&client).await;

    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("sunion").with_arguments(object!({"keys": ["a", "b"]})),
        )
        .await
        .unwrap();
    assert_eq!(
        sorted_members(&result.structured_content.unwrap()),
        vec!["x", "y", "z"]
    );

    guard.kill();
}

#[tokio::test]
async fn sdiff_returns_members_in_first_set_not_in_others() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("sdiff.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;
    seed_two_sets(&client).await;

    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("sdiff").with_arguments(object!({"keys": ["a", "b"]})),
        )
        .await
        .unwrap();
    assert_eq!(
        sorted_members(&result.structured_content.unwrap()),
        vec!["x"]
    );

    guard.kill();
}

#[tokio::test]
async fn sinterstore_stores_the_intersection_and_returns_its_length() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("sinterstore.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;
    seed_two_sets(&client).await;

    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("sinterstore")
                .with_arguments(object!({"dest": "dest", "keys": ["a", "b"]})),
        )
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["length"], 1);

    let members = client
        .peer()
        .call_tool(CallToolRequestParams::new("smembers").with_arguments(object!({"key": "dest"})))
        .await
        .unwrap();
    assert_eq!(
        sorted_members(&members.structured_content.unwrap()),
        vec!["y"]
    );

    guard.kill();
}

#[tokio::test]
async fn sunionstore_stores_the_union_and_returns_its_length() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("sunionstore.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;
    seed_two_sets(&client).await;

    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("sunionstore")
                .with_arguments(object!({"dest": "dest", "keys": ["a", "b"]})),
        )
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["length"], 3);

    guard.kill();
}

#[tokio::test]
async fn sdiffstore_stores_the_difference_and_deletes_dest_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("sdiffstore.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;
    seed_two_sets(&client).await;

    // b minus a is {z}, non-empty.
    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("sdiffstore")
                .with_arguments(object!({"dest": "dest", "keys": ["b", "a"]})),
        )
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["length"], 1);

    // a minus a is empty -- dest (reused) must be deleted, not left as a phantom empty set.
    client
        .peer()
        .call_tool(CallToolRequestParams::new("sadd").with_arguments(object!({
            "key": "dest", "members": ["stale"]
        })))
        .await
        .unwrap();
    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("sdiffstore")
                .with_arguments(object!({"dest": "dest", "keys": ["a", "a"]})),
        )
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["length"], 0);
    // `exists` (from Plan 2) takes a plural `keys` array and has no structured_content yet --
    // read its plain text reply instead, per this project's Plans 1-2 backfill note.
    let exists = client
        .peer()
        .call_tool(CallToolRequestParams::new("exists").with_arguments(object!({"keys": ["dest"]})))
        .await
        .unwrap();
    assert!(format!("{:?}", exists.content).contains('0'));

    guard.kill();
}
