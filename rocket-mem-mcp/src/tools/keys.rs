use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::errors::redis_error_to_tool_result;
use crate::server::RocketMemMcpServer;

#[derive(Deserialize, JsonSchema)]
pub struct KeysListParams {
    keys: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct KeysGlobParams {
    /// A glob pattern. rocket-mem's glob support is partial — see
    /// docs/command-compatibility.md for exactly which glob features are supported.
    pattern: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ScanParams {
    /// 0 starts a new scan. Pass back whatever cursor the previous call returned until it comes
    /// back 0, which means the scan is complete.
    cursor: u64,
    /// Optional glob filter, same partial support as the `keys` tool.
    #[serde(default)]
    match_pattern: Option<String>,
    /// Optional type filter (e.g. "string", "hash").
    #[serde(default)]
    type_filter: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RenameParams {
    source: String,
    destination: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct KeyOnlyParams {
    key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct NoParams {}

#[derive(Deserialize, JsonSchema)]
pub struct ExpireParams {
    key: String,
    /// A negative value is clamped to 0 (an immediate expiry), not rejected as an error.
    seconds: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct PExpireParams {
    key: String,
    milliseconds: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct ExpireAtParams {
    key: String,
    unix_seconds: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct PExpireAtParams {
    key: String,
    unix_milliseconds: i64,
}

#[tool_router(router = keys_router, vis = "pub")]
impl RocketMemMcpServer {
    #[tool(
        description = "Delete one or more keys. Returns the count of keys that actually \
        existed and were deleted — not the count of keys given."
    )]
    async fn del(
        &self,
        Parameters(KeysListParams { keys }): Parameters<KeysListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("DEL");
        for key in &keys {
            command.arg(key);
        }
        let result: Result<i64, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(
                n.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Count how many of the given keys exist. Duplicate keys in the input \
        are each counted separately if the key exists."
    )]
    async fn exists(
        &self,
        Parameters(KeysListParams { keys }): Parameters<KeysListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("EXISTS");
        for key in &keys {
            command.arg(key);
        }
        let result: Result<i64, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(
                n.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "List every key matching a glob pattern. rocket-mem's glob support is \
        partial — see docs/command-compatibility.md. For a large keyspace, prefer `scan` instead \
        of `keys`, which returns everything in one call."
    )]
    async fn keys(
        &self,
        Parameters(KeysGlobParams { pattern }): Parameters<KeysGlobParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Vec<String>, redis::RedisError> = redis::cmd("KEYS")
            .arg(&pattern)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(keys) => {
                let text = keys.join(", ");
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "keys": keys }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Iterate the keyspace one shard at a time. Pass cursor 0 to start; \
        keep calling with the returned cursor until it comes back 0, which means the scan is \
        complete. rocket-mem accepts but silently ignores the COUNT option real Redis defines \
        (its cursor already advances a whole shard per call, so a page-size hint has nothing to \
        act on) — this tool does not expose a count parameter, since it would do nothing."
    )]
    async fn scan(
        &self,
        Parameters(ScanParams {
            cursor,
            match_pattern,
            type_filter,
        }): Parameters<ScanParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("SCAN");
        command.arg(cursor);
        if let Some(pattern) = &match_pattern {
            command.arg("MATCH").arg(pattern);
        }
        if let Some(type_name) = &type_filter {
            command.arg("TYPE").arg(type_name);
        }
        let result: Result<(u64, Vec<String>), redis::RedisError> =
            command.query_async(&mut conn).await;
        match result {
            Ok((next_cursor, keys)) => {
                let text = format!("cursor={next_cursor}, keys=[{}]", keys.join(", "));
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "cursor": next_cursor, "keys": keys }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Rename a key. Errors if the source key does not exist. The \
        destination's TTL always ends up matching the source's — if the source had a \
        remaining TTL, the destination gets it; if not, the destination ends up with no TTL, \
        even if it previously had one."
    )]
    async fn rename(
        &self,
        Parameters(RenameParams {
            source,
            destination,
        }): Parameters<RenameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<(), redis::RedisError> = redis::cmd("RENAME")
            .arg(&source)
            .arg(&destination)
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
        description = "Rename a key, but only if the destination does not already exist. \
        Returns false (not an error) if the destination exists; errors if the source is missing."
    )]
    async fn rename_nx(
        &self,
        Parameters(RenameParams {
            source,
            destination,
        }): Parameters<RenameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("RENAMENX")
            .arg(&source)
            .arg(&destination)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        name = "type",
        description = "Report a key's type: \"string\", \"hash\", \"list\", \
        \"set\", \"zset\", or \"none\" if the key does not exist. Unlike most commands, a \
        missing key is reported as the type \"none\", not an error."
    )]
    async fn type_cmd(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<String, redis::RedisError> =
            redis::cmd("TYPE").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(type_name) => Ok(CallToolResult::success(vec![ContentBlock::text(type_name)])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Return one key chosen uniformly at random from the whole keyspace. \
        Returns null, not an error, if the keyspace is empty."
    )]
    async fn randomkey(
        &self,
        Parameters(NoParams {}): Parameters<NoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> =
            redis::cmd("RANDOMKEY").query_async(&mut conn).await;
        match result {
            Ok(key) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    key.clone().unwrap_or_else(|| "(nil)".to_string()),
                )]);
                r.structured_content = Some(json!({ "key": key }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Set a key's expiry, in seconds from now. A negative value is clamped \
        to 0 (an immediate expiry), not rejected. Returns true if the key existed and got the \
        expiry set, false if the key did not exist."
    )]
    async fn expire(
        &self,
        Parameters(ExpireParams { key, seconds }): Parameters<ExpireParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("EXPIRE")
            .arg(&key)
            .arg(seconds)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Set a key's expiry, in milliseconds from now. A negative value is \
        clamped to 0. Returns true if the key existed and got the expiry set, false otherwise."
    )]
    async fn pexpire(
        &self,
        Parameters(PExpireParams { key, milliseconds }): Parameters<PExpireParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("PEXPIRE")
            .arg(&key)
            .arg(milliseconds)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Set a key's expiry to an absolute Unix timestamp, in seconds. A \
        timestamp in the past is clamped to an immediate expiry, not rejected. Returns true if \
        the key existed and got the expiry set, false otherwise."
    )]
    async fn expire_at(
        &self,
        Parameters(ExpireAtParams { key, unix_seconds }): Parameters<ExpireAtParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("EXPIREAT")
            .arg(&key)
            .arg(unix_seconds)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Set a key's expiry to an absolute Unix timestamp, in milliseconds. \
        Returns true if the key existed and got the expiry set, false otherwise."
    )]
    async fn pexpire_at(
        &self,
        Parameters(PExpireAtParams {
            key,
            unix_milliseconds,
        }): Parameters<PExpireAtParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("PEXPIREAT")
            .arg(&key)
            .arg(unix_milliseconds)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Get a key's remaining time-to-live in seconds. Returns -2 if the key \
        does not exist, -1 if the key exists but has no expiry, or the remaining seconds \
        (floored at 1 — never 0, even with under a second left) otherwise."
    )]
    async fn ttl(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> =
            redis::cmd("TTL").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(
                n.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Get a key's remaining time-to-live in milliseconds. Returns -2 if the \
        key does not exist, -1 if the key exists but has no expiry, or the remaining \
        milliseconds (floored at 1) otherwise."
    )]
    async fn pttl(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> =
            redis::cmd("PTTL").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(
                n.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Remove a key's expiry, making it persist forever. Returns true if the \
        key existed and had an expiry that was removed, false otherwise (including if the key \
        existed but already had no expiry)."
    )]
    async fn persist(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> =
            redis::cmd("PERSIST").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Get the approximate memory footprint of a key's value, in bytes. \
        Returns null, not an error, if the key does not exist."
    )]
    async fn memory_usage(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<i64>, redis::RedisError> = redis::cmd("MEMORY")
            .arg("USAGE")
            .arg(&key)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(usage) => {
                let text = usage
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "(nil)".to_string());
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "bytes": usage }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(
        description = "Report the engine's internal type name for a key's value (e.g. \
        \"string\", \"hash\") — this is rocket-mem's own type name, not a real Redis encoding \
        like \"listpack\" or \"embstr\". Unlike TTL's -2 sentinel, a missing key here is a real \
        tool-level error: \"no such key\"."
    )]
    async fn object_encoding(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<String, redis::RedisError> = redis::cmd("OBJECT")
            .arg("ENCODING")
            .arg(&key)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(encoding) => Ok(CallToolResult::success(vec![ContentBlock::text(encoding)])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}
