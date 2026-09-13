use clap::Parser;

/// Startup configuration for rocket-mem-mcp: which rocket-mem instance to connect to.
#[derive(Parser, Debug, Clone)]
#[command(name = "rocket-mem-mcp")]
pub struct Config {
    /// host:port of the rocket-mem instance this server exposes as MCP tools.
    #[arg(
        long,
        env = "ROCKET_MEM_MCP_TARGET_ADDR",
        default_value = "127.0.0.1:6379"
    )]
    pub target_addr: String,
}
