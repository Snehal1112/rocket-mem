use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::errors::redis_error_to_tool_result;
use crate::server::RocketMemMcpServer;

#[derive(Deserialize, JsonSchema)]
pub struct SetMembersParams {
    key: String,
    /// One or more members to add or remove. This is variadic at the wire level — all members
    /// are sent in a single command. An empty array is rejected by the server with a
    /// wrong-number-of-arguments error (at least one member is required per call).
    members: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetKeyParams {
    key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetMemberParams {
    key: String,
    member: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SScanParams {
    key: String,
    /// 0 starts a new scan. rocket-mem's SSCAN always completes in one page (a set lives fully
    /// in memory already), so the returned cursor is always 0 — there is no second page to fetch.
    cursor: u64,
    /// Optional glob filter over members. Same partial glob support as the top-level `keys` tool.
    #[serde(default)]
    match_pattern: Option<String>,
}

#[tool_router(router = set_router, vis = "pub")]
impl RocketMemMcpServer {
    #[tool(
        description = "Add one or more members to a set, creating it if missing. Returns the \
        count of members that were newly added (members already present don't count, and \
        duplicates within the same call only count once)."
    )]
    async fn sadd(
        &self,
        Parameters(SetMembersParams { key, members }): Parameters<SetMembersParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("SADD");
        command.arg(&key);
        for member in &members {
            command.arg(member);
        }
        let result: Result<i64, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(added) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(added.to_string())]);
                r.structured_content = Some(json!({ "added": added }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Remove one or more members from a set. Returns the count of members \
        that actually existed and were removed — not the count of members given. A missing key \
        returns 0, not an error."
    )]
    async fn srem(
        &self,
        Parameters(SetMembersParams { key, members }): Parameters<SetMembersParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("SREM");
        command.arg(&key);
        for member in &members {
            command.arg(member);
        }
        let result: Result<i64, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(removed) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(removed.to_string())]);
                r.structured_content = Some(json!({ "removed": removed }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Get every member of a set. A missing key returns an empty set of \
        members, not an error. Member order is not meaningful (sets are unordered)."
    )]
    async fn smembers(
        &self,
        Parameters(SetKeyParams { key }): Parameters<SetKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Vec<String>, redis::RedisError> = redis::cmd("SMEMBERS")
            .arg(&key)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(members) => {
                let text = members.join(", ");
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "members": members }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Check whether a value is a member of a set. A missing key reports \
        false, not an error."
    )]
    async fn sismember(
        &self,
        Parameters(SetMemberParams { key, member }): Parameters<SetMemberParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("SISMEMBER")
            .arg(&key)
            .arg(&member)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(is_member) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    if is_member { "1" } else { "0" }.to_string(),
                )]);
                r.structured_content = Some(json!({ "is_member": is_member }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Get the number of members in a set. A missing key returns 0, not an error."
    )]
    async fn scard(
        &self,
        Parameters(SetKeyParams { key }): Parameters<SetKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<usize, redis::RedisError> =
            redis::cmd("SCARD").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(len) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(len.to_string())]);
                r.structured_content = Some(json!({ "length": len }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Remove and return a random member of a set. A missing or empty set \
        reports found: false, not an error. There is no optional count in rocket-mem — this \
        always pops at most one member, unlike real Redis's `SPOP key [count]` form."
    )]
    async fn spop(
        &self,
        Parameters(SetKeyParams { key }): Parameters<SetKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> =
            redis::cmd("SPOP").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(value) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    value.clone().unwrap_or_else(|| "(nil)".to_string()),
                )]);
                r.structured_content = Some(json!({ "found": value.is_some(), "value": value }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Get a random member of a set without removing it. A missing or empty \
        set reports found: false, not an error. There is no optional count in rocket-mem — this \
        always returns at most one member, unlike real Redis's `SRANDMEMBER key [count]` form."
    )]
    async fn srandmember(
        &self,
        Parameters(SetKeyParams { key }): Parameters<SetKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> = redis::cmd("SRANDMEMBER")
            .arg(&key)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(value) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    value.clone().unwrap_or_else(|| "(nil)".to_string()),
                )]);
                r.structured_content = Some(json!({ "found": value.is_some(), "value": value }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Iterate a set's members. Unlike the top-level scan tool, rocket-mem's \
        SSCAN always completes in a single call (a set already lives fully in memory) — the \
        returned cursor is always 0, meaning there is never a second page to fetch. MATCH \
        filters members by glob (same partial support as the keys/scan tools); there is no TYPE \
        option for SSCAN in real Redis either, and COUNT would have nothing to act on (there's \
        no paging), so neither is exposed here."
    )]
    async fn sscan(
        &self,
        Parameters(SScanParams {
            key,
            cursor,
            match_pattern,
        }): Parameters<SScanParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("SSCAN");
        command.arg(&key).arg(cursor);
        if let Some(pattern) = &match_pattern {
            command.arg("MATCH").arg(pattern);
        }
        let result: Result<(u64, Vec<String>), redis::RedisError> =
            command.query_async(&mut conn).await;
        match result {
            Ok((next_cursor, members)) => {
                let text = format!("cursor={next_cursor}, members=[{}]", members.join(", "));
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "cursor": next_cursor, "members": members }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}
