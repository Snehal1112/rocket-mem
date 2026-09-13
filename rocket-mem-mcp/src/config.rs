use std::path::PathBuf;

use clap::Parser;

/// Startup configuration for rocket-mem-mcp: which rocket-mem instance to connect to, and how.
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

    /// ACL username to AUTH with, if the target instance has ACLs enabled. Env-only — never a
    /// CLI flag paired with a password, to avoid a habit of passing secrets as arguments.
    #[arg(long, env = "ROCKET_MEM_MCP_ACL_USERNAME")]
    pub acl_username: Option<String>,

    /// ACL password to AUTH with. Env-only, deliberately not a CLI flag — a flag value is
    /// visible in `ps` output and shell history; an env var, read once at startup, is not.
    #[arg(long, env = "ROCKET_MEM_MCP_ACL_PASSWORD")]
    pub acl_password: Option<String>,

    /// Path to a CA certificate (PEM) to verify the target instance's TLS certificate against.
    /// Its presence is what turns TLS on for the connection to rocket-mem — there is no
    /// separate on/off flag, and no way to skip verification.
    #[arg(long, env = "ROCKET_MEM_MCP_TLS_CA_PATH")]
    pub tls_ca_path: Option<PathBuf>,
}
