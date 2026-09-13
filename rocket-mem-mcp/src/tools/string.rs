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
}
