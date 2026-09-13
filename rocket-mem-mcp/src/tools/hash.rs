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

#[derive(Deserialize, JsonSchema)]
pub struct HashKeyParams {
    key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct HIncrByParams {
    key: String,
    field: String,
    /// Amount to add. Negative decrements. There is no HINCRBYFLOAT in rocket-mem.
    delta: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct HScanParams {
    key: String,
    /// 0 starts a new scan. rocket-mem's HSCAN always completes in one page (a hash lives fully
    /// in memory already), so the returned cursor is always 0 — there is no second page to fetch.
    cursor: u64,
    /// Optional glob filter over field names. Same partial glob support as the top-level `keys`
    /// tool.
    #[serde(default)]
    match_pattern: Option<String>,
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

    #[tool(
        description = "Get every field and value in a hash. A missing key returns an empty \
        set of fields, not an error."
    )]
    async fn hgetall(
        &self,
        Parameters(HashKeyParams { key }): Parameters<HashKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<std::collections::HashMap<String, String>, redis::RedisError> =
            redis::cmd("HGETALL").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(fields) => {
                let text = fields
                    .iter()
                    .map(|(f, v)| format!("{f}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "fields": fields }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Get the number of fields in a hash. A missing key returns 0, not an \
        error."
    )]
    async fn hlen(
        &self,
        Parameters(HashKeyParams { key }): Parameters<HashKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<usize, redis::RedisError> =
            redis::cmd("HLEN").arg(&key).query_async(&mut conn).await;
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
        description = "Add delta (may be negative) to an integer hash field. A missing field \
        initializes as if it were 0. There is no HINCRBYFLOAT in rocket-mem. Errors if the \
        existing value is not an integer, or if the result would overflow i64."
    )]
    async fn hincrby(
        &self,
        Parameters(HIncrByParams { key, field, delta }): Parameters<HIncrByParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> = redis::cmd("HINCRBY")
            .arg(&key)
            .arg(&field)
            .arg(delta)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(n) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(n.to_string())]);
                r.structured_content = Some(json!({ "value": n }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "List every field name in a hash. A missing key returns an empty list, \
        not an error."
    )]
    async fn hkeys(
        &self,
        Parameters(HashKeyParams { key }): Parameters<HashKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Vec<String>, redis::RedisError> =
            redis::cmd("HKEYS").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(fields) => {
                let text = fields.join(", ");
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "fields": fields }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "List every value in a hash (order matches hkeys' field order for the \
        same hash). A missing key returns an empty list, not an error."
    )]
    async fn hvals(
        &self,
        Parameters(HashKeyParams { key }): Parameters<HashKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Vec<String>, redis::RedisError> =
            redis::cmd("HVALS").arg(&key).query_async(&mut conn).await;
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
        description = "Get multiple hash fields' values at once, in the order requested. A \
        missing field comes back as null in that position; a missing key returns null for every \
        requested field."
    )]
    async fn hmget(
        &self,
        Parameters(HashFieldsParams { key, fields }): Parameters<HashFieldsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("HMGET");
        command.arg(&key);
        for field in &fields {
            command.arg(field);
        }
        let result: Result<Vec<Option<String>>, redis::RedisError> =
            command.query_async(&mut conn).await;
        match result {
            Ok(values) => {
                let text = values
                    .iter()
                    .map(|v| v.clone().unwrap_or_else(|| "(nil)".to_string()))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "values": values }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Iterate a hash's fields. Unlike the top-level scan tool, rocket-mem's \
        HSCAN always completes in a single call (a hash already lives fully in memory) — the \
        returned cursor is always 0, meaning there is never a second page to fetch. MATCH \
        filters field names by glob (same partial support as the keys/scan tools); there is no \
        TYPE option for HSCAN in real Redis either, and COUNT would have nothing to act on \
        (there's no paging), so neither is exposed here."
    )]
    async fn hscan(
        &self,
        Parameters(HScanParams {
            key,
            cursor,
            match_pattern,
        }): Parameters<HScanParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("HSCAN");
        command.arg(&key).arg(cursor);
        if let Some(pattern) = &match_pattern {
            command.arg("MATCH").arg(pattern);
        }
        let result: Result<(u64, std::collections::HashMap<String, String>), redis::RedisError> =
            command.query_async(&mut conn).await;
        match result {
            Ok((next_cursor, fields)) => {
                let text = format!(
                    "cursor={next_cursor}, fields=[{}]",
                    fields
                        .iter()
                        .map(|(f, v)| format!("{f}={v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "cursor": next_cursor, "fields": fields }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}
