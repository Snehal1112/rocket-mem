use redis::AsyncCommands;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::errors::redis_error_to_tool_result;
use crate::pool::Pool;

#[derive(Deserialize, JsonSchema)]
struct GetParams {
    /// The key to read.
    key: String,
}

#[derive(Deserialize, JsonSchema)]
struct SetParams {
    /// The key to write.
    key: String,
    /// The value to store.
    value: String,
}

#[derive(Clone)]
pub struct RocketMemMcpServer {
    pool: Pool,
}

impl RocketMemMcpServer {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[tool_router]
impl RocketMemMcpServer {
    #[tool(description = "Get the string value of a key. A missing key is not an error.")]
    async fn get(
        &self,
        Parameters(GetParams { key }): Parameters<GetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool.connection();
        let value: Result<Option<String>, redis::RedisError> = conn.get(&key).await;
        match value {
            Ok(Some(value)) => Ok(CallToolResult::success(vec![ContentBlock::text(value)])),
            Ok(None) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "(nil)".to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set the string value of a key.")]
    async fn set(
        &self,
        Parameters(SetParams { key, value }): Parameters<SetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool.connection();
        let result: Result<(), redis::RedisError> = conn.set(&key, &value).await;
        match result {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "OK".to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}

#[tool_handler]
impl ServerHandler for RocketMemMcpServer {}
