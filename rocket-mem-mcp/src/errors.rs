use redis::RedisError;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};

/// Splits a `redis::RedisError` into the two kinds of MCP failure the agent needs to tell
/// apart: a *protocol-level* error (the connection to rocket-mem itself is broken — the agent
/// should treat this as "retry me"), returned as `Err(ErrorData)`; or a *tool-level* failure
/// (WRONGTYPE, unknown command, etc. — the command reached rocket-mem and rocket-mem rejected
/// it), returned as `Ok(CallToolResult::error(..))` so the agent sees the real message and can
/// decide what to do next, rather than the failure being swallowed into a generic error.
pub fn redis_error_to_tool_result(err: RedisError) -> Result<CallToolResult, ErrorData> {
    if err.is_io_error() || err.is_connection_dropped() || err.is_connection_refusal() {
        return Err(ErrorData::internal_error(
            format!("lost connection to rocket-mem: {err}"),
            None,
        ));
    }
    Ok(CallToolResult::error(vec![ContentBlock::text(
        err.to_string(),
    )]))
}
