use clap::Parser;
use rmcp::transport::io::stdio;
use rmcp::ServiceExt;

use rocket_mem_mcp::config::Config;
use rocket_mem_mcp::pool::Pool;
use rocket_mem_mcp::server::RocketMemMcpServer;

// The stdio MCP transport below uses stdout as its wire channel — any future logging added to
// this crate MUST go to stderr, never stdout, or it will corrupt MCP protocol frames.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    let pool = Pool::connect(
        &config.target_addr,
        config.acl_username.as_deref(),
        config.acl_password.as_deref(),
        config.tls_ca_path.as_deref(),
    )
    .await?;
    let server = RocketMemMcpServer::new(pool);

    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
