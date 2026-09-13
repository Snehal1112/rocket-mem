use rmcp::{tool_handler, tool_router, ServerHandler};

use crate::pool::Pool;

/// The MCP-facing view of one rocket-mem instance. Holds a `Pool` (Task 1); each tool method
/// (added starting in Task 3) borrows a connection from it for the duration of one call.
#[derive(Clone)]
pub struct RocketMemMcpServer {
    pool: Pool,
}

impl RocketMemMcpServer {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

// `allow_empty` is required here because this impl block has no `#[tool]` fn yet — Task 3 adds
// the first ones (`get`/`set`). Without it, `#[tool_router]` refuses to generate a router that
// would serve zero tools.
#[tool_router(allow_empty)]
impl RocketMemMcpServer {
    // Tool methods land here, starting with `get`/`set` in Task 3.
}

#[tool_handler]
impl ServerHandler for RocketMemMcpServer {}
