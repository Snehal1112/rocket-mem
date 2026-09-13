use rmcp::{tool_handler, ServerHandler};

use crate::pool::Pool;

/// The MCP-facing view of one rocket-mem instance. Holds a `Pool`; each tool method (spread
/// across `crate::tools::*`, one file per rocket-mem command family) borrows a connection from
/// it for the duration of one call.
#[derive(Clone)]
pub struct RocketMemMcpServer {
    pool: Pool,
}

impl RocketMemMcpServer {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub(crate) fn pool(&self) -> &Pool {
        &self.pool
    }
}

impl RocketMemMcpServer {
    fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::string_router() + Self::keys_router()
    }
}

#[tool_handler]
impl ServerHandler for RocketMemMcpServer {}
