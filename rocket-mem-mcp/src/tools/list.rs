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

#[derive(Deserialize, JsonSchema)]
pub struct LSetParams {
    key: String,
    /// Negative indices count from the end (-1 is the last element).
    index: i64,
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct LRemParams {
    key: String,
    /// A positive count removes up to that many matches starting from the head; negative
    /// removes up to -count matches starting from the tail; 0 removes every match.
    count: i64,
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct LInsertParams {
    key: String,
    /// true inserts before the pivot element, false inserts after it.
    before: bool,
    pivot: String,
    value: String,
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

    #[tool(
        description = "Set the value at an index in a list, replacing what's there. \
        Negative indices count from the end (-1 is the last element). Errors with \"no such \
        key\" if the key doesn't exist at all, or \"index out of range\" if the key exists but \
        the index is out of bounds — these are two distinct tool-level errors, not one generic \
        failure."
    )]
    async fn lset(
        &self,
        Parameters(LSetParams { key, index, value }): Parameters<LSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<(), redis::RedisError> = redis::cmd("LSET")
            .arg(&key)
            .arg(index)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "OK".to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Trim a list so only the given index range remains, discarding \
        everything else. Negative indices count from the end (-1 is the last element). A \
        missing key is a silent no-op, not an error. Trimming to an empty range deletes the \
        key entirely."
    )]
    async fn ltrim(
        &self,
        Parameters(ListRangeParams { key, start, stop }): Parameters<ListRangeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<(), redis::RedisError> = redis::cmd("LTRIM")
            .arg(&key)
            .arg(start)
            .arg(stop)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "OK".to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Remove occurrences of a value from a list. A positive count removes \
        up to that many matches starting from the head; a negative count removes up to its \
        absolute value starting from the tail; 0 removes every match. Returns the count \
        actually removed. A missing key returns 0, not an error."
    )]
    async fn lrem(
        &self,
        Parameters(LRemParams { key, count, value }): Parameters<LRemParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> = redis::cmd("LREM")
            .arg(&key)
            .arg(count)
            .arg(&value)
            .query_async(&mut conn)
            .await;
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
        description = "Insert a value immediately before or after the first occurrence of \
        a pivot value in a list. Returns the list's new length on success, -1 if the pivot \
        value isn't found anywhere in the list, or 0 if the key doesn't exist at all — a \
        length of exactly 0 can only mean the key is missing, since a pivot can never be found \
        in an empty list."
    )]
    async fn linsert(
        &self,
        Parameters(LInsertParams {
            key,
            before,
            pivot,
            value,
        }): Parameters<LInsertParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let keyword = if before { "BEFORE" } else { "AFTER" };
        let result: Result<i64, redis::RedisError> = redis::cmd("LINSERT")
            .arg(&key)
            .arg(keyword)
            .arg(&pivot)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(len) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(len.to_string())]);
                r.structured_content = Some(json!({ "length": len }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}
