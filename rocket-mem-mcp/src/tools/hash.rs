use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::errors::redis_error_to_tool_result;
use crate::server::RocketMemMcpServer;

#[derive(Deserialize, JsonSchema)]
pub struct HashFieldValuePair {
    field: String,
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct HSetParams {
    key: String,
    /// One or more field/value pairs to set. HSET is variadic at the wire level — all pairs are
    /// sent in a single HSET command.
    pairs: Vec<HashFieldValuePair>,
}

#[derive(Deserialize, JsonSchema)]
pub struct HashFieldParams {
    key: String,
    field: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct HashFieldsParams {
    key: String,
    fields: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct HSetNxParams {
    key: String,
    /// HSETNX (unlike HSET) takes exactly one field/value pair — it is not variadic.
    field: String,
    value: String,
}

#[tool_router(router = hash_router, vis = "pub")]
impl RocketMemMcpServer {
    #[tool(
        description = "Set one or more field/value pairs in a hash, creating the hash if \
        missing. Returns the count of fields that were newly added (fields that already existed \
        were overwritten but not counted)."
    )]
    async fn hset(
        &self,
        Parameters(HSetParams { key, pairs }): Parameters<HSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("HSET");
        command.arg(&key);
        for pair in &pairs {
            command.arg(&pair.field).arg(&pair.value);
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
        description = "Get the value of a field in a hash. A missing key or a missing field \
        is not an error — both come back as found: false."
    )]
    async fn hget(
        &self,
        Parameters(HashFieldParams { key, field }): Parameters<HashFieldParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> = redis::cmd("HGET")
            .arg(&key)
            .arg(&field)
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
        description = "Delete one or more fields from a hash. Returns the count of fields \
        that actually existed and were removed — not the count of fields given."
    )]
    async fn hdel(
        &self,
        Parameters(HashFieldsParams { key, fields }): Parameters<HashFieldsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("HDEL");
        command.arg(&key);
        for field in &fields {
            command.arg(field);
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
        description = "Check whether a field exists in a hash. A missing key reports false, \
        not an error."
    )]
    async fn hexists(
        &self,
        Parameters(HashFieldParams { key, field }): Parameters<HashFieldParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("HEXISTS")
            .arg(&key)
            .arg(&field)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(exists) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    if exists { "1" } else { "0" }.to_string(),
                )]);
                r.structured_content = Some(json!({ "exists": exists }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Set a hash field's value only if the field does not already exist. \
        Unlike hset, this takes exactly one field/value pair. Returns false (not an error) if \
        the field already existed."
    )]
    async fn hsetnx(
        &self,
        Parameters(HSetNxParams { key, field, value }): Parameters<HSetNxParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("HSETNX")
            .arg(&key)
            .arg(&field)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    if applied { "1" } else { "0" }.to_string(),
                )]);
                r.structured_content = Some(json!({ "applied": applied }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}
