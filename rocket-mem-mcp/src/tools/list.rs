use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::errors::redis_error_to_tool_result;
use crate::server::RocketMemMcpServer;

#[derive(Deserialize, JsonSchema)]
pub struct ListValuesParams {
    key: String,
    /// One or more values to push. This is variadic at the wire level — all values are sent in
    /// a single command. An empty array is rejected by the server with a wrong-number-of-
    /// arguments error (at least one value is required per call).
    values: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListKeyParams {
    key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListRangeParams {
    key: String,
    /// Negative indices count from the end (-1 is the last element). Inclusive on both ends.
    start: i64,
    stop: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct LIndexParams {
    key: String,
    /// Negative indices count from the end (-1 is the last element). Out-of-range returns
    /// found: false, not an error.
    index: i64,
}

#[tool_router(router = list_router, vis = "pub")]
impl RocketMemMcpServer {
    #[tool(
        description = "Prepend one or more values to the head of a list, creating it if \
        missing. With multiple values, each is prepended in argument order, so the *last* \
        argument ends up at the front of the list. Returns the list's new length."
    )]
    async fn lpush(
        &self,
        Parameters(ListValuesParams { key, values }): Parameters<ListValuesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("LPUSH");
        command.arg(&key);
        for value in &values {
            command.arg(value);
        }
        let result: Result<usize, redis::RedisError> = command.query_async(&mut conn).await;
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
        description = "Append one or more values to the tail of a list, creating it if \
        missing, in the order given. Returns the list's new length."
    )]
    async fn rpush(
        &self,
        Parameters(ListValuesParams { key, values }): Parameters<ListValuesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("RPUSH");
        command.arg(&key);
        for value in &values {
            command.arg(value);
        }
        let result: Result<usize, redis::RedisError> = command.query_async(&mut conn).await;
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
        description = "Remove and return the first element of a list. A missing or empty \
        list reports found: false, not an error. There is no optional count — this always \
        pops at most one element."
    )]
    async fn lpop(
        &self,
        Parameters(ListKeyParams { key }): Parameters<ListKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> =
            redis::cmd("LPOP").arg(&key).query_async(&mut conn).await;
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
        description = "Remove and return the last element of a list. A missing or empty \
        list reports found: false, not an error. There is no optional count — this always \
        pops at most one element."
    )]
    async fn rpop(
        &self,
        Parameters(ListKeyParams { key }): Parameters<ListKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> =
            redis::cmd("RPOP").arg(&key).query_async(&mut conn).await;
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
        description = "Get the number of elements in a list. A missing key returns 0, not \
        an error."
    )]
    async fn llen(
        &self,
        Parameters(ListKeyParams { key }): Parameters<ListKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<usize, redis::RedisError> =
            redis::cmd("LLEN").arg(&key).query_async(&mut conn).await;
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
        description = "Get a range of elements from a list, inclusive on both ends. \
        Negative indices count from the end (-1 is the last element). A missing key returns an \
        empty list, not an error."
    )]
    async fn lrange(
        &self,
        Parameters(ListRangeParams { key, start, stop }): Parameters<ListRangeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Vec<String>, redis::RedisError> = redis::cmd("LRANGE")
            .arg(&key)
            .arg(start)
            .arg(stop)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(values) => {
                let text = values.join(", ");
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "values": values }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Get the element at an index in a list. Negative indices count from \
        the end (-1 is the last element). A missing key or an out-of-range index both report \
        found: false, not an error — the reply cannot distinguish the two cases."
    )]
    async fn lindex(
        &self,
        Parameters(LIndexParams { key, index }): Parameters<LIndexParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> = redis::cmd("LINDEX")
            .arg(&key)
            .arg(index)
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
}
