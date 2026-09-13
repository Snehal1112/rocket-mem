use redis::AsyncCommands;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::errors::redis_error_to_tool_result;
use crate::server::RocketMemMcpServer;

#[derive(Deserialize, JsonSchema)]
pub struct GetParams {
    /// The key to read.
    key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetParams {
    /// The key to write.
    key: String,
    /// The value to store.
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct KeyValueParams {
    key: String,
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct KeyOnlyParams {
    key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct IncrByParams {
    key: String,
    /// Amount to add. Negative decrements. Real rocket-mem has no INCRBYFLOAT — this is
    /// integer-only, matching INCR/DECR/INCRBY exactly (README's "Command coverage" table).
    delta: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetRangeParams {
    key: String,
    /// Start index. Negative counts from the end (-1 is the last byte). Out-of-range clamps
    /// rather than erroring.
    start: i64,
    /// End index, inclusive (unlike a typical Rust range). Negative counts from the end.
    end: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetRangeParams {
    key: String,
    offset: usize,
    /// The bytes to write at `offset`. An empty value is a documented no-op: it will not create
    /// a missing key and will not modify an existing one.
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct KeyValuePair {
    key: String,
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct MSetParams {
    pairs: Vec<KeyValuePair>,
}

#[derive(Deserialize, JsonSchema)]
pub struct MGetParams {
    keys: Vec<String>,
}

#[tool_router(router = string_router, vis = "pub")]
impl RocketMemMcpServer {
    #[tool(description = "Get the string value of a key. A missing key is not an error.")]
    async fn get(
        &self,
        Parameters(GetParams { key }): Parameters<GetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let value: Result<Option<String>, redis::RedisError> = conn.get(&key).await;
        match value {
            // `structured_content` carries a `found` flag so a caller can tell "the value is the
            // literal string `(nil)`" apart from "the key does not exist" — the human-readable
            // `content` text alone can't distinguish those two cases.
            Ok(Some(value)) => {
                let mut result = CallToolResult::success(vec![ContentBlock::text(value.clone())]);
                result.structured_content = Some(json!({ "found": true, "value": value }));
                Ok(result)
            }
            Ok(None) => {
                let mut result =
                    CallToolResult::success(vec![ContentBlock::text("(nil)".to_string())]);
                result.structured_content = Some(json!({ "found": false }));
                Ok(result)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set the string value of a key.")]
    async fn set(
        &self,
        Parameters(SetParams { key, value }): Parameters<SetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<(), redis::RedisError> = conn.set(&key, &value).await;
        match result {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "OK".to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Set a key's value and return its previous value. A missing key \
        returns null for the old value and still gets the new one written."
    )]
    async fn getset(
        &self,
        Parameters(KeyValueParams { key, value }): Parameters<KeyValueParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> = redis::cmd("GETSET")
            .arg(&key)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(old) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    old.clone().unwrap_or_else(|| "(nil)".to_string()),
                )]);
                r.structured_content = Some(json!({ "old_value": old }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Append a value to a string key, creating it if missing. Returns the \
        string's length after the append."
    )]
    async fn append(
        &self,
        Parameters(KeyValueParams { key, value }): Parameters<KeyValueParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<usize, redis::RedisError> = redis::cmd("APPEND")
            .arg(&key)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(len) => Ok(CallToolResult::success(vec![ContentBlock::text(
                len.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Get the length of a string value. A missing key returns 0, not an error."
    )]
    async fn strlen(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<usize, redis::RedisError> =
            redis::cmd("STRLEN").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(len) => Ok(CallToolResult::success(vec![ContentBlock::text(
                len.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Increment an integer string value by 1. A missing key initializes to \
        1. Errors if the existing value is not an integer, or if the increment would overflow i64."
    )]
    async fn incr(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> =
            redis::cmd("INCR").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(
                n.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Decrement an integer string value by 1. A missing key initializes to \
        -1. Errors if the existing value is not an integer, or if the decrement would overflow i64."
    )]
    async fn decr(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> =
            redis::cmd("DECR").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(
                n.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Add `delta` (may be negative) to an integer string value. A missing \
        key initializes as if it were 0. There is no INCRBYFLOAT or DECRBY in rocket-mem — this \
        integer-only tool covers both directions."
    )]
    async fn incr_by(
        &self,
        Parameters(IncrByParams { key, delta }): Parameters<IncrByParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> = redis::cmd("INCRBY")
            .arg(&key)
            .arg(delta)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(
                n.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Get a substring by byte index, inclusive on both ends. Negative \
        indices count from the end (-1 is the last byte). Out-of-range indices clamp to an \
        empty result rather than erroring. A missing key returns an empty string, not an error."
    )]
    async fn get_range(
        &self,
        Parameters(GetRangeParams { key, start, end }): Parameters<GetRangeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<String, redis::RedisError> = redis::cmd("GETRANGE")
            .arg(&key)
            .arg(start)
            .arg(end)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(s) => Ok(CallToolResult::success(vec![ContentBlock::text(s)])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Overwrite part of a string starting at a byte offset, zero-padding \
        first if the offset extends past the current length. An empty value is a documented \
        no-op: it will not create a missing key and will not modify an existing one. Returns \
        the string's length after the write. WARNING: Unlike real Redis (which caps string size \
        at 512MB), rocket-mem does not enforce a size limit — a very large offset drives an \
        uncapped, proportional server-side memory allocation that could exhaust memory."
    )]
    async fn set_range(
        &self,
        Parameters(SetRangeParams { key, offset, value }): Parameters<SetRangeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<usize, redis::RedisError> = redis::cmd("SETRANGE")
            .arg(&key)
            .arg(offset)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(len) => Ok(CallToolResult::success(vec![ContentBlock::text(
                len.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set multiple key/value pairs at once. Always succeeds and returns OK.")]
    async fn mset(
        &self,
        Parameters(MSetParams { pairs }): Parameters<MSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("MSET");
        for pair in &pairs {
            command.arg(&pair.key).arg(&pair.value);
        }
        let result: Result<(), redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "OK".to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Get multiple keys' string values at once, in the order requested. A \
        missing key or a non-string (WRONGTYPE) key both come back as null in that position — \
        MGET never errors on WRONGTYPE, unlike every other string command."
    )]
    async fn mget(
        &self,
        Parameters(MGetParams { keys }): Parameters<MGetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("MGET");
        for key in &keys {
            command.arg(key);
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
        description = "Set multiple key/value pairs only if none of the keys already exist. \
        If any key exists, nothing is written and this returns false."
    )]
    async fn msetnx(
        &self,
        Parameters(MSetParams { pairs }): Parameters<MSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("MSETNX");
        for pair in &pairs {
            command.arg(&pair.key).arg(&pair.value);
        }
        let result: Result<bool, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}
