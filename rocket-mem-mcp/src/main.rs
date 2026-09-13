use clap::Parser;
use rmcp::transport::io::stdio;
use rmcp::ServiceExt;

use rocket_mem_mcp::config::Config;
use rocket_mem_mcp::pool::Pool;
use rocket_mem_mcp::server::RocketMemMcpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    let pool = Pool::connect(&config.target_addr).await?;
    let server = RocketMemMcpServer::new(pool);

    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
