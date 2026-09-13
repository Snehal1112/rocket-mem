use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::errors::redis_error_to_tool_result;
use crate::server::RocketMemMcpServer;

#[derive(Deserialize, JsonSchema)]
pub struct SetKeysParams {
    /// One or more set keys to combine. This is variadic at the wire level — all keys are sent
    /// in a single command. An empty array is rejected by the server with a
    /// wrong-number-of-arguments error (at least one key is required per call). A key that
    /// doesn't exist is treated as an empty set, not an error.
    keys: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetStoreParams {
    dest: String,
    /// One or more source set keys to combine, same variadic rules as the non-storing form.
    keys: Vec<String>,
}

#[tool_router(router = set_ops_router, vis = "pub")]
impl RocketMemMcpServer {
    #[tool(
        description = "Get the intersection of two or more sets — members present in every \
        given set. A key that doesn't exist is treated as an empty set (making the whole result \
        empty), not an error."
    )]
    async fn sinter(
        &self,
        Parameters(SetKeysParams { keys }): Parameters<SetKeysParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("SINTER");
        for key in &keys {
            command.arg(key);
        }
        let result: Result<Vec<String>, redis::RedisError> = command.query_async(&mut conn).await;
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
        description = "Get the union of two or more sets — every distinct member across all \
        given sets. A key that doesn't exist is treated as an empty set, not an error."
    )]
    async fn sunion(
        &self,
        Parameters(SetKeysParams { keys }): Parameters<SetKeysParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("SUNION");
        for key in &keys {
            command.arg(key);
        }
        let result: Result<Vec<String>, redis::RedisError> = command.query_async(&mut conn).await;
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
        description = "Get the difference of two or more sets — members in the first key's \
        set that are not present in any of the other given sets. A key that doesn't exist is \
        treated as an empty set, not an error."
    )]
    async fn sdiff(
        &self,
        Parameters(SetKeysParams { keys }): Parameters<SetKeysParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("SDIFF");
        for key in &keys {
            command.arg(key);
        }
        let result: Result<Vec<String>, redis::RedisError> = command.query_async(&mut conn).await;
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
        description = "Compute the intersection of two or more source sets and store it at \
        dest, replacing whatever was there. Returns the stored set's length. If the computed \
        intersection is empty, dest is deleted entirely rather than left as a phantom empty set."
    )]
    async fn sinterstore(
        &self,
        Parameters(SetStoreParams { dest, keys }): Parameters<SetStoreParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("SINTERSTORE");
        command.arg(&dest);
        for key in &keys {
            command.arg(key);
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
        description = "Compute the union of two or more source sets and store it at dest, \
        replacing whatever was there. Returns the stored set's length. If the computed union is \
        empty (all source keys missing), dest is deleted entirely rather than left as a phantom \
        empty set."
    )]
    async fn sunionstore(
        &self,
        Parameters(SetStoreParams { dest, keys }): Parameters<SetStoreParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("SUNIONSTORE");
        command.arg(&dest);
        for key in &keys {
            command.arg(key);
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
        description = "Compute the difference of two or more source sets (first minus the \
        rest) and store it at dest, replacing whatever was there. Returns the stored set's \
        length. If the computed difference is empty, dest is deleted entirely rather than left \
        as a phantom empty set."
    )]
    async fn sdiffstore(
        &self,
        Parameters(SetStoreParams { dest, keys }): Parameters<SetStoreParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("SDIFFSTORE");
        command.arg(&dest);
        for key in &keys {
            command.arg(key);
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
}
