mod support;

use rmcp::model::CallToolRequestParams;
use rmcp::object;
use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn sadd_adds_members_and_reports_count_newly_added() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("sadd.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("sadd").with_arguments(object!({
            "key": "s", "members": ["a", "b", "a"]
        })))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["added"], 2);

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("sadd").with_arguments(object!({
            "key": "s", "members": ["a", "c"]
        })))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["added"], 1);

    let members = client
        .peer()
        .call_tool(CallToolRequestParams::new("smembers").with_arguments(object!({"key": "s"})))
        .await
        .unwrap();
    let mut members: Vec<String> = members.structured_content.unwrap()["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    members.sort();
    assert_eq!(members, vec!["a", "b", "c"]);

    guard.kill();
}

#[tokio::test]
async fn srem_removes_members_and_reports_count_removed() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("srem.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("sadd").with_arguments(object!({
            "key": "s", "members": ["a", "b"]
        })))
        .await
        .unwrap();

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("srem").with_arguments(object!({
            "key": "s", "members": ["a", "z"]
        })))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["removed"], 1);

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("srem").with_arguments(object!({
            "key": "missing", "members": ["a"]
        })))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["removed"], 0);

    guard.kill();
}

#[tokio::test]
async fn sismember_reports_membership_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("sismember.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("sadd").with_arguments(object!({
            "key": "s", "members": ["a"]
        })))
        .await
        .unwrap();

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("sismember").with_arguments(object!({
            "key": "s", "member": "a"
        })))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["is_member"], true);

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("sismember").with_arguments(object!({
            "key": "s", "member": "z"
        })))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["is_member"], false);

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("sismember").with_arguments(object!({
            "key": "missing", "member": "a"
        })))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["is_member"], false);

    guard.kill();
}

#[tokio::test]
async fn scard_returns_member_count_and_zero_for_missing_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("scard.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("sadd").with_arguments(object!({
            "key": "s", "members": ["a", "b"]
        })))
        .await
        .unwrap();

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("scard").with_arguments(object!({"key": "s"})))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["length"], 2);

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("scard").with_arguments(object!({"key": "missing"})))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["length"], 0);

    guard.kill();
}

#[tokio::test]
async fn spop_removes_and_returns_a_member_found_false_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("spop.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("sadd").with_arguments(object!({
            "key": "s", "members": ["only"]
        })))
        .await
        .unwrap();

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("spop").with_arguments(object!({"key": "s"})))
        .await
        .unwrap();
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["found"], true);
    assert_eq!(structured["value"], "only");

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("spop").with_arguments(object!({"key": "s"})))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["found"], false);

    guard.kill();
}

#[tokio::test]
async fn srandmember_returns_a_member_without_removing_it_found_false_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("srandmember.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("sadd").with_arguments(object!({
            "key": "s", "members": ["only"]
        })))
        .await
        .unwrap();

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("srandmember").with_arguments(object!({"key": "s"})))
        .await
        .unwrap();
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["found"], true);
    assert_eq!(structured["value"], "only");

    // srandmember must not remove the member.
    let card = client
        .peer()
        .call_tool(CallToolRequestParams::new("scard").with_arguments(object!({"key": "s"})))
        .await
        .unwrap();
    assert_eq!(card.structured_content.unwrap()["length"], 1);

    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("srandmember").with_arguments(object!({"key": "missing"})),
        )
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["found"], false);

    guard.kill();
}

#[tokio::test]
async fn sscan_returns_every_member_in_one_page_with_cursor_zero() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("sscan.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("sadd").with_arguments(object!({
            "key": "s", "members": ["apple", "pear"]
        })))
        .await
        .unwrap();

    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("sscan")
                .with_arguments(object!({"key": "s", "cursor": 0, "match_pattern": "a*"})),
        )
        .await
        .unwrap();
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["cursor"], 0);
    assert_eq!(
        structured["members"].as_array().unwrap(),
        &vec![serde_json::json!("apple")]
    );

    guard.kill();
}
