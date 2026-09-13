use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use rmcp::service::{RoleClient, RunningService};
use rmcp::ServiceExt;
use rocket_mem_mcp::pool::Pool;
use rocket_mem_mcp::server::RocketMemMcpServer;

fn rocket_mem_bin() -> PathBuf {
    if let Ok(path) = std::env::var("ROCKET_MEM_BIN") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug/rocket-mem")
}

/// Owns a spawned `rocket-mem` child process and guarantees it is killed — whether the test
/// finishes normally, returns early, or panics mid-assertion. Without this, a panicking test
/// (e.g. a failed `assert!`) would skip its own `child.kill()` call and leak the process.
pub struct RocketMemGuard(Option<Child>);

impl RocketMemGuard {
    /// Kills the child immediately and reaps it. Idempotent — safe to call explicitly (e.g. to
    /// simulate a lost connection mid-test) and again implicitly via `Drop` at scope end.
    pub fn kill(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for RocketMemGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Spawns the real compiled `rocket-mem` binary bound to an OS-assigned port, reading its
/// startup banner on stdout to discover which port it actually got. Returns a guard that kills
/// the process on drop (so it's cleaned up even on panic — see `RocketMemGuard`) and the bound
/// `"host:port"` string. Mirrors `crates/server/tests/kill_and_recover.rs`'s `spawn_server`.
pub fn spawn_rocket_mem(aof_path: &std::path::Path) -> (RocketMemGuard, String) {
    let bin = rocket_mem_bin();
    let mut child = Command::new(&bin)
        .env("ROCKET_MEM_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_AOF_PATH", aof_path)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "failed to spawn {bin:?}: {e} — build it first with `cargo build -p rocket-mem` \
                 from the repo root, or set ROCKET_MEM_BIN"
            )
        });

    let stdout = child.stdout.take().expect("child stdout was not piped");
    let mut reader = BufReader::new(stdout);
    let mut addr = None;
    for _ in 0..20 {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim().trim_matches(|c| c == '│' || c == ' ');
                let mut parts = trimmed.split_whitespace();
                if parts.next() == Some("RESP") {
                    if let Some(addr_str) = parts.next() {
                        addr = Some(addr_str.to_string());
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    let addr = addr.expect("rocket-mem never printed its listening address on stdout");
    (RocketMemGuard(Some(child)), addr)
}

/// Wires up an `RocketMemMcpServer` and an `rmcp` client on opposite ends of an in-process
/// duplex pipe, completes the MCP handshake on both sides, and returns the connected client.
/// Shared by every test file that needs to drive tools through a real MCP client rather than
/// calling Rust functions directly. Each `tests/*.rs` file compiles this `support` module into
/// its own separate test binary, and not every one of those binaries calls this particular
/// helper (e.g. `tests/pool.rs` only needs `spawn_rocket_mem`) — hence `allow(dead_code)` rather
/// than a per-binary warning.
#[allow(dead_code)]
pub async fn connect_client_and_server(pool: Pool) -> RunningService<RoleClient, ()> {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);

    let server = RocketMemMcpServer::new(pool);
    tokio::spawn(async move {
        let running = server
            .serve((server_read, server_write))
            .await
            .expect("server should complete the MCP handshake");
        running.waiting().await.ok();
    });

    ().serve((client_read, client_write))
        .await
        .expect("client should complete the MCP handshake")
}
